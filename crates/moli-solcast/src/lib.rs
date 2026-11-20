//! Solcast: the solar production expected on the house's roofs, half-hour by
//! half-hour, from Solcast's rooftop sites (a free « home user » account
//! allows 10 calls a day). Read only.
//!
//! The day's calls are spread over the hours of light (`model::next_refresh`),
//! the answers kept on disk when the vault can (a restart costs no call), and
//! each half-hour keeps the forecast it had before it passed: today's whole
//! curve, past part included, can be set against what the panels made.
mod model;

use anyhow::ensure;
use http::header::{ACCEPT, AUTHORIZATION, HOST, HeaderValue, USER_AGENT};
use jiff::tz::TimeZone;
use model::{Cache, Slot, Summary};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API_HOST: &str = "api.solcast.com.au";
const SECRET: &str = "api_key";
/// The forecasts and the day's calls, kept in the instance's private state.
const STATE: &str = "forecast";
const NATIVE: &str = "prevision";
const TIMEOUT: Duration = Duration::from_secs(30);
/// Two days of half-hours are about 20 kB: far below this.
const MAX_RESPONSE: usize = 1024 * 1024;
/// How often the device is said again (the power expected now moves every
/// half-hour, what remains of the day all the time).
const TICK: Duration = Duration::from_secs(60);
/// After a network failure, Solcast is asked again this much later.
const RETRY: i64 = 600;
/// After a refused key, this much later (the person fixes it meanwhile).
const RETRY_REFUSED: i64 = 3600;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The rooftop sites' resource ids (solcast.com, « Manage sites »):
    /// their forecasts are summed (two roofs facing two ways).
    pub sites: Vec<String>,
    /// Calls allowed a day (10 for a home user account made after 2024).
    #[serde(default = "default_calls")]
    pub calls_per_day: u32,
    /// The house's time zone: what « today » and « tomorrow » mean.
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_calls() -> u32 {
    10
}

fn default_timezone() -> String {
    "Europe/Paris".into()
}

/// A resource id as Solcast writes them (`abcd-1234-ef56-7890`): nothing
/// else reaches the request's path.
fn valid_site(site: &str) -> bool {
    (4..=64).contains(&site.len()) && site.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Debug)]
pub struct Solcast {
    config: Config,
    tz: TimeZone,
}

impl Solcast {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let invalid = || moli_i18n::tr!("pilotes.solcast.config_invalide");
        ensure!(
            !config.sites.is_empty()
                && config.sites.len() <= 4
                && config.sites.iter().all(|s| valid_site(s)),
            invalid()
        );
        ensure!((1..=1000).contains(&config.calls_per_day), invalid());
        ensure!(
            usize::try_from(config.calls_per_day).is_ok_and(|n| n >= config.sites.len()),
            invalid()
        );
        let tz = TimeZone::get(&config.timezone).map_err(|_| anyhow::anyhow!(invalid()))?;
        Ok(Self { config, tz })
    }
}

impl Driver for Solcast {
    fn kind(&self) -> &'static str {
        "solcast"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.run_inner(ctx))
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Why a call brought nothing.
enum Failure {
    /// 401/403: the key is wrong, or the site is not this account's.
    Refused,
    /// 429: the day's calls are used up.
    Exhausted,
    /// Solcast answered, but not with a forecast.
    Answered,
    /// Solcast could not be reached (the call may not have counted).
    Network,
}

impl Solcast {
    async fn run_inner(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let Some(key) = ctx.secret(SECRET) else {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.solcast.cle_manquante",
                instance = ctx.instance(),
                key = SECRET
            ));
            ctx.cancelled().await;
            return Ok(());
        };
        let bearer = HeaderValue::from_str(&format!("Bearer {}", key.trim()))
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.solcast.cle_illisible")))?;
        let store = ctx.private_state();
        let mut cache: Cache = store
            .as_ref()
            .and_then(|s| s.get(STATE))
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        cache.keep_sites(&self.config.sites);
        ctx.upsert_device(device(ctx.instance()));
        let id = ctx.device_id(NATIVE);
        self.publish(ctx, &id, &cache);
        ctx.ready();

        // Not before this (a failure waits a little; a refused key longer).
        let mut hold_until = 0_i64;
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = ctx.next_command() => {
                    let Some(command) = command else { return Ok(()) };
                    command.reply(Err(moli_i18n::tr!("pilotes.solcast.lecture_seule")));
                }
                _ = tick.tick() => {
                    let now = now_secs();
                    let due = model::next_refresh(&cache, self.config.sites.len(), self.config.calls_per_day, now, &self.tz);
                    if due <= now && now >= hold_until {
                        match self.refresh(&bearer, &mut cache, now).await {
                            Ok(()) => {
                                ctx.clear_warning();
                                tracing::info!(instance = %ctx.instance(), calls = cache.calls, "solcast forecast received");
                            }
                            Err(Failure::Refused) => {
                                hold_until = now + RETRY_REFUSED;
                                ctx.warn(moli_i18n::tr!("pilotes.solcast.cle_refusee", instance = ctx.instance(), key = SECRET));
                            }
                            Err(Failure::Exhausted) => {
                                tracing::warn!(instance = %ctx.instance(), "solcast: the day's calls are used up");
                            }
                            Err(Failure::Answered | Failure::Network) => {
                                hold_until = now + RETRY;
                                tracing::warn!(instance = %ctx.instance(), "solcast unreachable or unreadable, next try later");
                            }
                        }
                        if let Some(store) = &store
                            && let Ok(text) = serde_json::to_string(&cache)
                            && let Err(e) = store.set_if_changed(vec![(STATE.into(), text.clone(), text)]).await
                        {
                            tracing::warn!(instance = %ctx.instance(), error = %e, "solcast: forecast not kept on disk");
                        }
                    }
                    self.publish(ctx, &id, &cache);
                }
            }
        }
    }

    /// Every site asked once; the first refusal stops the round.
    async fn refresh(
        &self,
        bearer: &HeaderValue,
        cache: &mut Cache,
        now: i64,
    ) -> Result<(), Failure> {
        for site in &self.config.sites {
            let answer = match fetch(site, bearer).await {
                Ok(answer) => {
                    cache.count_call(now);
                    answer
                }
                Err(Failure::Exhausted) => {
                    cache.mark_exhausted(now);
                    return Err(Failure::Exhausted);
                }
                Err(Failure::Network) => return Err(Failure::Network),
                Err(other) => {
                    cache.count_call(now);
                    return Err(other);
                }
            };
            cache.merge(site, answer, now);
        }
        Ok(())
    }

    fn publish(&self, ctx: &DriverCtx, id: &DeviceId, cache: &Cache) {
        let now = now_secs();
        let total = cache.total();
        let Summary {
            power_now,
            today,
            remaining,
            tomorrow,
            next_hour,
        } = model::summary(&total, now, &self.tz);
        let kwh =
            |v: Option<f64>| v.map_or(Value::Null, |v| Value::Float((v * 100.0).round() / 100.0));
        let parts = |s: Option<Slot>| {
            (
                kwh(s.map(|s| s.kw)),
                kwh(s.map(|s| s.lo)),
                kwh(s.map(|s| s.hi)),
            )
        };
        ctx.set_state(id, "power_now", power_now.map_or(Value::Null, Value::Float));
        let (t, tl, th) = parts(today);
        ctx.set_state(id, "today_kwh", t);
        ctx.set_state(id, "today_low_kwh", tl);
        ctx.set_state(id, "today_high_kwh", th);
        ctx.set_state(id, "remaining_kwh", kwh(remaining.map(|s| s.kw)));
        let (m, ml, mh) = parts(tomorrow);
        ctx.set_state(id, "tomorrow_kwh", m);
        ctx.set_state(id, "tomorrow_low_kwh", ml);
        ctx.set_state(id, "tomorrow_high_kwh", mh);
        ctx.set_state(id, "next_hour_kwh", kwh(next_hour));
        if let Some(curve) = model::curve(&total, now, &self.tz) {
            ctx.set_state(id, "curve", Value::Text(curve.into()));
        }
        let (used, exhausted) = cache.calls_today(now);
        let left = if exhausted {
            0
        } else {
            self.config.calls_per_day.saturating_sub(used)
        };
        ctx.set_state(id, "calls_left", Value::Float(f64::from(left)));
        if let Some(at) = cache
            .fetched
            .and_then(|s| jiff::Timestamp::from_second(s).ok())
        {
            ctx.set_state(id, "updated", Value::Text(at.to_string().into()));
        }
        ctx.set_availability(id, !total.is_empty());
    }
}

/// One site's forecast, two days ahead.
async fn fetch(
    site: &str,
    bearer: &HeaderValue,
) -> Result<std::collections::BTreeMap<i64, Slot>, Failure> {
    let builder = http::Request::builder()
        .method(http::Method::GET)
        .uri(format!(
            "/rooftop_sites/{site}/forecasts?format=json&hours=48"
        ))
        .header(HOST, API_HOST)
        .header(USER_AGENT, "github.com/rudy-io/moli-os")
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, bearer.clone());
    let request = moli_net::empty(builder).map_err(|_| Failure::Network)?;
    // Underlying errors are dropped on purpose: no request detail (the key)
    // in a log or a status.
    let (status, body) = moli_net::web(API_HOST, 443, true, request, TIMEOUT, MAX_RESPONSE)
        .await
        .map_err(|_| Failure::Network)?;
    match status.as_u16() {
        401 | 403 | 404 => Err(Failure::Refused),
        429 => Err(Failure::Exhausted),
        s if (200..300).contains(&s) => model::parse(&body).map_err(|_| Failure::Answered),
        _ => Err(Failure::Answered),
    }
}

fn spec(key: &str, label: String, kind: Kind, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        // Forecasts, never counters to add up.
        semantic: Semantic::Other,
    }
}

fn number(key: &str, label: String, unit: Unit) -> PointSpec {
    let kind = Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: None,
    };
    spec(key, label, kind, Some(unit))
}

fn device(instance: &InstanceId) -> Device {
    let kwh = |key: &str, label: String| number(key, label, Unit::KiloWattHour);
    Device {
        id: DeviceId::new(instance, NATIVE),
        instance: instance.clone(),
        native_name: moli_i18n::tr!("pilotes.solcast.appareil").into(),
        manufacturer: Some("Solcast".into()),
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            number(
                "power_now",
                moli_i18n::tr!("pilotes.solcast.puissance_maintenant"),
                Unit::Watt,
            ),
            kwh("today_kwh", moli_i18n::tr!("pilotes.solcast.aujourdhui")),
            kwh(
                "today_low_kwh",
                moli_i18n::tr!("pilotes.solcast.aujourdhui_bas"),
            ),
            kwh(
                "today_high_kwh",
                moli_i18n::tr!("pilotes.solcast.aujourdhui_haut"),
            ),
            kwh("remaining_kwh", moli_i18n::tr!("pilotes.solcast.restant")),
            kwh("tomorrow_kwh", moli_i18n::tr!("pilotes.solcast.demain")),
            kwh(
                "tomorrow_low_kwh",
                moli_i18n::tr!("pilotes.solcast.demain_bas"),
            ),
            kwh(
                "tomorrow_high_kwh",
                moli_i18n::tr!("pilotes.solcast.demain_haut"),
            ),
            kwh(
                "next_hour_kwh",
                moli_i18n::tr!("pilotes.solcast.heure_suivante"),
            ),
            spec(
                "curve",
                moli_i18n::tr!("pilotes.solcast.courbe"),
                Kind::Text,
                None,
            ),
            spec(
                "calls_left",
                moli_i18n::tr!("pilotes.solcast.appels_restants"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: None,
                    step: Some(1.0),
                },
                None,
            ),
            spec(
                "updated",
                moli_i18n::tr!("pilotes.solcast.mise_a_jour"),
                Kind::Text,
                None,
            ),
        ],
    }
}

#[cfg(test)]
mod tests;

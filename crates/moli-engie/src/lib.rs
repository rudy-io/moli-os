//! An ENGIE Belgium customer account, through the API of the ENGIE Smart app
//! (unofficial: ENGIE may change it at any time).
//!
//! - **Contract**: the supplier's prices of the current period, per energy,
//!   direction and time slot (`electricity_offtake_peak`, € per kWh, VAT
//!   included), and the capacity tariff's quarter-hour peaks (the month's,
//!   today's, last month's).
//! - **Vehicle** (ENGIE smart charging): its battery and charge state, the
//!   current session's departure, target and « charge now » (writable), and
//!   what smart charging earned: the last session, this month, in all.
//!
//! The account is connected once from the dashboard (see [`auth`]); ENGIE
//! ends the session when the same account signs in elsewhere (the app,
//! engie.be): the account then asks for a new sign-in.

mod auth;
mod encode;
mod model;

use std::collections::HashMap;
use std::time::Duration;

use http::header::{ACCEPT, CONTENT_TYPE, HOST, USER_AGENT};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::auth::Failure;

const REFRESH: &str = "refresh_token";
const API_HOST: &str = "api.engie.be";
/// The endpoints the app calls from its web views.
const BROWSER_AGENT: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36 moli-os";
const LIMIT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 2 * 1024 * 1024;
/// The vehicles and the current session: every 5 minutes.
const FAST: Duration = Duration::from_secs(5 * 60);
/// Prices, peaks and rewards: every 12 fast turns (an hour).
const SLOW_EVERY: u32 = 12;
/// After a failure that is not the session's: try again in a minute.
const RETRY: Duration = Duration::from_secs(60);
/// ENGIE takes a departure at most this far ahead.
const MAX_AHEAD: Duration = Duration::from_secs(7 * 24 * 3600);
const STATUSES: [&str; 4] = ["sign_in", "connecting", "connected", "error"];

fn default_language() -> String {
    "fr".into()
}

fn default_timezone() -> String {
    "Europe/Brussels".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The language of ENGIE's sign-in page: `fr`, `nl`, `en`.
    #[serde(default = "default_language")]
    pub language: String,
    /// The zone of the account's days and months (peaks, departure times).
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// The vehicles Moli follows, by name or number (a shared account may
    /// hold someone else's car); empty: all of them. The others are never
    /// read, and their device goes.
    #[serde(default)]
    pub vehicles: Vec<String>,
}

impl Config {
    fn follows(&self, v: &model::Vehicle) -> bool {
        self.vehicles.is_empty()
            || self.vehicles.iter().any(|w| {
                let w = w.trim();
                w.eq_ignore_ascii_case(&v.name) || w == v.id.to_string()
            })
    }
}

#[derive(Debug)]
pub struct Engie {
    config: Config,
}

impl Engie {
    /// # Errors
    /// An unknown time zone.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        TimeZone::get(&config.timezone)
            .map_err(|e| anyhow::anyhow!("timezone {:?}: {e}", config.timezone))?;
        Ok(Self { config })
    }
}

impl Driver for Engie {
    fn kind(&self) -> &'static str {
        "engie"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn point(key: &str, label: &str, kind: Kind, write: bool, unit: Option<&str>) -> PointSpec {
    // Units are given as they are: a price, a peak in kW or a session's kWh
    // must never pass for the house's own power or energy.
    let unit = unit.map(|u| Unit::Other(u.into()));
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::Other,
        kind,
        access: Access { read: true, write },
        unit,
    }
}

fn number() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

fn set_text(ctx: &DriverCtx, id: &DeviceId, key: &str, text: &str) {
    ctx.set_state(id, key, Value::Text(text.into()));
}

fn set_number(ctx: &DriverCtx, id: &DeviceId, key: &str, n: Option<f64>) {
    if let Some(n) = n {
        ctx.set_state(id, key, Value::Float((n * 10_000.0).round() / 10_000.0));
    }
}

/// The account: its state, and where the dashboard signs it in.
fn account_device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.engie.compte").into(),
        manufacturer: Some("ENGIE".into()),
        model: Some(moli_i18n::tr!("pilotes.engie.modele_compte").into()),
        description: Some(moli_i18n::tr!("pilotes.engie.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            point(
                "status",
                &moli_i18n::tr!("pilotes.engie.etat_compte"),
                Kind::Enum {
                    values: STATUSES.iter().map(|s| (*s).into()).collect(),
                },
                false,
                None,
            ),
            point(
                "sign_in_url",
                &moli_i18n::tr!("pilotes.engie.page_engie"),
                Kind::Text,
                false,
                None,
            ),
            point(
                "sign_in",
                &moli_i18n::tr!("pilotes.engie.adresse_collee"),
                Kind::Text,
                true,
                None,
            ),
        ],
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let tz = TimeZone::get(&config.timezone)?;
    let account = ctx.device_id("account");
    ctx.upsert_device(account_device(ctx, &account));
    ctx.set_availability(&account, true);
    ctx.ready();
    // The stored token was refused: only a new sign-in helps.
    let mut refused = false;
    loop {
        let stored = ctx.secret(REFRESH).filter(|t| !t.is_empty() && !refused);
        let tokens = if let Some(stored) = stored {
            set_text(ctx, &account, "status", "connecting");
            match auth::refresh(&stored).await {
                Ok(tokens) => {
                    keep(ctx, &tokens).await;
                    tokens
                }
                Err(Failure::Refused) => {
                    tracing::warn!(instance = %ctx.instance(), "engie session refused: sign in again");
                    refused = true;
                    continue;
                }
                Err(Failure::Other(e)) => {
                    tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "engie unreachable");
                    set_text(ctx, &account, "status", "error");
                    if pause(ctx, RETRY).await {
                        return Ok(());
                    }
                    continue;
                }
            }
        } else {
            match sign_in(config, ctx, &account).await? {
                Some(tokens) => tokens,
                None => return Ok(()), // shutting down
            }
        };
        refused = false;
        match connected(config, ctx, &tz, &account, tokens).await {
            Ok(()) => return Ok(()),
            Err(Failure::Refused) => {
                tracing::warn!(instance = %ctx.instance(), "engie session refused: sign in again");
                refused = true;
            }
            Err(Failure::Other(e)) => {
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "engie failed");
                set_text(ctx, &account, "status", "error");
                if pause(ctx, RETRY).await {
                    return Ok(());
                }
            }
        }
    }
}

/// Waits `d`, or until Moli stops (true).
async fn pause(ctx: &DriverCtx, d: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(d) => false,
        () = ctx.cancelled() => true,
    }
}

/// Files the refresh token ENGIE just gave (the previous one is spent).
async fn keep(ctx: &DriverCtx, tokens: &auth::Tokens) {
    if let Err(e) = ctx.store_secret(REFRESH, &tokens.refresh).await {
        tracing::warn!(instance = %ctx.instance(), error = %e, "engie: the new session could not be filed: a sign-in will be needed after a restart");
    }
}

/// Waits for the person to sign in from the dashboard; `None` when Moli stops.
async fn sign_in(
    config: &Config,
    ctx: &mut DriverCtx,
    account: &DeviceId,
) -> anyhow::Result<Option<auth::Tokens>> {
    let mut pending = auth::SignIn::new(&config.language)?;
    set_text(ctx, account, "sign_in_url", &pending.url);
    set_text(ctx, account, "status", "sign_in");
    loop {
        let Some(command) = ctx.next_command().await else {
            return Ok(None);
        };
        if command.device.id != *account || &*command.key != "sign_in" {
            command.reply(Err(moli_i18n::tr!("pilotes.engie.pas_connecte")));
            continue;
        }
        let Value::Text(pasted) = &command.value else {
            command.reply(Err(moli_i18n::tr!("pilotes.engie.adresse_fausse")));
            continue;
        };
        let code = match pending.code(pasted) {
            Ok(code) => code,
            Err(why) => {
                command.reply(Err(match why {
                    auth::Pasted::NotTheEnd => moli_i18n::tr!("pilotes.engie.adresse_fausse"),
                    auth::Pasted::Stale => moli_i18n::tr!("pilotes.engie.adresse_ancienne"),
                    auth::Pasted::NoCode => moli_i18n::tr!("pilotes.engie.adresse_sans_code"),
                }));
                continue;
            }
        };
        match pending.tokens(&code).await {
            Ok(tokens) => {
                if let Err(e) = ctx.store_secret(REFRESH, &tokens.refresh).await {
                    command.reply(Err(moli_i18n::tr!("pilotes.engie.coffre", why = e)));
                    continue;
                }
                set_text(ctx, account, "sign_in_url", "");
                command.reply(Ok(()));
                tracing::info!(instance = %ctx.instance(), "engie account connected");
                return Ok(Some(tokens));
            }
            Err(e) => {
                // A code serves once: a new page for the next try.
                pending = auth::SignIn::new(&config.language)?;
                set_text(ctx, account, "sign_in_url", &pending.url);
                command.reply(Err(moli_i18n::tr!(
                    "pilotes.engie.connexion_refusee",
                    why = e
                )));
            }
        }
    }
}

/// The API, with a session that renews itself.
struct Api {
    tokens: auth::Tokens,
}

impl Api {
    async fn renew(&mut self, ctx: &DriverCtx) -> Result<(), Failure> {
        let tokens = auth::refresh(&self.tokens.refresh).await?;
        keep(ctx, &tokens).await;
        self.tokens = tokens;
        Ok(())
    }

    /// One call: its JSON, `None` for a 404 (nothing there, e.g. no session yet).
    async fn call(
        &mut self,
        ctx: &DriverCtx,
        method: &http::Method,
        path: &str,
        agent: &str,
        body: Option<&Json>,
    ) -> Result<Option<Json>, Failure> {
        if self.tokens.stale() {
            self.renew(ctx).await?;
        }
        for attempt in 0..2 {
            let builder = http::Request::builder()
                .method(method.clone())
                .uri(path)
                .header(HOST, API_HOST)
                .header(USER_AGENT, agent)
                .header(ACCEPT, "application/json, application/problem+json")
                .header("authorization", format!("Bearer {}", self.tokens.access));
            let request = match body {
                Some(body) => builder.header(CONTENT_TYPE, "application/json").body(
                    http_body_util::Full::new(moli_net::Body::from(body.to_string())),
                ),
                None => builder.body(http_body_util::Full::new(moli_net::Body::new())),
            }
            .map_err(|e| Failure::Other(e.into()))?;
            // No request detail in an error: the path holds account numbers.
            let (status, bytes) = moli_net::web(API_HOST, 443, true, request, LIMIT, MAX_BODY)
                .await
                .map_err(|_| Failure::Other(anyhow::anyhow!("{API_HOST} unreachable")))?;
            match status.as_u16() {
                401 if attempt == 0 => self.renew(ctx).await?,
                401 | 403 => return Err(Failure::Refused),
                404 => return Ok(None),
                s if (200..300).contains(&s) => {
                    return Ok(Some(serde_json::from_slice(&bytes).unwrap_or(Json::Null)));
                }
                s => {
                    return Err(Failure::Other(anyhow::anyhow!("{API_HOST}: HTTP {s}")));
                }
            }
        }
        Err(Failure::Refused)
    }

    async fn get(
        &mut self,
        ctx: &DriverCtx,
        path: &str,
        agent: &str,
    ) -> Result<Option<Json>, Failure> {
        self.call(ctx, &http::Method::GET, path, agent, None).await
    }
}

/// Ignores what failed for another reason than the session (logged once
/// per new reason); the session's refusal goes up.
fn tolerate<T>(
    ctx: &DriverCtx,
    last: &mut Option<String>,
    r: Result<T, Failure>,
) -> Result<Option<T>, Failure> {
    match r {
        Ok(v) => {
            *last = None;
            Ok(Some(v))
        }
        Err(Failure::Refused) => Err(Failure::Refused),
        Err(Failure::Other(e)) => {
            let e = format!("{e:#}");
            if last.as_deref() != Some(e.as_str()) {
                tracing::warn!(instance = %ctx.instance(), error = %e, "engie: a reading failed");
                *last = Some(e);
            }
            Ok(None)
        }
    }
}

/// What the session knows of the account.
#[derive(Default)]
struct Known {
    can: Option<String>,
    agreements: Vec<model::Agreement>,
    /// EAN → `ELECTRICITY` / `GAS`.
    divisions: HashMap<String, String>,
    /// Contract → the price keys its device has.
    price_keys: HashMap<String, Vec<String>>,
    vehicles: HashMap<u64, DeviceId>,
    last_error: Option<String>,
    told_no_vehicle: bool,
    left_out: std::collections::HashSet<u64>,
    /// Vehicle → its name, as sessions name it.
    names: HashMap<u64, String>,
}

/// A JSON answer's shape, for a log: keys and list lengths, never a value.
fn shape(json: &Json) -> String {
    match json {
        Json::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| match v {
                    Json::Array(a) => format!("{k}[{}]", a.len()),
                    Json::Object(_) => format!("{k}{{…}}"),
                    _ => k.clone(),
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Json::Array(a) => format!("[{}]", a.len()),
        _ => "scalar".to_owned(),
    }
}

async fn connected(
    config: &Config,
    ctx: &mut DriverCtx,
    tz: &TimeZone,
    account: &DeviceId,
    tokens: auth::Tokens,
) -> Result<(), Failure> {
    let mut api = Api { tokens };
    let relations = api
        .get(
            ctx,
            "/engie/ms/accounts/customer/v1/customer-account-relations?withBusinessAgreements=SMART_APP",
            auth::APP_AGENT,
        )
        .await?
        .unwrap_or(Json::Null);
    let found = model::account(&relations);
    let mut known = Known {
        can: found.number,
        agreements: found.agreements,
        ..Known::default()
    };
    set_text(ctx, account, "status", "connected");
    tracing::info!(
        instance = %ctx.instance(),
        contracts = known.agreements.len(),
        "engie account read"
    );
    let mut tick = tokio::time::interval(FAST);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut turn: u32 = 0;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if turn.is_multiple_of(SLOW_EVERY) {
                    slow(ctx, tz, &mut api, &mut known).await?;
                }
                fast(config, ctx, tz, &mut api, &mut known).await?;
                turn = turn.wrapping_add(1);
            }
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = order(ctx, tz, &mut api, &known, &command).await;
                match result {
                    Err(Failure::Refused) => {
                        command.reply(Err(moli_i18n::tr!("pilotes.engie.session_perdue")));
                        return Err(Failure::Refused);
                    }
                    Err(Failure::Other(e)) => command.reply(Err(format!("{e:#}"))),
                    Ok(Err(why)) => command.reply(Err(why)),
                    Ok(Ok(())) => command.reply(Ok(())),
                }
            }
        }
    }
}

/// Midnight today and the first of the month, in the account's zone.
fn day_and_month(
    tz: &TimeZone,
    now: Timestamp,
) -> Option<(Timestamp, Timestamp, jiff::civil::Date)> {
    let today = now.to_zoned(tz.clone()).date();
    let day = today.to_zoned(tz.clone()).ok()?.timestamp();
    let first = jiff::civil::Date::new(today.year(), today.month(), 1).ok()?;
    let month = first.to_zoned(tz.clone()).ok()?.timestamp();
    Some((day, month, today))
}

fn local(tz: &TimeZone, t: Timestamp) -> String {
    t.to_zoned(tz.clone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// Prices and peaks of each contract, and the rewards of smart charging.
async fn slow(
    ctx: &DriverCtx,
    tz: &TimeZone,
    api: &mut Api,
    known: &mut Known,
) -> Result<(), Failure> {
    let Some((day, _, today)) = day_and_month(tz, Timestamp::now()) else {
        return Ok(());
    };
    for agreement in known.agreements.clone() {
        let ban = &agreement.number;
        let path = format!(
            "/engie/ms/billing/customer/v1/business-agreements/{ban}/supplier-energy-prices?maxGranularity=MONTHLY"
        );
        let r = api.get(ctx, &path, BROWSER_AGENT).await;
        let prices_json = tolerate(ctx, &mut known.last_error, r)?.flatten();
        let mut prices = Vec::new();
        if let Some(json) = &prices_json {
            for item in json["items"].as_array().into_iter().flatten() {
                let Some(ean) = item["ean"].as_str() else {
                    continue;
                };
                if known.divisions.contains_key(ean) {
                    continue;
                }
                let path = format!("/engie/ms/premises/customer/v1/service-points/{ean}");
                let r = api.get(ctx, &path, BROWSER_AGENT).await;
                if let Some(division) = tolerate(ctx, &mut known.last_error, r)?
                    .flatten()
                    .as_ref()
                    .and_then(model::division)
                {
                    known.divisions.insert(ean.to_owned(), division);
                }
            }
            prices = model::prices(json, today, |ean| known.divisions.get(ean).cloned());
        }
        let path = format!(
            "/engie/ms/b2c-energy-insights/v1/private/customers/me/contract-accounts/{ban}/energy-insights/peaks?year={}&month={}",
            today.year(),
            today.month()
        );
        let r = api.get(ctx, &path, auth::APP_AGENT).await;
        let peaks = tolerate(ctx, &mut known.last_error, r)?
            .flatten()
            .map(|j| model::peaks(&j, day));

        let id = ctx.device_id(&format!("contract-{ban}"));
        let keys: Vec<String> = prices.iter().map(|p| p.key.clone()).collect();
        if prices_json.is_some() && known.price_keys.get(ban) != Some(&keys) {
            ctx.upsert_device(contract_device(ctx, &id, &agreement, &keys));
            known.price_keys.insert(ban.clone(), keys);
        } else if !known.price_keys.contains_key(ban) {
            ctx.upsert_device(contract_device(ctx, &id, &agreement, &[]));
            known.price_keys.insert(ban.clone(), Vec::new());
        }
        ctx.set_availability(&id, true);
        for p in &prices {
            set_number(ctx, &id, &p.key, Some(p.value));
        }
        if let Some(peaks) = peaks {
            set_number(
                ctx,
                &id,
                "peak_month_kw",
                peaks.month.as_ref().map(|p| p.kw),
            );
            if let Some(p) = &peaks.month {
                set_text(ctx, &id, "peak_month_at", &local(tz, p.start));
            }
            set_number(
                ctx,
                &id,
                "peak_today_kw",
                Some(peaks.today.map_or(0.0, |p| p.kw)),
            );
            set_number(
                ctx,
                &id,
                "peak_previous_month_kw",
                peaks.previous_month.map(|p| p.kw),
            );
        }
    }

    rewards(ctx, tz, api, known).await
}

/// At most this many pages of sessions (20 each) are read for the rewards.
const MAX_PAGES: u64 = 25;

/// What smart charging earned for each followed vehicle, from its own
/// sessions of the last two years (the account's summary would mix in the
/// other cars of a shared account).
async fn rewards(
    ctx: &DriverCtx,
    tz: &TimeZone,
    api: &mut Api,
    known: &mut Known,
) -> Result<(), Failure> {
    let (Some(can), Some((_, month, today))) =
        (known.can.clone(), day_and_month(tz, Timestamp::now()))
    else {
        return Ok(());
    };
    if known.vehicles.is_empty() {
        return Ok(());
    }
    let from = jiff::civil::Date::new(today.year() - 2, today.month(), 1).unwrap_or(today);
    let mut by_vehicle = HashMap::new();
    let mut page = 0;
    loop {
        let path = format!(
            "/engie/ms/ev/customer/v2/customer-accounts/{can}/charging-sessions?startDate={from}&endDate={today}&pageSize=20&pageNumber={page}"
        );
        let r = api.get(ctx, &path, auth::APP_AGENT).await;
        let Some(json) = tolerate(ctx, &mut known.last_error, r)?.flatten() else {
            return Ok(()); // incomplete: the figures stay as they were
        };
        let pages = model::add_rewards(&json, month, &mut by_vehicle);
        page += 1;
        if page >= pages.min(MAX_PAGES) {
            break;
        }
    }
    for (vehicle, id) in &known.vehicles {
        let name = known.names.get(vehicle).map(|n| n.to_lowercase());
        let r = name
            .and_then(|n| by_vehicle.get(&n).copied())
            .unwrap_or_default();
        set_number(ctx, id, "reward_month", Some(r.month));
        set_number(ctx, id, "reward_total", Some(r.total));
    }
    Ok(())
}

fn price_label(key: &str) -> String {
    let word = |part: &str| match part {
        "electricity" => moli_i18n::tr!("pilotes.engie.electricite"),
        "gas" => moli_i18n::tr!("pilotes.engie.gaz"),
        "offtake" => moli_i18n::tr!("pilotes.engie.prelevement"),
        "injection" => moli_i18n::tr!("pilotes.engie.injection"),
        "peak" => moli_i18n::tr!("pilotes.engie.heures_pleines"),
        "offpeak" => moli_i18n::tr!("pilotes.engie.heures_creuses"),
        "superoffpeak" => moli_i18n::tr!("pilotes.engie.super_creuses"),
        other => other.replace('_', " "),
    };
    let mut parts = key.splitn(3, '_');
    let energy = parts.next().map(word).unwrap_or_default();
    let direction = parts.next().map(word).unwrap_or_default();
    let slot = parts.next().map(word);
    match slot {
        Some(slot) => moli_i18n::tr!(
            "pilotes.engie.prix_plage",
            energie = energy,
            sens = direction,
            plage = slot
        ),
        None => moli_i18n::tr!("pilotes.engie.prix", energie = energy, sens = direction),
    }
}

fn contract_device(
    ctx: &DriverCtx,
    id: &DeviceId,
    agreement: &model::Agreement,
    price_keys: &[String],
) -> Device {
    let mut points: Vec<PointSpec> = price_keys
        .iter()
        .map(|k| point(k, &price_label(k), number(), false, Some("€/kWh")))
        .collect();
    points.extend([
        point(
            "peak_month_kw",
            &moli_i18n::tr!("pilotes.engie.pic_mois"),
            number(),
            false,
            Some("kW"),
        ),
        point(
            "peak_month_at",
            &moli_i18n::tr!("pilotes.engie.pic_mois_quand"),
            Kind::Text,
            false,
            None,
        ),
        point(
            "peak_today_kw",
            &moli_i18n::tr!("pilotes.engie.pic_jour"),
            number(),
            false,
            Some("kW"),
        ),
        point(
            "peak_previous_month_kw",
            &moli_i18n::tr!("pilotes.engie.pic_mois_precedent"),
            number(),
            false,
            Some("kW"),
        ),
    ]);
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.engie.contrat").into(),
        manufacturer: Some("ENGIE".into()),
        model: Some(moli_i18n::tr!("pilotes.engie.modele_contrat").into()),
        description: agreement.address.clone().map(Into::into),
        native_room: None,
        members: Vec::new(),
        points,
    }
}

#[allow(clippy::too_many_lines)] // one line per point
fn vehicle_device(ctx: &DriverCtx, id: &DeviceId, v: &model::Vehicle) -> Device {
    let tr = |k: &str| moli_i18n::tr!(k);
    let mut target = point(
        "target_level",
        &tr("pilotes.engie.objectif"),
        Kind::Numeric {
            min: Some(0.0),
            max: Some(100.0),
            step: Some(1.0),
        },
        true,
        None,
    );
    target.unit = Some(Unit::Percent);
    let mut soc = point("soc", &tr("pilotes.engie.batterie"), number(), false, None);
    soc.unit = Some(Unit::Percent);
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: v.name.clone().into(),
        manufacturer: v.brand.clone().map(Into::into),
        model: Some(
            v.model
                .clone()
                .unwrap_or_else(|| tr("pilotes.engie.vehicule"))
                .into(),
        ),
        description: Some(tr("pilotes.engie.description_vehicule").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            soc,
            point(
                "range_km",
                &tr("pilotes.engie.autonomie"),
                number(),
                false,
                Some("km"),
            ),
            point(
                "charge_power_kw",
                &tr("pilotes.engie.puissance_charge"),
                number(),
                false,
                Some("kW"),
            ),
            point(
                "charge_status",
                &tr("pilotes.engie.etat_charge"),
                Kind::Text,
                false,
                None,
            ),
            point(
                "plugged",
                &tr("pilotes.engie.branche"),
                Kind::Binary,
                false,
                None,
            ),
            point(
                "reachable",
                &tr("pilotes.engie.joignable"),
                Kind::Binary,
                false,
                None,
            ),
            point(
                "departure",
                &tr("pilotes.engie.depart"),
                Kind::Text,
                true,
                None,
            ),
            target,
            point(
                "direct",
                &tr("pilotes.engie.charger_maintenant"),
                Kind::Binary,
                true,
                None,
            ),
            point(
                "session_kwh",
                &tr("pilotes.engie.session_kwh"),
                number(),
                false,
                Some("kWh"),
            ),
            point(
                "session_cost",
                &tr("pilotes.engie.session_cout"),
                number(),
                false,
                Some("€"),
            ),
            point(
                "session_reward",
                &tr("pilotes.engie.session_gain"),
                number(),
                false,
                Some("€"),
            ),
            point(
                "session_outcome",
                &tr("pilotes.engie.session_resultat"),
                Kind::Text,
                false,
                None,
            ),
            point(
                "reward_month",
                &tr("pilotes.engie.gain_mois"),
                number(),
                false,
                Some("€"),
            ),
            point(
                "reward_total",
                &tr("pilotes.engie.gain_total"),
                number(),
                false,
                Some("€"),
            ),
        ],
    }
}

/// The vehicles, their last session and its settings.
async fn fast(
    config: &Config,
    ctx: &DriverCtx,
    tz: &TimeZone,
    api: &mut Api,
    known: &mut Known,
) -> Result<(), Failure> {
    let Some(can) = known.can.clone() else {
        return Ok(());
    };
    let path =
        format!("/engie/ms/ev/customer/v2/customer-accounts/{can}/vehicles?includeInactive=true");
    let r = api.get(ctx, &path, auth::APP_AGENT).await;
    let answer = tolerate(ctx, &mut known.last_error, r)?;
    let found = answer
        .as_ref()
        .and_then(Option::as_ref)
        .map(model::vehicles)
        .unwrap_or_default();
    let (found, others): (Vec<_>, Vec<_>) = found.into_iter().partition(|v| config.follows(v));
    for v in others {
        // Someone else's car: not followed, its device gone (once).
        if known.left_out.insert(v.id) {
            ctx.remove_device(&ctx.device_id(&format!("vehicle-{}", v.id)));
            tracing::info!(instance = %ctx.instance(), "engie: a vehicle left out (option vehicles)");
        }
    }
    if found.is_empty() && !known.told_no_vehicle {
        // Once: what came back, by its shape only (no value of the account).
        let shape = match &answer {
            None => "unreachable".to_owned(),
            Some(None) => "404".to_owned(),
            Some(Some(json)) => shape(json),
        };
        tracing::info!(instance = %ctx.instance(), shape, "engie: no smart-charging vehicle");
        known.told_no_vehicle = true;
    }
    let mut first_sight = false;
    for v in found {
        let id = ctx.device_id(&format!("vehicle-{}", v.id));
        if let std::collections::hash_map::Entry::Vacant(e) = known.vehicles.entry(v.id) {
            ctx.upsert_device(vehicle_device(ctx, &id, &v));
            e.insert(id.clone());
            known.names.insert(v.id, v.name.clone());
            first_sight = true;
        }
        ctx.set_availability(&id, true);
        set_number(ctx, &id, "soc", v.battery);
        set_number(ctx, &id, "range_km", v.range_km);
        set_number(ctx, &id, "charge_power_kw", v.power_kw);
        if let Some(status) = &v.status {
            set_text(ctx, &id, "charge_status", status);
            ctx.set_state(&id, "plugged", Value::Bool(status != "unplugged"));
        }
        if let Some(r) = v.reachable {
            ctx.set_state(&id, "reachable", Value::Bool(r));
        }
        let path = format!(
            "/engie/ms/ev/customer/v2/vehicles/{}/charging-sessions/latest",
            v.id
        );
        let r = api.get(ctx, &path, auth::APP_AGENT).await;
        if let Some(s) = tolerate(ctx, &mut known.last_error, r)?
            .flatten()
            .as_ref()
            .and_then(model::session)
        {
            set_number(ctx, &id, "session_kwh", s.kwh);
            set_number(ctx, &id, "session_cost", s.cost);
            set_number(ctx, &id, "session_reward", s.reward);
            if let Some(outcome) = s.outcome.or(s.status) {
                set_text(ctx, &id, "session_outcome", &outcome);
            }
        }
        let r = latest_settings(ctx, api, v.id).await;
        if let Some(Some(settings)) = tolerate(ctx, &mut known.last_error, r)? {
            publish_settings(ctx, tz, &id, &settings);
        }
    }
    // The rewards of a vehicle seen for the first time: at once, not in an hour.
    if first_sight {
        rewards(ctx, tz, api, known).await?;
    }
    Ok(())
}

async fn latest_settings(
    ctx: &DriverCtx,
    api: &mut Api,
    vehicle: u64,
) -> Result<Option<model::Settings>, Failure> {
    let path = format!(
        "/engie/ms/ev/customer/v2/vehicles/{vehicle}/charging-sessions/latest/charge-settings"
    );
    Ok(api
        .get(ctx, &path, auth::APP_AGENT)
        .await?
        .map(|j| model::settings(&j)))
}

fn publish_settings(ctx: &DriverCtx, tz: &TimeZone, id: &DeviceId, s: &model::Settings) {
    if let Some(d) = s.departure {
        set_text(ctx, id, "departure", &local(tz, d));
    }
    if let Some(t) = s.target {
        ctx.set_state(id, "target_level", Value::Float(f64::from(t)));
    }
    if let Some(direct) = s.direct {
        ctx.set_state(id, "direct", Value::Bool(direct));
    }
}

/// A departure as a person or an automation gives it: `07:30` (the next
/// one), `2026-11-12 07:30` (the house's time) or a full RFC 3339 instant.
/// It must be ahead, and at most 7 days ahead.
fn departure(text: &str, tz: &TimeZone, now: Timestamp) -> Result<Timestamp, String> {
    let text = text.trim();
    let wrong = || moli_i18n::tr!("pilotes.engie.depart_illisible", texte = text);
    let at = if let Ok(t) = text.parse::<Timestamp>() {
        t
    } else if let Ok(dt) = text.replacen(' ', "T", 1).parse::<jiff::civil::DateTime>() {
        dt.to_zoned(tz.clone()).map_err(|_| wrong())?.timestamp()
    } else {
        let time = if text.len() == 5 {
            format!("{text}:00")
        } else {
            text.to_owned()
        };
        let time: jiff::civil::Time = time.parse().map_err(|_| wrong())?;
        let today = now.to_zoned(tz.clone()).date();
        let mut at = today
            .to_datetime(time)
            .to_zoned(tz.clone())
            .map_err(|_| wrong())?
            .timestamp();
        if at <= now {
            let tomorrow = today.tomorrow().map_err(|_| wrong())?;
            at = tomorrow
                .to_datetime(time)
                .to_zoned(tz.clone())
                .map_err(|_| wrong())?
                .timestamp();
        }
        at
    };
    let ahead = at.duration_since(now);
    if ahead.is_negative() || ahead.is_zero() {
        return Err(moli_i18n::tr!("pilotes.engie.depart_passe"));
    }
    if ahead.unsigned_abs() > MAX_AHEAD {
        return Err(moli_i18n::tr!("pilotes.engie.depart_trop_loin"));
    }
    Ok(at)
}

/// A command: the sign-in again (already connected), or a vehicle's session.
async fn order(
    ctx: &DriverCtx,
    tz: &TimeZone,
    api: &mut Api,
    known: &Known,
    command: &CommandRequest,
) -> Result<Result<(), String>, Failure> {
    let Some(vehicle) = known
        .vehicles
        .iter()
        .find(|(_, id)| **id == command.device.id)
        .map(|(v, _)| *v)
    else {
        return Ok(Err(moli_i18n::tr!("pilotes.engie.deja_connecte")));
    };
    let Some(current) = latest_settings(ctx, api, vehicle).await? else {
        return Ok(Err(moli_i18n::tr!("pilotes.engie.pas_de_session")));
    };
    let (mut departure_at, mut target, mut direct) = (
        current.departure,
        current.target,
        current.direct.unwrap_or(false),
    );
    match (&*command.key, &command.value) {
        ("departure", Value::Text(text)) => match departure(text, tz, Timestamp::now()) {
            Ok(at) => departure_at = Some(at),
            Err(why) => return Ok(Err(why)),
        },
        ("target_level", v) => {
            let Some(n) = v.as_f64().filter(|n| (0.0..=100.0).contains(n)) else {
                return Ok(Err(moli_i18n::tr!("pilotes.engie.objectif_invalide")));
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = n.round() as u8;
            target = Some(n);
        }
        ("direct", Value::Bool(b)) => direct = *b,
        (key, _) => {
            return Ok(Err(moli_i18n::tr!(
                "pilotes.engie.lecture_seule",
                point = key
            )));
        }
    }
    let (Some(departure_at), Some(target)) = (departure_at, target) else {
        return Ok(Err(moli_i18n::tr!("pilotes.engie.pas_de_session")));
    };
    let body = model::settings_body(departure_at, target, direct);
    let path = format!(
        "/engie/ms/ev/customer/v2/vehicles/{vehicle}/charging-sessions/latest/charge-settings"
    );
    let answer = api
        .call(ctx, &http::Method::PUT, &path, auth::APP_AGENT, Some(&body))
        .await?;
    let Some(answer) = answer else {
        return Ok(Err(moli_i18n::tr!("pilotes.engie.pas_de_session")));
    };
    publish_settings(ctx, tz, &command.device.id, &model::settings(&answer));
    tracing::info!(instance = %ctx.instance(), key = %command.key, "engie: charge settings changed");
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brussels() -> TimeZone {
        TimeZone::get("Europe/Brussels").unwrap()
    }

    #[test]
    fn a_departure_is_the_next_one_and_at_most_a_week_ahead() {
        let tz = brussels();
        // 11 November 2026, 10:00 in Brussels (UTC+1).
        let now: Timestamp = "2026-11-11T09:00:00Z".parse().unwrap();
        // Later today.
        assert_eq!(
            departure("18:30", &tz, now).unwrap(),
            "2026-11-11T17:30:00Z".parse::<Timestamp>().unwrap()
        );
        // Already past today: tomorrow.
        assert_eq!(
            departure("07:30", &tz, now).unwrap(),
            "2026-11-12T06:30:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            departure("2026-11-13 06:00", &tz, now).unwrap(),
            "2026-11-13T05:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            departure("2026-11-12T06:30:00Z", &tz, now).unwrap(),
            "2026-11-12T06:30:00Z".parse::<Timestamp>().unwrap()
        );
        assert!(departure("2026-11-10 07:00", &tz, now).is_err(), "past");
        assert!(departure("2026-11-30 07:00", &tz, now).is_err(), "too far");
        assert!(departure("demain", &tz, now).is_err());
    }

    #[test]
    fn days_and_months_start_in_the_accounts_zone() {
        let tz = brussels();
        let now: Timestamp = "2026-11-11T09:00:00Z".parse().unwrap();
        let (day, month, today) = day_and_month(&tz, now).unwrap();
        assert_eq!(day, "2026-11-10T23:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(month, "2026-10-31T23:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(today.to_string(), "2026-11-11");
    }

    #[test]
    fn price_points_have_readable_labels() {
        assert!(!price_label("electricity_offtake_peak").is_empty());
        assert!(!price_label("gas_offtake").is_empty());
    }

    #[test]
    fn only_the_vehicles_asked_for_are_followed() {
        let config = |v: &[&str]| Config {
            language: "fr".into(),
            timezone: "Europe/Brussels".into(),
            vehicles: v.iter().map(|s| (*s).to_owned()).collect(),
        };
        let car = |id, name: &str| model::Vehicle {
            id,
            name: name.into(),
            ..model::Vehicle::default()
        };
        assert!(config(&[]).follows(&car(1, "Ma voiture")));
        let mine = config(&["ma voiture"]);
        assert!(mine.follows(&car(1, "Ma voiture")));
        assert!(!mine.follows(&car(2, "Model 3")));
        assert!(config(&["2"]).follows(&car(2, "Model 3")));
    }

    #[test]
    fn a_shape_tells_no_value() {
        let json = serde_json::json!({"items": [{"vin": "SECRET"}], "page": {"n": 1}, "total": 7});
        let s = shape(&json);
        assert_eq!(s, "{items[1], page{…}, total}");
        assert!(!s.contains("SECRET"));
    }

    #[test]
    fn an_unknown_zone_is_refused() {
        assert!(
            Engie::new(Config {
                language: "fr".into(),
                timezone: "Nowhere/Never".into(),
                vehicles: Vec::new(),
            })
            .is_err()
        );
    }
}

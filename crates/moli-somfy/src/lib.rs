//! Somfy Protect alarms (Home Alarm, Somfy One), through Somfy's cloud:
//! read-only for now. Each site (an alarm) is a Moli device with its mode,
//! each of its devices (Link, detectors, sirens, remotes, cameras) another,
//! hanging from it, with its battery, radio and temperature when it has them.
//!
//! The account (`email`, `password`) and the app's OAuth client
//! (`client_id`, `client_secret`) come from the encrypted secret store; the
//! token is kept there as well and refreshed. A guest account of its own is
//! best: Moli's reads then never get mixed with a person's.

mod cloud;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

use crate::cloud::{Client, Cloud, Refusal, Session};

const EMAIL: &str = "email";
const PASSWORD: &str = "password";
const CLIENT_ID: &str = "client_id";
const CLIENT_SECRET: &str = "client_secret";
/// Written by the driver: the token and its refresh.
const SESSION: &str = "session";

/// The sites and devices are read again this often.
const RESCAN: Duration = Duration::from_secs(60);

/// Somfy's levels and their Moli value (as for other alarms: HomeKit's
/// « Home » is the partial mode, « Away » the full one).
const MODES: [(&str, &str); 3] = [
    ("disarmed", "disarmed"),
    ("partial", "home"),
    ("armed", "away"),
];

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {}

#[derive(Debug)]
pub struct Somfy {
    _config: Config,
}

impl Somfy {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { _config: config }
    }
}

impl Driver for Somfy {
    fn kind(&self) -> &'static str {
        "somfy"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(ctx))
    }
}

fn now_s() -> i64 {
    i64::try_from(moli_core::now_ms() / 1000).unwrap_or(i64::MAX)
}

/// Waits for a person (shown everywhere), until Moli stops.
async fn wait(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

async fn save(ctx: &DriverCtx, session: &Session) -> anyhow::Result<()> {
    ctx.store_secret(SESSION, &serde_json::to_string(session)?)
        .await
        .context("saving the Somfy session")
}

async fn run(ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let instance = ctx.instance().to_string();
    let found = (
        ctx.secret(EMAIL),
        ctx.secret(PASSWORD),
        ctx.secret(CLIENT_ID),
        ctx.secret(CLIENT_SECRET),
    );
    let (Some(email), Some(password), Some(id), Some(secret)) = found else {
        let reason = moli_i18n::tr!(
            "pilotes.somfy.identifiants_manquants",
            instance = instance,
            email = EMAIL,
            password = PASSWORD,
            client_id = CLIENT_ID,
            client_secret = CLIENT_SECRET
        );
        return wait(ctx, reason).await;
    };
    if !ctx.can_store_secrets() {
        anyhow::bail!("the Somfy session cannot be kept: the secret store is unavailable");
    }
    let client = Client {
        id: id.trim().to_owned(),
        secret: secret.trim().to_owned(),
    };
    let session = ctx
        .secret(SESSION)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let mut cloud = Cloud { session };
    let mut known: BTreeMap<String, Device> = BTreeMap::new();
    let mut shapes: BTreeSet<String> = BTreeSet::new();
    let mut rescan = tokio::time::interval(RESCAN);
    rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.somfy.lecture_seule"))),
                None => return Ok(()),
            },
            _ = rescan.tick() => {
                if let Err(e) = connect(ctx, &mut cloud, &client, &email, &password).await {
                    if matches!(e.downcast_ref::<Refusal>(), Some(Refusal::Credentials)) {
                        let reason = moli_i18n::tr!(
                            "pilotes.somfy.compte_refuse",
                            instance = instance,
                            password = PASSWORD,
                            client_id = CLIENT_ID
                        );
                        return wait(ctx, reason).await;
                    }
                    return Err(e);
                }
                match inventory(ctx, &cloud, &mut known, &mut shapes).await {
                    Ok(()) => ctx.ready(),
                    Err(e) => {
                        if matches!(e.downcast_ref::<Refusal>(), Some(Refusal::Expired)) {
                            // Refreshed, or logged in again, on the next run.
                            cloud.session.access_token.clear();
                            save(ctx, &cloud.session).await?;
                        }
                        return Err(e);
                    }
                }
            }
        }
    }
}

/// A fresh token: refreshed when one is kept, a login otherwise or when
/// the refresh is refused.
async fn connect(
    ctx: &DriverCtx,
    cloud: &mut Cloud,
    client: &Client,
    email: &str,
    password: &str,
) -> anyhow::Result<()> {
    let now = now_s();
    if cloud.session.fresh(now) {
        return Ok(());
    }
    let refreshed = if cloud.session.refresh_token.is_empty() {
        false
    } else {
        match cloud.refresh(client, now).await {
            Ok(()) => true,
            Err(e) if e.downcast_ref::<Refusal>().is_some() => false,
            Err(e) => return Err(e),
        }
    };
    if !refreshed {
        cloud.log_in(client, email, password, now).await?;
        tracing::info!(instance = %ctx.instance(), "somfy: logged in");
    }
    save(ctx, &cloud.session).await
}

/// One Moli device per site and per device of a site.
async fn inventory(
    ctx: &DriverCtx,
    cloud: &Cloud,
    known: &mut BTreeMap<String, Device>,
    shapes: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let sites = cloud.sites().await.context("Somfy sites")?;
    let mut seen = BTreeSet::new();
    for site in &sites {
        let device = site_device(ctx, site);
        let mode = MODES
            .iter()
            .find(|(somfy, _)| *somfy == site.security_level)
            .map(|(_, moli)| Value::Text((*moli).into()));
        publish(
            ctx,
            known,
            &site.id,
            device,
            mode.map(|m| ("guard_mode", m)).into_iter().collect(),
        );
        seen.insert(site.id.clone());
        let devices = cloud.devices(&site.id).await.context("Somfy devices")?;
        for d in &devices {
            // What each kind reports, once, to learn what to read next.
            if shapes.insert(d.definition.label.clone()) {
                let keys: Vec<&str> = d.status.keys().map(String::as_str).collect();
                tracing::info!(instance = %ctx.instance(), kind = %d.definition.label, keys = keys.join(","), "somfy: device seen");
            }
            let values = values(d);
            publish(
                ctx,
                known,
                &d.id,
                part_device(ctx, site, d, &values),
                values,
            );
            seen.insert(d.id.clone());
        }
    }
    let gone: Vec<String> = known
        .keys()
        .filter(|k| !seen.contains(*k))
        .cloned()
        .collect();
    for id in gone {
        if let Some(device) = known.remove(&id) {
            ctx.remove_device(&device.id);
        }
    }
    if known.is_empty() {
        tracing::warn!(instance = %ctx.instance(), "somfy: the account sees no alarm");
    }
    Ok(())
}

fn publish(
    ctx: &DriverCtx,
    known: &mut BTreeMap<String, Device>,
    native: &str,
    device: Device,
    values: Vec<(&'static str, Value)>,
) {
    let id = device.id.clone();
    if known.get(native) != Some(&device) {
        ctx.upsert_device(device.clone());
        known.insert(native.to_owned(), device);
    }
    ctx.set_availability(&id, true);
    for (key, value) in values {
        ctx.set_state(&id, key, value);
    }
}

/// Battery, radio and temperature, from what the device reports.
fn values(device: &cloud::Device) -> Vec<(&'static str, Value)> {
    let number = |key: &str| device.status.get(key).and_then(serde_json::Value::as_f64);
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation)] // checked to be 0..=100
    if let Some(level) = number("battery_level").filter(|l| (0.0..=100.0).contains(l)) {
        out.push(("battery", Value::Int(level.round() as i64)));
    }
    #[allow(clippy::cast_possible_truncation)] // checked to be 0..=100
    if let Some(quality) = number("rlink_quality_percent").filter(|l| (0.0..=100.0).contains(l)) {
        out.push(("signal", Value::Int(quality.round() as i64)));
    }
    if let Some(t) = number("temperature").filter(|t| (-40.0..=80.0).contains(t)) {
        out.push(("temperature", Value::Float(t)));
    }
    out
}

fn read_only(key: &str, label: String, kind: Kind, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        semantic: Semantic::infer(key, unit.as_ref()),
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
    }
}

fn percent() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: Some(100.0),
        step: Some(1.0),
    }
}

fn site_device(ctx: &DriverCtx, site: &cloud::Site) -> Device {
    let mode = PointSpec {
        semantic: Semantic::Control,
        ..read_only(
            "guard_mode",
            moli_i18n::tr!("pilotes.somfy.mode"),
            Kind::Enum {
                values: MODES.iter().map(|(_, m)| (*m).into()).collect(),
            },
            None,
        )
    };
    Device {
        id: ctx.device_id(&site.id),
        instance: ctx.instance().clone(),
        native_name: if site.label.is_empty() {
            "Somfy Protect".into()
        } else {
            site.label.clone().into()
        },
        manufacturer: Some("Somfy".into()),
        model: Some("Somfy Protect".into()),
        description: None,
        native_room: None,
        parent: None,
        members: Vec::new(),
        points: vec![mode],
    }
}

fn part_device(
    ctx: &DriverCtx,
    site: &cloud::Site,
    device: &cloud::Device,
    values: &[(&str, Value)],
) -> Device {
    let points = values
        .iter()
        .map(|(key, _)| match *key {
            "battery" => read_only(
                "battery",
                moli_i18n::tr!("pilotes.somfy.batterie"),
                percent(),
                Some(Unit::Percent),
            ),
            "signal" => read_only(
                "signal",
                moli_i18n::tr!("pilotes.somfy.signal"),
                percent(),
                Some(Unit::Percent),
            ),
            _ => read_only(
                "temperature",
                moli_i18n::tr!("pilotes.somfy.temperature"),
                Kind::Numeric {
                    min: None,
                    max: None,
                    step: Some(0.1),
                },
                Some(Unit::Celsius),
            ),
        })
        .collect();
    let model = &device.definition.label;
    Device {
        id: ctx.device_id(&device.id),
        instance: ctx.instance().clone(),
        native_name: if device.label.is_empty() {
            model.clone().into()
        } else {
            device.label.clone().into()
        },
        manufacturer: Some("Somfy".into()),
        model: (!model.is_empty()).then(|| model.clone().into()),
        description: None,
        native_room: None,
        parent: Some(ctx.device_id(&site.id)),
        members: Vec::new(),
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(status: &serde_json::Value) -> cloud::Device {
        serde_json::from_value(serde_json::json!({ "device_id": "d", "status": status })).unwrap()
    }

    #[test]
    fn values_are_read_when_sensible() {
        let read = values(&device(&serde_json::json!({
            "battery_level": 87,
            "rlink_quality_percent": 54.4,
            "temperature": 19.5,
            "other": "x"
        })));
        assert_eq!(
            read,
            [
                ("battery", Value::Int(87)),
                ("signal", Value::Int(54)),
                ("temperature", Value::Float(19.5)),
            ]
        );
        let odd = values(&device(
            &serde_json::json!({ "battery_level": 255, "temperature": null }),
        ));
        assert!(odd.is_empty());
    }

    #[test]
    fn somfy_levels_become_moli_modes() {
        let moli: Vec<&str> = ["disarmed", "partial", "armed"]
            .iter()
            .filter_map(|l| MODES.iter().find(|(s, _)| s == l).map(|(_, m)| *m))
            .collect();
        assert_eq!(moli, ["disarmed", "home", "away"]);
    }
}

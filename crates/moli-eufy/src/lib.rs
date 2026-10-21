//! Eufy Security: HomeBases, cameras and doorbells, through Eufy's cloud
//! (read-only for now). Ported from eufy-security-client (MIT, see NOTICE).
//!
//! The account (`email`, `password`) comes from the encrypted secret store:
//! a guest account of its own, since Eufy keeps one session per account. A
//! first login asks for a code e-mailed to it (sometimes a captcha first):
//! the person files it in the store and restarts Moli; Moli is then a
//! trusted phone and logs in alone for months. The session (Moli's key, its
//! phone identity, the token) is kept in the store as well.

mod cloud;
mod crypto;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::media::Image;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

use crate::cloud::{Answer, Cloud, Login, Refusal, Session};

const EMAIL: &str = "email";
const PASSWORD: &str = "password";
/// Written by the driver: Moli's identity with Eufy and its token.
const SESSION: &str = "session";
/// Written by a person: the e-mailed code, the captcha's characters.
const CODE: &str = "verify_code";
const CAPTCHA: &str = "captcha";
/// Written by the driver: which captcha the answer is for.
const CAPTCHA_ID: &str = "captcha_id";

/// The list is read again this often (names, battery, guard mode).
const RESCAN: Duration = Duration::from_secs(10 * 60);

/// Parameters (`param_type`) read from the lists.
const BATTERY: i64 = 1101;
const GUARD_MODE: i64 = 1224;

/// Guard modes as Eufy numbers them, and their Moli value.
const MODES: [(i64, &str); 8] = [
    (0, "away"),
    (1, "home"),
    (2, "schedule"),
    (3, "custom1"),
    (4, "custom2"),
    (5, "custom3"),
    (47, "geofencing"),
    (63, "disarmed"),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The account's country (ISO 3166, `FR`): it chooses Eufy's servers.
    pub country: String,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "fr".into()
}

#[derive(Debug)]
pub struct Eufy {
    config: Config,
}

impl Eufy {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        anyhow::ensure!(
            config.country.len() == 2 && config.country.bytes().all(|b| b.is_ascii_alphabetic()),
            "country: two letters (FR, DE, US…)"
        );
        anyhow::ensure!(
            config.language.len() == 2 && config.language.bytes().all(|b| b.is_ascii_alphabetic()),
            "language: two letters (fr, en…)"
        );
        Ok(Self { config })
    }
}

impl Driver for Eufy {
    fn kind(&self) -> &'static str {
        "eufy"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
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
        .context("saving the Eufy session")
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let instance = ctx.instance().to_string();
    let (Some(email), Some(password)) = (ctx.secret(EMAIL), ctx.secret(PASSWORD)) else {
        let reason = moli_i18n::tr!(
            "pilotes.eufy.identifiants_manquants",
            instance = instance,
            email = EMAIL,
            password = PASSWORD
        );
        return wait(ctx, reason).await;
    };
    if !ctx.can_store_secrets() {
        anyhow::bail!("the Eufy session cannot be kept: the secret store is unavailable");
    }
    let session = if let Some(text) = ctx.secret(SESSION) {
        serde_json::from_str(&text).context("stored Eufy session")?
    } else {
        let session = Session::new()?;
        save(ctx, &session).await?;
        session
    };
    let mut cloud = Cloud::new(session, &config.country, &config.language)?;
    if !cloud.session.logged_in(now_s()) && !log_in(ctx, &mut cloud, &email, &password).await? {
        return Ok(());
    }
    serve(ctx, &mut cloud).await
}

/// `false`: a person must act first (the reason is shown, Moli waits).
async fn log_in(
    ctx: &DriverCtx,
    cloud: &mut Cloud,
    email: &str,
    password: &str,
) -> anyhow::Result<bool> {
    let instance = ctx.instance().to_string();
    let code = ctx.secret(CODE);
    let captcha = ctx.secret(CAPTCHA).zip(ctx.secret(CAPTCHA_ID));
    let answer = match (&code, &captcha) {
        (Some(code), _) => Some(Answer::Code(code.trim())),
        (None, Some((text, id))) => Some(Answer::Captcha {
            id,
            text: text.trim(),
        }),
        (None, None) => None,
    };
    let result = cloud.login(email, password, answer).await;
    // An answer serves once, right or wrong.
    if code.is_some() {
        ctx.forget_secret(CODE).await?;
    }
    if captcha.is_some() {
        ctx.forget_secret(CAPTCHA).await?;
        ctx.forget_secret(CAPTCHA_ID).await?;
    }
    match result {
        Ok(Login::Done) => {
            if let Some(code) = &code {
                cloud.trust(code.trim()).await?;
            }
            save(ctx, &cloud.session).await?;
            tracing::info!(instance, "eufy: logged in");
            Ok(true)
        }
        Ok(Login::NeedCode) => {
            save(ctx, &cloud.session).await?;
            let reason =
                moli_i18n::tr!("pilotes.eufy.code_envoye", instance = instance, code = CODE);
            wait(ctx, reason).await?;
            Ok(false)
        }
        Ok(Login::NeedCaptcha { id, image }) => {
            ctx.store_secret(CAPTCHA_ID, &id).await?;
            save(ctx, &cloud.session).await?;
            let link = captcha_image(&image)
                .and_then(|image| ctx.media_publisher().publish("png", image))
                .map_or_else(String::new, |name| format!("/api/media/{name}"));
            let reason = moli_i18n::tr!(
                "pilotes.eufy.captcha",
                instance = instance,
                captcha = CAPTCHA,
                link = link
            );
            wait(ctx, reason).await?;
            Ok(false)
        }
        Err(e) => match e.downcast_ref::<Refusal>() {
            Some(Refusal::Password) => {
                let reason = moli_i18n::tr!(
                    "pilotes.eufy.mot_de_passe_refuse",
                    instance = instance,
                    password = PASSWORD
                );
                wait(ctx, reason).await?;
                Ok(false)
            }
            // A wrong code: start again, a new one is e-mailed.
            Some(Refusal::Code) => Box::pin(log_in(ctx, cloud, email, password)).await,
            _ => Err(e),
        },
    }
}

/// The captcha's picture, from its `data:image/…;base64,` URL.
fn captcha_image(url: &str) -> Option<Image> {
    let (head, data) = url.split_once(',')?;
    let content_type = head.strip_prefix("data:")?.strip_suffix(";base64")?;
    if !content_type.starts_with("image/") {
        return None;
    }
    Some(Image {
        content_type: content_type.into(),
        bytes: crypto::unbase64(data)?,
    })
}

async fn serve(ctx: &mut DriverCtx, cloud: &mut Cloud) -> anyhow::Result<()> {
    let mut known: BTreeMap<String, Device> = BTreeMap::new();
    let mut rescan = tokio::time::interval(RESCAN);
    rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.eufy.lecture_seule"))),
                None => return Ok(()),
            },
            _ = rescan.tick() => {
                if let Err(e) = inventory(ctx, cloud, &mut known).await {
                    if matches!(e.downcast_ref::<Refusal>(), Some(Refusal::Expired)) {
                        // Log in again on the next run.
                        cloud.session.token.clear();
                        save(ctx, &cloud.session).await?;
                    }
                    return Err(e);
                }
                ctx.ready();
            }
        }
    }
}

/// One Moli device per station and per device; a Wi-Fi camera is both a
/// station and a device: one Moli device.
async fn inventory(
    ctx: &DriverCtx,
    cloud: &mut Cloud,
    known: &mut BTreeMap<String, Device>,
) -> anyhow::Result<()> {
    let stations = cloud.stations().await.context("Eufy stations")?;
    let devices = cloud.devices().await.context("Eufy devices")?;
    let mut seen = Vec::new();
    for device in &devices {
        let station = stations.iter().find(|s| s.serial == device.serial);
        let mut params = device.params.clone();
        if let Some(station) = station {
            params.extend(station.params.iter().cloned());
        }
        let found = Found {
            serial: &device.serial,
            name: &device.name,
            model: &device.model,
            params: &params,
        };
        publish(ctx, known, &found);
        seen.push(device.serial.clone());
    }
    let alone: Vec<_> = stations
        .iter()
        .filter(|s| !seen.contains(&s.serial))
        .collect();
    for station in alone {
        let found = Found {
            serial: &station.serial,
            name: &station.name,
            model: &station.model,
            params: &station.params,
        };
        publish(ctx, known, &found);
        seen.push(station.serial.clone());
    }
    for gone in known
        .keys()
        .filter(|k| !seen.contains(k))
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Some(device) = known.remove(&gone) {
            ctx.remove_device(&device.id);
        }
    }
    if known.is_empty() {
        tracing::warn!(instance = %ctx.instance(), "eufy: the account sees no device (shared with it?)");
    }
    Ok(())
}

struct Found<'a> {
    serial: &'a str,
    name: &'a str,
    model: &'a str,
    params: &'a [cloud::Param],
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

fn values(params: &[cloud::Param]) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if let Some(level) = cloud::param(params, BATTERY).and_then(|v| v.trim().parse::<i64>().ok())
        && (0..=100).contains(&level)
    {
        out.push(("battery", Value::Int(level)));
    }
    if let Some(mode) = cloud::param(params, GUARD_MODE)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .and_then(|n| MODES.iter().find(|(m, _)| *m == n))
    {
        out.push(("guard_mode", Value::Text(mode.1.into())));
    }
    out
}

fn device(ctx: &DriverCtx, found: &Found<'_>, values: &[(&str, Value)]) -> Device {
    let mut points = Vec::new();
    for (key, _) in values {
        points.push(match *key {
            "battery" => read_only(
                "battery",
                moli_i18n::tr!("pilotes.eufy.batterie"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                Some(Unit::Percent),
            ),
            _ => read_only(
                "guard_mode",
                moli_i18n::tr!("pilotes.eufy.mode"),
                Kind::Enum {
                    values: MODES.iter().map(|(_, v)| (*v).into()).collect(),
                },
                None,
            ),
        });
    }
    Device {
        id: ctx.device_id(found.serial),
        instance: ctx.instance().clone(),
        native_name: if found.name.is_empty() {
            found.model.into()
        } else {
            found.name.into()
        },
        manufacturer: Some("Eufy".into()),
        model: (!found.model.is_empty()).then(|| found.model.into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

fn publish(ctx: &DriverCtx, known: &mut BTreeMap<String, Device>, found: &Found<'_>) {
    if found.serial.is_empty() {
        return;
    }
    let values = values(found.params);
    let device = device(ctx, found, &values);
    let id = device.id.clone();
    if known.get(found.serial) != Some(&device) {
        ctx.upsert_device(device.clone());
        known.insert(found.serial.to_owned(), device);
    }
    ctx.set_availability(&id, true);
    for (key, value) in values {
        ctx.set_state(&id, key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::Param;

    fn p(kind: i64, value: &str) -> Param {
        Param {
            param_type: kind,
            param_value: value.into(),
        }
    }

    #[test]
    fn battery_and_guard_mode_are_read_from_parameters() {
        let v = values(&[p(1101, "87"), p(1224, "63"), p(9999, "x")]);
        assert_eq!(
            v,
            vec![
                ("battery", Value::Int(87)),
                ("guard_mode", Value::Text("disarmed".into()))
            ]
        );
        assert!(values(&[p(1101, "250"), p(1224, "12")]).is_empty());
    }

    #[test]
    fn the_captcha_picture_is_read_from_its_data_url() {
        let image = captcha_image("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(image.content_type, "image/png");
        assert_eq!(image.bytes, b"\x89PNG\r\n\x1a\n");
        assert!(captcha_image("data:text/html;base64,PGI+").is_none());
        assert!(captcha_image("https://example.org/x.png").is_none());
    }

    #[test]
    fn the_configuration_is_checked() {
        let ok = |country: &str, language: &str| {
            Eufy::new(Config {
                country: country.into(),
                language: language.into(),
            })
            .is_ok()
        };
        assert!(ok("FR", "fr"));
        assert!(!ok("FRA", "fr"));
        assert!(!ok("F1", "fr"));
        assert!(!ok("FR", ""));
    }
}

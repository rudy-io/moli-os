//! Sonos players, through their local UPnP API (HTTP 1400): play/pause,
//! volume, mute, what is playing. Polled every two seconds. With
//! `[driver.options.announce]`, spoken announcements too (`announce.rs`),
//! and `quiet`: while on, no announcement is said (whoever asks), until it
//! is switched off or [`QUIET_FOR`] has passed (a silence forgotten on).

mod announce;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::upnp::{tag, unescape};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;

const PORT: u16 = 1400;
const POLL: Duration = Duration::from_secs(2);
/// A silence ends by itself after this long.
pub const QUIET_FOR: Duration = Duration::from_secs(4 * 3600);
const AV: (&str, &str) = (
    "/MediaRenderer/AVTransport/Control",
    "urn:schemas-upnp-org:service:AVTransport:1",
);
const RENDERING: (&str, &str) = (
    "/MediaRenderer/RenderingControl/Control",
    "urn:schemas-upnp-org:service:RenderingControl:1",
);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    /// Spoken announcements (absent: none).
    #[serde(default)]
    pub announce: Option<announce::Config>,
}

/// Where the speaker's local API certificate fingerprint is kept.
const WS_CERT: &str = "ws_cert";

#[derive(Debug)]
pub struct Sonos {
    config: Config,
}

impl Sonos {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Sonos {
    fn kind(&self) -> &'static str {
        "sonos"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

async fn soap(
    host: &str,
    (path, service): (&str, &str),
    action: &str,
    args: &[(&str, &str)],
) -> anyhow::Result<String> {
    moli_net::upnp::soap(host, PORT, path, service, action, args).await
}

fn point(
    key: &str,
    label: &str,
    kind: Kind,
    write: bool,
    unit: Option<Unit>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

#[allow(clippy::too_many_lines)] // one entry per point
fn device(ctx: &DriverCtx, id: &DeviceId, room: &str, model: &str, speaks: bool) -> Device {
    let mut device = Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: room.into(),
        manufacturer: Some("Sonos".into()),
        model: Some(model.into()),
        description: None,
        native_room: Some(room.into()),
        members: Vec::new(),
        points: vec![
            point(
                "playing",
                &moli_i18n::tr!("pilotes.sonos.lecture"),
                Kind::Binary,
                true,
                None,
                Semantic::OnOff,
            ),
            point(
                "volume",
                &moli_i18n::tr!("pilotes.sonos.volume"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                true,
                Some(Unit::Percent),
                Semantic::infer("volume", None),
            ),
            point(
                "mute",
                &moli_i18n::tr!("pilotes.sonos.sourdine"),
                Kind::Binary,
                true,
                None,
                Semantic::infer("mute", None),
            ),
            point(
                "state",
                &moli_i18n::tr!("pilotes.sonos.etat"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "title",
                &moli_i18n::tr!("pilotes.sonos.titre"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "artist",
                &moli_i18n::tr!("pilotes.sonos.artiste"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
        ],
    };
    if speaks {
        device.points.push(PointSpec {
            key: "quiet".into(),
            label: moli_i18n::tr!("pilotes.sonos.annonces_coupees").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        });
        device.points.push(PointSpec {
            key: "quiet_until".into(),
            label: moli_i18n::tr!("pilotes.sonos.annonces_coupees_jusqua").into(),
            kind: Kind::Numeric {
                min: None,
                max: None,
                step: None,
            },
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Other,
        });
        device.points.push(PointSpec {
            key: "announce".into(),
            label: moli_i18n::tr!("pilotes.sonos.annonce").into(),
            kind: Kind::Text,
            // Words to say, not a state: nothing to read back.
            access: Access {
                read: false,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        });
    }
    device
}

async fn describe(host: &str) -> anyhow::Result<(String, String, String)> {
    let xml = moli_net::upnp::fetch(host, PORT, "/xml/device_description.xml").await?;
    let udn = tag(&xml, "UDN")
        .context("no UDN")?
        .trim_start_matches("uuid:")
        .to_owned();
    let room = tag(&xml, "roomName").unwrap_or("Sonos").to_owned();
    let model = tag(&xml, "modelName").unwrap_or("Sonos").to_owned();
    Ok((udn, room, model))
}

async fn poll(host: &str) -> anyhow::Result<Vec<(&'static str, Value)>> {
    let transport = soap(host, AV, "GetTransportInfo", &[("InstanceID", "0")]).await?;
    let state = tag(&transport, "CurrentTransportState")
        .unwrap_or("UNKNOWN")
        .to_owned();
    let volume = soap(
        host,
        RENDERING,
        "GetVolume",
        &[("InstanceID", "0"), ("Channel", "Master")],
    )
    .await?;
    let mute = soap(
        host,
        RENDERING,
        "GetMute",
        &[("InstanceID", "0"), ("Channel", "Master")],
    )
    .await?;
    let position = soap(host, AV, "GetPositionInfo", &[("InstanceID", "0")]).await?;
    let meta = unescape(tag(&position, "TrackMetaData").unwrap_or(""));
    let text = |s: Option<&str>| {
        s.map(|s| unescape(s).trim().to_owned())
            .filter(|s| !s.is_empty() && s != "NOT_IMPLEMENTED")
    };
    let mut values = vec![
        (
            "playing",
            Value::Bool(state == "PLAYING" || state == "TRANSITIONING"),
        ),
        ("state", Value::Text(state.to_lowercase().into())),
        ("mute", Value::Bool(tag(&mute, "CurrentMute") == Some("1"))),
        (
            "title",
            text(tag(&meta, "title")).map_or(Value::Null, |t| Value::Text(t.into())),
        ),
        (
            "artist",
            text(tag(&meta, "creator")).map_or(Value::Null, |t| Value::Text(t.into())),
        ),
    ];
    if let Some(v) = tag(&volume, "CurrentVolume").and_then(|v| v.parse::<i64>().ok()) {
        values.push(("volume", Value::Int(v)));
    }
    Ok(values)
}

async fn apply(host: &str, command: &CommandRequest) -> anyhow::Result<()> {
    match (&*command.key, &command.value) {
        ("playing", Value::Bool(true)) => {
            soap(host, AV, "Play", &[("InstanceID", "0"), ("Speed", "1")]).await?
        }
        ("playing", Value::Bool(false)) => soap(host, AV, "Pause", &[("InstanceID", "0")]).await?,
        ("mute", Value::Bool(m)) => {
            soap(
                host,
                RENDERING,
                "SetMute",
                &[
                    ("InstanceID", "0"),
                    ("Channel", "Master"),
                    ("DesiredMute", if *m { "1" } else { "0" }),
                ],
            )
            .await?
        }
        ("volume", v) => {
            #[allow(clippy::cast_possible_truncation)]
            let level = v.as_f64().context("volume")?.round().clamp(0.0, 100.0) as i64;
            soap(
                host,
                RENDERING,
                "SetVolume",
                &[
                    ("InstanceID", "0"),
                    ("Channel", "Master"),
                    ("DesiredVolume", &level.to_string()),
                ],
            )
            .await?
        }
        (key, _) => bail!(moli_i18n::tr!("pilotes.sonos.ne_se_regle_pas", point = key)),
    };
    Ok(())
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let host = config.host.as_str();
    let (udn, room, model) = describe(host)
        .await
        .with_context(|| format!("sonos {host}"))?;
    let id = ctx.device_id(&udn);
    let announcer = match &config.announce {
        Some(settings) => Some(announce::Announcer::new(
            host,
            &udn,
            settings.clone(),
            speaker_pin(host, ctx).await,
            ctx.media_publisher(),
        )),
        None => None,
    };
    ctx.upsert_device(device(ctx, &id, &room, &model, announcer.is_some()));
    ctx.ready();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut online = None;
    // A silence survives a restart (the hub keeps the last values).
    let mut quiet_until = match (ctx.current(&id, "quiet"), ctx.current(&id, "quiet_until")) {
        (Some(Value::Bool(true)), Some(Value::Int(until))) => u64::try_from(until).ok(),
        _ => None,
    }
    .filter(|u| *u > moli_core::now_ms());
    set_quiet(ctx, &id, quiet_until);
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if &*command.key == "announce" {
                    let result = if quiet_until.is_some_and(|u| u > moli_core::now_ms()) {
                        // Silenced: nothing said, nobody bothered with an error.
                        tracing::info!("announcement not said: announcements are silenced");
                        Ok(())
                    } else {
                        speak(announcer.as_ref(), &command.value)
                    };
                    command.reply(result);
                    continue;
                }
                if &*command.key == "quiet" {
                    let on = matches!(command.value, Value::Bool(true));
                    quiet_until = on.then(|| moli_core::now_ms() + u64::try_from(QUIET_FOR.as_millis()).unwrap_or(u64::MAX));
                    set_quiet(ctx, &id, quiet_until);
                    command.reply(Ok(()));
                    continue;
                }
                let result = apply(host, &command).await.map_err(|e| format!("{e:#}"));
                if result.is_ok() {
                    ctx.set_state(&id, &command.key, command.value.clone());
                }
                command.reply(result);
            }
            _ = tick.tick() => {
                if quiet_until.is_some_and(|u| u <= moli_core::now_ms()) {
                    // Forgotten on: the house speaks again.
                    quiet_until = None;
                    set_quiet(ctx, &id, None);
                }
                match poll(host).await {
                Ok(values) => {
                    if online != Some(true) {
                        online = Some(true);
                        ctx.set_availability(&id, true);
                    }
                    for (key, value) in values {
                        ctx.set_state(&id, key, value);
                    }
                }
                Err(e) => {
                    if online != Some(false) {
                        online = Some(false);
                        ctx.set_availability(&id, false);
                        tracing::debug!(host, error = %e, "sonos unreachable");
                    }
                }
                }
            }
        }
    }
}

/// The silence's two points (until: ms since epoch, 0 when not silenced).
fn set_quiet(ctx: &DriverCtx, id: &DeviceId, until: Option<u64>) {
    ctx.set_state(id, "quiet", Value::Bool(until.is_some()));
    let until = i64::try_from(until.unwrap_or(0)).unwrap_or(i64::MAX);
    ctx.set_state(id, "quiet_until", Value::Int(until));
}

/// Queues an announcement (it takes seconds: speech, then the clip) and
/// answers at once; a failure later is told to the house.
fn speak(announcer: Option<&Arc<announce::Announcer>>, value: &Value) -> Result<(), String> {
    let announcer =
        announcer.ok_or_else(|| moli_i18n::tr!("pilotes.sonos.annonces_non_configurees"))?;
    let Value::Text(text) = value else {
        return Err(moli_i18n::tr!("pilotes.sonos.annonce_texte"));
    };
    let text = announce::check_text(text)
        .map_err(|e| e.to_string())?
        .to_owned();
    announcer.enqueue(text)
}

/// The fingerprint of the speaker's local API certificate: pinned at first
/// contact, then required. Without it, announcements still work, unpinned.
async fn speaker_pin(host: &str, ctx: &DriverCtx) -> Option<String> {
    if let Some(pin) = ctx.secret(WS_CERT) {
        return Some(pin);
    }
    let probe = moli_net::PinnedHttps::new(host, 1443, None).ok()?;
    if let Err(e) = probe.handshake().await {
        tracing::warn!(host, error = %format!("{e:#}"), "sonos local API unreachable: announcements unpinned");
        return None;
    }
    let seen = probe.seen_fingerprint()?;
    if ctx.can_store_secrets()
        && let Err(e) = ctx.store_secret(WS_CERT, &seen).await
    {
        tracing::warn!(error = %e, "sonos certificate not stored");
    }
    Some(seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_soap_and_escaped_didl() {
        let position = "<s:Envelope><s:Body><u:GetPositionInfoResponse><Track>1</Track><TrackMetaData>&lt;DIDL-Lite&gt;&lt;item&gt;&lt;dc:title&gt;Bohemian Rhapsody&lt;/dc:title&gt;&lt;dc:creator&gt;Queen &amp;amp; co&lt;/dc:creator&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;</TrackMetaData></u:GetPositionInfoResponse></s:Body></s:Envelope>";
        let meta = unescape(tag(position, "TrackMetaData").unwrap());
        assert_eq!(tag(&meta, "title"), Some("Bohemian Rhapsody"));
        assert_eq!(unescape(tag(&meta, "creator").unwrap()), "Queen & co");
        assert_eq!(
            tag("<CurrentVolume>23</CurrentVolume>", "CurrentVolume"),
            Some("23")
        );
    }
}

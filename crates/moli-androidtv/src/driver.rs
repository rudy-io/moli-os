//! Android TV boxes and TVs, whoever made them: the Android TV core alone
//! (`moli_androidtv::tv`), with no maker's API beside it. Paired with the
//! certificates Home Assistant already made (Android TV Remote), it says the
//! app on screen, opens apps, and takes the remote's keys, held ones
//! included; Google Cast says what plays and takes play / pause (with
//! `adb = true`, what any app plays); with `overlay = true`, notifications
//! over the picture (TvOverlay). The screen's power is read, never written:
//! nothing here switches the box on or off.

use std::time::Duration;

use crate::tv::{Android, Waiting, adb_key};
use anyhow::bail;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;

/// How often the core looks at its connections (the box back: reconnected
/// at once, otherwise once a minute).
const POLL: Duration = Duration::from_secs(5);

/// Remote keys, each one an Android key code (a press each, never repeated).
pub const KEYS: [&str; 23] = [
    "CursorUp",
    "CursorDown",
    "CursorLeft",
    "CursorRight",
    "Confirm",
    "Back",
    "Home",
    "Options",
    "Info",
    "PlayPause",
    "Play",
    "Pause",
    "Stop",
    "Next",
    "Previous",
    "FastForward",
    "Rewind",
    "VolumeUp",
    "VolumeDown",
    "Mute",
    "ChannelStepUp",
    "ChannelStepDown",
    "Source",
];

/// The keys that can be held (a video seeks faster, a list scrolls), and
/// the word that lets go.
pub const HOLD_KEYS: [&str; 7] = [
    "CursorLeft",
    "CursorRight",
    "CursorUp",
    "CursorDown",
    "FastForward",
    "Rewind",
    "release",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    /// What any app plays (title, playing or paused, position), through
    /// ADB: the box's « network debugging » must be on, and it asks once on
    /// its screen whether to allow Moli.
    #[serde(default)]
    pub adb: bool,
    /// Notifications drawn over whatever plays (point `notify`), by the
    /// TvOverlay app installed on the box.
    #[serde(default)]
    pub overlay: bool,
}

#[derive(Debug)]
pub struct AndroidTv {
    config: Config,
}

impl AndroidTv {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for AndroidTv {
    fn kind(&self) -> &'static str {
        "androidtv"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The Android key code of a remote key (`None`: not a key Android knows,
/// never guessed).
fn android_key(name: &str) -> Option<u32> {
    Some(match name {
        "CursorUp" => 19,
        "CursorDown" => 20,
        "CursorLeft" => 21,
        "CursorRight" => 22,
        "Confirm" => 23,
        "Back" => 4,
        "Home" => 3,
        "Options" => 82,
        "Info" => 165,
        "PlayPause" => 85,
        "Play" => 126,
        "Pause" => 127,
        "Stop" => 86,
        "Next" => 87,
        "Previous" => 88,
        "FastForward" => 90,
        "Rewind" => 89,
        "VolumeUp" => 24,
        "VolumeDown" => 25,
        "Mute" => 164,
        "ChannelStepUp" => 166,
        "ChannelStepDown" => 167,
        "Source" => 178,
        _ => return None,
    })
}

/// A key press through the remote, which must be up (a press is never
/// replayed: one lost is better than one twice).
fn press(android: &Android, name: &str) -> anyhow::Result<()> {
    let Some(code) = android_key(name) else {
        bail!(moli_i18n::tr!(
            "pilotes.androidtv.ne_se_regle_pas",
            point = name
        ));
    };
    match android.press(code) {
        Some(sent) => sent,
        None => bail!(moli_i18n::tr!("pilotes.androidtv.pas_de_telecommande")),
    }
}

/// The Android key a held key name stands for (`None`: let go).
fn held(name: &str) -> anyhow::Result<Option<u32>> {
    match name {
        "release" => Ok(None),
        key if HOLD_KEYS.contains(&key) => Ok(android_key(key)),
        other => bail!(moli_i18n::tr!(
            "pilotes.androidtv.touche_inconnue",
            key = format!("{other:?}")
        )),
    }
}

fn spec(key: &str, label: &str, kind: Kind, write: bool, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit: None,
        semantic,
    }
}

fn enumeration(values: &[&str]) -> Kind {
    Kind::Enum {
        values: values.iter().map(|v| (*v).into()).collect(),
    }
}

fn seconds(key: &str, label: &str) -> PointSpec {
    PointSpec {
        unit: Some(Unit::Second),
        ..spec(
            key,
            label,
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: None,
            },
            false,
            Semantic::Other,
        )
    }
}

#[allow(clippy::too_many_lines)] // one entry per point
fn device(ctx: &DriverCtx, id: &DeviceId, overlay: bool) -> Device {
    let mut device = Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.androidtv.tele").into(),
        manufacturer: None,
        model: Some("Android TV".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            // Read: what the Android remote says of the screen. Nothing
            // switches it on or off (no Wake-on-LAN either).
            spec(
                "power",
                &moli_i18n::tr!("pilotes.androidtv.allumee"),
                Kind::Binary,
                false,
                Semantic::OnOff,
            ),
            // Written: opens that app (Android package, link or known app).
            spec(
                "app",
                &moli_i18n::tr!("pilotes.androidtv.application"),
                Kind::Text,
                true,
                Semantic::Other,
            ),
            // Written: the text of the field open on the box (a search).
            PointSpec {
                key: "text".into(),
                label: moli_i18n::tr!("pilotes.androidtv.texte").into(),
                kind: Kind::Text,
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            PointSpec {
                key: "key".into(),
                label: moli_i18n::tr!("pilotes.androidtv.touche").into(),
                kind: enumeration(&KEYS),
                // A press, not a state: nothing to read back.
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            // A key held down until `release` (a video seeks faster).
            PointSpec {
                key: "hold".into(),
                label: moli_i18n::tr!("pilotes.androidtv.touche_maintenue").into(),
                kind: enumeration(&HOLD_KEYS),
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            spec(
                "media_app",
                &moli_i18n::tr!("pilotes.androidtv.appli_en_lecture"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            spec(
                "media_state",
                &moli_i18n::tr!("pilotes.androidtv.lecture"),
                enumeration(&["playing", "paused", "buffering", "idle"]),
                false,
                Semantic::Other,
            ),
            spec(
                "media_title",
                &moli_i18n::tr!("pilotes.androidtv.titre"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            spec(
                "media_subtitle",
                &moli_i18n::tr!("pilotes.androidtv.serie_artiste"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            spec(
                "media_image",
                &moli_i18n::tr!("pilotes.androidtv.image"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            seconds("media_duration", &moli_i18n::tr!("pilotes.androidtv.duree")),
            seconds(
                "media_position",
                &moli_i18n::tr!("pilotes.androidtv.position"),
            ),
            PointSpec {
                key: "media_control".into(),
                label: moli_i18n::tr!("pilotes.androidtv.lecture_commande").into(),
                kind: enumeration(&["play", "pause", "stop"]),
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
        ],
    };
    if overlay {
        // Written: a notification over the picture (text, or TvOverlay's
        // JSON: a camera's live video in a corner…).
        device.points.push(PointSpec {
            key: "notify".into(),
            label: moli_i18n::tr!("pilotes.androidtv.notification").into(),
            kind: Kind::Text,
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

/// One order. An app (answered by the core when the box takes it or has
/// refused every link) and a notification (off the driver's loop: the box
/// answers when it can) keep their order; the rest is [`apply`].
fn order(command: CommandRequest, android: &mut Android, config: &Config) {
    if let ("app", Value::Text(target)) = (&*command.key, &command.value) {
        let target = target.to_string();
        android.launch(&target, Waiting::order(command));
        return;
    }
    if let ("notify", Value::Text(text)) = (&*command.key, &command.value)
        && config.overlay
    {
        let host = config.host.clone();
        let notification = crate::overlay::notification(text);
        tokio::spawn(async move {
            let result = match notification {
                Ok(notification) => {
                    crate::overlay::notify(&host, crate::overlay::PORT, &notification).await
                }
                Err(e) => Err(e),
            };
            command.reply(result.map_err(|e| format!("{e:#}")));
        });
        return;
    }
    let result = apply(android, &command).map_err(|e| format!("{e:#}"));
    command.reply(result);
}

/// What the core takes: playback, keys, held keys, text. Anything else
/// (the screen's power, read-only) is refused, never guessed.
fn apply(android: &mut Android, command: &CommandRequest) -> anyhow::Result<()> {
    match (&*command.key, &command.value) {
        ("media_control", Value::Text(word)) => android.control(word)?,
        ("key", Value::Text(key)) => press(android, key)?,
        ("text", Value::Text(text)) => android.write(text)?,
        ("hold", Value::Text(key)) => android.hold(held(key)?)?,
        (key, _) => bail!(moli_i18n::tr!(
            "pilotes.androidtv.ne_se_regle_pas",
            point = key
        )),
    }
    Ok(())
}

/// Until `at`; forever without one (a `select!` branch that never fires).
async fn until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // The paired remote (both secrets read by the core); without it the
    // driver still runs, Cast alone.
    let adb_key = if config.adb { adb_key(ctx).await } else { None };
    let (mut android, mut ups) = Android::start(ctx, &config.host, adb_key);
    let id = ctx.device_id(&config.host);
    ctx.upsert_device(device(ctx, &id, config.overlay));
    ctx.ready();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                order(command, &mut android, config);
            }
            Some(up) = ups.recv() => android.on(up, ctx, &id).await,
            () = until(android.due()) => android.settle(),
            _ = tick.tick() => {
                // The box is on the network (nothing here probes it: the
                // core's connections do, and this is what a nudge waits for).
                android.tick(true);
                // What the remote says of the screen; Cast never says it.
                if android.can_launch() {
                    ctx.set_state(&id, "power", Value::Bool(android.says_on()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_remote_key_has_its_android_key() {
        for key in KEYS {
            assert!(android_key(key).is_some(), "{key}");
        }
        assert_eq!(android_key("CursorDown"), Some(20));
        assert_eq!(android_key("Confirm"), Some(23));
        assert_eq!(android_key("VolumeUp"), Some(24));
        // Not an Android key, and no driver of this kind may send it.
        assert_eq!(android_key("Standby"), None);
    }

    #[test]
    fn an_unknown_held_key_is_refused() {
        for key in HOLD_KEYS.iter().filter(|k| **k != "release") {
            assert!(held(key).unwrap().is_some(), "{key}");
        }
        assert_eq!(held("CursorRight").unwrap(), Some(22));
        assert_eq!(held("release").unwrap(), None);
        assert!(held("Home").is_err(), "not a key to hold");
        assert!(held("volume").is_err(), "no such key");
    }
}

//! The `appletv` driver: an Apple TV through its Companion protocol, the
//! one Apple's Remote on an iPhone speaks. Paired once with the code the
//! Apple TV shows; then it wakes and sleeps (HDMI-CEC takes the screen
//! along when the TV allows it), takes the remote's keys, held ones
//! included, opens apps, plays and pauses. What plays (title, picture) is
//! not said here: that is MediaRemote, another protocol.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::companion::{self, Event, Pairing, Session};
use crate::hap::Credentials;
use crate::opack::Value as Op;

/// The vault's names: the pairing Moli keeps, and the code a person gives.
pub const CREDENTIALS: &str = "credentials";
pub const PIN: &str = "pin";

/// How long the code on the screen is waited for (the Apple TV itself
/// drops the request sooner: measured, under a minute).
const PIN_WAIT: Duration = Duration::from_secs(2 * 60);
/// How often the vault is looked at for the code.
const PIN_POLL: Duration = Duration::from_secs(1);
/// Asked this often, the Apple TV says whether it sleeps (and the link
/// proves it is still there).
const CHECK: Duration = Duration::from_secs(60);
/// Between two connection attempts, doubling up to the last.
const RETRY: [u64; 4] = [5, 15, 30, 60];

/// Remote keys: a HID button each, or a media command (`_mcc`).
pub const KEYS: [&str; 17] = [
    "CursorUp",
    "CursorDown",
    "CursorLeft",
    "CursorRight",
    "Confirm",
    "Back",
    "Home",
    "PlayPause",
    "Play",
    "Pause",
    "Next",
    "Previous",
    "VolumeUp",
    "VolumeDown",
    "ChannelStepUp",
    "ChannelStepDown",
    "Options",
];

/// The keys that can be held (a list scrolls, a video seeks), and the word
/// that lets go.
pub const HOLD_KEYS: [&str; 6] = [
    "CursorUp",
    "CursorDown",
    "CursorLeft",
    "CursorRight",
    "Confirm",
    "release",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The name the Apple TV lists Moli under (Settings → Remotes and
    /// Devices).
    #[serde(default = "default_name")]
    pub name: String,
}

fn default_port() -> u16 {
    companion::PORT
}

fn default_name() -> String {
    "Moli".into()
}

#[derive(Debug)]
pub struct AppleTv {
    config: Config,
}

impl AppleTv {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for AppleTv {
    fn kind(&self) -> &'static str {
        "appletv"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// What a key sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    /// A HID button (pyatv's `HidCommand`).
    Button(i64),
    /// A media command (pyatv's `MediaControlCommand`).
    Media(i64),
}

fn key(name: &str) -> Option<Press> {
    Some(match name {
        "CursorUp" => Press::Button(1),
        "CursorDown" => Press::Button(2),
        "CursorLeft" => Press::Button(3),
        "CursorRight" => Press::Button(4),
        "Back" => Press::Button(5),
        "Confirm" => Press::Button(6),
        "Home" => Press::Button(7),
        "VolumeUp" => Press::Button(8),
        "VolumeDown" => Press::Button(9),
        "PlayPause" => Press::Button(14),
        "ChannelStepUp" => Press::Button(15),
        "ChannelStepDown" => Press::Button(16),
        // The guide: what the Apple TV's « … » opens in the TV app.
        "Options" => Press::Button(17),
        "Play" => Press::Media(1),
        "Pause" => Press::Media(2),
        "Next" => Press::Media(3),
        "Previous" => Press::Media(4),
        _ => return None,
    })
}

const WAKE: i64 = 13;
const SLEEP: i64 = 12;

/// `SystemStatus`: 1 asleep; 2 screensaver, 3 awake, 4 idle are on.
fn awake(state: i64) -> Option<bool> {
    match state {
        1 => Some(false),
        2..=4 => Some(true),
        _ => None,
    }
}

fn spec(
    key: &str,
    label: &str,
    kind: Kind,
    read: bool,
    write: bool,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read, write },
        unit: None,
        semantic,
    }
}

fn enumeration(values: &[&str]) -> Kind {
    Kind::Enum {
        values: values.iter().map(|v| (*v).into()).collect(),
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: "Apple TV".into(),
        manufacturer: Some("Apple".into()),
        model: Some("Apple TV".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "power",
                &moli_i18n::tr!("pilotes.appletv.allumee"),
                Kind::Binary,
                true,
                true,
                Semantic::OnOff,
            ),
            // Written: opens an app (its bundle id, a link, or its name).
            spec(
                "app",
                &moli_i18n::tr!("pilotes.appletv.application"),
                Kind::Text,
                false,
                true,
                Semantic::Other,
            ),
            // The apps it can open, « name=bundle id » one per line.
            spec(
                "apps",
                &moli_i18n::tr!("pilotes.appletv.applications"),
                Kind::Text,
                true,
                false,
                Semantic::Other,
            ),
            spec(
                "key",
                &moli_i18n::tr!("pilotes.appletv.touche"),
                enumeration(&KEYS),
                false,
                true,
                Semantic::Other,
            ),
            spec(
                "hold",
                &moli_i18n::tr!("pilotes.appletv.touche_maintenue"),
                enumeration(&HOLD_KEYS),
                false,
                true,
                Semantic::Other,
            ),
            spec(
                "media_control",
                &moli_i18n::tr!("pilotes.appletv.lecture_commande"),
                enumeration(&["play", "pause"]),
                false,
                true,
                Semantic::Other,
            ),
        ],
    }
}

/// The apps the Apple TV can open: (bundle id, name), by name, each bundle
/// id once. The `apps` point is « name=bundle id » per line: a name loses
/// its `=` and line breaks, a bundle id holding one is left out.
fn apps(content: &Op) -> Vec<(String, String)> {
    let Op::Dict(entries) = content else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut list: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(k, v)| {
            let bundle = k.as_str()?.trim();
            let name = v
                .as_str()?
                .replace(['=', '\n', '\r'], " ")
                .trim()
                .to_owned();
            let usable = !bundle.is_empty()
                && !name.is_empty()
                && !bundle.contains(['=', '\n', '\r'])
                && seen.insert(bundle.to_owned());
            usable.then(|| (bundle.to_owned(), name))
        })
        .collect();
    list.sort_by_key(|a| a.1.to_lowercase());
    list
}

/// What to open for `target`: a link as it is, a bundle id the Apple TV
/// lists, or the bundle id of the app of that name (any case). Anything
/// else is refused, never guessed.
fn launch_target<'a>(
    target: &'a str,
    apps: &'a [(String, String)],
) -> anyhow::Result<(&'static str, &'a str)> {
    let target = target.trim();
    if target.contains("://") {
        return Ok(("_urlS", target));
    }
    if let Some((bundle, _)) = apps.iter().find(|(b, _)| b == target) {
        return Ok(("_bundleID", bundle));
    }
    let wanted = target.to_lowercase();
    if let Some((bundle, _)) = apps.iter().find(|(_, n)| n.to_lowercase() == wanted) {
        return Ok(("_bundleID", bundle));
    }
    // A bundle id the list does not have yet (a list not read): sent as is.
    if apps.is_empty() && target.contains('.') && !target.contains(' ') {
        return Ok(("_bundleID", target));
    }
    bail!(moli_i18n::tr!(
        "pilotes.appletv.appli_inconnue",
        app = target
    ))
}

/// A connected Apple TV.
struct Live {
    session: Session,
    apps: Vec<(String, String)>,
    /// The button held down, if any (let go by `release`).
    held: Option<i64>,
}

impl Live {
    async fn button(&mut self, code: i64, down: bool) -> anyhow::Result<()> {
        self.session
            .request(
                "_hidC",
                Op::dict([
                    ("_hBtS", Op::Int(if down { 1 } else { 2 })),
                    ("_hidC", Op::Int(code)),
                ]),
            )
            .await?;
        Ok(())
    }

    async fn press(&mut self, name: &str) -> anyhow::Result<()> {
        match key(name) {
            Some(Press::Button(code)) => {
                self.button(code, true).await?;
                self.button(code, false).await
            }
            Some(Press::Media(code)) => self.media(code).await,
            None => bail!(moli_i18n::tr!(
                "pilotes.appletv.touche_inconnue",
                key = name
            )),
        }
    }

    async fn media(&mut self, code: i64) -> anyhow::Result<()> {
        self.session
            .request("_mcc", Op::dict([("_mcc", Op::Int(code))]))
            .await?;
        Ok(())
    }

    async fn hold(&mut self, name: &str) -> anyhow::Result<()> {
        // Whatever was held is let go first: one key down at a time.
        if let Some(code) = self.held.take() {
            self.button(code, false).await?;
        }
        if name == "release" {
            return Ok(());
        }
        let (true, Some(Press::Button(code))) = (HOLD_KEYS.contains(&name), key(name)) else {
            bail!(moli_i18n::tr!(
                "pilotes.appletv.touche_inconnue",
                key = name
            ));
        };
        self.button(code, true).await?;
        self.held = Some(code);
        Ok(())
    }

    async fn apply(&mut self, command: &CommandRequest) -> anyhow::Result<()> {
        match (&*command.key, &command.value) {
            // As pyatv does: the button let go alone wakes or sleeps it.
            ("power", Value::Bool(on)) => self.button(if *on { WAKE } else { SLEEP }, false).await,
            ("key", Value::Text(name)) => self.press(name).await,
            ("hold", Value::Text(name)) => self.hold(name).await,
            ("media_control", Value::Text(word)) => match &**word {
                "play" => self.media(1).await,
                "pause" => self.media(2).await,
                other => bail!(moli_i18n::tr!(
                    "pilotes.appletv.ne_se_regle_pas",
                    point = other
                )),
            },
            ("app", Value::Text(target)) => {
                let (field, value) = launch_target(target, &self.apps)?;
                self.session
                    .request("_launchApp", Op::dict([(field, Op::from(value))]))
                    .await?;
                Ok(())
            }
            (key, _) => bail!(moli_i18n::tr!(
                "pilotes.appletv.ne_se_regle_pas",
                point = key
            )),
        }
    }
}

/// Opens a session and says who Moli is, as Apple's Remote does.
async fn connect(
    config: &Config,
    credentials: &Credentials,
    ctx: &DriverCtx,
    id: &DeviceId,
) -> anyhow::Result<(Live, mpsc::Receiver<Event>)> {
    let (mut session, events) = Session::open(&config.host, config.port, credentials).await?;
    session
        .request(
            "_systemInfo",
            Op::dict([
                ("_bf", Op::Int(0)),
                ("_cf", Op::Int(512)),
                ("_clFl", Op::Int(128)),
                ("_i", Op::from("6d6f6c69")),
                ("_idsID", Op::from(credentials.client())),
                ("_pubID", Op::from("AA:BB:CC:DD:EE:FF")),
                ("_sf", Op::Int(256)),
                ("_sv", Op::from("170.18")),
                ("model", Op::from("iPhone14,3")),
                ("name", Op::from(config.name.as_str())),
            ]),
        )
        .await
        .context("system info")?;
    let rng = ring::rand::SystemRandom::new();
    let mut sid = [0u8; 4];
    ring::rand::SecureRandom::fill(&rng, &mut sid).ok();
    session
        .request(
            "_sessionStart",
            Op::dict([
                ("_srvT", Op::from("com.apple.tvremoteservices")),
                ("_sid", Op::Int(i64::from(u32::from_le_bytes(sid)))),
            ]),
        )
        .await
        .context("session start")?;
    // Older tvOS answer without it; newer ones want it before keys.
    if let Err(e) = session
        .request(
            "TVRCSessionStart",
            Op::dict([("ProtocolVersionKey", Op::from("1.2"))]),
        )
        .await
    {
        tracing::debug!("Apple TV: TVRCSessionStart: {e:#}");
    }
    for event in ["SystemStatus", "TVSystemStatus"] {
        session
            .event(
                "_interest",
                Op::dict([("_regEvents", Op::List(vec![Op::from(event)]))]),
            )
            .await?;
    }
    let apps = match session
        .request("FetchLaunchableApplicationsEvent", Op::dict::<&str>([]))
        .await
    {
        Ok(content) => apps(&content),
        Err(e) => {
            tracing::debug!("Apple TV: apps: {e:#}");
            Vec::new()
        }
    };
    let listed = apps
        .iter()
        .map(|(bundle, name)| format!("{name}={bundle}"))
        .collect::<Vec<_>>()
        .join("\n");
    ctx.set_state(id, "apps", Value::Text(listed.into()));
    let mut live = Live {
        session,
        apps,
        held: None,
    };
    check(&mut live, ctx, id).await;
    Ok((live, events))
}

/// Asks whether it sleeps (older tvOS do not say; the events then will).
async fn check(live: &mut Live, ctx: &DriverCtx, id: &DeviceId) {
    match live
        .session
        .request("FetchAttentionState", Op::dict::<&str>([]))
        .await
    {
        Ok(content) => {
            if let Some(on) = content.get("state").and_then(Op::as_int).and_then(awake) {
                ctx.set_state(id, "power", Value::Bool(on));
            }
        }
        Err(e) => tracing::debug!("Apple TV: attention state: {e:#}"),
    }
}

/// Pairing: the Apple TV shows a code, a person puts it in the vault.
/// Returns the credentials, stored; or `None` when nobody gave the code in
/// time (the driver then waits for a restart: a new code each time would
/// keep lighting up the family's screen).
async fn pair(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<Option<Credentials>> {
    // A code left from an earlier attempt is not this one's.
    let _ = ctx.take_secret(PIN).await;
    let pairing = Pairing::start(&config.host, config.port).await?;
    let command = format!("moli-os secrets set {} {PIN}", ctx.instance());
    ctx.wait_for(moli_i18n::tr!(
        "pilotes.appletv.attend_code",
        command = command
    ));
    let deadline = tokio::time::Instant::now() + PIN_WAIT;
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(None) };
                command.reply(Err(moli_i18n::tr!("pilotes.appletv.pas_appairee")));
            }
            () = tokio::time::sleep(PIN_POLL) => {
                let Some(pin) = ctx.take_secret(PIN).await? else { continue };
                let pin: String = pin.chars().filter(char::is_ascii_digit).collect();
                let credentials = pairing.finish(&pin, &config.name).await?;
                ctx.store_secret(CREDENTIALS, &credentials.to_secret()).await?;
                tracing::info!(instance = %ctx.instance(), "Apple TV paired");
                return Ok(Some(credentials));
            }
        }
    }
    Ok(None)
}

/// Refuses every order, forever (unpaired, waiting for a restart).
async fn park(ctx: &mut DriverCtx) -> anyhow::Result<()> {
    while let Some(command) = ctx.next_command().await {
        command.reply(Err(moli_i18n::tr!("pilotes.appletv.pas_appairee")));
    }
    Ok(())
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let credentials = if let Some(secret) = ctx.secret(CREDENTIALS) {
        Credentials::from_secret(&secret)?
    } else {
        if !ctx.can_store_secrets() {
            ctx.wait_for(ctx.secrets_problem().unwrap_or("no vault").to_owned());
            return park(ctx).await;
        }
        match pair(config, ctx).await {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                ctx.wait_for(moli_i18n::tr!("pilotes.appletv.code_trop_tard"));
                return park(ctx).await;
            }
            // Not returned as an error: the restart would show a new code
            // on the family's screen, again and again.
            Err(e) => {
                tracing::warn!(instance = %ctx.instance(), "Apple TV pairing failed: {e:#}");
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.appletv.appairage_echoue",
                    raison = format!("{e:#}")
                ));
                return park(ctx).await;
            }
        }
    };
    let id = ctx.device_id(&credentials.device());
    ctx.upsert_device(device(ctx, &id));
    let mut attempt = 0usize;
    loop {
        let (mut live, mut events) = match connect(config, &credentials, ctx, &id).await {
            Ok(connected) => connected,
            Err(e) => {
                ctx.set_availability(&id, false);
                let wait = RETRY[attempt.min(RETRY.len() - 1)];
                attempt += 1;
                tracing::debug!("Apple TV unreachable ({e:#}), again in {wait} s");
                // Orders meanwhile are refused at once, never replayed later.
                let until = tokio::time::Instant::now() + Duration::from_secs(wait);
                loop {
                    tokio::select! {
                        command = ctx.next_command() => {
                            let Some(command) = command else { return Ok(()) };
                            command.reply(Err(moli_i18n::tr!("pilotes.appletv.injoignable")));
                        }
                        () = tokio::time::sleep_until(until) => break,
                    }
                }
                continue;
            }
        };
        attempt = 0;
        ctx.set_availability(&id, true);
        ctx.ready();
        let mut tick = tokio::time::interval(CHECK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tokio::select! {
                command = ctx.next_command() => {
                    let Some(command) = command else { return Ok(()) };
                    if command.abandoned() {
                        continue;
                    }
                    let result = live.apply(&command).await.map_err(|e| format!("{e:#}"));
                    command.reply(result);
                }
                event = events.recv() => {
                    let Some((name, content)) = event else { break };
                    if (name == "SystemStatus" || name == "TVSystemStatus")
                        && let Some(on) = content.get("state").and_then(Op::as_int).and_then(awake)
                    {
                            ctx.set_state(&id, "power", Value::Bool(on));
                    }
                }
                _ = tick.tick() => {
                    if !live.session.alive() {
                        break;
                    }
                    check(&mut live, ctx, &id).await;
                }
            }
        }
        ctx.set_availability(&id, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_sends_something_and_holds_are_buttons() {
        for name in KEYS {
            assert!(key(name).is_some(), "{name}");
        }
        for name in HOLD_KEYS.iter().filter(|k| **k != "release") {
            assert!(matches!(key(name), Some(Press::Button(_))), "{name}");
        }
        assert_eq!(key("Confirm"), Some(Press::Button(6)));
        assert_eq!(
            key("Back"),
            Some(Press::Button(5)),
            "Menu is the Apple TV's back"
        );
        assert_eq!(key("Next"), Some(Press::Media(3)));
        assert_eq!(key("Mute"), None, "an Apple TV has no mute");
    }

    #[test]
    fn sleep_is_off_and_the_rest_is_on() {
        assert_eq!(awake(1), Some(false));
        assert_eq!(awake(2), Some(true));
        assert_eq!(awake(3), Some(true));
        assert_eq!(awake(0), None);
    }

    #[test]
    fn an_app_opens_by_name_bundle_or_link_never_by_guess() {
        let list = apps(&Op::dict([
            ("com.netflix.Netflix", Op::from("Netflix")),
            ("com.google.ios.youtube", Op::from("YouTube")),
            ("com.apple.TVWatchList", Op::from("TV")),
        ]));
        assert_eq!(list[0].1, "Netflix", "sorted by name");
        // Each bundle id once; no `=` or line break where the point splits.
        let odd = apps(&Op::Dict(vec![
            (Op::from("com.a"), Op::from("A")),
            (Op::from("com.a"), Op::from("A again")),
            (Op::from("com.b"), Op::from("B=1\nx")),
            (Op::from("com=c"), Op::from("C")),
        ]));
        assert_eq!(
            odd,
            [
                ("com.a".to_owned(), "A".to_owned()),
                ("com.b".to_owned(), "B 1 x".to_owned()),
            ]
        );
        assert_eq!(
            launch_target("netflix", &list).unwrap(),
            ("_bundleID", "com.netflix.Netflix")
        );
        assert_eq!(
            launch_target("com.google.ios.youtube", &list).unwrap(),
            ("_bundleID", "com.google.ios.youtube")
        );
        assert_eq!(
            launch_target("https://www.youtube.com/watch?v=x", &list).unwrap(),
            ("_urlS", "https://www.youtube.com/watch?v=x")
        );
        assert!(launch_target("Disney+", &list).is_err());
        assert!(
            launch_target("com.disney.disneyplus", &list).is_err(),
            "not on this Apple TV"
        );
        // The list not read yet: a bundle id goes as it is, a name does not.
        assert!(launch_target("com.disney.disneyplus", &[]).is_ok());
        assert!(launch_target("Disney", &[]).is_err());
    }
}

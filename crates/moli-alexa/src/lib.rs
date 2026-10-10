//! Amazon Echo speakers in the house, through the Alexa app's own web API
//! (unofficial: Amazon publishes nothing to drive music): music by search on
//! a speaker or a multi-room group, play and pause, volume, a sentence, an
//! announcement, an Alexa command. The account is connected once from the
//! dashboard: Amazon's own sign-in page, then its last address pasted into
//! Moli (no password ever goes through Moli); the refresh token it gives is
//! kept in the encrypted secret store.

mod api;
mod auth;
mod echo;
mod encode;

use std::collections::HashMap;
use std::time::Duration;

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use tokio::time::{Instant, MissedTickBehavior};

use crate::api::{Failure, Session};
use crate::echo::Echo;

const REFRESH: &str = "refresh_token";
const SERIAL: &str = "device_serial";
/// What the speakers play, looked at this often.
const POLL: Duration = Duration::from_secs(30);
/// The speakers' volumes.
const VOLUMES: Duration = Duration::from_secs(120);
/// The account's speakers (one added, renamed…).
const DEVICES: Duration = Duration::from_secs(30 * 60);
/// The site's cookies are good two weeks: renewed well before.
const SESSION: Duration = Duration::from_secs(4 * 24 * 3600);
const RETRY_MIN: Duration = Duration::from_secs(30);
/// Why a session ends without a fault: new cookies are due, or Amazon
/// ended it; a new one starts at once.
const RENEW: &str = "the Alexa session is renewed";
const RETRY_MAX: Duration = Duration::from_secs(15 * 60);

fn default_domain() -> String {
    "amazon.fr".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The account's Amazon site: `amazon.fr`, `amazon.de`, `amazon.com`…
    #[serde(default = "default_domain")]
    pub domain: String,
    /// The music service by default (`AMAZON_MUSIC`, `SPOTIFY`, `DEEZER`,
    /// `TUNEIN`…); empty: the account's own.
    #[serde(default)]
    pub music: String,
}

impl Config {
    /// (`fr-FR`, `fr_FR`) from the site.
    fn languages(&self) -> (&'static str, &'static str) {
        match self.domain.rsplit('.').next().unwrap_or_default() {
            "fr" => ("fr-FR", "fr_FR"),
            "de" => ("de-DE", "de_DE"),
            "it" => ("it-IT", "it_IT"),
            "es" => ("es-ES", "es_ES"),
            "uk" => ("en-GB", "en_GB"),
            _ => ("en-US", "en_US"),
        }
    }
}

#[derive(Debug)]
pub struct Alexa {
    config: Config,
}

impl Alexa {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Alexa {
    fn kind(&self) -> &'static str {
        "alexa"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn text_point(key: &str, label: &str, write: bool) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Text,
        access: Access { read: true, write },
        unit: None,
        semantic: Semantic::Config,
    }
}

/// The account: its state, and where the dashboard signs it in.
fn account_device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.alexa.compte").into(),
        manufacturer: Some("Amazon".into()),
        model: Some("Alexa".into()),
        description: Some(moli_i18n::tr!("pilotes.alexa.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            text_point(
                "status",
                &moli_i18n::tr!("pilotes.alexa.etat_compte"),
                false,
            ),
            text_point(
                "sign_in_url",
                &moli_i18n::tr!("pilotes.alexa.page_amazon"),
                false,
            ),
            text_point(
                "sign_in",
                &moli_i18n::tr!("pilotes.alexa.adresse_collee"),
                true,
            ),
        ],
    }
}

fn set_text(ctx: &DriverCtx, id: &DeviceId, key: &str, text: &str) {
    ctx.set_state(id, key, Value::Text(text.into()));
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let account = ctx.device_id("account");
    ctx.upsert_device(account_device(ctx, &account));
    ctx.set_availability(&account, true);
    let mut pause = RETRY_MIN;
    loop {
        let Some(refresh) = ctx.secret(REFRESH) else {
            if !sign_in(config, ctx, &account).await? {
                return Ok(());
            }
            continue;
        };
        set_text(ctx, &account, "status", "connecting");
        let started = Instant::now();
        match connected(config, ctx, &account, &refresh).await {
            Ok(()) => return Ok(()),
            Err(e) if format!("{e:#}").contains(RENEW) => {
                tracing::info!(instance = %ctx.instance(), "alexa session renewed");
            }
            Err(e) if format!("{e:#}").contains(auth::SIGN_IN_AGAIN) => {
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "alexa: sign in again");
                ctx.forget_secret(REFRESH).await?;
            }
            Err(e) => {
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "alexa unreachable");
                set_text(ctx, &account, "status", "error");
                if started.elapsed() > RETRY_MAX {
                    pause = RETRY_MIN;
                }
                tokio::select! {
                    () = tokio::time::sleep(pause) => {}
                    () = ctx.cancelled() => return Ok(()),
                }
                pause = (pause * 2).min(RETRY_MAX);
            }
        }
    }
}

/// Waits for the person to sign in from the dashboard. `false`: stopped.
async fn sign_in(config: &Config, ctx: &mut DriverCtx, account: &DeviceId) -> anyhow::Result<bool> {
    let (_, language) = config.languages();
    let mut pending = auth::SignIn::new(language)?;
    set_text(ctx, account, "sign_in_url", &pending.url);
    set_text(ctx, account, "status", "sign_in");
    ctx.wait_for(moli_i18n::tr!("pilotes.alexa.a_connecter"));
    loop {
        // `None` once Moli stops.
        let Some(command) = ctx.next_command().await else {
            return Ok(false);
        };
        if command.device.id != *account || &*command.key != "sign_in" {
            command.reply(Err(moli_i18n::tr!("pilotes.alexa.pas_connecte")));
            continue;
        }
        let Value::Text(pasted) = &command.value else {
            command.reply(Err(moli_i18n::tr!("pilotes.alexa.adresse_fausse")));
            continue;
        };
        let code = match auth::code_of(pasted) {
            Ok(code) => code,
            Err(auth::Pasted::NotTheEnd) => {
                command.reply(Err(moli_i18n::tr!("pilotes.alexa.adresse_fausse")));
                continue;
            }
            Err(auth::Pasted::NoCode) => {
                command.reply(Err(moli_i18n::tr!("pilotes.alexa.adresse_sans_code")));
                continue;
            }
        };
        match auth::register(&pending, &code).await {
            Ok(registered) => {
                if let Err(e) = keep(ctx, &registered.refresh_token, &pending.serial).await {
                    command.reply(Err(moli_i18n::tr!("pilotes.alexa.coffre", why = e)));
                    return Err(e);
                }
                set_text(ctx, account, "sign_in_url", "");
                command.reply(Ok(()));
                tracing::info!(instance = %ctx.instance(), "alexa account connected");
                ctx.ready();
                return Ok(true);
            }
            Err(e) => {
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "alexa sign-in refused");
                // A code serves once: a new page for the next try.
                pending = auth::SignIn::new(language)?;
                set_text(ctx, account, "sign_in_url", &pending.url);
                command.reply(Err(moli_i18n::tr!(
                    "pilotes.alexa.refuse",
                    why = format!("{e:#}")
                )));
            }
        }
    }
}

async fn keep(ctx: &DriverCtx, refresh: &str, serial: &str) -> anyhow::Result<()> {
    ctx.store_secret(REFRESH, refresh).await?;
    ctx.store_secret(SERIAL, serial).await?;
    Ok(())
}

/// The speakers by Moli device, and the account's session.
struct Home {
    session: Session,
    speakers: HashMap<DeviceId, Echo>,
    music: String,
    /// Speakers whose state could not be read, said once per session.
    unreadable: std::collections::HashSet<String>,
}

impl Home {
    fn echo(&self, serial: &str) -> Option<&Echo> {
        self.speakers.values().find(|e| e.serial == serial)
    }
}

/// Connected: the speakers, then their orders and their state, until stopped.
async fn connected(
    config: &Config,
    ctx: &mut DriverCtx,
    account: &DeviceId,
    refresh: &str,
) -> anyhow::Result<()> {
    let (locale, _) = config.languages();
    let session = Session::open(refresh, &config.domain, locale).await?;
    let music = if config.music.is_empty() {
        session
            .default_music()
            .await
            .unwrap_or_else(|| "AMAZON_MUSIC".into())
    } else {
        config.music.clone()
    };
    let mut home = Home {
        session,
        speakers: HashMap::new(),
        music,
        unreadable: std::collections::HashSet::new(),
    };
    discover(ctx, &mut home).await?;
    set_text(ctx, account, "status", "connected");
    ctx.ready();
    tracing::info!(
        instance = %ctx.instance(),
        speakers = home.speakers.len(),
        music = %home.music,
        "alexa connected"
    );
    let mut poll = tokio::time::interval(POLL);
    let mut volumes = tokio::time::interval(VOLUMES);
    let mut devices = tokio::time::interval(DEVICES);
    for timer in [&mut poll, &mut volumes, &mut devices] {
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        timer.reset();
    }
    let renew_at = Instant::now() + SESSION;
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = order(config, &mut home, refresh, &command).await;
                command.reply(result.map_err(|e| moli_i18n::tr!("pilotes.alexa.echec", why = e)));
                poll.reset_after(Duration::from_secs(3));
            }
            _ = poll.tick() => playing(ctx, &mut home).await?,
            _ = volumes.tick() => match home.session.volumes().await {
                Ok(all) => {
                    for (id, e) in &home.speakers {
                        if let Some((_, v)) = all.iter().find(|(s, _)| *s == e.serial) {
                            ctx.set_state(id, "volume", Value::Float(*v));
                        }
                    }
                }
                Err(e) => tracing::warn!(instance = %ctx.instance(), error = %e, "alexa volumes"),
            },
            _ = devices.tick() => discover(ctx, &mut home).await?,
            () = tokio::time::sleep_until(renew_at) => {
                // New cookies: `connected` starts again.
                anyhow::bail!("{RENEW}");
            }
        }
    }
}

/// The account's speakers and groups, shown in the house.
async fn discover(ctx: &DriverCtx, home: &mut Home) -> anyhow::Result<()> {
    let answer = home
        .session
        .devices()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    home.speakers.clear();
    for e in echo::speakers(&answer) {
        tracing::info!(
            instance = %ctx.instance(),
            name = %e.name,
            family = %e.family,
            device_type = %e.device_type,
            group = e.group,
            online = e.online,
            "alexa speaker"
        );
        let id = ctx.device_id(&e.serial);
        ctx.upsert_device(echo::device(ctx, &id, &e));
        ctx.set_availability(&id, e.online);
        home.speakers.insert(id, e);
    }
    Ok(())
}

/// What each speaker plays (a session over: an error, `connected` starts again).
async fn playing(ctx: &DriverCtx, home: &mut Home) -> anyhow::Result<()> {
    let Home {
        session,
        speakers,
        unreadable,
        ..
    } = home;
    for (id, e) in speakers.iter() {
        if !e.online {
            continue;
        }
        match session.playing(e).await {
            Ok(p) => {
                ctx.set_state(id, "playing", Value::Bool(p.state == "PLAYING"));
                set_text(ctx, id, "state", &p.state.to_lowercase());
                set_text(ctx, id, "title", &p.title);
                set_text(ctx, id, "artist", &p.artist);
                if let (Some(v), false) = (p.volume, e.group) {
                    ctx.set_state(id, "volume", Value::Float(v));
                }
            }
            Err(Failure::Expired) => anyhow::bail!("{RENEW}"),
            Err(Failure::Other(err)) => {
                if unreadable.insert(e.serial.clone()) {
                    tracing::warn!(speaker = %e.name, error = %format!("{err:#}"), "alexa player state");
                }
            }
        }
    }
    Ok(())
}

/// One order; a session that expired is renewed once and the order sent again.
async fn order(
    config: &Config,
    home: &mut Home,
    refresh: &str,
    command: &CommandRequest,
) -> Result<(), String> {
    match send(home, command).await {
        Err(Failure::Expired) => {
            let (locale, _) = config.languages();
            home.session = Session::open(refresh, &config.domain, locale)
                .await
                .map_err(|e| format!("{e:#}"))?;
            send(home, command).await.map_err(|e| e.to_string())
        }
        other => other.map_err(|e| e.to_string()),
    }
}

fn text(value: &Value) -> Result<&str, Failure> {
    match value {
        Value::Text(t) => api::sayable(t).map_err(Failure::Other),
        _ => Err(Failure::Other(anyhow::anyhow!("text expected"))),
    }
}

async fn send(home: &Home, command: &CommandRequest) -> Result<(), Failure> {
    let e = home
        .speakers
        .get(&command.device.id)
        .ok_or_else(|| Failure::Other(anyhow::anyhow!("unknown speaker")))?;
    let s = &home.session;
    let (customer, locale) = (s.customer.as_str(), s.locale.as_str());
    // A group's members, for what a group cannot do itself.
    let members: Vec<&Echo> = e.members.iter().filter_map(|m| home.echo(m)).collect();
    let each = |f: &dyn Fn(&Echo) -> serde_json::Value| {
        if e.group && !members.is_empty() {
            api::together(members.iter().map(|m| f(m)).collect())
        } else {
            f(e)
        }
    };
    match (&*command.key, &command.value) {
        ("playing", Value::Bool(play)) => {
            s.player(e, if *play { "PlayCommand" } else { "PauseCommand" })
                .await
        }
        ("volume", v) => {
            let level = v
                .as_f64()
                .ok_or_else(|| Failure::Other(anyhow::anyhow!("a number is expected")))?;
            #[allow(clippy::cast_possible_truncation)]
            let level = level.round() as i64;
            s.run(&api::volume(e, customer, locale, level)).await
        }
        ("play", v) => {
            let phrase = text(v)?;
            s.play(e, phrase, &home.music).await
        }
        ("say", v) => {
            let words = text(v)?;
            s.run(&each(&|m| api::speak(m, customer, locale, words)))
                .await
        }
        ("announce", v) => {
            let words = text(v)?;
            let targets: Vec<&Echo> = if e.group { members.clone() } else { vec![e] };
            s.run(&api::announce(&targets, customer, locale, words))
                .await
        }
        ("command", v) => {
            let words = text(v)?;
            s.run(&each(&|m| api::text_command(m, customer, locale, words)))
                .await
        }
        (key, _) => Err(Failure::Other(anyhow::anyhow!("{key} cannot be written"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_site_gives_the_languages() {
        let config = |domain: &str| Config {
            domain: domain.into(),
            music: String::new(),
        };
        assert_eq!(config("amazon.fr").languages(), ("fr-FR", "fr_FR"));
        assert_eq!(config("amazon.co.uk").languages(), ("en-GB", "en_GB"));
        assert_eq!(config("amazon.com").languages(), ("en-US", "en_US"));
        let parsed: Config = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(parsed.domain, "amazon.fr");
        assert!(serde_json::from_value::<Config>(serde_json::json!({ "password": "x" })).is_err());
    }
}

//! Pushover, straight to its API: Moli's notifications reach the household's
//! phones (Android, iOS, desktop) through the Pushover app, with no Moli app,
//! no pairing and no way in: one HTTPS call out to `api.pushover.net`.
//!
//! One device, « Pushover », whose `push` point sends its text, like a phone
//! paired through the Moli app: the « Téléphone » channel of automations and
//! the assistant's reminders reach it with nothing changed elsewhere. A
//! command like any other: journaled, guarded.
//!
//! - **Secrets** `token` (the application's API token) and `user` (the user's
//!   or group's key), the names of the Pushover API, in the vault: never in
//!   `moli.toml`.
//! - **At start**: the keys are checked (`users/validate.json`) and the
//!   devices named in the configuration too; a refusal stops the driver
//!   until a human fixes it. The month's quota is read and published.
//! - **A cap per instance** (a few messages a minute, a few dozen an hour):
//!   a runaway automation or an agent cannot spend the month in an evening.
//!   What goes beyond is refused and journaled.
//! - **A refusal stops the sending** (a key revoked, a device gone): Pushover
//!   blocks for a while an address that keeps sending bad requests. A spent
//!   quota stops it until the month starts again.
//! - **Text stays text**: no HTML, no link, no attachment; the title and the
//!   message cut to Pushover's limits instead of refused.

mod api;

use std::collections::VecDeque;
use std::time::Duration;

use api::{Client, Failure, Message, Options, Quota, Web};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use tokio::time::Instant;

const TOKEN: &str = "token";
const USER: &str = "user";
/// Pushover's limits (characters).
const MAX_TITLE: usize = 250;
const MAX_BODY: usize = 1024;
const MAX_DEVICES: usize = 10;
const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Pushover device names to reach; empty: every device of the account.
    #[serde(default)]
    pub devices: Vec<String>,
    /// -2 (nothing shown) to 1 (high); 2 (emergency) is not for now.
    #[serde(default)]
    pub priority: i8,
    /// A Pushover sound name; the account's own when absent.
    #[serde(default)]
    pub sound: Option<String>,
    /// Messages at most in a minute.
    #[serde(default = "default_per_minute")]
    pub per_minute: u32,
    /// Messages at most in an hour.
    #[serde(default = "default_per_hour")]
    pub per_hour: u32,
}

const fn default_per_minute() -> u32 {
    5
}

const fn default_per_hour() -> u32 {
    30
}

#[derive(Debug)]
pub struct Pushover {
    config: Config,
}

impl Pushover {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (-2..=1).contains(&config.priority),
            "priority: from -2 to 1 (2, emergency, is not supported)"
        );
        anyhow::ensure!(
            config.devices.len() <= MAX_DEVICES,
            "devices: {MAX_DEVICES} at most"
        );
        anyhow::ensure!(
            config.devices.iter().all(|d| name_ok(d, 25)),
            "devices: Pushover device names (letters, digits, - and _, 25 at most)"
        );
        anyhow::ensure!(
            config.sound.as_deref().is_none_or(|s| name_ok(s, 64)),
            "sound: a Pushover sound name (letters, digits, - and _)"
        );
        anyhow::ensure!(
            (1..=30).contains(&config.per_minute),
            "per_minute: from 1 to 30"
        );
        anyhow::ensure!(
            (config.per_minute..=600).contains(&config.per_hour),
            "per_hour: from per_minute to 600"
        );
        Ok(Self { config })
    }
}

impl Driver for Pushover {
    fn kind(&self) -> &'static str {
        "pushover"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn name_ok(name: &str, max: usize) -> bool {
    !name.is_empty()
        && name.len() <= max
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Application tokens and user keys: 30 letters and digits.
fn key_ok(key: &str) -> bool {
    key.len() == 30 && key.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// What is wrong with a stored key, told without the key: which secret, how
/// long it is, how many characters are not letters or digits.
fn key_problem(name: &str, key: &str) -> Option<String> {
    if key_ok(key) {
        return None;
    }
    let other = key.chars().filter(|c| !c.is_ascii_alphanumeric()).count();
    Some(moli_i18n::tr!(
        "pilotes.pushover.cle_invalide",
        name = name,
        len = key.chars().count(),
        other = other
    ))
}

/// « Title\nMessage », or just a message (titled « Moli »), cut to
/// Pushover's limits rather than refused.
fn message(text: &str) -> Option<Message> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (title, body) = match text.split_once('\n') {
        Some((t, b)) if !t.trim().is_empty() && !b.trim().is_empty() => (t.trim(), b.trim()),
        _ => ("Moli", text),
    };
    Some(Message {
        title: cut(title, MAX_TITLE),
        body: cut(body, MAX_BODY),
    })
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// The messages that left in the last hour, to keep to the instance's cap.
#[derive(Debug)]
struct Cap {
    per_minute: usize,
    per_hour: usize,
    sent: VecDeque<Instant>,
}

impl Cap {
    fn new(config: &Config) -> Self {
        Self {
            per_minute: config.per_minute as usize,
            per_hour: config.per_hour as usize,
            sent: VecDeque::new(),
        }
    }

    /// One more message may leave now (and is counted).
    fn take(&mut self, now: Instant) -> bool {
        while self
            .sent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= HOUR)
        {
            self.sent.pop_front();
        }
        let last_minute = self
            .sent
            .iter()
            .filter(|t| now.duration_since(**t) < MINUTE)
            .count();
        if last_minute >= self.per_minute || self.sent.len() >= self.per_hour {
            return false;
        }
        self.sent.push_back(now);
        true
    }
}

/// Why nothing leaves any more.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stop {
    /// Refused by Pushover: until a human fixes it and restarts.
    Refused(String),
    /// The month's quota is spent: until `reset` (Unix seconds), or an hour
    /// when Pushover did not say.
    Quota(u64),
}

/// What one `push` did, for the device to show.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    result: Result<(), String>,
    quota: Option<Quota>,
    /// The sending just stopped (the reason) or started again.
    stopped: Option<String>,
    resumed: bool,
}

impl Default for Outcome {
    fn default() -> Self {
        Self {
            result: Ok(()),
            quota: None,
            stopped: None,
            resumed: false,
        }
    }
}

/// The instance's sending: options, cap, and whether it is stopped.
struct Sender<'a> {
    client: Client<'a>,
    options: Options,
    cap: Cap,
    stop: Option<Stop>,
}

impl<'a> Sender<'a> {
    fn new(client: Client<'a>, config: &Config) -> Self {
        Self {
            client,
            options: Options {
                devices: config.devices.clone(),
                priority: config.priority,
                sound: config.sound.clone(),
            },
            cap: Cap::new(config),
            stop: None,
        }
    }

    /// `push`: one message, unless stopped or over the cap.
    async fn push(&mut self, text: &str, now: Instant, unix: u64) -> Outcome {
        let mut outcome = Outcome::default();
        match &self.stop {
            Some(Stop::Refused(why)) => {
                outcome.result = Err(moli_i18n::tr!("pilotes.pushover.arrete", why = why));
                return outcome;
            }
            Some(Stop::Quota(until)) if unix < *until => {
                outcome.result = Err(moli_i18n::tr!("pilotes.pushover.quota_epuise"));
                return outcome;
            }
            Some(Stop::Quota(_)) => {
                self.stop = None;
                outcome.resumed = true;
            }
            None => {}
        }
        let Some(message) = message(text) else {
            outcome.result = Err(moli_i18n::tr!("pilotes.pushover.message_vide"));
            return outcome;
        };
        if !self.cap.take(now) {
            tracing::warn!(
                per_minute = self.cap.per_minute,
                per_hour = self.cap.per_hour,
                "pushover: over the cap, message refused"
            );
            outcome.result = Err(moli_i18n::tr!(
                "pilotes.pushover.trop_de_messages",
                per_minute = self.cap.per_minute,
                per_hour = self.cap.per_hour
            ));
            return outcome;
        }
        match self.client.send(&message, &self.options).await {
            Ok(quota) => {
                outcome.quota = quota;
                outcome.result = Ok(());
            }
            Err(failure) => {
                let text = failure.text();
                match failure {
                    Failure::Refused { .. } => {
                        self.stop = Some(Stop::Refused(text.clone()));
                        outcome.stopped = Some(text.clone());
                    }
                    Failure::Quota { reset } => {
                        self.stop = Some(Stop::Quota(reset.unwrap_or(unix + HOUR.as_secs())));
                        outcome.stopped = Some(text.clone());
                        outcome.quota = Some(Quota {
                            remaining: 0,
                            reset,
                        });
                    }
                    Failure::Network(_) | Failure::Server { .. } => {}
                }
                outcome.result = Err(text);
            }
        }
        outcome
    }
}

fn describe(ctx: &DriverCtx, id: &DeviceId, devices: &[String]) -> Device {
    let model = if devices.is_empty() {
        moli_i18n::tr!("pilotes.pushover.tous_les_appareils")
    } else {
        devices.join(", ")
    };
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: "Pushover".into(),
        manufacturer: Some("Pushover".into()),
        model: Some(model.into()),
        description: Some(moli_i18n::tr!("pilotes.pushover.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            PointSpec {
                key: "push".into(),
                label: moli_i18n::tr!("pilotes.pushover.envoyer").into(),
                kind: Kind::Text,
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            PointSpec {
                key: "remaining".into(),
                label: moli_i18n::tr!("pilotes.pushover.restants").into(),
                kind: Kind::Numeric {
                    min: Some(0.0),
                    max: None,
                    step: Some(1.0),
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Other,
            },
        ],
    }
}

async fn park(ctx: &DriverCtx, id: &DeviceId, reason: String) -> anyhow::Result<()> {
    ctx.set_availability(id, false);
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn unix_now() -> u64 {
    moli_core::now_ms() / 1000
}

fn publish_quota(ctx: &DriverCtx, id: &DeviceId, quota: Quota) {
    #[allow(clippy::cast_precision_loss)]
    ctx.set_state(id, "remaining", Value::Float(quota.remaining as f64));
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let id = ctx.device_id("pushover");
    ctx.upsert_device(describe(ctx, &id, &config.devices));
    let (Some(token), Some(user)) = (ctx.secret(TOKEN), ctx.secret(USER)) else {
        return park(ctx, &id, moli_i18n::tr!("pilotes.pushover.cles_manquantes")).await;
    };
    let (token, user) = (token.trim().to_owned(), user.trim().to_owned());
    if let Some(problem) = key_problem(TOKEN, &token).or_else(|| key_problem(USER, &user)) {
        return park(ctx, &id, problem).await;
    }
    let web: &dyn Web = &api::Internet;
    let client = Client::new(web, token, user);
    match start(&client, &config.devices).await {
        Ok(Ok(quota)) => {
            if let Some(quota) = quota {
                publish_quota(ctx, &id, quota);
            }
        }
        // A refused key will not get better by asking every minute.
        Ok(Err(reason)) => return park(ctx, &id, reason).await,
        Err(failure) => anyhow::bail!(failure.text()),
    }
    ctx.set_availability(&id, true);
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), devices = config.devices.len(), "pushover ready");

    let mut sender = Sender::new(client, config);
    while let Some(command) = ctx.next_command().await {
        let result = match (&*command.key, &command.value) {
            ("push", Value::Text(text)) => {
                let outcome = sender.push(text, Instant::now(), unix_now()).await;
                if let Some(quota) = outcome.quota {
                    publish_quota(ctx, &id, quota);
                }
                if outcome.resumed {
                    ctx.set_availability(&id, true);
                    ctx.ready();
                }
                if let Some(reason) = outcome.stopped {
                    tracing::warn!(instance = %ctx.instance(), "pushover: sending stopped");
                    ctx.set_availability(&id, false);
                    ctx.wait_for(reason);
                }
                outcome.result
            }
            ("push", _) => Err(moli_i18n::tr!("pilotes.pushover.message_texte")),
            _ => Err(moli_i18n::tr!(
                "pilotes.pushover.ne_s_ecrit_pas",
                point = command.key
            )),
        };
        command.reply(result);
    }
    Ok(())
}

/// The keys and devices checked, the quota read. `Ok(Err(reason))`: refused,
/// to be fixed by a human; `Err`: Pushover unreachable, try again later.
async fn start(
    client: &Client<'_>,
    wanted: &[String],
) -> Result<Result<Option<Quota>, String>, Failure> {
    let known = match client.validate().await {
        Ok(known) => known,
        Err(Failure::Refused { why, .. }) => {
            return Ok(Err(moli_i18n::tr!(
                "pilotes.pushover.cles_refusees",
                why = why
            )));
        }
        Err(failure) => return Err(failure),
    };
    let unknown: Vec<&str> = wanted
        .iter()
        .filter(|d| !known.iter().any(|k| k == *d))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Ok(Err(moli_i18n::tr!(
            "pilotes.pushover.appareils_inconnus",
            unknown = unknown.join(", "),
            known = known.join(", ")
        )));
    }
    // The quota is a nicety: without it, the first message brings it.
    Ok(Ok(client.limits().await.ok()))
}

#[cfg(test)]
mod tests {
    use super::api::fake::{Fake, Reply};
    use super::*;

    const KEY: &str = "faketokenfaketokenfaketoken000";

    fn config() -> Config {
        Config {
            devices: Vec::new(),
            priority: 0,
            sound: None,
            per_minute: default_per_minute(),
            per_hour: default_per_hour(),
        }
    }

    fn client(web: &Fake) -> Client<'_> {
        Client::new(web, KEY.into(), KEY.into())
    }

    #[test]
    fn the_configuration_is_checked() {
        assert!(Pushover::new(config()).is_ok());
        let bad = [
            Config {
                priority: 2,
                ..config()
            },
            Config {
                priority: -3,
                ..config()
            },
            Config {
                devices: vec!["mon iphone".into()],
                ..config()
            },
            Config {
                devices: vec!["d".into(); 11],
                ..config()
            },
            Config {
                sound: Some("../x".into()),
                ..config()
            },
            Config {
                per_minute: 0,
                ..config()
            },
            Config {
                per_minute: 10,
                per_hour: 5,
                ..config()
            },
            Config {
                per_hour: 601,
                ..config()
            },
        ];
        for config in bad {
            assert!(Pushover::new(config.clone()).is_err(), "{config:?}");
        }
    }

    #[test]
    fn keys_look_like_pushovers() {
        assert!(key_ok(KEY));
        assert!(!key_ok(""));
        assert!(!key_ok("short"));
        assert!(!key_ok("faketokenfaketokenfaketoken00/"));
        assert_eq!(key_problem(TOKEN, KEY), None);
        let said = key_problem(USER, "abc def").unwrap();
        assert!(said.contains("user") && said.contains('7') && said.contains('1'));
        assert!(!said.contains("abc"));
    }

    #[test]
    fn messages_are_shaped_and_cut() {
        assert_eq!(message("  \n "), None);
        let m = message("Lave-linge\nLe linge est prêt").unwrap();
        assert_eq!(
            (m.title.as_str(), m.body.as_str()),
            ("Lave-linge", "Le linge est prêt")
        );
        let m = message("Une ligne").unwrap();
        assert_eq!((m.title.as_str(), m.body.as_str()), ("Moli", "Une ligne"));
        let m = message("Titre\nligne 1\nligne 2").unwrap();
        assert_eq!(m.body, "ligne 1\nligne 2");
        let m = message(&format!("{}\n{}", "t".repeat(300), "é".repeat(2000))).unwrap();
        assert_eq!(m.title.chars().count(), MAX_TITLE);
        assert_eq!(m.body.chars().count(), MAX_BODY);
        assert!(m.title.ends_with('…') && m.body.ends_with('…'));
    }

    #[test]
    fn the_cap_counts_a_minute_and_an_hour() {
        let mut cap = Cap::new(&Config {
            per_minute: 2,
            per_hour: 3,
            ..config()
        });
        let t = Instant::now();
        assert!(cap.take(t));
        assert!(cap.take(t));
        assert!(!cap.take(t + Duration::from_secs(59)));
        assert!(cap.take(t + MINUTE));
        // Three in the hour: nothing more until the first one is an hour old.
        assert!(!cap.take(t + Duration::from_secs(1800)));
        assert!(!cap.take(t + HOUR - Duration::from_secs(1)));
        assert!(cap.take(t + HOUR));
    }

    #[tokio::test]
    async fn a_push_leaves_and_brings_the_quota() {
        let web = Fake::default().then(Reply::Json(
            200,
            r#"{"status":1}"#,
            vec![("X-Limit-App-Remaining", "42")],
        ));
        let mut sender = Sender::new(client(&web), &config());
        let outcome = sender
            .push("Poubelles\nSortir le bac jaune", Instant::now(), 0)
            .await;
        assert_eq!(outcome.result, Ok(()));
        assert_eq!(outcome.quota.map(|q| q.remaining), Some(42));
        assert!(web.asked()[0].body.contains("title=Poubelles"));
    }

    #[tokio::test]
    async fn an_empty_push_asks_nothing() {
        let web = Fake::default();
        let mut sender = Sender::new(client(&web), &config());
        assert!(sender.push("   ", Instant::now(), 0).await.result.is_err());
        assert!(web.asked().is_empty());
    }

    #[tokio::test]
    async fn over_the_cap_nothing_leaves() {
        let web = Fake::default().then(Fake::ok()).then(Fake::ok());
        let mut sender = Sender::new(
            client(&web),
            &Config {
                per_minute: 1,
                ..config()
            },
        );
        let t = Instant::now();
        assert_eq!(sender.push("un", t, 0).await.result, Ok(()));
        let refused = sender.push("deux", t, 0).await;
        assert!(refused.result.is_err());
        assert_eq!(refused.stopped, None);
        assert_eq!(web.asked().len(), 1);
        assert_eq!(sender.push("trois", t + MINUTE, 0).await.result, Ok(()));
    }

    #[tokio::test]
    async fn a_refusal_stops_the_sending_until_restart() {
        let web = Fake::default().then(Reply::Json(
            400,
            r#"{"status":0,"errors":["application token is invalid"]}"#,
            Vec::new(),
        ));
        let mut sender = Sender::new(client(&web), &config());
        let t = Instant::now();
        let first = sender.push("un", t, 0).await;
        assert!(first.result.is_err());
        assert!(
            first
                .stopped
                .unwrap()
                .contains("application token is invalid")
        );
        let second = sender.push("deux", t + HOUR, u64::MAX).await;
        assert!(second.result.is_err());
        assert_eq!(web.asked().len(), 1, "nothing more asked after a refusal");
    }

    #[tokio::test]
    async fn a_spent_quota_waits_for_the_new_month() {
        let web = Fake::default()
            .then(Reply::Json(
                429,
                r#"{"status":0}"#,
                vec![
                    ("X-Limit-App-Remaining", "0"),
                    ("X-Limit-App-Reset", "1000"),
                ],
            ))
            .then(Fake::ok());
        let mut sender = Sender::new(client(&web), &config());
        let t = Instant::now();
        let spent = sender.push("un", t, 900).await;
        assert!(spent.stopped.is_some());
        assert_eq!(spent.quota.map(|q| q.remaining), Some(0));
        assert!(sender.push("deux", t + MINUTE, 999).await.result.is_err());
        assert_eq!(web.asked().len(), 1);
        let back = sender.push("trois", t + 2 * MINUTE, 1000).await;
        assert!(back.resumed);
        assert_eq!(back.result, Ok(()));
    }

    #[tokio::test]
    async fn trouble_on_the_way_does_not_stop_the_sending() {
        let web = Fake::default()
            .then(Reply::Down)
            .then(Reply::Json(502, "", Vec::new()))
            .then(Fake::ok());
        let mut sender = Sender::new(client(&web), &config());
        let t = Instant::now();
        assert_eq!(sender.push("un", t, 0).await.stopped, None);
        assert_eq!(sender.push("deux", t, 0).await.stopped, None);
        assert_eq!(sender.push("trois", t, 0).await.result, Ok(()));
    }

    #[tokio::test]
    async fn start_checks_the_keys_and_the_devices() {
        let devices = r#"{"status":1,"devices":["iphone","desktop"]}"#;
        let limits = r#"{"status":1,"remaining":9000,"reset":5}"#;

        let web = Fake::default()
            .then(Reply::Json(200, devices, Vec::new()))
            .then(Reply::Json(200, limits, Vec::new()));
        let started = start(&client(&web), &["iphone".into()]).await;
        assert_eq!(
            started,
            Ok(Ok(Some(Quota {
                remaining: 9000,
                reset: Some(5)
            })))
        );

        let web = Fake::default().then(Reply::Json(200, devices, Vec::new()));
        let unknown = start(&client(&web), &["ipad".into()])
            .await
            .unwrap()
            .unwrap_err();
        assert!(unknown.contains("ipad") && unknown.contains("iphone"));

        let web = Fake::default().then(Reply::Json(
            400,
            r#"{"status":0,"errors":["user key is invalid"]}"#,
            Vec::new(),
        ));
        let refused = start(&client(&web), &[]).await.unwrap().unwrap_err();
        assert!(refused.contains("user key is invalid"));

        let web = Fake::default().then(Reply::Down);
        assert!(matches!(
            start(&client(&web), &[]).await,
            Err(Failure::Network(_))
        ));

        // The quota is a nicety: without it, the start goes on.
        let web = Fake::default()
            .then(Reply::Json(200, devices, Vec::new()))
            .then(Reply::Down);
        assert_eq!(start(&client(&web), &[]).await, Ok(Ok(None)));
    }
}

//! The household's phones, through the Moli app: each one a device of the
//! house (battery, charging, home or away, Wi-Fi, location), like Home
//! Assistant's mobile app integration.
//!
//! - **Pairing**: the app, inside the dashboard's own session (LAN or
//!   Cloudflare Access), asks `POST /api/mobile/register` and gets an id and
//!   a token, shown once and kept in the phone's keychain. Moli keeps only
//!   the token's SHA-256 (`data/phones.json`).
//! - **Reports**: `POST /api/phones/<id>/report` with the token, from the app
//!   in front or in the background (location changes, geofence). That path
//!   needs no Cloudflare login: the token is the proof.
//! - **Home**: learned, not guessed. The first precise location reported
//!   while on the home Wi-Fi (`home_wifi`) becomes the home; then « at home »
//!   = on that Wi-Fi, or within `radius` metres of it. The app gets it back to
//!   watch the home's boundary (geofence).

mod push;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context as _, bail, ensure};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

const MAX_PHONES: usize = 20;
const MAX_TEXT: usize = 60;
/// A location this precise (m) may become the home.
const HOME_ACCURACY: f64 = 60.0;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The home's Wi-Fi network names: on one of them, the phone is home.
    #[serde(default)]
    pub home_wifi: Vec<String>,
    /// Where the home is, if known; otherwise learned (see above).
    #[serde(default)]
    pub home: Option<Home>,
    /// How far from home still counts as home (m).
    #[serde(default = "default_radius")]
    pub radius: f64,
}

fn default_radius() -> f64 {
    150.0
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Home {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Phone {
    pub id: String,
    pub name: String,
    pub platform: String,
    #[serde(default)]
    pub model: String,
    /// SHA-256 of the token, hex: the token itself is never kept.
    pub token_sha256: String,
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_token: Option<String>,
    /// The person it belongs to (`personnes:<id>`): who paired it, signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
}

/// A phone as the household sees it (no token).
#[derive(Clone, Debug, Serialize)]
pub struct PhoneInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub model: String,
    pub person: Option<String>,
    pub created_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    phones: Vec<Phone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<Home>,
}

/// What the app says about the phone. Every field optional (a battery event
/// alone, a location alone…).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub battery: Option<f64>,
    pub charging: Option<bool>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub accuracy: Option<f64>,
    pub wifi: Option<String>,
    pub app: Option<String>,
    /// What woke the app: « open », « battery », « region », « location »…
    pub reason: Option<String>,
    /// The app's current push token (notifications allowed later, or a new
    /// token from the system).
    pub push_token: Option<String>,
}

impl Report {
    fn check(&self) -> anyhow::Result<()> {
        let within = |v: Option<f64>, lo: f64, hi: f64| {
            v.is_none_or(|v| v.is_finite() && (lo..=hi).contains(&v))
        };
        ensure!(within(self.battery, 0.0, 100.0), "battery: 0–100");
        ensure!(within(self.latitude, -90.0, 90.0), "latitude: −90–90");
        ensure!(within(self.longitude, -180.0, 180.0), "longitude: −180–180");
        ensure!(within(self.accuracy, 0.0, 100_000.0), "accuracy: 0–100 km");
        ensure!(
            self.latitude.is_some() == self.longitude.is_some(),
            "latitude and longitude go together"
        );
        for text in [&self.wifi, &self.app, &self.reason].into_iter().flatten() {
            ensure!(text_ok(text), "texts: {MAX_TEXT} characters at most");
        }
        if let Some(t) = &self.push_token {
            ensure!(
                t.len() <= 200 && !t.chars().any(char::is_control),
                "push token: 200 characters at most"
            );
        }
        Ok(())
    }
}

fn text_ok(s: &str) -> bool {
    s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn sha256(token: &str) -> String {
    hex(digest(&SHA256, token.as_bytes()).as_ref())
}

/// Metres between two points (haversine).
fn distance(a: Home, lat: f64, lon: f64) -> f64 {
    let r = 6_371_000.0_f64;
    let (p1, p2) = (a.latitude.to_radians(), lat.to_radians());
    let dp = (lat - a.latitude).to_radians();
    let dl = (lon - a.longitude).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

/// The phones on disk, shared by the API (pairing, reports) and the driver.
#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    file: Mutex<File>,
}

impl Registry {
    fn open(path: PathBuf) -> anyhow::Result<Self> {
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is unreadable", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, File> {
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, file: &File) -> anyhow::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(file)? + "\n")?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    #[must_use]
    pub fn phones(&self) -> Vec<Phone> {
        self.lock().phones.clone()
    }

    fn home(&self) -> Option<Home> {
        self.lock().home
    }
}

/// Events from the API to the driver.
#[derive(Debug)]
enum Event {
    Registered(Phone),
    Report(String, Report),
    Removed(String),
}

/// What the API holds: pairing and reports.
#[derive(Clone, Debug)]
pub struct Gateway {
    registry: Arc<Registry>,
    config: Arc<Config>,
    tx: mpsc::Sender<Event>,
}

/// Answer to a pairing: the token is shown this once.
#[derive(Debug, Serialize)]
pub struct Paired {
    pub id: String,
    pub token: String,
    pub home: Option<HomeView>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct HomeView {
    pub latitude: f64,
    pub longitude: f64,
    pub radius: f64,
}

impl Gateway {
    /// The home: configured, or learned from a phone on the home Wi-Fi.
    #[must_use]
    pub fn home(&self) -> Option<HomeView> {
        self.config
            .home
            .or_else(|| self.registry.home())
            .map(|h| HomeView {
                latitude: h.latitude,
                longitude: h.longitude,
                radius: self.config.radius,
            })
    }

    /// A new phone. `name`: the person's word for it (« iPhone de Sam »).
    pub fn register(
        &self,
        name: &str,
        platform: &str,
        model: &str,
        push_token: Option<&str>,
        person: Option<&str>,
    ) -> anyhow::Result<Paired> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        ensure!(
            !name.is_empty() && text_ok(&name),
            moli_i18n::tr!("pilotes.phones.nom", max = MAX_TEXT)
        );
        ensure!(
            matches!(platform, "ios" | "android"),
            "platform: ios or android"
        );
        ensure!(text_ok(model), "model: {MAX_TEXT} characters at most");
        if let Some(t) = push_token {
            ensure!(
                t.len() <= 200 && !t.chars().any(char::is_control),
                "push token: 200 characters at most"
            );
        }
        let rng = SystemRandom::new();
        let mut raw = [0u8; 32];
        rng.fill(&mut raw)
            .map_err(|_| anyhow::anyhow!("no randomness for the token"))?;
        let token = hex(&raw);
        let mut short = [0u8; 4];
        rng.fill(&mut short)
            .map_err(|_| anyhow::anyhow!("no randomness for the id"))?;
        let phone = Phone {
            id: format!("p{}", hex(&short)),
            name,
            platform: platform.to_owned(),
            model: model.to_owned(),
            token_sha256: sha256(&token),
            created_ms: moli_core::now_ms(),
            push_token: push_token.map(str::to_owned),
            person: person.map(str::to_owned),
        };
        {
            let mut file = self.registry.lock();
            if file.phones.len() >= MAX_PHONES {
                bail!(moli_i18n::tr!("pilotes.phones.trop", max = MAX_PHONES));
            }
            file.phones.push(phone.clone());
            self.registry.save(&file)?;
        }
        let _ = self.tx.try_send(Event::Registered(phone.clone()));
        tracing::info!(phone = phone.id, name = phone.name, "phone paired");
        Ok(Paired {
            id: phone.id,
            token,
            home: self.home(),
        })
    }

    /// The household's phones.
    #[must_use]
    pub fn list(&self) -> Vec<PhoneInfo> {
        self.registry
            .lock()
            .phones
            .iter()
            .map(|p| PhoneInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                platform: p.platform.clone(),
                model: p.model.clone(),
                person: p.person.clone(),
                created_ms: p.created_ms,
            })
            .collect()
    }

    /// Gives a phone to a person (or to nobody).
    pub fn assign(&self, id: &str, person: Option<&str>) -> anyhow::Result<bool> {
        let mut file = self.registry.lock();
        let Some(phone) = file.phones.iter_mut().find(|p| p.id == id) else {
            return Ok(false);
        };
        phone.person = person.map(str::to_owned);
        let phone = phone.clone();
        self.registry.save(&file)?;
        drop(file);
        let _ = self.tx.try_send(Event::Registered(phone));
        Ok(true)
    }

    /// A phone forgotten (lost, sold): its token stops working at once.
    pub fn remove(&self, id: &str) -> anyhow::Result<bool> {
        let mut file = self.registry.lock();
        let before = file.phones.len();
        file.phones.retain(|p| p.id != id);
        if file.phones.len() == before {
            return Ok(false);
        }
        self.registry.save(&file)?;
        drop(file);
        let _ = self.tx.try_send(Event::Removed(id.to_owned()));
        tracing::info!(phone = id, "phone removed");
        Ok(true)
    }

    /// Whether `token` is this phone's.
    #[must_use]
    pub fn verify(&self, id: &str, token: &str) -> bool {
        let hash = sha256(token);
        // Hashes compared, not tokens: timing could only leak a hash.
        self.registry
            .lock()
            .phones
            .iter()
            .any(|p| p.id == id && p.token_sha256 == hash)
    }

    /// A report from a verified phone; returns the home (to watch its edge).
    pub fn report(&self, id: &str, report: Report) -> anyhow::Result<Option<HomeView>> {
        report.check()?;
        // What each report carries (never its values): what the phone can say.
        tracing::info!(
            phone = id,
            reason = report.reason.as_deref().unwrap_or("?"),
            battery = report.battery.is_some(),
            location = report.latitude.is_some(),
            wifi = report.wifi.is_some(),
            "phone report"
        );
        if let Some(token) = &report.push_token {
            let mut file = self.registry.lock();
            if let Some(phone) = file.phones.iter_mut().find(|p| p.id == id)
                && phone.push_token.as_deref() != Some(token.as_str())
            {
                phone.push_token = Some(token.clone());
                self.registry.save(&file)?;
                tracing::info!(phone = id, "phone push token updated");
            }
        }
        // Learn the home: precise, and on the home's Wi-Fi.
        if self.config.home.is_none()
            && let (Some(lat), Some(lon), Some(acc), Some(wifi)) = (
                report.latitude,
                report.longitude,
                report.accuracy,
                &report.wifi,
            )
            && acc <= HOME_ACCURACY
            && self.config.home_wifi.iter().any(|w| w == wifi)
        {
            let mut file = self.registry.lock();
            if file.home.is_none() {
                file.home = Some(Home {
                    latitude: lat,
                    longitude: lon,
                });
                self.registry.save(&file)?;
                tracing::info!(phone = id, "home located from a phone on the home Wi-Fi");
            }
        }
        self.tx
            .try_send(Event::Report(id.to_owned(), report))
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.phones.occupe")))?;
        Ok(self.home())
    }
}

/// The driver: one device per phone.
#[derive(Debug)]
pub struct Phones {
    gateway: Gateway,
    events: tokio::sync::Mutex<mpsc::Receiver<Event>>,
}

/// The pair the binary wires: the gateway for the API, the driver for the hub.
pub fn open(config: Config, data_dir: &Path) -> anyhow::Result<(Gateway, Phones)> {
    ensure!(
        config.radius.is_finite() && (20.0..=5_000.0).contains(&config.radius),
        "radius: 20–5000 m"
    );
    for w in &config.home_wifi {
        ensure!(!w.is_empty() && text_ok(w), "home_wifi: network names");
    }
    let registry = Arc::new(Registry::open(data_dir.join("phones.json"))?);
    let (tx, rx) = mpsc::channel(64);
    let gateway = Gateway {
        registry,
        config: Arc::new(config),
        tx,
    };
    Ok((
        gateway.clone(),
        Phones {
            gateway,
            events: tokio::sync::Mutex::new(rx),
        },
    ))
}

impl Driver for Phones {
    fn kind(&self) -> &'static str {
        "phones"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

fn numeric() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

#[allow(clippy::too_many_lines)]
fn device(ctx: &DriverCtx, phone: &Phone) -> Device {
    Device {
        id: ctx.device_id(&phone.id),
        instance: ctx.instance().clone(),
        native_name: phone.name.as_str().into(),
        manufacturer: Some(
            if phone.platform == "ios" {
                "Apple"
            } else {
                "Android"
            }
            .into(),
        ),
        model: Some(
            if phone.model.is_empty() {
                moli_i18n::tr!("pilotes.phones.telephone")
            } else {
                phone.model.clone()
            }
            .into(),
        ),
        description: Some(moli_i18n::tr!("pilotes.phones.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "person",
                &moli_i18n::tr!("pilotes.phones.personne"),
                Kind::Text,
                None,
                Semantic::Other,
            ),
            spec(
                "home",
                &moli_i18n::tr!("pilotes.phones.a_la_maison"),
                Kind::Binary,
                None,
                Semantic::Occupancy,
            ),
            spec(
                "battery",
                &moli_i18n::tr!("pilotes.phones.batterie"),
                numeric(),
                Some(Unit::Percent),
                Semantic::infer("battery", Some(&Unit::Percent)),
            ),
            spec(
                "charging",
                &moli_i18n::tr!("pilotes.phones.en_charge"),
                Kind::Binary,
                None,
                Semantic::Other,
            ),
            spec(
                "wifi",
                &moli_i18n::tr!("pilotes.phones.wifi"),
                Kind::Text,
                None,
                Semantic::Other,
            ),
            spec(
                "latitude",
                &moli_i18n::tr!("pilotes.phones.latitude"),
                numeric(),
                None,
                Semantic::Other,
            ),
            spec(
                "longitude",
                &moli_i18n::tr!("pilotes.phones.longitude"),
                numeric(),
                None,
                Semantic::Other,
            ),
            spec(
                "accuracy",
                &moli_i18n::tr!("pilotes.phones.precision"),
                numeric(),
                Unit::parse("m"),
                Semantic::Other,
            ),
            spec(
                "distance",
                &moli_i18n::tr!("pilotes.phones.distance"),
                numeric(),
                Unit::parse("m"),
                Semantic::Other,
            ),
            spec(
                "app",
                &moli_i18n::tr!("pilotes.phones.version_appli"),
                Kind::Text,
                None,
                Semantic::Other,
            ),
            PointSpec {
                key: "push".into(),
                label: moli_i18n::tr!("pilotes.phones.notifier").into(),
                kind: Kind::Text,
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
        ],
    }
}

/// A phone's device, and whose it is.
fn publish(ctx: &DriverCtx, phone: &Phone) {
    let id = ctx.device_id(&phone.id);
    ctx.upsert_device(device(ctx, phone));
    let person = phone
        .person
        .as_deref()
        .map_or(Value::Null, |p| Value::Text(p.into()));
    ctx.set_state(&id, "person", person);
}

async fn run(this: &Phones, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    for phone in this.gateway.registry.phones() {
        publish(ctx, &phone);
    }
    ctx.ready();
    let mut events = this.events.lock().await;
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    let result = notify(this, &command).await;
                    command.reply(result);
                }
                None => return Ok(()),
            },
            event = events.recv() => match event {
                Some(Event::Registered(phone)) => publish(ctx, &phone),
                Some(Event::Report(id, report)) => apply(this, ctx, &id, &report),
                Some(Event::Removed(id)) => ctx.set_availability(&ctx.device_id(&id), false),
                // The API is gone first when Moli stops: wait for the end.
                None => {
                    ctx.cancelled().await;
                    return Ok(());
                }
            },
        }
    }
}

/// `push`: a notification to this phone (a title line, then the message;
/// or a single line).
async fn notify(this: &Phones, command: &moli_runtime::CommandRequest) -> Result<(), String> {
    let (key, value) = (&*command.key, &command.value);
    let Value::Text(text) = value else {
        return Err(moli_i18n::tr!("pilotes.phones.du_texte", point = key));
    };
    if key != "push" {
        return Err(moli_i18n::tr!("pilotes.phones.se_notifie"));
    }
    let id = command
        .device
        .id
        .as_str()
        .split_once(':')
        .map_or("", |(_, id)| id);
    let token = this
        .gateway
        .registry
        .phones()
        .into_iter()
        .find(|p| p.id == id)
        .and_then(|p| p.push_token)
        .ok_or_else(|| moli_i18n::tr!("pilotes.phones.sans_jeton"))?;
    let (title, body) =
        push::message(text).ok_or_else(|| moli_i18n::tr!("pilotes.phones.message_vide"))?;
    push::send(&token, &title, &body).await
}

fn apply(this: &Phones, ctx: &DriverCtx, id: &str, r: &Report) {
    let device: DeviceId = ctx.device_id(id);
    ctx.set_availability(&device, true);
    let set = |key: &str, value: Value| ctx.set_state(&device, key, value);
    if let Some(b) = r.battery {
        set("battery", Value::Float(b.round()));
    }
    if let Some(c) = r.charging {
        set("charging", Value::Bool(c));
    }
    if let Some(w) = &r.wifi {
        set("wifi", Value::Text(w.as_str().into()));
    }
    if let Some(a) = &r.app {
        set("app", Value::Text(a.as_str().into()));
    }
    let home = this.gateway.home();
    let on_home_wifi = r
        .wifi
        .as_ref()
        .map(|w| this.gateway.config.home_wifi.iter().any(|h| h == w));
    let mut away_from = None;
    if let (Some(lat), Some(lon)) = (r.latitude, r.longitude) {
        set("latitude", Value::Float((lat * 1e5).round() / 1e5));
        set("longitude", Value::Float((lon * 1e5).round() / 1e5));
        if let Some(acc) = r.accuracy {
            set("accuracy", Value::Float(acc.round()));
        }
        if let Some(h) = home {
            let d = distance(
                Home {
                    latitude: h.latitude,
                    longitude: h.longitude,
                },
                lat,
                lon,
            );
            set("distance", Value::Float(d.round()));
            away_from = Some(d > h.radius + r.accuracy.unwrap_or(0.0).min(200.0));
        }
    }
    // Home: the home Wi-Fi says yes; a location far enough says no.
    let at_home = match (on_home_wifi, away_from) {
        (Some(true), _) => Some(true),
        (_, Some(away)) => Some(!away),
        (Some(false) | None, None) => None,
    };
    if let Some(h) = at_home {
        set("home", Value::Bool(h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway(dir: &Path, config: Config) -> (Gateway, Phones) {
        open(config, dir).unwrap()
    }

    fn config() -> Config {
        Config {
            home_wifi: vec!["Maison-Wifi".into()],
            home: None,
            radius: 150.0,
        }
    }

    #[test]
    fn a_phone_pairs_and_only_its_token_opens() {
        let dir = std::env::temp_dir().join(format!("moli-phones-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones) = gateway(&dir, config());
        let paired = g
            .register("iPhone  de Sam", "ios", "iPhone 15", None, None)
            .unwrap();
        assert_eq!(paired.token.len(), 64);
        assert!(g.verify(&paired.id, &paired.token));
        assert!(!g.verify(&paired.id, &"0".repeat(64)));
        assert!(!g.verify("p00000000", &paired.token));
        assert!(g.remove(&paired.id).unwrap());
        assert!(
            !g.verify(&paired.id, &paired.token),
            "a removed phone's token is dead"
        );
        let paired = g
            .register("iPhone  de Sam", "ios", "iPhone 15", None, None)
            .unwrap();
        let kept = std::fs::read_to_string(dir.join("phones.json")).unwrap();
        assert!(
            !kept.contains(&paired.token),
            "only the token's hash is kept"
        );
        assert!(kept.contains("iPhone de Sam"));
        assert!(g.register("", "ios", "", None, None).is_err());
        assert!(g.register("x", "symbian", "", None, None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_home_is_learned_on_its_wifi_only() {
        let dir = std::env::temp_dir().join(format!("moli-phones-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones) = gateway(&dir, config());
        let at = |lat, wifi: &str, acc| Report {
            latitude: Some(lat),
            longitude: Some(2.35),
            accuracy: Some(acc),
            wifi: Some(wifi.into()),
            ..Report::default()
        };
        assert!(
            g.report("p1", at(48.85, "Café", 10.0)).unwrap().is_none(),
            "another Wi-Fi: not home"
        );
        assert!(
            g.report("p1", at(48.85, "Maison-Wifi", 500.0))
                .unwrap()
                .is_none(),
            "too vague"
        );
        let home = g
            .report("p1", at(48.85, "Maison-Wifi", 15.0))
            .unwrap()
            .unwrap();
        assert!((home.latitude - 48.85).abs() < 1e-9 && (home.radius - 150.0).abs() < 1e-9);
        assert!(
            g.report("p1", at(48.95, "Maison-Wifi", 15.0))
                .unwrap()
                .unwrap()
                .latitude
                < 48.9,
            "learned once"
        );
        assert!(
            g.report(
                "p1",
                Report {
                    battery: Some(120.0),
                    ..Report::default()
                }
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn distances_are_in_metres() {
        let home = Home {
            latitude: 48.8584,
            longitude: 2.2945,
        };
        assert!(distance(home, 48.8584, 2.2945) < 0.01);
        let d = distance(home, 48.8684, 2.2945);
        assert!((d - 1112.0).abs() < 5.0, "{d}");
    }
}

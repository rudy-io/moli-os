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
//! - **Whose**: a phone of someone no longer of the house (removed, a guest
//!   whose stay ended: [`Gateway::members`]) is notified of nothing more.

mod push;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use anyhow::{Context as _, bail, ensure};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub};
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
    /// Legacy only: the push token used to live here, in clear. It now lives
    /// in the encrypted store (`<instance>/<id>/push_token`); the field stays
    /// readable so old files load, but is never written back.
    #[serde(default, skip_serializing)]
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

/// `phones.json` says who owns which phone and where the home is: for Moli's
/// user alone (0600 on unix, like `people.json` and the secret store).
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    // A file left there (a crash) kept its own mode.
    private(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(unix)]
fn private(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn private(_: &Path) -> anyhow::Result<()> {
    Ok(())
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
            Ok(bytes) => {
                // Names of the household's people and the home's position:
                // the file stays owned by its user (0600), old files included.
                private(&path).with_context(|| format!("cannot chmod {}", path.display()))?;
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is unreadable", path.display()))?
            }
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
        write_private(
            &tmp,
            (serde_json::to_string_pretty(file)? + "\n").as_bytes(),
        )?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Removes a legacy push token from the in-memory file and saves it.
    fn clear_push_token(&self, id: &str) {
        let mut file = self.lock();
        if let Some(phone) = file.phones.iter_mut().find(|p| p.id == id) {
            phone.push_token = None;
            if let Err(e) = self.save(&file) {
                tracing::warn!(phone = id, error = %e, "phones.json not rewritten after the token moved to the vault");
            }
        }
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
    members: Members,
    /// The push tokens live in the hub's encrypted store, never in phones.json.
    vault: Option<Vault>,
}

/// One name in the encrypted store: the push token of phone `<id>` is
/// `<instance>/<id>/push_token`, read back by the driver through its context.
#[derive(Clone)]
struct Vault {
    hub: Hub,
    instance: InstanceId,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

impl Vault {
    fn name(phone: &str) -> String {
        format!("{phone}/push_token")
    }
}

/// Whether a person still belongs to the house.
type Belongs = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Who still belongs to the house: the API knows its people. Until it says,
/// every phone's person does.
#[derive(Clone, Default)]
struct Members(Arc<OnceLock<Belongs>>);

impl std::fmt::Debug for Members {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Members")
    }
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

    /// Files a phone's push token in the encrypted store
    /// (`<instance>/<phone id>/push_token`), where the driver reads it.
    async fn store_push_token(&self, phone: &str, token: &str) -> anyhow::Result<()> {
        let vault = self
            .vault
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no encrypted store (MOLI_MASTER_KEY)"))?;
        vault
            .hub
            .store_secret(&vault.instance, &Vault::name(phone), token)
            .await
            .map_err(anyhow::Error::from)
    }

    /// Removes a phone's push token from the vault (a forgotten phone).
    async fn forget_push_token(&self, phone: &str) -> anyhow::Result<()> {
        if let Some(vault) = self.vault.as_ref() {
            vault
                .hub
                .forget_secret(&vault.instance, &Vault::name(phone))
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from)?;
        }
        Ok(())
    }

    /// Legacy phones.json kept push tokens in clear (before the encrypted
    /// store): move each into the vault, then out of the file. Returns how
    /// many were moved.
    pub async fn migrate_legacy_tokens(&self) -> usize {
        let legacy: Vec<(String, String)> = self
            .registry
            .lock()
            .phones
            .iter()
            .filter_map(|p| p.push_token.as_ref().map(|t| (p.id.clone(), t.clone())))
            .collect();
        let mut moved = 0usize;
        for (id, token) in &legacy {
            if let Err(e) = self.store_push_token(id, token).await {
                tracing::warn!(
                    phone = %id,
                    error = %e,
                    "legacy push token not migrated to the encrypted store"
                );
                continue;
            }
            self.registry.clear_push_token(id);
            moved += 1;
        }
        moved
    }

    /// A new phone. `name`: the person's word for it (« iPhone de Sam »).
    /// The push token goes to the encrypted store, never to phones.json.
    pub async fn register(
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
            push_token: None,
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
        // The push token is an app credential: encrypted store only.
        if let Some(pt) = push_token
            && let Err(e) = self.store_push_token(&phone.id, pt).await
        {
            tracing::warn!(
                phone = %phone.id,
                error = %e,
                "push token not stored: notifications will not work for this phone"
            );
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
    pub async fn remove(&self, id: &str) -> anyhow::Result<bool> {
        // The file change stays inside the block: a std lock guard must
        // never live across an await (the future has to stay Send).
        let removed = {
            let mut file = self.registry.lock();
            let before = file.phones.len();
            file.phones.retain(|p| p.id != id);
            if file.phones.len() == before {
                false
            } else {
                self.registry.save(&file)?;
                true
            }
        };
        if !removed {
            return Ok(false);
        }
        // Its push token dies with it.
        if let Err(e) = self.forget_push_token(id).await {
            tracing::warn!(phone = id, error = %e, "push token not removed from the vault");
        }
        let _ = self.tx.try_send(Event::Removed(id.to_owned()));
        tracing::info!(phone = id, "phone removed");
        Ok(true)
    }

    /// Every phone of a person forgotten (removed from the house): their
    /// tokens stop working at once. Returns the phones forgotten.
    pub async fn forget(&self, person: &str) -> anyhow::Result<Vec<String>> {
        // The file change stays inside the block: a std lock guard must
        // never live across an await (the future has to stay Send).
        let gone = {
            let mut file = self.registry.lock();
            let theirs = |p: &Phone| p.person.as_deref() == Some(person);
            let gone: Vec<String> = file
                .phones
                .iter()
                .filter(|p| theirs(p))
                .map(|p| p.id.clone())
                .collect();
            if gone.is_empty() {
                return Ok(gone);
            }
            file.phones.retain(|p| !theirs(p));
            self.registry.save(&file)?;
            gone
        };
        for id in &gone {
            if let Err(e) = self.forget_push_token(id).await {
                tracing::warn!(phone = %id, error = %e, "push token not removed from the vault");
            }
            let _ = self.tx.try_send(Event::Removed(id.clone()));
            tracing::info!(phone = %id, "phone removed with its person");
        }
        Ok(gone)
    }

    /// Says who still belongs to the house (the API, which knows the
    /// people), once: a phone of anyone else is notified of nothing.
    pub fn members(&self, belongs: impl Fn(&str) -> bool + Send + Sync + 'static) {
        if self.members.0.set(Box::new(belongs)).is_err() {
            tracing::warn!("the phones were already told who belongs to the house");
        }
    }

    /// Whether phone `id` is the household's: nobody's in particular, or
    /// the phone of someone who still belongs to the house.
    #[must_use]
    pub fn belongs(&self, id: &str) -> bool {
        let person = self
            .registry
            .lock()
            .phones
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.person.clone());
        person.is_none_or(|p| self.members.0.get().is_none_or(|belongs| belongs(&p)))
    }

    /// Whether `token` is this phone's. Constant-time: the comparison cannot
    /// leak the stored hash one byte at a time.
    #[must_use]
    pub fn verify(&self, id: &str, token: &str) -> bool {
        let hash = sha256(token);
        self.registry
            .lock()
            .phones
            .iter()
            .find(|p| p.id == id)
            .is_some_and(|p| {
                moli_core::constant_time_eq(p.token_sha256.as_bytes(), hash.as_bytes())
            })
    }

    /// A report from a verified phone; returns the home (to watch its edge).
    pub async fn report(&self, id: &str, report: Report) -> anyhow::Result<Option<HomeView>> {
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
        // The push token is an app credential: encrypted store only, and
        // only written when it actually changed.
        if let Some(token) = &report.push_token
            && let Some(vault) = self.vault.as_ref()
            && vault
                .hub
                .secret(&vault.instance, &Vault::name(id))
                .as_deref()
                != Some(token.as_str())
        {
            if let Err(e) = self.store_push_token(id, token).await {
                tracing::warn!(phone = id, error = %e, "phone push token not updated");
            } else {
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
/// `hub` carries the encrypted store: the push tokens live there (the driver
/// reads them back through its context), never in phones.json.
pub fn open(
    config: Config,
    data_dir: &Path,
    hub: Option<&Hub>,
    instance: &InstanceId,
) -> anyhow::Result<(Gateway, Phones)> {
    ensure!(
        config.radius.is_finite() && (20.0..=5_000.0).contains(&config.radius),
        "radius: 20–5000 m"
    );
    for w in &config.home_wifi {
        ensure!(!w.is_empty() && text_ok(w), "home_wifi: network names");
    }
    let registry = Arc::new(Registry::open(data_dir.join("phones.json"))?);
    let (tx, rx) = mpsc::channel(64);
    let vault = hub.and_then(|hub| {
        if hub.has_secret_store() {
            Some(Vault {
                hub: hub.clone(),
                instance: instance.clone(),
            })
        } else {
            tracing::warn!(
                instance = %instance,
                "no encrypted store: phone push tokens cannot be kept (MOLI_MASTER_KEY)"
            );
            None
        }
    });
    let gateway = Gateway {
        registry,
        config: Arc::new(config),
        tx,
        members: Members::default(),
        vault,
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
    // Old phones.json kept push tokens in clear: move them to the encrypted
    // store before the first push.
    this.gateway.migrate_legacy_tokens().await;
    for phone in this.gateway.registry.phones() {
        publish(ctx, &phone);
    }
    ctx.ready();
    let mut events = this.events.lock().await;
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    let result = notify(this, ctx, &command).await;
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
async fn notify(
    this: &Phones,
    ctx: &DriverCtx,
    command: &moli_runtime::CommandRequest,
) -> Result<(), String> {
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
    // Someone removed, a guest whose stay ended: nothing more of the house.
    if !this.gateway.belongs(id) {
        return Err(moli_i18n::tr!("pilotes.phones.hors_maison"));
    }
    // The push token lives in the encrypted store, never in phones.json.
    let token = ctx
        .secret(&format!("{id}/push_token"))
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
    use moli_runtime::{HubOptions, MasterKey};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn gateway(dir: &Path, config: Config) -> (Gateway, Phones, Hub) {
        let hub = Hub::new(HubOptions {
            secrets: Some((dir.join("secrets.enc"), MasterKey::generate().unwrap())),
            ..HubOptions::default()
        })
        .unwrap();
        let (g, phones) = open(config, dir, Some(&hub), &InstanceId::from("phones")).unwrap();
        (g, phones, hub)
    }

    fn config() -> Config {
        Config {
            home_wifi: vec!["Maison-Wifi".into()],
            home: None,
            radius: 150.0,
        }
    }

    #[test]
    fn gateway_stays_send_for_the_api_handlers() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Gateway>();
        assert_sync::<Gateway>();
        assert_send::<Hub>();
        assert_sync::<Hub>();
    }

    #[tokio::test]
    async fn gateway_method_futures_are_send() {
        let dir = std::env::temp_dir().join(format!("moli-phones-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones, _hub) = gateway(&dir, config());
        // tokio::spawn requires the futures to be Send: the API handlers
        // capture the gateway across awaits.
        tokio::spawn(async move {
            let _ = g.remove("pX").await;
            let _ = g.forget("x").await;
            let _ = g.register("n", "ios", "", None, None).await;
            let _ = g.report("pX", Report::default()).await;
        })
        .await
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_phone_pairs_and_only_its_token_opens() {
        let dir = std::env::temp_dir().join(format!("moli-phones-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones, _hub) = gateway(&dir, config());
        let paired = g
            .register("iPhone  de Sam", "ios", "iPhone 15", None, None)
            .await
            .unwrap();
        assert_eq!(paired.token.len(), 64);
        assert!(g.verify(&paired.id, &paired.token));
        assert!(!g.verify(&paired.id, &"0".repeat(64)));
        assert!(!g.verify("p00000000", &paired.token));
        assert!(g.remove(&paired.id).await.unwrap());
        assert!(
            !g.verify(&paired.id, &paired.token),
            "a removed phone's token is dead"
        );
        let paired = g
            .register("iPhone  de Sam", "ios", "iPhone 15", None, None)
            .await
            .unwrap();
        let kept = std::fs::read_to_string(dir.join("phones.json")).unwrap();
        assert!(
            !kept.contains(&paired.token),
            "only the token's hash is kept"
        );
        assert!(kept.contains("iPhone de Sam"));
        assert!(g.register("", "ios", "", None, None).await.is_err());
        assert!(g.register("x", "symbian", "", None, None).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_phone_follows_its_person_out_of_the_house() {
        let dir = std::env::temp_dir().join(format!("moli-phones-whose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones, _hub) = gateway(&dir, config());
        let sam = g
            .register("Téléphone", "ios", "", None, Some("sam"))
            .await
            .unwrap();
        let guest = g
            .register("Téléphone", "ios", "", None, Some("passage"))
            .await
            .unwrap();
        let house = g
            .register("Téléphone", "ios", "", None, None)
            .await
            .unwrap();
        assert!(g.belongs(&guest.id), "until the API says who belongs");
        // A guest whose stay ended: its phone no longer belongs.
        g.members(|person| person == "sam");
        assert!(g.belongs(&sam.id) && g.belongs(&house.id));
        assert!(!g.belongs(&guest.id));
        // Sam removed: Sam's phones are forgotten, their tokens dead.
        assert_eq!(
            g.forget("sam").await.unwrap(),
            std::slice::from_ref(&sam.id)
        );
        assert!(!g.verify(&sam.id, &sam.token));
        assert!(g.verify(&house.id, &house.token));
        assert!(g.forget("sam").await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn push_tokens_live_only_in_the_vault() {
        let dir = std::env::temp_dir().join(format!("moli-phones-vault-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones, hub) = gateway(&dir, config());
        let vault_name = |id: &str| format!("{id}/push_token");
        let paired = g
            .register(
                "iPhone  de Sam",
                "ios",
                "iPhone 15",
                Some("expo-token-123"),
                None,
            )
            .await
            .unwrap();
        // Never in the registry file (push tokens are app credentials).
        let kept = std::fs::read_to_string(dir.join("phones.json")).unwrap();
        assert!(
            !kept.contains("expo-token-123"),
            "the push token must not sit in phones.json"
        );
        // …and in the encrypted store, namespaced by the driver instance.
        assert_eq!(
            hub.secret(&InstanceId::from("phones"), &vault_name(&paired.id)),
            Some("expo-token-123".into())
        );
        // A report with the same token changes nothing; a new one is filed.
        g.report(
            &paired.id,
            Report {
                push_token: Some("expo-token-123".into()),
                ..Report::default()
            },
        )
        .await
        .unwrap();
        g.report(
            &paired.id,
            Report {
                push_token: Some("expo-token-456".into()),
                ..Report::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            hub.secret(&InstanceId::from("phones"), &vault_name(&paired.id)),
            Some("expo-token-456".into())
        );
        // Removal and forgetting purge the vault too.
        assert!(g.remove(&paired.id).await.unwrap());
        assert_eq!(
            hub.secret(&InstanceId::from("phones"), &vault_name(&paired.id)),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn legacy_push_tokens_are_migrated_to_the_vault() {
        let dir = std::env::temp_dir().join(format!("moli-phones-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A pre-vault phones.json, with the token in clear and loose mode.
        std::fs::write(
            dir.join("phones.json"),
            r#"{ "phones": [ { "id": "p00000001", "name": "L'iPhone", "platform": "ios", "model": "", "token_sha256": "ab", "created_ms": 1, "push_token": "legacy-token" } ] }"#,
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            dir.join("phones.json"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let (g, _phones, hub) = gateway(&dir, config());
        // The file is tightened on open (0600), whatever its old mode was.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("phones.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "existing phones.json must become 0600");
        }
        assert_eq!(g.migrate_legacy_tokens().await, 1);
        let kept = std::fs::read_to_string(dir.join("phones.json")).unwrap();
        assert!(!kept.contains("legacy-token"), "the clear token is gone");
        assert_eq!(
            hub.secret(&InstanceId::from("phones"), "p00000001/push_token"),
            Some("legacy-token".into())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_home_is_learned_on_its_wifi_only() {
        let dir = std::env::temp_dir().join(format!("moli-phones-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (g, _phones, _hub) = gateway(&dir, config());
        let at = |lat, wifi: &str, acc| Report {
            latitude: Some(lat),
            longitude: Some(2.35),
            accuracy: Some(acc),
            wifi: Some(wifi.into()),
            ..Report::default()
        };
        assert!(
            g.report("p1", at(48.85, "Café", 10.0))
                .await
                .unwrap()
                .is_none(),
            "another Wi-Fi: not home"
        );
        assert!(
            g.report("p1", at(48.85, "Maison-Wifi", 500.0))
                .await
                .unwrap()
                .is_none(),
            "too vague"
        );
        let home = g
            .report("p1", at(48.85, "Maison-Wifi", 15.0))
            .await
            .unwrap()
            .unwrap();
        assert!((home.latitude - 48.85).abs() < 1e-9 && (home.radius - 150.0).abs() < 1e-9);
        assert!(
            g.report("p1", at(48.95, "Maison-Wifi", 15.0))
                .await
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
            .await
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

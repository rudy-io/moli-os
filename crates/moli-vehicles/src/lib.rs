//! The household's vehicles, through an OBD-II dongle and the Torque Pro app
//! (an Android phone or head unit): where the car is, its mileage, its fault
//! light and codes, its engine's main readings, and a garage book (what was
//! done, what is due, by distance or by time).
//!
//! - **Reports**: Torque's « Upload to web server » points at
//!   `<moli>/api/torque/<token>` (GET or POST, answered `OK!`). The token is
//!   the vehicle's: Moli keeps only its SHA-256, in the configuration. That
//!   path needs no Cloudflare login: the token is the proof.
//! - **Mileage**: an OBD-II car rarely gives its odometer. Moli counts the
//!   distance driven (the ECU's « distance since codes cleared » when Torque
//!   logs it, else speed × time) from a reading a person gives
//!   (`odometer_set`, or the km of a maintenance record).
//! - **Faults**: the light (`mil`) from the ECU's distance driven with it
//!   on, or from PID 01 01 logged as a custom PID (`mil_key`); the codes from
//!   a companion app that adds `dtc=P0401,…` to the same upload.
//! - **Garage**: `[[…maintenance]]` items with their intervals; a record
//!   (`maintenance_done`) restarts the count. `alert` + `detail` put a
//!   vehicle that needs care on the dashboard's fleet page.
//! - **Privacy**: positions are points like any other: they stay in this
//!   house's history, for its retention, and go nowhere else.

pub mod garage;
pub mod torque;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use garage::{Done, Item, Record};

const MAX_VEHICLES: usize = 10;
const MAX_ITEMS: usize = 30;
const MAX_RECORDS: usize = 500;
/// Keys beyond the known ones that become points (what else Torque logs).
const MAX_EXTRAS: usize = 60;
/// Without an upload for this long, the vehicle is parked.
const DRIVING_FOR_MS: u64 = 120_000;
/// Two uploads further apart than this: no distance counted from speed.
const MAX_GAP_MS: u64 = 15_000;
/// More than this between two readings of the ECU's counter: not believed
/// (a cleared then refilled counter, another car's dongle).
const MAX_UNHEARD_KM: f64 = 3_000.0;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub vehicle: Vec<VehicleConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleConfig {
    /// Its native id (`<instance>:<id>`): never renamed.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// SHA-256 (hex) of the token in Torque's upload address.
    pub token_sha256: String,
    /// The Torque key of a custom PID `0101` (equation `A`): the fault light
    /// (bit 7) and how many codes are stored (bits 0–6).
    #[serde(default)]
    pub mil_key: Option<String>,
    #[serde(default)]
    pub maintenance: Vec<Item>,
    /// What the car's own screen may read of the house (presence, a
    /// temperature, the doors): nothing else is ever shown to it.
    #[serde(default)]
    pub share: Vec<Shared>,
    /// What the car's screen may order, each a fixed point and value.
    #[serde(default)]
    pub action: Vec<CarAction>,
}

/// A point of the house shown on the car's screen, under its own id.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Shared {
    pub id: String,
    pub point: String,
}

/// An order the car's screen may give: one point, one value.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CarAction {
    pub id: String,
    pub name: String,
    pub point: String,
    pub value: serde_json::Value,
}

/// Torque keys Moli knows, the point each becomes, its unit.
const KNOWN: &[(&str, &str, Option<&str>)] = &[
    ("kff1006", "latitude", None),
    ("kff1005", "longitude", None),
    ("kff1239", "gps_accuracy", Some("m")),
    ("kff1010", "altitude", Some("m")),
    ("kff1001", "gps_speed", Some("km/h")),
    ("kd", "speed", Some("km/h")),
    ("kc", "rpm", Some("rpm")),
    ("k5", "coolant", Some("°C")),
    ("k5c", "oil_temp", Some("°C")),
    ("kf", "intake_temp", Some("°C")),
    ("k46", "ambient_temp", Some("°C")),
    ("k4", "engine_load", Some("%")),
    ("k11", "throttle", Some("%")),
    ("k2f", "fuel_level", Some("%")),
    ("kb", "intake_pressure", Some("kPa")),
    ("k33", "barometer", Some("kPa")),
    ("kff1238", "adapter_voltage", Some("V")),
    ("k42", "module_voltage", Some("V")),
    ("k21", "km_with_mil", Some("km")),
    ("k31", "km_since_clear", Some("km")),
    ("ka6", "ecu_odometer", Some("km")),
];

fn known(key: &str) -> Option<&'static (&'static str, &'static str, Option<&'static str>)> {
    KNOWN.iter().find(|(k, _, _)| *k == key)
}

fn sha256_hex(token: &str) -> String {
    use std::fmt::Write as _;
    digest(&SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn point_ok(p: &str) -> bool {
    p.len() <= 160
        && p.split_once('/')
            .is_some_and(|(d, k)| d.contains(':') && !d.is_empty() && !k.is_empty())
        && !p.chars().any(char::is_control)
}

impl Config {
    fn check(&self) -> anyhow::Result<()> {
        ensure!(
            self.vehicle.len() <= MAX_VEHICLES,
            "{MAX_VEHICLES} vehicles at most"
        );
        let mut ids = std::collections::HashSet::new();
        for v in &self.vehicle {
            ensure!(id_ok(&v.id), "vehicle id {:?}: a-z, 0-9, _ or -", v.id);
            ensure!(ids.insert(&v.id), "vehicle {:?} twice", v.id);
            ensure!(
                !v.name.trim().is_empty() && v.name.chars().count() <= 60,
                "vehicle {:?}: a name of 1–60 characters",
                v.id
            );
            ensure!(
                v.token_sha256.len() == 64 && v.token_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "vehicle {:?}: token_sha256 is the token's SHA-256 in hex",
                v.id
            );
            if let Some(k) = &v.mil_key {
                ensure!(
                    k.starts_with('k')
                        && k.len() > 1
                        && k[1..].bytes().all(|b| b.is_ascii_hexdigit()),
                    "vehicle {:?}: mil_key is a Torque key like k101",
                    v.id
                );
            }
            ensure!(
                v.maintenance.len() <= MAX_ITEMS,
                "vehicle {:?}: {MAX_ITEMS} maintenance items at most",
                v.id
            );
            ensure!(
                v.share.len() <= 40 && v.action.len() <= 20,
                "vehicle {:?}: 40 shared points and 20 actions at most",
                v.id
            );
            let mut shown = std::collections::HashSet::new();
            for s in &v.share {
                ensure!(
                    id_ok(&s.id) && shown.insert(&s.id),
                    "share id {:?}: a-z, 0-9, _ or -, once",
                    s.id
                );
                ensure!(
                    point_ok(&s.point),
                    "share {:?}: a point <device>/<key>",
                    s.id
                );
            }
            let mut orders = std::collections::HashSet::new();
            for a in &v.action {
                ensure!(
                    id_ok(&a.id) && orders.insert(&a.id),
                    "action id {:?}: a-z, 0-9, _ or -, once",
                    a.id
                );
                ensure!(
                    point_ok(&a.point),
                    "action {:?}: a point <device>/<key>",
                    a.id
                );
                ensure!(
                    !a.name.trim().is_empty() && a.name.chars().count() <= 40,
                    "action {:?}: a name of 1–40 characters",
                    a.id
                );
                ensure!(
                    !a.value.is_object() && !a.value.is_array() && !a.value.is_null(),
                    "action {:?}: value is a number, a boolean or a text",
                    a.id
                );
            }
            let mut items = std::collections::HashSet::new();
            for item in &v.maintenance {
                ensure!(
                    id_ok(&item.id),
                    "maintenance id {:?}: a-z, 0-9, _ or -",
                    item.id
                );
                ensure!(items.insert(&item.id), "maintenance {:?} twice", item.id);
                ensure!(
                    item.every_km.is_some() || item.every_months.is_some(),
                    "maintenance {:?}: every_km, every_months or both",
                    item.id
                );
                ensure!(
                    item.every_km.is_none_or(|k| k.is_finite() && k > 0.0),
                    "maintenance {:?}: every_km > 0",
                    item.id
                );
                ensure!(
                    item.every_months.is_none_or(|m| (1..=240).contains(&m)),
                    "maintenance {:?}: every_months 1–240",
                    item.id
                );
            }
        }
        Ok(())
    }
}

/// What Moli keeps of a vehicle (`data/vehicles.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    /// Km counted since the vehicle was first seen.
    #[serde(default)]
    pub counted_km: f64,
    /// The odometer a person read, and the count then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
    #[serde(default)]
    pub records: Vec<Record>,
    /// What Torque calls each key it logs, and its unit.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub units: BTreeMap<String, String>,
    /// The fault codes last reported.
    #[serde(default)]
    pub dtc: Vec<String>,
    /// The ECU's « distance since codes cleared » last seen (PID 01 31):
    /// what it counted while Moli heard nothing (Torque closed, a trip
    /// without network) is counted at the next upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_since_clear: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Calibration {
    pub km: f64,
    pub counted_km: f64,
    pub at_ms: u64,
}

impl Book {
    /// The odometer now, if a person ever gave a reading.
    #[must_use]
    pub fn odometer(&self) -> Option<f64> {
        self.calibration
            .map(|c| c.km + (self.counted_km - c.counted_km))
    }

    fn calibrate(&mut self, km: f64, at_ms: u64) {
        self.calibration = Some(Calibration {
            km,
            counted_km: self.counted_km,
            at_ms,
        });
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    vehicles: BTreeMap<String, Book>,
}

/// The books on disk, shared by the API and the driver.
#[derive(Debug)]
struct Store {
    path: PathBuf,
    file: Mutex<File>,
}

impl Store {
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

    fn book(&self, id: &str) -> Book {
        self.lock().vehicles.get(id).cloned().unwrap_or_default()
    }

    /// Changes a book and writes it down.
    fn update<T>(&self, id: &str, change: impl FnOnce(&mut Book) -> T) -> anyhow::Result<T> {
        let mut file = self.lock();
        let out = change(file.vehicles.entry(id.to_owned()).or_default());
        self.save(&file)?;
        Ok(out)
    }
}

#[derive(Debug)]
enum Event {
    Upload(String, torque::Sample),
}

/// What the API holds: the uploads' way in, and the books to show.
#[derive(Clone, Debug)]
pub struct Gateway {
    config: Arc<Config>,
    /// The driver's instance, once it runs (`<instance>:<vehicle>`).
    instance: Arc<std::sync::OnceLock<String>>,
    store: Arc<Store>,
    tx: mpsc::Sender<Event>,
}

/// A vehicle as the household sees it.
#[derive(Clone, Debug, Serialize)]
pub struct VehicleView {
    pub id: String,
    pub name: String,
    pub odometer: Option<f64>,
    pub calibration: Option<Calibration>,
    pub maintenance: Vec<ItemView>,
    pub records: Vec<Record>,
    pub dtc: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemView {
    #[serde(flatten)]
    pub item: Item,
    pub km_left: Option<f64>,
    pub days_left: Option<f64>,
    pub due: bool,
}

impl Gateway {
    /// The vehicle whose token this is.
    #[must_use]
    pub fn vehicle_for(&self, token: &str) -> Option<String> {
        if token.is_empty() || token.len() > 128 {
            return None;
        }
        // Hashes compared, not tokens: timing could only leak a hash.
        let hash = sha256_hex(token);
        self.config
            .vehicle
            .iter()
            .find(|v| v.token_sha256.eq_ignore_ascii_case(&hash))
            .map(|v| v.id.clone())
    }

    /// An upload from Torque (query string or form body).
    pub fn upload(&self, vehicle: &str, raw: &str) -> anyhow::Result<()> {
        ensure!(raw.len() <= torque::MAX_LEN, "upload too large");
        let sample = torque::read(&torque::parse_pairs(raw))?;
        if sample.is_empty() {
            // Torque's « test settings », or nothing logged yet.
            return Ok(());
        }
        // What each upload carries (never its values).
        tracing::debug!(
            vehicle,
            values = sample.values.len(),
            names = sample.names.len(),
            dtc = sample.dtc.is_some(),
            "vehicle upload"
        );
        self.tx
            .try_send(Event::Upload(vehicle.to_owned(), sample))
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.vehicles.occupe")))
    }

    /// The vehicle's own device id, once the driver runs.
    #[must_use]
    pub fn device_id(&self, vehicle: &str) -> Option<String> {
        self.instance.get().map(|i| format!("{i}:{vehicle}"))
    }

    /// What the vehicle's screen may read of the house.
    #[must_use]
    pub fn shared(&self, vehicle: &str) -> Vec<Shared> {
        self.config
            .vehicle
            .iter()
            .find(|v| v.id == vehicle)
            .map(|v| v.share.clone())
            .unwrap_or_default()
    }

    /// What the vehicle's screen may order.
    #[must_use]
    pub fn actions(&self, vehicle: &str) -> Vec<CarAction> {
        self.config
            .vehicle
            .iter()
            .find(|v| v.id == vehicle)
            .map(|v| v.action.clone())
            .unwrap_or_default()
    }

    /// The vehicles, their books and what is due.
    #[must_use]
    pub fn list(&self) -> Vec<VehicleView> {
        let now = moli_core::now_ms();
        self.config
            .vehicle
            .iter()
            .map(|v| {
                let book = self.store.book(&v.id);
                let odometer = book.odometer();
                VehicleView {
                    id: v.id.clone(),
                    name: v.name.clone(),
                    odometer,
                    calibration: book.calibration,
                    maintenance: v
                        .maintenance
                        .iter()
                        .map(|item| {
                            let d = garage::due(item, &book.records, odometer, now);
                            ItemView {
                                item: item.clone(),
                                due: d.is_due(),
                                km_left: d.km_left,
                                days_left: d.days_left,
                            }
                        })
                        .collect(),
                    records: book.records.clone(),
                    dtc: book.dtc.clone(),
                }
            })
            .collect()
    }
}

/// The driver: one device per vehicle.
#[derive(Debug)]
pub struct Vehicles {
    gateway: Gateway,
    events: tokio::sync::Mutex<mpsc::Receiver<Event>>,
}

/// The pair the binary wires: the gateway for the API, the driver for the hub.
pub fn open(config: Config, data_dir: &Path) -> anyhow::Result<(Gateway, Vehicles)> {
    config.check()?;
    let store = Arc::new(Store::open(data_dir.join("vehicles.json"))?);
    let (tx, rx) = mpsc::channel(64);
    let gateway = Gateway {
        config: Arc::new(config),
        instance: Arc::default(),
        store,
        tx,
    };
    Ok((
        gateway.clone(),
        Vehicles {
            gateway,
            events: tokio::sync::Mutex::new(rx),
        },
    ))
}

impl Driver for Vehicles {
    fn kind(&self) -> &'static str {
        "vehicles"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

fn numeric() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

fn read_only(
    key: &str,
    label: &str,
    kind: Kind,
    unit: Option<&str>,
    semantic: Semantic,
) -> PointSpec {
    let unit = unit.and_then(Unit::parse);
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

fn label(point: &str) -> String {
    moli_i18n::tr(&format!("pilotes.vehicles.{point}"))
}

/// The extra key's point: `pid_<key>`.
fn extra_point(key: &str) -> String {
    format!("pid_{key}")
}

#[allow(clippy::too_many_lines)]
fn device(ctx: &DriverCtx, v: &VehicleConfig, book: &Book, extras: &[String]) -> Device {
    let mut points = vec![
        read_only(
            "driving",
            &label("en_route"),
            Kind::Binary,
            None,
            Semantic::Other,
        ),
        read_only(
            "odometer",
            &label("compteur"),
            numeric(),
            Some("km"),
            Semantic::Other,
        ),
        read_only(
            "trip",
            &label("trajet"),
            numeric(),
            Some("km"),
            Semantic::Other,
        ),
        read_only("mil", &label("voyant"), Kind::Binary, None, Semantic::Other),
        read_only(
            "dtc_count",
            &label("nb_defauts"),
            numeric(),
            None,
            Semantic::Other,
        ),
        read_only("dtc", &label("defauts"), Kind::Text, None, Semantic::Other),
        read_only(
            "alert",
            &label("a_voir"),
            Kind::Binary,
            None,
            Semantic::Alert,
        ),
        read_only(
            "detail",
            &label("detail"),
            Kind::Text,
            None,
            Semantic::Other,
        ),
    ];
    for (_, point, unit) in KNOWN {
        let semantic = match *point {
            "coolant" | "oil_temp" | "intake_temp" | "ambient_temp" => Semantic::Temperature,
            "adapter_voltage" | "module_voltage" => Semantic::Voltage,
            "intake_pressure" | "barometer" => Semantic::Pressure,
            _ => Semantic::Other,
        };
        points.push(read_only(point, &label(point), numeric(), *unit, semantic));
    }
    for item in &v.maintenance {
        points.push(read_only(
            &format!("{}_km_left", item.id),
            &moli_i18n::tr!("pilotes.vehicles.km_restants", item = item.name),
            numeric(),
            Some("km"),
            Semantic::Other,
        ));
        points.push(read_only(
            &format!("{}_days_left", item.id),
            &moli_i18n::tr!("pilotes.vehicles.jours_restants", item = item.name),
            numeric(),
            Some("d"),
            Semantic::Other,
        ));
        points.push(read_only(
            &format!("{}_due", item.id),
            &moli_i18n::tr!("pilotes.vehicles.a_faire", item = item.name),
            Kind::Binary,
            None,
            Semantic::Other,
        ));
    }
    for key in extras {
        let name = book.names.get(key).cloned().unwrap_or_else(|| key.clone());
        points.push(read_only(
            &extra_point(key),
            &name,
            numeric(),
            book.units.get(key).map(String::as_str),
            Semantic::Other,
        ));
    }
    let writable = |key: &str, label: String, kind: Kind, unit: Option<&str>| PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: false,
            write: true,
        },
        unit: unit.and_then(Unit::parse),
        semantic: Semantic::Config,
    };
    points.push(writable(
        "odometer_set",
        moli_i18n::tr!("pilotes.vehicles.regler_compteur"),
        Kind::Numeric {
            min: Some(0.0),
            max: Some(2_000_000.0),
            step: Some(1.0),
        },
        Some("km"),
    ));
    points.push(writable(
        "maintenance_done",
        moli_i18n::tr!("pilotes.vehicles.entretien_fait"),
        Kind::Text,
        None,
    ));
    Device {
        id: ctx.device_id(&v.id),
        instance: ctx.instance().clone(),
        native_name: v.name.as_str().into(),
        manufacturer: v.manufacturer.as_deref().map(Into::into),
        model: v.model.as_deref().map(Into::into),
        description: Some(moli_i18n::tr!("pilotes.vehicles.description").into()),
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// What the driver follows of a vehicle between uploads.
#[derive(Debug, Default)]
struct Live {
    session: Option<String>,
    session_start_km: f64,
    last_ms: u64,
    speed: Option<f64>,
    extras: Vec<String>,
    mil_from_pid: Option<(bool, u32)>,
    saved_km: f64,
}

struct State<'a> {
    this: &'a Vehicles,
    live: HashMap<String, Live>,
}

impl State<'_> {
    fn config(&self, id: &str) -> Option<&VehicleConfig> {
        self.this.gateway.config.vehicle.iter().find(|v| v.id == id)
    }

    fn publish_device(&self, ctx: &DriverCtx, id: &str) {
        let Some(v) = self.config(id) else { return };
        let book = self.this.gateway.store.book(id);
        let extras = self
            .live
            .get(id)
            .map(|l| l.extras.clone())
            .unwrap_or_default();
        ctx.upsert_device(device(ctx, v, &book, &extras));
    }

    /// The book's points: mileage, faults, what is due.
    fn publish_book(&self, ctx: &DriverCtx, id: &str) {
        let Some(v) = self.config(id) else { return };
        let dev = ctx.device_id(id);
        let book = self.this.gateway.store.book(id);
        let live = self.live.get(id);
        let odometer = book.odometer();
        let num =
            |x: Option<f64>| x.map_or(Value::Null, |x| Value::Float((x * 10.0).round() / 10.0));
        ctx.set_state(&dev, "odometer", num(odometer));
        ctx.set_state(
            &dev,
            "trip",
            num(live
                .filter(|l| l.session.is_some())
                .map(|l| book.counted_km - l.session_start_km)),
        );
        let (pid_mil, pid_count) = live
            .and_then(|l| l.mil_from_pid)
            .map_or((None, None), |(m, c)| (Some(m), Some(c)));
        let mil = pid_mil.unwrap_or(false) || !book.dtc.is_empty();
        ctx.set_state(&dev, "mil", Value::Bool(mil));
        let count = pid_count.map_or_else(|| book.dtc.len(), |c| c as usize);
        ctx.set_state(
            &dev,
            "dtc_count",
            Value::Int(i64::try_from(count).unwrap_or(0)),
        );
        ctx.set_state(&dev, "dtc", Value::Text(book.dtc.join(", ").into()));
        let now = moli_core::now_ms();
        let mut notes = Vec::new();
        if mil {
            notes.push(if book.dtc.is_empty() {
                moli_i18n::tr!("pilotes.vehicles.voyant_allume")
            } else {
                moli_i18n::tr!("pilotes.vehicles.voyant_codes", codes = book.dtc.join(", "))
            });
        }
        for item in &v.maintenance {
            let d = garage::due(item, &book.records, odometer, now);
            ctx.set_state(
                &dev,
                &format!("{}_km_left", item.id),
                num(d.km_left.map(f64::round)),
            );
            ctx.set_state(
                &dev,
                &format!("{}_days_left", item.id),
                num(d.days_left.map(f64::floor)),
            );
            ctx.set_state(&dev, &format!("{}_due", item.id), Value::Bool(d.is_due()));
            if d.is_due() {
                notes.push(moli_i18n::tr!("pilotes.vehicles.a_faire", item = item.name));
            }
        }
        ctx.set_state(&dev, "alert", Value::Bool(!notes.is_empty()));
        ctx.set_state(&dev, "detail", Value::Text(notes.join(" · ").into()));
    }

    #[allow(clippy::too_many_lines)]
    fn upload(&mut self, ctx: &DriverCtx, id: &str, sample: &torque::Sample) -> anyhow::Result<()> {
        let Some(v) = self.config(id).cloned() else {
            return Ok(());
        };
        let dev = ctx.device_id(id);
        let now = moli_core::now_ms();
        let store = Arc::clone(&self.this.gateway.store);
        let mut book = store.book(id);
        let mut book_changed = false;
        let live = self.live.entry(id.to_owned()).or_default();

        // A new trip.
        if sample.session.is_some() && sample.session != live.session {
            live.session.clone_from(&sample.session);
            live.session_start_km = book.counted_km;
            live.speed = None;
            book_changed = true;
            tracing::info!(vehicle = id, "vehicle: new Torque session");
        }

        // Names and units Torque gives at a session's start.
        let mut device_changed = false;
        for (map, incoming) in [
            (&mut book.names, &sample.names),
            (&mut book.units, &sample.units),
        ] {
            for (key, text) in incoming {
                if map.get(key) != Some(text) && map.len() < 200 {
                    map.insert(key.clone(), text.clone());
                    book_changed = true;
                    device_changed |= live.extras.contains(key);
                }
            }
        }
        // What else Torque logs: one point each (bounded).
        for key in sample.values.keys() {
            if known(key).is_none() && !live.extras.contains(key) && live.extras.len() < MAX_EXTRAS
            {
                live.extras.push(key.clone());
                device_changed = true;
            }
        }

        // Distance: the ECU's own counter when logged (kept between trips,
        // so a trip Moli did not hear is counted too), else speed × time.
        let gap = now.saturating_sub(live.last_ms);
        let mut driven = 0.0;
        if let Some(&clear) = sample.values.get("k31") {
            if let Some(before) = book.last_since_clear {
                let delta = clear - before;
                // Below zero: codes were cleared, the counter starts again.
                if (0.0..=MAX_UNHEARD_KM).contains(&delta) {
                    driven = delta;
                }
            }
            if book.last_since_clear != Some(clear) {
                book.last_since_clear = Some(clear);
                book_changed = true;
            }
        } else if book.last_since_clear.is_none()
            && let Some(&speed) = sample
                .values
                .get("kd")
                .or_else(|| sample.values.get("kff1001"))
            && (0.0..=250.0).contains(&speed)
        {
            if let Some(before) = live.speed
                && gap <= MAX_GAP_MS
            {
                #[allow(clippy::cast_precision_loss)] // gap ≤ 15 000 ms
                let hours = gap as f64 / 3_600_000.0;
                driven = f64::midpoint(before, speed) * hours;
            }
            live.speed = Some(speed);
        }
        if driven > 0.0 {
            book.counted_km += driven;
        }
        if let Some(&odo) = sample.values.get("ka6")
            && odo > 0.0
            && book.odometer().is_none_or(|o| (o - odo).abs() > 1.0)
        {
            // The ECU knows the odometer: it wins.
            book.calibrate(odo, now);
            book_changed = true;
        }
        live.last_ms = now;

        // The fault light from a custom PID 01 01 (byte A).
        if let Some(key) = &v.mil_key
            && let Some(&a) = sample.values.get(key)
            && (0.0..=255.0).contains(&a)
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a byte
            let a = a as u32;
            live.mil_from_pid = Some((a & 0x80 != 0, a & 0x7f));
        }
        if let Some(codes) = &sample.dtc
            && *codes != book.dtc
        {
            let new: Vec<&String> = codes.iter().filter(|c| !book.dtc.contains(c)).collect();
            if !new.is_empty() {
                tracing::warn!(vehicle = id, codes = ?new, "vehicle: new fault codes");
            }
            book.dtc.clone_from(codes);
            book_changed = true;
        }

        // Written down at each change of the book, and every half km.
        let write = book_changed || book.counted_km - live.saved_km >= 0.5;
        if write {
            live.saved_km = book.counted_km;
        }
        if write {
            store.update(id, |b| *b = book)?;
        } else {
            // The count lives in the book; kept current without writing.
            store.lock().vehicles.insert(id.to_owned(), book);
        }

        // The points exist before their values.
        if device_changed {
            self.publish_device(ctx, id);
        }
        for (key, value) in &sample.values {
            let point = known(key).map_or_else(|| extra_point(key), |(_, p, _)| (*p).to_owned());
            ctx.set_state(
                &dev,
                &point,
                Value::Float((value * 1000.0).round() / 1000.0),
            );
        }
        ctx.set_availability(&dev, true);
        ctx.set_state(&dev, "driving", Value::Bool(true));
        self.publish_book(ctx, id);
        Ok(())
    }

    /// Uploads stopped: parked. The last count is written down.
    fn tick(&mut self, ctx: &DriverCtx) {
        let now = moli_core::now_ms();
        let store = &self.this.gateway.store;
        for (id, live) in &mut self.live {
            if live.last_ms > 0 && now.saturating_sub(live.last_ms) > DRIVING_FOR_MS {
                ctx.set_state(&ctx.device_id(id), "driving", Value::Bool(false));
                live.speed = None;
                let book = store.book(id);
                if (book.counted_km - live.saved_km).abs() > f64::EPSILON {
                    live.saved_km = book.counted_km;
                    if let Err(e) = store.update(id, |_| ()) {
                        tracing::warn!(vehicle = id, error = %e, "vehicle book not written");
                    }
                }
            }
        }
        // Days pass: what is due by time.
        let ids: Vec<String> = self
            .this
            .gateway
            .config
            .vehicle
            .iter()
            .map(|v| v.id.clone())
            .collect();
        for id in ids {
            self.publish_book(ctx, &id);
        }
    }

    fn command(&mut self, ctx: &DriverCtx, command: &CommandRequest) -> Result<(), String> {
        let id = command
            .device
            .id
            .as_str()
            .split_once(':')
            .map_or("", |(_, id)| id)
            .to_owned();
        let Some(v) = self.config(&id).cloned() else {
            return Err(moli_i18n::tr!("pilotes.vehicles.inconnu"));
        };
        let now = moli_core::now_ms();
        let store = &self.this.gateway.store;
        let result = match (&*command.key, &command.value) {
            ("odometer_set", value) => {
                let km = value
                    .as_f64()
                    .ok_or_else(|| moli_i18n::tr!("pilotes.vehicles.un_nombre"))?;
                store.update(&id, |b| b.calibrate(km, now))
            }
            ("maintenance_done", Value::Text(text)) => {
                let done = Done::parse(text).map_err(|e| format!("{e:#}"))?;
                if !v.maintenance.iter().any(|i| i.id == done.item) {
                    let ids: Vec<&str> = v.maintenance.iter().map(|i| i.id.as_str()).collect();
                    return Err(moli_i18n::tr!(
                        "pilotes.vehicles.entretien_inconnu",
                        item = done.item,
                        items = ids.join(", ")
                    ));
                }
                if let Some(km) = done.km
                    && !(km.is_finite() && (0.0..=2_000_000.0).contains(&km))
                {
                    return Err(moli_i18n::tr!("pilotes.vehicles.un_nombre"));
                }
                if done.note.as_ref().is_some_and(|n| n.chars().count() > 200) {
                    return Err("note: 200 characters at most".into());
                }
                let at_ms = done.at_ms(now).map_err(|e| format!("{e:#}"))?;
                store.update(&id, |b| {
                    // A reading of the dashboard: the best truth there is.
                    if let Some(km) = done.km {
                        b.calibrate(km, now);
                    }
                    let km = done.km.or_else(|| b.odometer());
                    b.records.push(Record {
                        item: done.item.clone(),
                        at_ms,
                        km,
                        note: done.note.clone(),
                    });
                    b.records.sort_by_key(|r| r.at_ms);
                    if b.records.len() > MAX_RECORDS {
                        b.records.remove(0);
                    }
                })
            }
            ("maintenance_done", _) => return Err(moli_i18n::tr!("pilotes.vehicles.du_texte")),
            _ => return Err(moli_i18n::tr!("pilotes.vehicles.ne_s_ecrit_pas")),
        };
        result.map_err(|e| format!("{e:#}"))?;
        if let Some(live) = self.live.get_mut(&id) {
            live.saved_km = store.book(&id).counted_km;
        }
        self.publish_book(ctx, &id);
        Ok(())
    }
}

async fn run(this: &Vehicles, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let _ = this
        .gateway
        .instance
        .set(ctx.instance().as_str().to_owned());
    let mut state = State {
        this,
        live: HashMap::new(),
    };
    for v in &this.gateway.config.vehicle {
        state.publish_device(ctx, &v.id);
        ctx.set_state(&ctx.device_id(&v.id), "driving", Value::Bool(false));
        state.publish_book(ctx, &v.id);
    }
    ctx.ready();
    let mut events = this.events.lock().await;
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    let result = state.command(ctx, &command);
                    command.reply(result);
                }
                None => return Ok(()),
            },
            event = events.recv() => match event {
                Some(Event::Upload(id, sample)) => {
                    if let Err(e) = state.upload(ctx, &id, &sample) {
                        tracing::warn!(vehicle = id, error = %e, "vehicle upload not kept");
                    }
                }
                // The API is gone first when Moli stops: wait for the end.
                None => {
                    ctx.cancelled().await;
                    return Ok(());
                }
            },
            _ = tick.tick() => state.tick(ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "torque-token-for-tests-0123456789";

    fn config() -> Config {
        Config {
            vehicle: vec![VehicleConfig {
                id: "car".into(),
                name: "Car".into(),
                manufacturer: None,
                model: None,
                token_sha256: sha256_hex(TOKEN),
                mil_key: Some("k101".into()),
                maintenance: vec![Item {
                    id: "oil".into(),
                    name: "Oil".into(),
                    every_km: Some(15_000.0),
                    every_months: Some(12),
                }],
                share: vec![Shared {
                    id: "door".into(),
                    point: "z2m:door/contact".into(),
                }],
                action: vec![CarAction {
                    id: "warm".into(),
                    name: "Warm up".into(),
                    point: "helpers:target/value".into(),
                    value: serde_json::json!(21),
                }],
            }],
        }
    }

    #[test]
    fn the_token_finds_its_vehicle_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("moli-vehicles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (gate, _driver) = open(config(), &dir).unwrap();
        assert_eq!(gate.vehicle_for(TOKEN).as_deref(), Some("car"));
        assert_eq!(gate.vehicle_for("nope"), None);
        assert_eq!(gate.vehicle_for(""), None);
        assert_eq!(gate.vehicle_for(&"x".repeat(500)), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_configuration_is_refused() {
        let mut c = config();
        c.vehicle[0].token_sha256 = "abc".into();
        assert!(c.check().is_err());
        let mut c = config();
        c.vehicle[0].maintenance[0].every_km = None;
        c.vehicle[0].maintenance[0].every_months = None;
        assert!(c.check().is_err());
        let mut c = config();
        c.vehicle[0].id = "Car 1".into();
        assert!(c.check().is_err());
        let mut c = config();
        c.vehicle.push(c.vehicle[0].clone());
        assert!(c.check().is_err());
        let mut c = config();
        c.vehicle[0].share[0].point = "nothing".into();
        assert!(c.check().is_err());
        let mut c = config();
        c.vehicle[0].action[0].value = serde_json::json!({ "a": 1 });
        assert!(c.check().is_err());
        assert!(config().check().is_ok());
    }

    #[test]
    fn the_odometer_follows_its_reading() {
        let mut b = Book::default();
        assert_eq!(b.odometer(), None);
        b.counted_km = 10.0;
        b.calibrate(100_000.0, 1);
        b.counted_km = 25.5;
        assert_eq!(b.odometer(), Some(100_015.5));
    }

    #[test]
    fn every_known_key_has_a_label_in_both_languages() {
        for lang in ["fr", "en"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../locales")
                .join(lang)
                .join("pilotes.json");
            let text = std::fs::read_to_string(path).unwrap();
            for (_, point, _) in KNOWN {
                assert!(
                    text.contains(&format!("\"pilotes.vehicles.{point}\"")),
                    "{lang}: pilotes.vehicles.{point}"
                );
            }
        }
    }

    mod driving {
        use super::*;
        use moli_core::{InstanceId, Origin, PointId};
        use moli_runtime::{Hub, HubOptions, spawn_driver};
        use tokio_util::sync::CancellationToken;

        fn point(key: &str) -> PointId {
            PointId::from(format!("voitures:car/{key}"))
        }

        async fn settled(hub: &Hub, key: &str, want: &Value) {
            for _ in 0..300 {
                if hub.state(&point(key)).is_some_and(|s| &s.value == want) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("{key} never became {want:?}: {:?}", hub.state(&point(key)));
        }

        /// Uploads become points, the mileage follows a reading, a fault and
        /// a maintenance show on the vehicle.
        #[tokio::test]
        async fn a_trip_reaches_the_house() {
            let dir =
                std::env::temp_dir().join(format!("moli-vehicles-trip-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let hub = Hub::new(HubOptions::default()).unwrap();
            let (gate, driver) = open(config(), &dir).unwrap();
            let cancel = CancellationToken::new();
            spawn_driver(
                &hub,
                InstanceId::from("voitures"),
                Arc::new(driver),
                cancel.clone(),
            );
            settled(&hub, "driving", &Value::Bool(false)).await;

            hub.command(
                &point("odometer_set"),
                Value::Int(100_000),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
            settled(&hub, "odometer", &Value::Float(100_000.0)).await;

            let car = gate.vehicle_for(TOKEN).unwrap();
            gate.upload(&car, "session=7&userFullNamek10=Air+flow&userUnitk10=g%2Fs")
                .unwrap();
            gate.upload(
                &car,
                "session=7&kd=60&kff1006=42.7&kff1005=2.9&k10=3.25&k31=1000",
            )
            .unwrap();
            settled(&hub, "driving", &Value::Bool(true)).await;
            settled(&hub, "latitude", &Value::Float(42.7)).await;
            settled(&hub, "speed", &Value::Float(60.0)).await;
            settled(&hub, "pid_k10", &Value::Float(3.25)).await;
            let device = hub
                .snapshot()
                .devices
                .into_iter()
                .find(|d| d.device.id.as_str() == "voitures:car")
                .unwrap();
            let maf = device.device.point("pid_k10").unwrap();
            assert_eq!(&*maf.label, "Air flow");

            // The ECU's distance counter: 2 km more.
            gate.upload(&car, "session=7&k31=1002").unwrap();
            settled(&hub, "odometer", &Value::Float(100_002.0)).await;
            settled(&hub, "trip", &Value::Float(2.0)).await;
            // A trip Moli did not hear (Torque closed), then a new session.
            gate.upload(&car, "session=8&k31=1010").unwrap();
            settled(&hub, "odometer", &Value::Float(100_010.0)).await;
            // Codes cleared: the counter starts again, nothing is lost.
            gate.upload(&car, "session=8&k31=0").unwrap();
            gate.upload(&car, "session=8&k31=1").unwrap();
            settled(&hub, "odometer", &Value::Float(100_011.0)).await;

            gate.upload(&car, "session=7&dtc=P0401").unwrap();
            settled(&hub, "mil", &Value::Bool(true)).await;
            settled(&hub, "alert", &Value::Bool(true)).await;
            settled(&hub, "dtc", &Value::Text("P0401".into())).await;

            hub.command(
                &point("maintenance_done"),
                Value::Text("oil 100020".into()),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
            settled(&hub, "oil_km_left", &Value::Float(15_000.0)).await;
            settled(&hub, "oil_due", &Value::Bool(false)).await;
            settled(&hub, "odometer", &Value::Float(100_020.0)).await;
            assert!(
                hub.command(
                    &point("maintenance_done"),
                    Value::Text("brakes".into()),
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
            );

            // Codes cleared: the light goes off.
            gate.upload(&car, "session=7&dtc=").unwrap();
            settled(&hub, "mil", &Value::Bool(false)).await;
            settled(&hub, "alert", &Value::Bool(false)).await;

            // The book survives a restart.
            let list = gate.list();
            assert_eq!(list[0].records.len(), 1);
            assert_eq!(list[0].odometer, Some(100_020.0));
            cancel.cancel();
            let (again, _) = open(config(), &dir).unwrap();
            assert_eq!(again.list()[0].records.len(), 1);
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

//! Teleinfo (Linky TIC) driver: the electricity meter's serial channel.
//!
//! Reads a Linky meter through a USB-TIC interface (for instance a
//! Cartelectronic USBTICLC on `/dev/ttyUSB0`), parses the teleinfo frames
//! (STX…ETX, per-line checksums; two wire formats — 9600 baud « standard »
//! with tabs, 1200 baud « historique » with spaces — both 7E1) and publishes
//! the readings as read-only points on a single `teleinfo:<id>` device.
//!
//! The meter pushes one frame roughly every 10 s while powered. The driver:
//!
//! - opens the port on a blocking thread, retrying with backoff and holding
//!   the driver in `waiting` meanwhile (module unplugged, no dialout access…);
//! - marks the device offline after `offline_after` s without a frame, back
//!   online on the next valid frame;
//! - drops corrupted lines (failed checksum) and counts them — one bad line
//!   never aborts a frame.
//!
//! Energy integration: cumulative counters are published in Wh (the energy
//! module applies its Wh → kWh factor), powers in VA, currents in A and
//! voltages in V. Teleinfo sub-units (0.1 VA, 0.1 A, 0.1 V) are divided out
//! here, so a `PAPP` of `00120` publishes `12.0` VA.

mod frame;

pub use frame::{Frame, FrameParser, Line, Mode};

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
use tokio::sync::mpsc;

/// Driver options (`[driver.options]` for a `kind = "teleinfo"` driver).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The serial port of the TIC interface (default `/dev/ttyUSB0`).
    #[serde(default = "default_port")]
    pub port: String,
    /// The Linky's rate: 9600 (« standard ») or 1200 (« historique »).
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// The meter's native id: the device is `<instance>:<id>`.
    #[serde(default = "default_id")]
    pub id: String,
    /// The device's display name (default "Linky").
    #[serde(default = "default_name")]
    pub name: String,
    /// Seconds without a frame before the device counts offline (default 90).
    #[serde(default = "default_offline_after")]
    pub offline_after: u64,
}

fn default_port() -> String {
    "/dev/ttyUSB0".into()
}

fn default_baud() -> u32 {
    9600
}

fn default_id() -> String {
    "linky".into()
}

fn default_name() -> String {
    "Linky".into()
}

fn default_offline_after() -> u64 {
    90
}

/// The teleinfo driver.
#[derive(Clone, Debug)]
pub struct Teleinfo {
    config: Config,
}

impl Teleinfo {
    /// Validates the options. No I/O here: the port is only opened in `run`,
    /// so the binary can build the driver while refusing bad options.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        ensure!(!config.port.trim().is_empty(), "port: empty path");
        ensure!(!config.id.is_empty(), "id: must not be empty");
        ensure!(
            !config.id.contains([':', '/']),
            "id {:?}: no ':' or '/' (the device id is <instance>:<id>)",
            config.id
        );
        ensure!(!config.name.trim().is_empty(), "name: must not be empty");
        ensure!(
            config.baud == 1200 || config.baud == 9600,
            "baud: 9600 (standard) or 1200 (historique)"
        );
        ensure!(
            config.offline_after >= 30,
            "offline_after: 30 s at least (frames arrive ~every 10 s)"
        );
        Ok(Self { config })
    }
}

impl Driver for Teleinfo {
    fn kind(&self) -> &'static str {
        "teleinfo"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// One published point: its stable key, the teleinfo tag(s) feeding it and
/// its shape.
#[derive(Clone, Debug)]
struct PointDef {
    key: &'static str,
    label_key: String,
    tags: &'static [&'static str],
    is_text: bool,
    unit: Option<Unit>,
    semantic: Semantic,
    /// Teleinfo sub-unit divisor: PAPP / IINST / UINST / SINSTS carry 0.1×
    /// the displayed unit (`00120` PAPP = 12.0 VA). Counters are in Wh.
    divisor: i64,
}

fn num(
    key: &'static str,
    tags: &'static [&'static str],
    unit: Unit,
    semantic: Semantic,
    divisor: i64,
) -> PointDef {
    PointDef {
        key,
        label_key: format!("pilotes.teleinfo.{key}"),
        tags,
        is_text: false,
        unit: Some(unit),
        semantic,
        divisor,
    }
}

fn text(key: &'static str, tags: &'static [&'static str], semantic: Semantic) -> PointDef {
    PointDef {
        key,
        label_key: format!("pilotes.teleinfo.{key}"),
        tags,
        is_text: true,
        unit: None,
        semantic,
        divisor: 1,
    }
}

/// The point registry, shared by the device declaration and the publisher.
fn point_defs() -> &'static [PointDef] {
    static DEFS: OnceLock<Vec<PointDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        vec![
            // Identity and tariff (text).
            text("adco", &["ADCO"], Semantic::Other),
            text("adsc", &["ADSC"], Semantic::Other),
            text("brid", &["BRID"], Semantic::Other),
            text("adt", &["ADT", "DATE"], Semantic::Other),
            text("ngtf", &["NGTF"], Semantic::Other),
            text("ptec", &["PTEC"], Semantic::Other),
            text("ltarf", &["LTARF"], Semantic::Other),
            text("optarif", &["OPTARIF"], Semantic::Other),
            text("motdetat", &["MOTDETAT"], Semantic::Other),
            text("demain", &["DEMAIN"], Semantic::Other),
            // Powers (teleinfo carries them in 0.1 VA).
            num(
                "papp",
                &["PAPP"],
                Unit::VoltAmpere,
                Semantic::ApparentPower,
                10,
            ),
            num(
                "sinsts",
                &["SINSTS"],
                Unit::VoltAmpere,
                Semantic::ApparentPower,
                10,
            ),
            num(
                "pmax",
                &["PMAX", "PPMAX"],
                Unit::VoltAmpere,
                Semantic::ApparentPower,
                10,
            ),
            // Currents (0.1 A, except ISOUSC/IMAX in 1 A).
            num(
                "iinst",
                &["IINST", "IINST01"],
                Unit::Ampere,
                Semantic::Current,
                10,
            ),
            num("irms1", &["IRMS1"], Unit::Ampere, Semantic::Current, 10),
            num("irms2", &["IRMS2"], Unit::Ampere, Semantic::Current, 10),
            num("irms3", &["IRMS3"], Unit::Ampere, Semantic::Current, 10),
            num("isousc", &["ISOUSC"], Unit::Ampere, Semantic::Current, 1),
            num("imax", &["IMAX"], Unit::Ampere, Semantic::Current, 1),
            // Voltages (0.1 V).
            num("uinst01", &["UINST01"], Unit::Volt, Semantic::Voltage, 10),
            num("uinst02", &["UINST02"], Unit::Volt, Semantic::Voltage, 10),
            num("uinst03", &["UINST03"], Unit::Volt, Semantic::Voltage, 10),
            num("urms1", &["URMS1"], Unit::Volt, Semantic::Voltage, 10),
            num("urms2", &["URMS2"], Unit::Volt, Semantic::Voltage, 10),
            num("urms3", &["URMS3"], Unit::Volt, Semantic::Voltage, 10),
            // Cumulative counters (Wh).
            num("hchc", &["HCHC"], Unit::WattHour, Semantic::Energy, 1),
            num("hchp", &["HCHP"], Unit::WattHour, Semantic::Energy, 1),
            num("hchc1", &["HCHC1"], Unit::WattHour, Semantic::Energy, 1),
            num("hchc2", &["HCHC2"], Unit::WattHour, Semantic::Energy, 1),
            num("hchc3", &["HCHC3"], Unit::WattHour, Semantic::Energy, 1),
            num("hchp1", &["HCHP1"], Unit::WattHour, Semantic::Energy, 1),
            num("hchp2", &["HCHP2"], Unit::WattHour, Semantic::Energy, 1),
            num("hchp3", &["HCHP3"], Unit::WattHour, Semantic::Energy, 1),
            num("base", &["BASE"], Unit::WattHour, Semantic::Energy, 1),
            num("hphc", &["HHPHC"], Unit::WattHour, Semantic::Energy, 1),
            num("east", &["EAST"], Unit::WattHour, Semantic::Energy, 1),
            num("easf01", &["EASF01"], Unit::WattHour, Semantic::Energy, 1),
            num("easf02", &["EASF02"], Unit::WattHour, Semantic::Energy, 1),
            num("easf03", &["EASF03"], Unit::WattHour, Semantic::Energy, 1),
            num("easf04", &["EASF04"], Unit::WattHour, Semantic::Energy, 1),
            num("easf05", &["EASF05"], Unit::WattHour, Semantic::Energy, 1),
            num("easf06", &["EASF06"], Unit::WattHour, Semantic::Energy, 1),
            num("easf07", &["EASF07"], Unit::WattHour, Semantic::Energy, 1),
            num("easf08", &["EASF08"], Unit::WattHour, Semantic::Energy, 1),
            num("easf09", &["EASF09"], Unit::WattHour, Semantic::Energy, 1),
            num("easf10", &["EASF10"], Unit::WattHour, Semantic::Energy, 1),
            num("easd01", &["EASD01"], Unit::WattHour, Semantic::Energy, 1),
            num("easd02", &["EASD02"], Unit::WattHour, Semantic::Energy, 1),
            num("easd03", &["EASD03"], Unit::WattHour, Semantic::Energy, 1),
            num("easd04", &["EASD04"], Unit::WattHour, Semantic::Energy, 1),
        ]
    })
}

/// The device this driver publishes: one meter, all its points.
fn device(ctx: &DriverCtx, config: &Config) -> Device {
    Device {
        id: ctx.device_id(&config.id),
        instance: ctx.instance().clone(),
        native_name: config.name.as_str().into(),
        manufacturer: Some("Enedis".into()),
        model: Some("Linky (TIC)".into()),
        description: Some(format!("TIC {} @ {} baud", config.port, config.baud).into()),
        native_room: None,
        members: Vec::new(),
        points: point_specs(),
    }
}

/// The device's points, in registry order.
fn point_specs() -> Vec<PointSpec> {
    point_defs()
        .iter()
        .map(|p| PointSpec {
            key: Arc::from(p.key),
            label: Arc::from(moli_i18n::tr(p.label_key.as_str())),
            kind: if p.is_text {
                Kind::Text
            } else {
                Kind::Numeric {
                    min: Some(0.0),
                    max: None,
                    step: None,
                }
            },
            access: Access {
                read: true,
                write: false,
            },
            unit: p.unit.clone(),
            semantic: p.semantic,
        })
        .collect()
}

/// The canonical value a line feeds a known point, if any.
/// Numeric lines are published as Float, with the 0.1× sub-units divided
/// out (counters stay integral, in Wh); text lines are trimmed of padding.
// Teleinfo counters stay far below 2^53, so the i64→f64 widening is exact
// for every value the meter can emit.
#[allow(clippy::cast_precision_loss)]
fn publish_value(def: &PointDef, line: &Line) -> Option<Value> {
    if !def.tags.iter().any(|t| *t == line.tag) {
        return None;
    }
    let raw = line.values.first()?.trim();
    if raw.is_empty() {
        return None;
    }
    if def.is_text {
        return Some(Value::Text(raw.to_owned().into()));
    }
    let n: i64 = raw.parse().ok()?;
    if n < 0 {
        return None;
    }
    Some(Value::Float(n as f64 / (def.divisor as f64)))
}

/// Publishes every known point carried by a frame; unknown tags are ignored.
fn publish_frame(ctx: &DriverCtx, device: &moli_core::DeviceId, frame: &Frame) {
    for line in &frame.lines {
        for def in point_defs() {
            if let Some(value) = publish_value(def, line) {
                ctx.set_state(device, def.key, value);
            }
        }
    }
}

/// The driver loop: port (re)open with backoff, a blocking reader task,
/// frame parsing, point publication, and the offline watchdog.
async fn run(this: &Teleinfo, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let device_id = ctx.device_id(&this.config.id);
    ctx.upsert_device(device(ctx, &this.config));
    ctx.set_availability(&device_id, false);

    let offline_after = Duration::from_secs(this.config.offline_after);

    loop {
        // (Re)open the port, backoff and `waiting` included; None = cancelled.
        let Some(port) = open_serial(this, ctx).await else {
            return Ok(());
        };

        // The blocking reader hands raw chunks to this loop.
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
        let reader = tokio::task::spawn_blocking(move || read_session(port, &tx));

        let mut parser = FrameParser::new();
        let mut up = false;
        let mut offline_reported = false;
        let opened_at = Instant::now();
        let mut last_frame_at: Option<Instant> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        'session: loop {
            tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    command.reply(Err(moli_i18n::tr!("pilotes.teleinfo.lecture_seule")));
                }
                None => return Ok(()),
            },
            chunk = rx.recv() => match chunk {
                Some(chunk) => {
                    for frame in parser.feed(&chunk) {
                        last_frame_at = Some(Instant::now());
                        if !up {
                            ctx.set_availability(&device_id, true);
                            up = true;
                        }
                        if offline_reported {
                            ctx.ready();
                            offline_reported = false;
                        }
                        if frame.bad_lines > 0 {
                            tracing::debug!(
                                bad_lines = frame.bad_lines,
                                "teleinfo: dropped corrupted lines"
                            );
                        }
                        publish_frame(ctx, &device_id, &frame);
                    }
                }
                None => break 'session, // the reader left (port error)
            },
            _ = tick.tick() => {
                    let quiet_since = last_frame_at.unwrap_or(opened_at);
                    if quiet_since.elapsed() >= offline_after && !offline_reported {
                        tracing::warn!(
                            port = %this.config.port,
                            offline_after_s = this.config.offline_after,
                            "teleinfo: no frame, device offline"
                        );
                        ctx.set_availability(&device_id, false);
                        let since_s = quiet_since.elapsed().as_secs();
                        ctx.wait_for(moli_i18n::tr!("pilotes.teleinfo.hors_ligne", since = since_s));
                        offline_reported = true;
                        up = false;
                    }
                }
            }
        }

        // The reader left (port error or driver shutdown): join it, give a
        // shutdown a chance to win the race, and otherwise reconnect after a beat.
        let _ = reader.await;
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    command.reply(Err(moli_i18n::tr!("pilotes.teleinfo.lecture_seule")));
                }
                None => return Ok(()),
            },
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

/// Opens the serial port (7E1: 7 data bits, even parity, 1 stop bit — the
/// teleinfo framing), retrying with capped backoff. Holds the driver in
/// `waiting` while retrying; returns None if the driver is cancelled.
async fn open_serial(this: &Teleinfo, ctx: &mut DriverCtx) -> Option<Box<dyn SerialPort>> {
    let mut delay = Duration::from_secs(1);
    loop {
        match open_once(this).await {
            Ok(port) => return Some(port),
            Err(e) => {
                tracing::warn!(
                    port = %this.config.port,
                    baud = this.config.baud,
                    error = %e,
                    "teleinfo: port open failed"
                );
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.teleinfo.port_absent",
                    port = this.config.port.as_str(),
                    cause = e.to_string().as_str()
                ));
            }
        }
        tokio::select! {
            () = ctx.cancelled() => return None,
            () = tokio::time::sleep(delay) => {}
        }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

/// One blocking open attempt, off the async runtime.
async fn open_once(this: &Teleinfo) -> anyhow::Result<Box<dyn SerialPort>> {
    let port = this.config.port.clone();
    let baud = this.config.baud;
    // The closure already wraps the open error with the port path; unwrap the
    // task-join layer so a single `Result` reaches the caller.
    tokio::task::spawn_blocking(move || {
        serialport::new(&port, baud)
            .data_bits(DataBits::Seven)
            .parity(Parity::Even)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(Duration::from_millis(250))
            .open()
            .map_err(anyhow::Error::from)
            .context(format!("port {port}"))
    })
    .await
    .map_err(|err| anyhow::anyhow!("serial open task: {err}"))?
}

/// Blocking reader: pulls chunks from the port until it errors, and sends
/// each chunk to the driver loop. A read timeout (no data yet) is normal —
/// the meter emits a frame only about every 10 s.
fn read_session(mut port: Box<dyn SerialPort>, tx: &mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 4096];
    loop {
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                if tx.blocking_send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                tracing::warn!(error = ?e, "teleinfo: serial read error");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(key: &str) -> &'static PointDef {
        point_defs().iter().find(|d| d.key == key).unwrap()
    }

    fn line(tag: &str, value: &str) -> Line {
        Line {
            tag: tag.into(),
            values: vec![value.to_owned()],
        }
    }

    #[test]
    fn config_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.port, "/dev/ttyUSB0");
        assert_eq!(c.baud, 9600);
        assert_eq!(c.id, "linky");
        assert_eq!(c.name, "Linky");
        assert_eq!(c.offline_after, 90);
        assert!(Teleinfo::new(c).is_ok());
    }

    #[test]
    fn config_refuses_unknown_keys_and_bad_values() {
        assert!(toml::from_str::<Config>("bogus = 1").is_err());
        let bad_baud = toml::from_str::<Config>("baud = 9601").unwrap();
        assert!(Teleinfo::new(bad_baud).is_err());
        let legacy_baud = toml::from_str::<Config>("baud = 1200").unwrap();
        assert!(Teleinfo::new(legacy_baud).is_ok());
        let bad_id = toml::from_str::<Config>("id = \"a:b\"").unwrap();
        assert!(Teleinfo::new(bad_id).is_err());
        let short_offline = toml::from_str::<Config>("offline_after = 5").unwrap();
        assert!(Teleinfo::new(short_offline).is_err());
    }

    #[test]
    fn the_registry_is_well_formed() {
        let specs = point_specs();
        assert_eq!(specs.len(), 50);
        let keys: Vec<&str> = specs.iter().map(|s| s.key.as_ref()).collect();
        let unique: Vec<&str> = {
            let mut v = keys.clone();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(keys.len(), unique.len(), "duplicate point keys");
        for spec in &specs {
            assert!(spec.access.read && !spec.access.write);
            assert_ne!(spec.unit.is_some(), spec.kind == Kind::Text);
        }
        let papp = specs.iter().find(|s| s.key.as_ref() == "papp").unwrap();
        assert_eq!(papp.unit, Some(Unit::VoltAmpere));
        assert_eq!(papp.semantic, Semantic::ApparentPower);
        let hchc = specs.iter().find(|s| s.key.as_ref() == "hchc").unwrap();
        assert_eq!(hchc.unit, Some(Unit::WattHour));
        assert_eq!(hchc.semantic, Semantic::Energy);
        let ptec = specs.iter().find(|s| s.key.as_ref() == "ptec").unwrap();
        assert_eq!(ptec.kind, Kind::Text);
    }

    #[test]
    fn labels_are_translated_in_both_languages() {
        moli_i18n::set_language("fr");
        for p in point_defs() {
            assert_ne!(
                moli_i18n::tr(p.label_key.as_str()),
                p.label_key,
                "fr label missing for {}",
                p.key
            );
        }
        moli_i18n::set_language("en");
        for p in point_defs() {
            assert_ne!(
                moli_i18n::tr(p.label_key.as_str()),
                p.label_key,
                "en label missing for {}",
                p.key
            );
        }
        moli_i18n::set_language("fr");
    }

    #[test]
    fn values_are_scaled_and_shaped() {
        // PAPP: 0.1 VA sub-units → 00120 publishes 12.0 VA.
        assert_eq!(
            publish_value(def("papp"), &line("PAPP", "00120")),
            Some(Value::Float(12.0))
        );
        // Counters are published as Float (integral values, in Wh).
        assert_eq!(
            publish_value(def("hchc"), &line("HCHC", "0021849106")),
            Some(Value::Float(21_849_106.0))
        );
        // ISOUSC is already in amperes.
        assert_eq!(
            publish_value(def("isousc"), &line("ISOUSC", "45")),
            Some(Value::Float(45.0))
        );
        // Text is trimmed of its padding.
        assert_eq!(
            publish_value(def("ptec"), &line("PTEC", "  HC  BLANC  ")),
            Some(Value::Text("HC  BLANC".into()))
        );
        // Unparseable or unknown: nothing published.
        assert_eq!(publish_value(def("papp"), &line("PAPP", "AB")), None);
        assert_eq!(publish_value(def("iinst"), &line("PAPP", "001")), None);
        assert_eq!(publish_value(def("hchc"), &line("HCHC", "  ")), None);
        // IINST01 feeds the same iinst point as IINST.
        assert_eq!(
            publish_value(def("iinst"), &line("IINST01", "001")),
            Some(Value::Float(0.1))
        );
    }
}

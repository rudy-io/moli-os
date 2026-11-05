//! A Linky meter read on the spot, through its TIC serial output (a USB module such
//! as the uTinfo or the Micro-Teleinfo, seen by the system as `/dev/ttyACM0` or
//! `/dev/ttyUSB0`).
//!
//! The meter repeats a frame of values every second or two: instantaneous power,
//! the energy indexes of each tariff period, the period in force. Nothing is asked
//! of the meter and nothing goes to the Internet: the driver only listens.
//!
//! A line that goes silent is the failure to expect (a module that hangs, a cable
//! half pulled out): with no valid group for `silence_seconds`, the device goes
//! offline and the port is closed and reopened, as the supervisor restarts the
//! driver. A group whose checksum fails is dropped; the next frame carries it again.

mod frame;

use std::io::{ErrorKind, Read};
use std::time::Duration;

use frame::{Event, Group, Mode, Reader};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How long a read waits for bytes before looking again whether anyone still listens.
const READ_TIMEOUT: Duration = Duration::from_millis(500);
const DEFAULT_SILENCE: u64 = 30;
/// A frame can take two seconds at 1200 baud: below this, the meter would look dead.
const MIN_SILENCE: u64 = 5;
/// Energy indexes a meter can hold (`EASF01` to `EASF10`).
const INDEXES: u8 = 10;

fn default_silence() -> u64 {
    DEFAULT_SILENCE
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The serial device of the TIC module, e.g. `/dev/ttyACM0`.
    pub port: String,
    /// `standard` (9600 baud, the default of recent meters) or `historique` (1200 baud).
    #[serde(default)]
    pub mode: Mode,
    /// Seconds without a valid group before the port is reopened (at least 5).
    #[serde(default = "default_silence")]
    pub silence_seconds: u64,
}

#[derive(Debug)]
pub struct LinkyTic {
    config: Config,
}

impl LinkyTic {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for LinkyTic {
    fn kind(&self) -> &'static str {
        "linky-tic"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

fn spec(key: &str, label: String, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
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

fn number() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: None,
    }
}

/// The points of the meter, whatever the mode: the ones a frame does not carry stay empty.
fn points() -> Vec<PointSpec> {
    let mut points = vec![
        spec(
            "apparent_power",
            moli_i18n::tr!("pilotes.linky_tic.puissance"),
            number(),
            Some(Unit::VoltAmpere),
            Semantic::ApparentPower,
        ),
        spec(
            "energy_total",
            moli_i18n::tr!("pilotes.linky_tic.energie_totale"),
            number(),
            Some(Unit::WattHour),
            Semantic::Energy,
        ),
    ];
    for n in 1..=INDEXES {
        points.push(spec(
            &format!("energy_index_{n:02}"),
            moli_i18n::tr!("pilotes.linky_tic.index", numero = n),
            number(),
            Some(Unit::WattHour),
            Semantic::Energy,
        ));
    }
    points.push(spec(
        "tariff",
        moli_i18n::tr!("pilotes.linky_tic.periode"),
        Kind::Text,
        None,
        Semantic::Other,
    ));
    points.push(spec(
        "tariff_index",
        moli_i18n::tr!("pilotes.linky_tic.numero_periode"),
        number(),
        None,
        Semantic::Other,
    ));
    for n in 1..=3 {
        points.push(spec(
            &format!("current_{n}"),
            moli_i18n::tr!("pilotes.linky_tic.courant", numero = n),
            number(),
            Some(Unit::Ampere),
            Semantic::Current,
        ));
    }
    points.push(spec(
        "voltage_1",
        moli_i18n::tr!("pilotes.linky_tic.tension"),
        number(),
        Some(Unit::Volt),
        Semantic::Voltage,
    ));
    points.push(spec(
        "max_apparent_power",
        moli_i18n::tr!("pilotes.linky_tic.puissance_max"),
        number(),
        Some(Unit::VoltAmpere),
        Semantic::ApparentPower,
    ));
    points
}

fn describe(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.linky_tic.compteur").into(),
        manufacturer: Some("Enedis".into()),
        model: Some("Linky (TIC)".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: points(),
    }
}

fn finite_number(text: &str) -> Option<Value> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map(Value::Float)
}

/// The index a label of the form `EASF07` stands for.
fn index_of(label: &str) -> Option<u8> {
    let digits = label.strip_prefix("EASF")?;
    let n: u8 = digits.parse().ok()?;
    (digits.len() == 2 && (1..=INDEXES).contains(&n)).then_some(n)
}

/// The points a group sets. A label this driver does not know sets none.
fn readings(mode: Mode, group: &Group) -> Vec<(String, Value)> {
    let label = group.label.as_str();
    let text = group.value.trim();
    let mut out: Vec<(String, Value)> = Vec::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            out.push((key.to_owned(), value));
        }
    };
    match (mode, label) {
        (Mode::Standard, "SINSTS") | (Mode::Historic, "PAPP") => {
            put("apparent_power", finite_number(text));
        }
        (Mode::Standard, "EAST") => put("energy_total", finite_number(text)),
        (Mode::Standard, "SMAXSN") => put("max_apparent_power", finite_number(text)),
        (Mode::Standard, "NTARF") => put("tariff_index", finite_number(text)),
        (Mode::Standard, "LTARF") if !text.is_empty() => {
            put("tariff", Some(Value::Text(text.into())));
        }
        (Mode::Standard, "IRMS1") | (Mode::Historic, "IINST") => {
            put("current_1", finite_number(text));
        }
        (Mode::Standard, "IRMS2") => put("current_2", finite_number(text)),
        (Mode::Standard, "IRMS3") => put("current_3", finite_number(text)),
        (Mode::Standard, "URMS1") => put("voltage_1", finite_number(text)),
        (Mode::Standard, _) => {
            if let Some(n) = index_of(label) {
                put(&format!("energy_index_{n:02}"), finite_number(text));
            }
        }
        // Historic mode counts by tariff option: one index, or heures creuses and pleines.
        (Mode::Historic, "BASE") => {
            put("energy_total", finite_number(text));
            put("energy_index_01", finite_number(text));
        }
        (Mode::Historic, "HCHC") => put("energy_index_01", finite_number(text)),
        (Mode::Historic, "HCHP") => put("energy_index_02", finite_number(text)),
        // "HP.." and "HC..": the dots only pad the field.
        (Mode::Historic, "PTEC") => {
            let period = text.trim_end_matches('.');
            if !period.is_empty() {
                put("tariff", Some(Value::Text(period.into())));
            }
        }
        (Mode::Historic, _) => {}
    }
    out
}

/// What the reading thread tells the driver.
enum Chunk {
    Bytes(Vec<u8>),
    Failed(String),
}

/// Reads the port until the driver stops listening. Runs on a thread of its own:
/// serial reads block.
fn read_port(port: &str, mode: Mode, tx: &mpsc::Sender<Chunk>) {
    let opened = serialport::new(port, mode.baud_rate())
        .data_bits(serialport::DataBits::Seven)
        .parity(serialport::Parity::Even)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(READ_TIMEOUT)
        .open();
    let mut serial = match opened {
        Ok(serial) => serial,
        Err(error) => {
            let _ = tx.blocking_send(Chunk::Failed(moli_i18n::tr!(
                "pilotes.linky_tic.ouverture",
                port = port,
                cause = error
            )));
            return;
        }
    };
    let mut buffer = [0_u8; 256];
    while !tx.is_closed() {
        let failure = match serial.read(&mut buffer) {
            Ok(0) => Some(moli_i18n::tr!("pilotes.linky_tic.port_ferme")),
            Ok(n) => {
                if tx
                    .blocking_send(Chunk::Bytes(buffer[..n].to_vec()))
                    .is_err()
                {
                    return;
                }
                None
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => None,
            Err(error) => Some(moli_i18n::tr!("pilotes.linky_tic.lecture", cause = error)),
        };
        if let Some(message) = failure {
            let _ = tx.blocking_send(Chunk::Failed(message));
            return;
        }
    }
}

async fn run(driver: &LinkyTic, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let id = ctx.device_id("linky");
    ctx.upsert_device(describe(ctx, &id));
    let port = driver.config.port.trim().to_owned();
    if port.is_empty() {
        ctx.set_availability(&id, false);
        ctx.wait_for(moli_i18n::tr!("pilotes.linky_tic.port_manquant"));
        ctx.cancelled().await;
        return Ok(());
    }
    let mode = driver.config.mode;
    let silence = Duration::from_secs(driver.config.silence_seconds.max(MIN_SILENCE));
    let (tx, mut rx) = mpsc::channel::<Chunk>(32);
    // Not awaited: the thread ends by itself, within a read timeout, once `rx` is gone.
    drop(tokio::task::spawn_blocking(move || {
        read_port(&port, mode, &tx);
    }));
    ctx.ready();
    ctx.set_availability(&id, false);
    let mut reader = Reader::new(mode);
    let mut online = false;
    let mut last_group = Instant::now();
    loop {
        tokio::select! {
            () = ctx.cancelled() => return Ok(()),
            () = tokio::time::sleep_until(last_group + silence) => {
                ctx.set_availability(&id, false);
                anyhow::bail!(moli_i18n::tr!(
                    "pilotes.linky_tic.silence",
                    secondes = silence.as_secs()
                ));
            }
            chunk = rx.recv() => match chunk {
                Some(Chunk::Bytes(bytes)) => {
                    for event in reader.push(&bytes) {
                        match event {
                            Event::Group(group) => {
                                last_group = Instant::now();
                                for (key, value) in readings(mode, &group) {
                                    ctx.set_state(&id, &key, value);
                                }
                                if !online {
                                    online = true;
                                    ctx.set_availability(&id, true);
                                }
                            }
                            Event::Dropped(fault) => {
                                tracing::debug!(instance = %ctx.instance(), ?fault, "TIC group dropped");
                            }
                            Event::FrameEnd => {}
                        }
                    }
                }
                Some(Chunk::Failed(message)) => {
                    ctx.set_availability(&id, false);
                    anyhow::bail!(message);
                }
                None => anyhow::bail!(moli_i18n::tr!("pilotes.linky_tic.port_ferme")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(label: &str, value: &str) -> Group {
        Group {
            label: label.into(),
            timestamp: None,
            value: value.into(),
        }
    }

    fn one(mode: Mode, label: &str, value: &str) -> Vec<(String, Value)> {
        readings(mode, &group(label, value))
    }

    fn float(n: f64) -> Value {
        Value::Float(n)
    }

    #[test]
    fn standard_values_become_points() {
        assert_eq!(
            one(Mode::Standard, "SINSTS", "00532"),
            vec![("apparent_power".into(), float(532.0))]
        );
        assert_eq!(
            one(Mode::Standard, "EAST", "027175524"),
            vec![("energy_total".into(), float(27_175_524.0))]
        );
        assert_eq!(
            one(Mode::Standard, "EASF02", "002390707"),
            vec![("energy_index_02".into(), float(2_390_707.0))]
        );
        assert_eq!(
            one(Mode::Standard, "EASF10", "000000000"),
            vec![("energy_index_10".into(), float(0.0))]
        );
        assert_eq!(
            one(Mode::Standard, "NTARF", "02"),
            vec![("tariff_index".into(), float(2.0))]
        );
        assert_eq!(
            one(Mode::Standard, "LTARF", "HEURE PLEINE "),
            vec![("tariff".into(), Value::Text("HEURE PLEINE".into()))]
        );
        assert_eq!(
            one(Mode::Standard, "IRMS1", "002"),
            vec![("current_1".into(), float(2.0))]
        );
        assert_eq!(
            one(Mode::Standard, "URMS1", "231"),
            vec![("voltage_1".into(), float(231.0))]
        );
    }

    #[test]
    fn what_the_driver_does_not_know_sets_nothing() {
        assert!(one(Mode::Standard, "ADSC", "000000000000").is_empty());
        assert!(one(Mode::Standard, "EASF00", "1").is_empty());
        assert!(one(Mode::Standard, "EASF11", "1").is_empty());
        assert!(one(Mode::Standard, "EASF1", "1").is_empty());
        assert!(one(Mode::Standard, "EASF001", "1").is_empty());
        assert!(one(Mode::Standard, "PAPP", "00532").is_empty());
    }

    #[test]
    fn a_value_that_is_not_a_number_sets_nothing() {
        assert!(one(Mode::Standard, "SINSTS", "abc").is_empty());
        assert!(one(Mode::Standard, "SINSTS", "NaN").is_empty());
        assert!(one(Mode::Standard, "SINSTS", "inf").is_empty());
        assert!(one(Mode::Standard, "LTARF", "  ").is_empty());
    }

    #[test]
    fn historic_values_become_points() {
        assert_eq!(
            one(Mode::Historic, "PAPP", "00532"),
            vec![("apparent_power".into(), float(532.0))]
        );
        assert_eq!(
            one(Mode::Historic, "HCHC", "012345678"),
            vec![("energy_index_01".into(), float(12_345_678.0))]
        );
        assert_eq!(
            one(Mode::Historic, "HCHP", "023456789"),
            vec![("energy_index_02".into(), float(23_456_789.0))]
        );
        assert_eq!(
            one(Mode::Historic, "PTEC", "HP.."),
            vec![("tariff".into(), Value::Text("HP".into()))]
        );
        assert_eq!(
            one(Mode::Historic, "IINST", "002"),
            vec![("current_1".into(), float(2.0))]
        );
        let base = one(Mode::Historic, "BASE", "012345678");
        assert_eq!(base.len(), 2);
        assert_eq!(base[0].0, "energy_total");
        assert_eq!(base[1].0, "energy_index_01");
    }

    #[test]
    fn a_label_is_read_in_its_own_mode_only() {
        assert!(one(Mode::Historic, "SINSTS", "00532").is_empty());
        assert!(one(Mode::Historic, "EAST", "027175524").is_empty());
    }

    fn replay(mode: Mode, stream: &[u8]) -> (Vec<(String, Value)>, usize, usize) {
        let mut reader = Reader::new(mode);
        let mut last: Vec<(String, Value)> = Vec::new();
        let (mut frames, mut dropped) = (0, 0);
        for event in reader.push(stream) {
            match event {
                Event::Group(group) => {
                    for (key, value) in readings(mode, &group) {
                        last.retain(|(k, _)| *k != key);
                        last.push((key, value));
                    }
                }
                Event::FrameEnd => frames += 1,
                Event::Dropped(_) => dropped += 1,
            }
        }
        (last, frames, dropped)
    }

    fn value<'a>(set: &'a [(String, Value)], key: &str) -> &'a Value {
        &set.iter().find(|(k, _)| k == key).unwrap().1
    }

    #[test]
    fn a_standard_frame_is_read_whole() {
        let stream = include_bytes!("../../../integrations/linky-tic/fixtures/frame_standard.tic");
        let (set, frames, dropped) = replay(Mode::Standard, stream);
        assert_eq!((frames, dropped), (2, 0));
        assert_eq!(*value(&set, "apparent_power"), float(532.0));
        assert_eq!(*value(&set, "energy_total"), float(1_234_567.0));
        assert_eq!(*value(&set, "energy_index_01"), float(600_000.0));
        assert_eq!(*value(&set, "energy_index_02"), float(634_567.0));
        assert_eq!(*value(&set, "tariff"), Value::Text("HEURE PLEINE".into()));
        assert_eq!(*value(&set, "tariff_index"), float(2.0));
        assert_eq!(*value(&set, "current_1"), float(2.0));
        assert_eq!(*value(&set, "voltage_1"), float(231.0));
        assert_eq!(*value(&set, "max_apparent_power"), float(3215.0));
    }

    #[test]
    fn a_historic_frame_is_read_whole() {
        let stream = include_bytes!("../../../integrations/linky-tic/fixtures/frame_historic.tic");
        let (set, frames, dropped) = replay(Mode::Historic, stream);
        assert_eq!((frames, dropped), (2, 0));
        assert_eq!(*value(&set, "apparent_power"), float(532.0));
        assert_eq!(*value(&set, "energy_index_01"), float(12_345_678.0));
        assert_eq!(*value(&set, "energy_index_02"), float(23_456_789.0));
        assert_eq!(*value(&set, "tariff"), Value::Text("HP".into()));
        assert_eq!(*value(&set, "current_1"), float(2.0));
    }

    #[test]
    fn every_point_a_frame_sets_is_declared() {
        let declared: Vec<String> = points().into_iter().map(|p| p.key.to_string()).collect();
        for (mode, stream) in [
            (
                Mode::Standard,
                &include_bytes!("../../../integrations/linky-tic/fixtures/frame_standard.tic")[..],
            ),
            (
                Mode::Historic,
                &include_bytes!("../../../integrations/linky-tic/fixtures/frame_historic.tic")[..],
            ),
        ] {
            for (key, _) in replay(mode, stream).0 {
                assert!(declared.contains(&key), "{key} is set but not declared");
            }
        }
        // Ten indexes, three currents, and no duplicate key.
        let mut sorted = declared.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), declared.len());
    }

    #[test]
    fn a_frame_with_one_flipped_bit_loses_that_group_only() {
        let mut stream =
            include_bytes!("../../../integrations/linky-tic/fixtures/frame_standard.tic").to_vec();
        // Flip a bit of the first "SINSTS" value: its checksum no longer matches.
        let at = stream.windows(7).position(|w| w == b"SINSTS	").unwrap() + 7;
        stream[at] ^= 0x01;
        let (set, frames, dropped) = replay(Mode::Standard, &stream);
        assert_eq!((frames, dropped), (2, 1));
        // The second frame carries the value again.
        assert_eq!(*value(&set, "apparent_power"), float(532.0));
    }

    #[test]
    fn the_configuration_has_defaults_and_refuses_unknown_options() {
        let config: Config = toml::from_str("port = \"/dev/ttyACM0\"").unwrap();
        assert_eq!(config.mode, Mode::Standard);
        assert_eq!(config.silence_seconds, DEFAULT_SILENCE);
        let historic: Config = toml::from_str(
            "port = \"/dev/ttyUSB0\"
mode = \"historique\"",
        )
        .unwrap();
        assert_eq!(historic.mode, Mode::Historic);
        assert!(
            toml::from_str::<Config>(
                "port = \"x\"
baud = 9600"
            )
            .is_err()
        );
        assert!(toml::from_str::<Config>("mode = \"standard\"").is_err());
    }
}

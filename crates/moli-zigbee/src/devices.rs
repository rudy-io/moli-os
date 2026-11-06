//! From ZCL to Moli's points: what an interviewed device exposes, what its
//! attributes mean, what an order sends. No IO.
//!
//! Generic, by cluster: any device that speaks the standard clusters works
//! without a line about its brand. Keys, units and ranges are Zigbee2MQTT's
//! (`moli-z2m` publishes the same), so that a house moving from one to the
//! other keeps its automations. Reporting values from zigbee-herdsman-converters
//! (MIT), `src/lib/reporting.ts`.

use std::sync::Arc;

use moli_core::color::{hex_to_xy, xy_to_hex};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_i18n::tr;

use crate::store::{ColorCaps, DeviceRecord, Ratio};
use crate::zcl::{self, Attribute, Data, Report, Request};

// Attributes.
const ON_OFF_STATE: u16 = 0x0000;
const CURRENT_LEVEL: u16 = 0x0000;
pub const CURRENT_X: u16 = 0x0003;
pub const CURRENT_Y: u16 = 0x0004;
pub const COLOR_TEMPERATURE: u16 = 0x0007;
pub const COLOR_MODE: u16 = 0x0008;
pub const COLOR_CAPABILITIES: u16 = 0x400A;
pub const COLOR_TEMP_MIN: u16 = 0x400B;
pub const COLOR_TEMP_MAX: u16 = 0x400C;
const MEASURED_VALUE: u16 = 0x0000;
const OCCUPANCY_STATE: u16 = 0x0000;
pub const ZONE_STATE: u16 = 0x0000;
pub const ZONE_TYPE: u16 = 0x0001;
pub const ZONE_STATUS: u16 = 0x0002;
pub const IAS_CIE_ADDRESS: u16 = 0x0010;
const SUMMATION_DELIVERED: u16 = 0x0000;
pub const METERING_MULTIPLIER: u16 = 0x0301;
pub const METERING_DIVISOR: u16 = 0x0302;
const INSTANTANEOUS_DEMAND: u16 = 0x0400;
const RMS_VOLTAGE: u16 = 0x0505;
const RMS_CURRENT: u16 = 0x0508;
const ACTIVE_POWER: u16 = 0x050B;
pub const VOLTAGE_MULTIPLIER: u16 = 0x0600;
pub const VOLTAGE_DIVISOR: u16 = 0x0601;
pub const CURRENT_MULTIPLIER: u16 = 0x0602;
pub const CURRENT_DIVISOR: u16 = 0x0603;
pub const POWER_MULTIPLIER: u16 = 0x0604;
pub const POWER_DIVISOR: u16 = 0x0605;
const BATTERY_VOLTAGE: u16 = 0x0020;
const BATTERY_PERCENTAGE: u16 = 0x0021;
const LOCAL_TEMPERATURE: u16 = 0x0000;
const HEATING_SETPOINT: u16 = 0x0012;
const SYSTEM_MODE: u16 = 0x001C;

/// `colorCapabilities` bits.
const CAPABILITY_XY: u64 = 0x08;
const CAPABILITY_TEMP: u64 = 0x10;
/// `colorMode`: 2 is white, 0 and 1 are a colour.
const MODE_XY: u64 = 1;
const MODE_TEMP: u64 = 2;

/// Zone status bits.
const ZONE_ALARM_1: u16 = 0x0001;
const ZONE_ALARM_2: u16 = 0x0002;
const ZONE_TAMPER: u16 = 0x0004;
const ZONE_BATTERY_LOW: u16 = 0x0008;

/// Zigbee dims from 0 to this level; people, the dashboard and agents speak
/// percent.
const LEVEL_MAX: f64 = 254.0;

/// Reports: every hour at the latest.
const HOUR: u16 = 3600;
/// herdsman-converters' `repInterval.MAX`.
const REPORT_MAX: u16 = 62000;

/// The clusters the generic devices read, in the order their points show.
const CLUSTERS: [u16; 13] = [
    zcl::ON_OFF,
    zcl::LEVEL,
    zcl::COLOR,
    zcl::THERMOSTAT,
    zcl::TEMPERATURE,
    zcl::HUMIDITY,
    zcl::PRESSURE,
    zcl::ILLUMINANCE,
    zcl::OCCUPANCY,
    zcl::IAS_ZONE,
    zcl::METERING,
    zcl::ELECTRICAL,
    zcl::POWER_CONFIG,
];

/// `system_mode`'s values, in ZCL's numbers (`SystemMode`).
const SYSTEM_MODES: [(&str, u8); 4] = [("off", 0), ("auto", 1), ("cool", 3), ("heat", 4)];

/// A supported cluster on one endpoint. The first endpoint with a cluster has
/// plain keys (`state`); the others say theirs (`state_2`).
struct Channel {
    endpoint: u8,
    cluster: u16,
    suffix: String,
}

/// The supported clusters of a device. Batteries only for what has one: a
/// mains device's Power Configuration says nothing useful.
fn channels(record: &DeviceRecord) -> Vec<Channel> {
    let mut found: Vec<Channel> = Vec::new();
    for endpoint in &record.endpoints {
        if !matches!(endpoint.profile, zcl::HA_PROFILE | zcl::ZLL_PROFILE) {
            continue;
        }
        for cluster in CLUSTERS {
            if !endpoint.in_clusters.contains(&cluster)
                || (cluster == zcl::POWER_CONFIG && record.mains())
            {
                continue;
            }
            let suffix = if found.iter().any(|c| c.cluster == cluster) {
                format!("_{}", endpoint.endpoint)
            } else {
                String::new()
            };
            found.push(Channel {
                endpoint: endpoint.endpoint,
                cluster,
                suffix,
            });
        }
    }
    found
}

/// A plug's Electrical Measurement says its power, more often and better
/// than its Metering: one source for the point, whatever they round to.
fn power_is_electrical(record: &DeviceRecord) -> bool {
    record.scale.power.is_some()
        && channels(record)
            .iter()
            .any(|channel| channel.cluster == zcl::ELECTRICAL)
}

fn suffix_of(record: &DeviceRecord, endpoint: u8, cluster: u16) -> Option<String> {
    channels(record)
        .into_iter()
        .find(|c| c.endpoint == endpoint && c.cluster == cluster)
        .map(|c| c.suffix)
}

/// What the device shows before its interview: it is there, nothing more.
#[must_use]
pub fn pending_device(instance: &InstanceId, id: &DeviceId, record: &DeviceRecord) -> Device {
    Device {
        id: id.clone(),
        instance: instance.clone(),
        native_name: record.ieee.as_str().into(),
        manufacturer: None,
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![link_quality()],
    }
}

/// The Moli device an interviewed record makes.
#[must_use]
pub fn device(instance: &InstanceId, id: &DeviceId, record: &DeviceRecord) -> Device {
    let mut points: Vec<PointSpec> = Vec::new();
    for channel in channels(record) {
        for point in points_of(record, &channel) {
            // The first says it: a plug's Metering and Electrical both know
            // `power`.
            if !points.iter().any(|known| known.key == point.key) {
                points.push(point);
            }
        }
    }
    points.push(link_quality());
    let name: Arc<str> = record
        .model
        .as_deref()
        .unwrap_or(record.ieee.as_str())
        .into();
    Device {
        id: id.clone(),
        instance: instance.clone(),
        native_name: name,
        manufacturer: record.manufacturer.as_deref().map(Into::into),
        model: record.model.as_deref().map(Into::into),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

fn label(base: &str) -> String {
    match base {
        "state" => tr!("pilotes.zigbee.point.state"),
        "brightness" => tr!("pilotes.zigbee.point.brightness"),
        "color_temp" => tr!("pilotes.zigbee.point.color_temp"),
        "color" => tr!("pilotes.zigbee.point.color"),
        "color.x" => tr!("pilotes.zigbee.point.color_x"),
        "color.y" => tr!("pilotes.zigbee.point.color_y"),
        "temperature" => tr!("pilotes.zigbee.point.temperature"),
        "humidity" => tr!("pilotes.zigbee.point.humidity"),
        "pressure" => tr!("pilotes.zigbee.point.pressure"),
        "illuminance" => tr!("pilotes.zigbee.point.illuminance"),
        "occupancy" => tr!("pilotes.zigbee.point.occupancy"),
        "contact" => tr!("pilotes.zigbee.point.contact"),
        "water_leak" => tr!("pilotes.zigbee.point.water_leak"),
        "smoke" => tr!("pilotes.zigbee.point.smoke"),
        "carbon_monoxide" => tr!("pilotes.zigbee.point.carbon_monoxide"),
        "vibration" => tr!("pilotes.zigbee.point.vibration"),
        "tamper" => tr!("pilotes.zigbee.point.tamper"),
        "battery_low" => tr!("pilotes.zigbee.point.battery_low"),
        "battery" => tr!("pilotes.zigbee.point.battery"),
        "voltage" => tr!("pilotes.zigbee.point.voltage"),
        "power" => tr!("pilotes.zigbee.point.power"),
        "energy" => tr!("pilotes.zigbee.point.energy"),
        "current" => tr!("pilotes.zigbee.point.current"),
        "linkquality" => tr!("pilotes.zigbee.point.linkquality"),
        "local_temperature" => tr!("pilotes.zigbee.point.local_temperature"),
        "occupied_heating_setpoint" => tr!("pilotes.zigbee.point.heating_setpoint"),
        "system_mode" => tr!("pilotes.zigbee.point.system_mode"),
        _ => tr!("pilotes.zigbee.point.alarm"),
    }
}

/// The key of `base` on a channel: `state_2`, `color_2.x`.
fn key_of(base: &str, suffix: &str) -> String {
    match base.split_once('.') {
        Some((group, member)) => format!("{group}{suffix}.{member}"),
        None => format!("{base}{suffix}"),
    }
}

fn point(base: &str, suffix: &str, kind: Kind, write: bool, unit: Option<Unit>) -> PointSpec {
    let key = key_of(base, suffix);
    let semantic = Semantic::infer(&key, unit.as_ref());
    PointSpec {
        key: key.into(),
        label: label(base).into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

fn reading() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

fn link_quality() -> PointSpec {
    point(
        "linkquality",
        "",
        Kind::Numeric {
            min: Some(0.0),
            max: Some(255.0),
            step: Some(1.0),
        },
        false,
        Unit::parse("lqi"),
    )
}

/// The white a lamp says it can reach, within what lamps really do: some
/// declare far more than they can (an Innr RB 287 C says 50 to 1000 mired for
/// 153 to 556), and clamp by themselves. Zigbee2MQTT's generic range for a
/// lamp that says nothing.
fn temp_range(caps: ColorCaps) -> (f64, f64) {
    const COLDEST: u16 = 150;
    const WARMEST: u16 = 600;
    let min = caps.temp_min.unwrap_or(COLDEST).clamp(COLDEST, WARMEST - 1);
    let max = caps
        .temp_max
        .filter(|&max| max > min)
        .unwrap_or(500.max(min + 1))
        .min(WARMEST);
    (f64::from(min), f64::from(max))
}

/// What a lamp can do. One whose capabilities could not be read is taken for
/// a colour one: the orders it does not understand answer with an error.
fn caps_of(record: &DeviceRecord) -> ColorCaps {
    record.color.unwrap_or(ColorCaps {
        xy: true,
        temp: true,
        ..ColorCaps::default()
    })
}

/// A lamp's `colorCapabilities` and the physical range of its white.
#[must_use]
pub fn color_caps(capabilities: Option<u64>, min: Option<u16>, max: Option<u16>) -> ColorCaps {
    // A lamp that does not say: both, as above.
    let bits = capabilities.unwrap_or(CAPABILITY_XY | CAPABILITY_TEMP);
    ColorCaps {
        xy: bits & CAPABILITY_XY != 0,
        temp: bits & CAPABILITY_TEMP != 0,
        temp_min: min,
        temp_max: max,
    }
}

fn color_points(record: &DeviceRecord, s: &str) -> Vec<PointSpec> {
    let caps = caps_of(record);
    let mut points = Vec::new();
    if caps.temp {
        let (min, max) = temp_range(caps);
        points.push(point(
            "color_temp",
            s,
            Kind::Numeric {
                min: Some(min),
                max: Some(max),
                step: Some(1.0),
            },
            true,
            Unit::parse("mired"),
        ));
    }
    if caps.xy {
        points.push(point("color", s, Kind::Text, true, None));
        let share = Kind::Numeric {
            min: Some(0.0),
            max: Some(1.0),
            step: None,
        };
        points.push(point("color.x", s, share.clone(), false, None));
        points.push(point("color.y", s, share, false, None));
    }
    points
}

fn points_of(record: &DeviceRecord, channel: &Channel) -> Vec<PointSpec> {
    let s = channel.suffix.as_str();
    let measure = |base: &str, unit: Unit| point(base, s, reading(), false, Some(unit));
    let flag = |base: &str| point(base, s, Kind::Binary, false, None);
    match channel.cluster {
        zcl::ON_OFF => vec![point("state", s, Kind::Binary, true, None)],
        zcl::LEVEL => vec![point(
            "brightness",
            s,
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0),
            },
            true,
            Some(Unit::Percent),
        )],
        zcl::COLOR => color_points(record, s),
        zcl::TEMPERATURE => vec![measure("temperature", Unit::Celsius)],
        zcl::HUMIDITY => vec![measure("humidity", Unit::Percent)],
        zcl::PRESSURE => vec![measure("pressure", Unit::HectoPascal)],
        zcl::ILLUMINANCE => vec![measure("illuminance", Unit::Lux)],
        zcl::OCCUPANCY => vec![flag("occupancy")],
        zcl::IAS_ZONE => vec![
            flag(zone_key(record.zone_type)),
            flag("tamper"),
            flag("battery_low"),
        ],
        zcl::METERING => vec![
            measure("power", Unit::Watt),
            measure("energy", Unit::KiloWattHour),
        ],
        zcl::ELECTRICAL => vec![
            measure("power", Unit::Watt),
            measure("voltage", Unit::Volt),
            measure("current", Unit::Ampere),
        ],
        zcl::POWER_CONFIG => vec![
            point(
                "battery",
                s,
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                false,
                Some(Unit::Percent),
            ),
            measure("voltage", Unit::MilliVolt),
        ],
        zcl::THERMOSTAT => vec![
            measure("local_temperature", Unit::Celsius),
            point(
                "occupied_heating_setpoint",
                s,
                Kind::Numeric {
                    min: Some(5.0),
                    max: Some(30.0),
                    step: Some(0.5),
                },
                true,
                Some(Unit::Celsius),
            ),
            point(
                "system_mode",
                s,
                Kind::Enum {
                    values: SYSTEM_MODES.iter().map(|&(name, _)| name.into()).collect(),
                },
                true,
                None,
            ),
        ],
        _ => Vec::new(),
    }
}

/// The key of a sensor's alarm, by `zoneType`.
fn zone_key(zone_type: Option<u16>) -> &'static str {
    match zone_type {
        Some(0x000D) => "occupancy",
        Some(0x0015) => "contact",
        Some(0x0028) => "smoke",
        Some(0x002A) => "water_leak",
        Some(0x002B) => "carbon_monoxide",
        Some(0x002D) => "vibration",
        _ => "alarm",
    }
}

/// A lamp's colour as it reports it, attribute by attribute.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorState {
    x: Option<f64>,
    y: Option<f64>,
    mode: Option<u64>,
}

impl ColorState {
    /// The lamp was told a colour or a white: until it says otherwise, what
    /// it reports of the other is not what it shows (a lamp in colour still
    /// reports a white, at the end of its range).
    pub fn set_mode(&mut self, mode: u64) {
        self.mode = Some(mode);
    }
}

fn round(value: f64, decimals: i32) -> f64 {
    let scale = 10f64.powi(decimals);
    (value * scale).round() / scale
}

/// A whole number, for a value already within `i64`.
#[allow(clippy::cast_possible_truncation)]
fn whole(value: f64) -> i64 {
    value.round() as i64
}

/// A Zigbee dimming level as a whole percent; a lit level is never 0 %.
fn level_to_percent(level: f64) -> i64 {
    let percent = whole(level * 100.0 / LEVEL_MAX).clamp(0, 100);
    if level > 0.0 { percent.max(1) } else { percent }
}

/// A percent as a Zigbee dimming level; a lit percent is never level 0.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent_to_level(percent: f64) -> u8 {
    let level = (percent.clamp(0.0, 100.0) * LEVEL_MAX / 100.0).round() as u8;
    if percent > 0.0 { level.max(1) } else { level }
}

fn ratio(raw: f64, scale: Option<Ratio>) -> f64 {
    scale.map_or(raw, |ratio| ratio.apply(raw))
}

/// What an attribute a device sent means: the values to set. Nothing for an
/// attribute no point reads, or for ZCL's « no value ».
#[must_use]
pub fn decode(
    record: &DeviceRecord,
    color: &mut ColorState,
    endpoint: u8,
    cluster: u16,
    attribute: &Attribute,
) -> Vec<(String, Value)> {
    let Some(suffix) = suffix_of(record, endpoint, cluster) else {
        return Vec::new();
    };
    let s = suffix.as_str();
    let data = &attribute.value;
    let number = data.as_f64();
    let one = |base: &str, value: Value| vec![(key_of(base, s), value)];
    let flag = |base: &str, on: bool| one(base, Value::Bool(on));
    let float =
        |base: &str, value: f64, decimals: i32| one(base, Value::Float(round(value, decimals)));
    let (Some(value), true) = (
        number.or(data.as_uint().map(to_f64)),
        *data != Data::Invalid,
    ) else {
        return Vec::new();
    };
    match (cluster, attribute.id) {
        (zcl::ON_OFF, ON_OFF_STATE) => flag("state", value != 0.0),
        (zcl::LEVEL, CURRENT_LEVEL) => one("brightness", Value::Int(level_to_percent(value))),
        (zcl::COLOR, CURRENT_X | CURRENT_Y | COLOR_TEMPERATURE | COLOR_MODE) => {
            decode_color(color, attribute.id, value, s)
        }
        (zcl::TEMPERATURE, MEASURED_VALUE) => float("temperature", value / 100.0, 2),
        (zcl::HUMIDITY, MEASURED_VALUE) => float("humidity", value / 100.0, 2),
        (zcl::PRESSURE, MEASURED_VALUE) => float("pressure", value, 1),
        (zcl::ILLUMINANCE, MEASURED_VALUE) => {
            let lux = if value > 0.0 {
                10f64.powf((value - 1.0) / 10_000.0)
            } else {
                0.0
            };
            one("illuminance", Value::Int(whole(lux)))
        }
        (zcl::OCCUPANCY, OCCUPANCY_STATE) => flag("occupancy", whole(value) & 1 != 0),
        (zcl::IAS_ZONE, ZONE_STATUS) => u16::try_from(whole(value))
            .map(|status| zone_values(record.zone_type, s, status))
            .unwrap_or_default(),
        (zcl::METERING, SUMMATION_DELIVERED) => {
            float("energy", ratio(value, record.scale.metering), 3)
        }
        (zcl::METERING, INSTANTANEOUS_DEMAND) if power_is_electrical(record) => Vec::new(),
        // A rate in kW once scaled, as Zigbee2MQTT reads it.
        (zcl::METERING, INSTANTANEOUS_DEMAND) => {
            let watts = record
                .scale
                .metering
                .map_or(value, |scale| scale.apply(value) * 1000.0);
            float("power", watts, 2)
        }
        (zcl::ELECTRICAL, RMS_VOLTAGE) => float("voltage", ratio(value, record.scale.voltage), 1),
        (zcl::ELECTRICAL, RMS_CURRENT) => float("current", ratio(value, record.scale.current), 3),
        (zcl::ELECTRICAL, ACTIVE_POWER) => float("power", ratio(value, record.scale.power), 2),
        (zcl::POWER_CONFIG, BATTERY_VOLTAGE) => one("voltage", Value::Int(whole(value * 100.0))),
        (zcl::POWER_CONFIG, BATTERY_PERCENTAGE) => {
            one("battery", Value::Int(whole(value / 2.0).clamp(0, 100)))
        }
        (zcl::THERMOSTAT, LOCAL_TEMPERATURE) => float("local_temperature", value / 100.0, 2),
        (zcl::THERMOSTAT, HEATING_SETPOINT) => float("occupied_heating_setpoint", value / 100.0, 2),
        (zcl::THERMOSTAT, SYSTEM_MODE) => SYSTEM_MODES
            .iter()
            .find(|&&(_, id)| (f64::from(id) - value).abs() < 0.5)
            .map(|&(name, _)| one("system_mode", Value::Text(name.into())))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[allow(clippy::cast_precision_loss)] // ZCL readings stay far below 2^52.
fn to_f64(value: u64) -> f64 {
    value as f64
}

/// Colour attributes: `color.x`, `color.y`, and `color` once both are known;
/// `color_temp` only when the lamp is in white (`Null` otherwise: a colour
/// has no temperature).
fn decode_color(color: &mut ColorState, id: u16, value: f64, s: &str) -> Vec<(String, Value)> {
    let share = round(value / 65535.0, 4);
    let mut values = Vec::new();
    match id {
        CURRENT_X => {
            color.x = Some(share);
            values.push((key_of("color.x", s), Value::Float(share)));
        }
        CURRENT_Y => {
            color.y = Some(share);
            values.push((key_of("color.y", s), Value::Float(share)));
        }
        COLOR_TEMPERATURE => {
            let in_white = color.mode.is_none_or(|mode| mode == MODE_TEMP);
            let temperature = if in_white {
                Value::Int(whole(value))
            } else {
                Value::Null
            };
            values.push((key_of("color_temp", s), temperature));
            return values;
        }
        _ => {
            let mode = u64::try_from(whole(value)).unwrap_or(MODE_TEMP);
            color.mode = Some(mode);
            if mode != MODE_TEMP {
                values.push((key_of("color_temp", s), Value::Null));
            }
            return values;
        }
    }
    if let (Some(x), Some(y)) = (color.x, color.y)
        && let Some(hex) = xy_to_hex(x, y)
    {
        values.push((key_of("color", s), Value::Text(hex.into())));
    }
    values
}

fn zone_values(zone_type: Option<u16>, s: &str, status: u16) -> Vec<(String, Value)> {
    let alarm = status & ZONE_ALARM_1 != 0;
    let base = zone_key(zone_type);
    let main = match base {
        // True is closed, as Moli and `moli-z2m` have it.
        "contact" => !alarm,
        // Sensors raise either alarm.
        "alarm" | "occupancy" | "vibration" => alarm || status & ZONE_ALARM_2 != 0,
        _ => alarm,
    };
    vec![
        (key_of(base, s), Value::Bool(main)),
        (key_of("tamper", s), Value::Bool(status & ZONE_TAMPER != 0)),
        (
            key_of("battery_low", s),
            Value::Bool(status & ZONE_BATTERY_LOW != 0),
        ),
    ]
}

/// What an IAS zone status (a notification, or the attribute) says.
#[must_use]
pub fn zone_points(record: &DeviceRecord, endpoint: u8, status: u16) -> Vec<(String, Value)> {
    suffix_of(record, endpoint, zcl::IAS_ZONE)
        .map(|s| zone_values(record.zone_type, &s, status))
        .unwrap_or_default()
}

/// Attributes to read on one cluster of one endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadPlan {
    pub endpoint: u8,
    pub cluster: u16,
    pub attributes: Vec<u16>,
}

/// What to read for a device's current values. On a lamp, its colour mode
/// comes first: it decides what its white temperature means.
#[must_use]
pub fn reads(record: &DeviceRecord) -> Vec<ReadPlan> {
    channels(record)
        .into_iter()
        .filter_map(|channel| {
            let attributes = match channel.cluster {
                zcl::ON_OFF => vec![ON_OFF_STATE],
                zcl::LEVEL => vec![CURRENT_LEVEL],
                zcl::COLOR => {
                    let caps = caps_of(record);
                    let mut wanted = vec![COLOR_MODE];
                    if caps.xy {
                        wanted.extend([CURRENT_X, CURRENT_Y]);
                    }
                    if caps.temp {
                        wanted.push(COLOR_TEMPERATURE);
                    }
                    wanted
                }
                zcl::TEMPERATURE | zcl::HUMIDITY | zcl::PRESSURE | zcl::ILLUMINANCE => {
                    vec![MEASURED_VALUE]
                }
                zcl::OCCUPANCY => vec![OCCUPANCY_STATE],
                zcl::IAS_ZONE => vec![ZONE_STATUS],
                zcl::METERING if power_is_electrical(record) => vec![SUMMATION_DELIVERED],
                zcl::METERING => vec![SUMMATION_DELIVERED, INSTANTANEOUS_DEMAND],
                zcl::ELECTRICAL => vec![RMS_VOLTAGE, RMS_CURRENT, ACTIVE_POWER],
                zcl::POWER_CONFIG => vec![BATTERY_VOLTAGE, BATTERY_PERCENTAGE],
                zcl::THERMOSTAT => vec![LOCAL_TEMPERATURE, HEATING_SETPOINT, SYSTEM_MODE],
                _ => return None,
            };
            Some(ReadPlan {
                endpoint: channel.endpoint,
                cluster: channel.cluster,
                attributes,
            })
        })
        .collect()
}

/// What to read to know a listening device is still there: its first value,
/// or `Basic`'s `zclVersion` for one with none (a repeater), as herdsman.
#[must_use]
pub fn ping(record: &DeviceRecord) -> Option<ReadPlan> {
    reads(record).into_iter().next().or_else(|| {
        record
            .endpoints
            .iter()
            .find(|endpoint| endpoint.in_clusters.contains(&zcl::BASIC))
            .map(|endpoint| ReadPlan {
                endpoint: endpoint.endpoint,
                cluster: zcl::BASIC,
                attributes: vec![0x0000],
            })
    })
}

/// A cluster to bind to the coordinator, with what it should report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub endpoint: u8,
    pub cluster: u16,
    pub reports: Vec<Report>,
}

fn report(attribute: u16, data_type: u8, min: u16, max: u16, change: Option<u64>) -> Report {
    Report {
        attribute,
        data_type,
        min,
        max,
        change,
    }
}

/// What to bind and report. IAS Zone is not here: a sensor enrolled at its
/// interview notifies by itself.
#[must_use]
pub fn bindings(record: &DeviceRecord) -> Vec<Binding> {
    channels(record)
        .into_iter()
        .filter_map(|channel| {
            let reports = reports_of(record, channel.cluster)?;
            Some(Binding {
                endpoint: channel.endpoint,
                cluster: channel.cluster,
                reports,
            })
        })
        .collect()
}

/// A report's threshold in a device's raw values: `amount` in the unit its
/// ratio leads to (herdsman-converters' `genericMeter`: a plug that reports
/// every 0.1 V would never be quiet), or `raw` for a device with no ratio;
/// at most `limit`, what its data type holds.
#[allow(clippy::cast_sign_loss)] // Clamped above zero.
fn change_of(amount: f64, scale: Option<Ratio>, raw: u64, limit: i64) -> u64 {
    scale.map_or(raw, |ratio| {
        let raw = amount * f64::from(ratio.divisor.max(1)) / f64::from(ratio.multiplier.max(1));
        whole(raw).clamp(1, limit) as u64
    })
}

fn reports_of(record: &DeviceRecord, cluster: u16) -> Option<Vec<Report>> {
    let measured =
        |data_type, change| vec![report(MEASURED_VALUE, data_type, 10, HOUR, Some(change))];
    Some(match cluster {
        zcl::ON_OFF => vec![report(ON_OFF_STATE, zcl::BOOLEAN, 0, HOUR, None)],
        zcl::LEVEL => vec![report(CURRENT_LEVEL, zcl::UINT8, 1, HOUR, Some(1))],
        zcl::COLOR => {
            let caps = caps_of(record);
            let mut reports = Vec::new();
            if caps.temp {
                reports.push(report(COLOR_TEMPERATURE, zcl::UINT16, 0, HOUR, Some(1)));
            }
            if caps.xy {
                // 0.5 % of the diagram: a colour, not the lamp's noise.
                reports.push(report(CURRENT_X, zcl::UINT16, 1, HOUR, Some(328)));
                reports.push(report(CURRENT_Y, zcl::UINT16, 1, HOUR, Some(328)));
            }
            // Last, and not every lamp can: a remote may change it, and what
            // the white means follows. Without it, orders and reads say it.
            reports.push(report(COLOR_MODE, zcl::ENUM8, 0, HOUR, None));
            reports
        }
        zcl::TEMPERATURE => measured(zcl::INT16, 100),
        zcl::HUMIDITY => measured(zcl::UINT16, 100),
        zcl::PRESSURE => measured(zcl::INT16, 5),
        zcl::ILLUMINANCE => measured(zcl::UINT16, 5),
        zcl::OCCUPANCY => vec![report(OCCUPANCY_STATE, zcl::BITMAP8, 0, HOUR, None)],
        zcl::METERING => {
            let scale = record.scale.metering;
            let energy = Some(change_of(0.1, scale, 257, 0xFFFF_FFFF));
            let mut reports = vec![report(SUMMATION_DELIVERED, zcl::UINT48, 10, HOUR, energy)];
            if !power_is_electrical(record) {
                // 5 W, in the kW its ratio leads to.
                let power = Some(change_of(0.005, scale, 5, 0x7F_FFFF));
                reports.push(report(INSTANTANEOUS_DEMAND, zcl::INT24, 10, HOUR, power));
            }
            reports
        }
        zcl::ELECTRICAL => {
            let scale = record.scale;
            vec![
                report(
                    ACTIVE_POWER,
                    zcl::INT16,
                    10,
                    HOUR,
                    Some(change_of(5.0, scale.power, 5, 0x7FFF)),
                ),
                report(
                    RMS_VOLTAGE,
                    zcl::UINT16,
                    10,
                    HOUR,
                    Some(change_of(5.0, scale.voltage, 5, 0xFFFF)),
                ),
                report(
                    RMS_CURRENT,
                    zcl::UINT16,
                    10,
                    HOUR,
                    Some(change_of(0.05, scale.current, 5, 0xFFFF)),
                ),
            ]
        }
        zcl::POWER_CONFIG => vec![
            report(BATTERY_VOLTAGE, zcl::UINT8, HOUR, REPORT_MAX, Some(0)),
            report(BATTERY_PERCENTAGE, zcl::UINT8, HOUR, REPORT_MAX, Some(0)),
        ],
        zcl::THERMOSTAT => vec![
            report(LOCAL_TEMPERATURE, zcl::INT16, 0, HOUR, Some(10)),
            report(HEATING_SETPOINT, zcl::INT16, 0, HOUR, Some(10)),
            report(SYSTEM_MODE, zcl::ENUM8, 10, HOUR, None),
        ],
        _ => return None,
    })
}

/// Why an order cannot be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No such point on this device, or not one that takes orders.
    UnknownPoint,
    /// A value the device cannot be given (black is no colour).
    BadValue,
}

/// An order, ready to send.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub endpoint: u8,
    pub cluster: u16,
    pub request: Request,
    /// The values to show once the device took it.
    pub effects: Vec<(String, Value)>,
    /// The `colorMode` it puts a lamp in.
    pub color_mode: Option<u64>,
}

/// What a write to `key` sends. Instantaneous, as Zigbee2MQTT by default.
///
/// # Errors
///
/// The point takes no orders, or the value cannot be sent.
pub fn command(record: &DeviceRecord, key: &str, value: &Value) -> Result<Command, Refused> {
    for channel in channels(record) {
        let Some(base) = key.strip_suffix(channel.suffix.as_str()) else {
            continue;
        };
        let request_of = match (channel.cluster, base) {
            (zcl::ON_OFF, "state") => on_off_command,
            (zcl::LEVEL, "brightness") => brightness_command,
            (zcl::COLOR, "color_temp") if caps_of(record).temp => color_temp_command,
            (zcl::COLOR, "color") if caps_of(record).xy => color_command,
            (zcl::THERMOSTAT, "occupied_heating_setpoint") => setpoint_command,
            (zcl::THERMOSTAT, "system_mode") => system_mode_command,
            _ => continue,
        };
        let (request, effects) = request_of(&channel.suffix, value)?;
        let color_mode = match base {
            "color" => Some(MODE_XY),
            "color_temp" => Some(MODE_TEMP),
            _ => None,
        };
        return Ok(Command {
            endpoint: channel.endpoint,
            cluster: channel.cluster,
            request,
            effects,
            color_mode,
        });
    }
    Err(Refused::UnknownPoint)
}

type Built = Result<(Request, Vec<(String, Value)>), Refused>;

fn on_off_command(s: &str, value: &Value) -> Built {
    let Value::Bool(on) = *value else {
        return Err(Refused::BadValue);
    };
    Ok((zcl::on_off(on), vec![(key_of("state", s), Value::Bool(on))]))
}

fn brightness_command(s: &str, value: &Value) -> Built {
    let percent = value.as_f64().ok_or(Refused::BadValue)?;
    Ok((
        zcl::move_to_level_with_on_off(percent_to_level(percent), 0),
        vec![
            (key_of("brightness", s), Value::Int(whole(percent))),
            (key_of("state", s), Value::Bool(percent > 0.0)),
        ],
    ))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn color_temp_command(s: &str, value: &Value) -> Built {
    let mired = value.as_f64().ok_or(Refused::BadValue)?;
    Ok((
        zcl::move_to_color_temp(mired.round().clamp(1.0, 65279.0) as u16, 0),
        vec![(key_of("color_temp", s), Value::Int(whole(mired)))],
    ))
}

/// A share of the diagram as `moveToColor` takes it (at most 0xFEFF).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn share(value: f64) -> u16 {
    (value * 65535.0).round().clamp(0.0, 65279.0) as u16
}

fn color_command(s: &str, value: &Value) -> Built {
    let Value::Text(hex) = value else {
        return Err(Refused::BadValue);
    };
    let (x, y) = hex_to_xy(hex).ok_or(Refused::BadValue)?;
    Ok((
        zcl::move_to_color(share(x), share(y), 0),
        vec![
            (key_of("color", s), value.clone()),
            (key_of("color.x", s), Value::Float(x)),
            (key_of("color.y", s), Value::Float(y)),
            (key_of("color_temp", s), Value::Null),
        ],
    ))
}

#[allow(clippy::cast_possible_truncation)]
fn setpoint_command(s: &str, value: &Value) -> Built {
    let celsius = value.as_f64().ok_or(Refused::BadValue)?;
    let hundredths = (celsius * 100.0).round() as i16;
    Ok((
        zcl::write_attribute(HEATING_SETPOINT, zcl::INT16, &hundredths.to_le_bytes()),
        vec![(
            key_of("occupied_heating_setpoint", s),
            Value::Float(celsius),
        )],
    ))
}

fn system_mode_command(s: &str, value: &Value) -> Built {
    let Value::Text(name) = value else {
        return Err(Refused::BadValue);
    };
    let &(_, id) = SYSTEM_MODES
        .iter()
        .find(|&&(known, _)| known == &**name)
        .ok_or(Refused::BadValue)?;
    Ok((
        zcl::write_attribute(SYSTEM_MODE, zcl::ENUM8, &[id]),
        vec![(key_of("system_mode", s), value.clone())],
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::store::{EndpointRecord, Scale};

    pub(crate) const LAMP_IEEE: &str = "0x00124b0000000002";

    fn endpoint(id: u8, clusters: &[u16]) -> EndpointRecord {
        EndpointRecord {
            endpoint: id,
            profile: zcl::HA_PROFILE,
            device_id: 0x0100,
            in_clusters: clusters.to_vec(),
            out_clusters: Vec::new(),
        }
    }

    /// A mains colour lamp, as an interview finds it.
    pub(crate) fn lamp() -> DeviceRecord {
        DeviceRecord {
            ieee: LAMP_IEEE.into(),
            nwk: Some(0x1234),
            interviewed: true,
            manufacturer: Some("Some maker".into()),
            model: Some("Some lamp".into()),
            logical_type: Some(1),
            capability: Some(0x8E),
            endpoints: vec![endpoint(
                1,
                &[zcl::BASIC, zcl::ON_OFF, zcl::LEVEL, zcl::COLOR],
            )],
            color: Some(ColorCaps {
                xy: true,
                temp: true,
                temp_min: Some(153),
                temp_max: Some(556),
            }),
            ..DeviceRecord::default()
        }
    }

    fn sensor(clusters: &[u16], zone_type: Option<u16>) -> DeviceRecord {
        DeviceRecord {
            ieee: "0x00124b0000000003".into(),
            interviewed: true,
            logical_type: Some(2),
            capability: Some(0x80),
            endpoints: vec![endpoint(1, clusters)],
            zone_type,
            ..DeviceRecord::default()
        }
    }

    fn published(record: &DeviceRecord) -> Device {
        let instance = InstanceId::from("zigbee");
        let id = DeviceId::new(&instance, &record.ieee);
        device(&instance, &id, record)
    }

    fn keys(device: &Device) -> Vec<&str> {
        device.points.iter().map(|p| &*p.key).collect()
    }

    /// The values one attribute gives on endpoint 1.
    fn read(record: &DeviceRecord, cluster: u16, id: u16, value: Data) -> HashMap<String, Value> {
        decode(
            record,
            &mut ColorState::default(),
            1,
            cluster,
            &Attribute { id, value },
        )
        .into_iter()
        .collect()
    }

    #[test]
    fn a_colour_lamp() {
        let device = published(&lamp());
        assert_eq!(
            keys(&device),
            [
                "state",
                "brightness",
                "color_temp",
                "color",
                "color.x",
                "color.y",
                "linkquality"
            ]
        );
        assert_eq!(&*device.native_name, "Some lamp");
        assert_eq!(device.manufacturer.as_deref(), Some("Some maker"));
        let state = device.point("state").unwrap();
        assert_eq!(
            (state.semantic, state.access.write),
            (Semantic::OnOff, true)
        );
        let brightness = device.point("brightness").unwrap();
        assert_eq!(brightness.unit, Some(Unit::Percent));
        assert_eq!(brightness.semantic, Semantic::Brightness);
        let white = device.point("color_temp").unwrap();
        assert_eq!(white.semantic, Semantic::ColorTemp);
        assert_eq!(
            white.kind,
            Kind::Numeric {
                min: Some(153.0),
                max: Some(556.0),
                step: Some(1.0),
            }
        );
        let color = device.point("color").unwrap();
        assert_eq!(
            (color.semantic, color.access.write),
            (Semantic::Color, true)
        );
        assert!(!device.point("color.x").unwrap().access.write);
        assert_eq!(
            device.point("linkquality").unwrap().semantic,
            Semantic::SignalStrength
        );
    }

    #[test]
    fn a_white_lamp_has_no_colour_point() {
        let mut white = lamp();
        white.color = Some(ColorCaps {
            xy: false,
            temp: true,
            ..ColorCaps::default()
        });
        let device = published(&white);
        assert!(device.point("color").is_none());
        assert!(device.point("color.x").is_none());
        assert_eq!(
            device.point("color_temp").unwrap().kind,
            Kind::Numeric {
                min: Some(150.0),
                max: Some(500.0),
                step: Some(1.0),
            },
            "no declared range: the widest"
        );
        assert_eq!(
            command(&white, "color", &Value::from("#ff0000")).map(|_| ()),
            Err(Refused::UnknownPoint)
        );
    }

    #[test]
    fn a_white_range_stays_within_what_lamps_do() {
        let range = |temp_min, temp_max| {
            temp_range(ColorCaps {
                temp_min,
                temp_max,
                ..ColorCaps::default()
            })
        };
        assert_eq!(range(Some(153), Some(556)), (153.0, 556.0));
        // What an Innr RB 287 C really declares.
        assert_eq!(range(Some(50), Some(1000)), (150.0, 600.0));
        assert_eq!(range(None, None), (150.0, 500.0));
        assert_eq!(range(Some(0), Some(0)), (150.0, 500.0));
        assert_eq!(range(Some(0xFFFF), Some(0xFFFF)), (599.0, 600.0));
        assert_eq!(range(Some(550), None), (550.0, 551.0), "never upside down");
    }

    #[test]
    fn capabilities_decide() {
        // Colour and white, its range.
        let both = color_caps(Some(0x1F), Some(153), Some(556));
        assert!(both.xy && both.temp);
        assert_eq!((both.temp_min, both.temp_max), (Some(153), Some(556)));
        // White only.
        let white = color_caps(Some(0x10), None, None);
        assert!(!white.xy && white.temp);
        // Hue and saturation only: no colour point in this version.
        let hue = color_caps(Some(0x01), None, None);
        assert!(!hue.xy && !hue.temp);
        // Not said: both.
        let unknown = color_caps(None, None, None);
        assert!(unknown.xy && unknown.temp);
    }

    #[test]
    fn sensors_by_zone_type() {
        let contact = sensor(&[zcl::IAS_ZONE, zcl::POWER_CONFIG], Some(0x0015));
        assert_eq!(
            keys(&published(&contact)),
            [
                "contact",
                "tamper",
                "battery_low",
                "battery",
                "voltage",
                "linkquality"
            ]
        );
        // True is closed.
        let values = read(&contact, zcl::IAS_ZONE, ZONE_STATUS, Data::Uint(0x0000));
        assert_eq!(values["contact"], Value::Bool(true));
        let values: HashMap<_, _> = zone_points(&contact, 1, 0x000D).into_iter().collect();
        assert_eq!(values["contact"], Value::Bool(false));
        assert_eq!(values["tamper"], Value::Bool(true));
        assert_eq!(values["battery_low"], Value::Bool(true));

        let leak = sensor(&[zcl::IAS_ZONE], Some(0x002A));
        let values: HashMap<_, _> = zone_points(&leak, 1, 0x0001).into_iter().collect();
        assert_eq!(values["water_leak"], Value::Bool(true));
        let smoke = sensor(&[zcl::IAS_ZONE], Some(0x0028));
        assert!(published(&smoke).point("smoke").is_some());
        let motion = sensor(&[zcl::IAS_ZONE], Some(0x000D));
        let values: HashMap<_, _> = zone_points(&motion, 1, 0x0002).into_iter().collect();
        assert_eq!(values["occupancy"], Value::Bool(true), "either alarm");
        let unknown = sensor(&[zcl::IAS_ZONE], Some(0x7777));
        assert!(published(&unknown).point("alarm").is_some());
    }

    #[test]
    fn a_battery_only_on_what_has_one() {
        let mut plug = lamp();
        plug.endpoints = vec![endpoint(1, &[zcl::ON_OFF, zcl::POWER_CONFIG])];
        assert!(published(&plug).point("battery").is_none());
        assert!(
            bindings(&plug)
                .iter()
                .all(|b| b.cluster != zcl::POWER_CONFIG)
        );
    }

    #[test]
    fn a_second_endpoint_says_its_own() {
        let mut double = lamp();
        double.endpoints = vec![
            endpoint(1, &[zcl::ON_OFF]),
            endpoint(2, &[zcl::ON_OFF, zcl::COLOR]),
        ];
        let device = published(&double);
        assert!(device.point("state").is_some());
        assert!(device.point("state_2").is_some());
        assert!(device.point("color").is_some(), "the first with a colour");
        let command = command(&double, "state_2", &Value::Bool(true)).unwrap();
        assert_eq!((command.endpoint, command.cluster), (2, zcl::ON_OFF));
        assert_eq!(command.effects[0].0, "state_2");
        let values = decode(
            &double,
            &mut ColorState::default(),
            2,
            zcl::ON_OFF,
            &Attribute {
                id: 0,
                value: Data::Bool(true),
            },
        );
        assert_eq!(values, [("state_2".to_owned(), Value::Bool(true))]);
        // An endpoint it never declared: nothing.
        assert!(
            decode(
                &double,
                &mut ColorState::default(),
                9,
                zcl::ON_OFF,
                &Attribute {
                    id: 0,
                    value: Data::Bool(true)
                },
            )
            .is_empty()
        );
    }

    #[test]
    fn profiles_that_are_not_lighting_or_home_are_skipped() {
        let mut lamp = lamp();
        lamp.endpoints.push(EndpointRecord {
            endpoint: 242,
            profile: 0xA1E0, // Green Power
            device_id: 0x0061,
            in_clusters: vec![zcl::ON_OFF],
            out_clusters: Vec::new(),
        });
        lamp.endpoints[0].profile = zcl::ZLL_PROFILE;
        let device = published(&lamp);
        assert!(device.point("state").is_some(), "ZLL is read");
        assert!(device.point("state_242").is_none());
    }

    #[test]
    fn levels_are_percents() {
        let lamp = lamp();
        let at =
            |level| read(&lamp, zcl::LEVEL, CURRENT_LEVEL, Data::Uint(level))["brightness"].clone();
        assert_eq!(at(254), Value::Int(100));
        assert_eq!(at(127), Value::Int(50));
        assert_eq!(at(1), Value::Int(1), "a lit lamp is never 0 %");
        assert_eq!(at(0), Value::Int(0));
        let to = |percent: i64| {
            let sent = command(&lamp, "brightness", &Value::Int(percent)).unwrap();
            sent.request.payload[0]
        };
        assert_eq!(to(100), 254);
        assert_eq!(to(50), 127);
        assert_eq!(to(1), 3);
        assert_eq!(to(0), 0);
    }

    #[test]
    fn no_value_publishes_nothing() {
        let sensor = sensor(&[zcl::TEMPERATURE], None);
        assert!(read(&sensor, zcl::TEMPERATURE, 0, Data::Invalid).is_empty());
        assert!(read(&sensor, zcl::TEMPERATURE, 0x77, Data::Int(1)).is_empty());
        assert!(read(&sensor, zcl::HUMIDITY, 0, Data::Uint(1)).is_empty());
        assert!(read(&sensor, zcl::TEMPERATURE, 0, Data::Text("?".into())).is_empty());
    }

    #[test]
    fn measures_in_their_units() {
        let all = sensor(
            &[
                zcl::TEMPERATURE,
                zcl::HUMIDITY,
                zcl::PRESSURE,
                zcl::ILLUMINANCE,
                zcl::OCCUPANCY,
                zcl::POWER_CONFIG,
            ],
            None,
        );
        let device = published(&all);
        assert_eq!(
            device.point("temperature").unwrap().unit,
            Some(Unit::Celsius)
        );
        let value = |cluster, id, data| read(&all, cluster, id, data);
        assert_eq!(
            value(zcl::TEMPERATURE, 0, Data::Int(2094))["temperature"],
            Value::Float(20.94)
        );
        assert_eq!(
            value(zcl::TEMPERATURE, 0, Data::Int(-150))["temperature"],
            Value::Float(-1.5)
        );
        assert_eq!(
            value(zcl::HUMIDITY, 0, Data::Uint(4567))["humidity"],
            Value::Float(45.67)
        );
        assert_eq!(
            value(zcl::PRESSURE, 0, Data::Int(1013))["pressure"],
            Value::Float(1013.0)
        );
        assert_eq!(
            value(zcl::ILLUMINANCE, 0, Data::Uint(30001))["illuminance"],
            Value::Int(1000)
        );
        assert_eq!(
            value(zcl::ILLUMINANCE, 0, Data::Uint(0))["illuminance"],
            Value::Int(0)
        );
        assert_eq!(
            value(zcl::OCCUPANCY, 0, Data::Uint(1))["occupancy"],
            Value::Bool(true)
        );
        assert_eq!(
            value(zcl::POWER_CONFIG, BATTERY_PERCENTAGE, Data::Uint(200))["battery"],
            Value::Int(100)
        );
        assert_eq!(
            value(zcl::POWER_CONFIG, BATTERY_VOLTAGE, Data::Uint(30))["voltage"],
            Value::Int(3000)
        );
    }

    #[test]
    fn a_plug_says_watts_and_kilowatt_hours() {
        let mut plug = lamp();
        plug.endpoints = vec![endpoint(1, &[zcl::ON_OFF, zcl::METERING, zcl::ELECTRICAL])];
        plug.scale = Scale {
            metering: Some(Ratio {
                multiplier: 1,
                divisor: 1000,
            }),
            voltage: Some(Ratio {
                multiplier: 1,
                divisor: 10,
            }),
            current: Some(Ratio {
                multiplier: 1,
                divisor: 1000,
            }),
            power: Some(Ratio {
                multiplier: 1,
                divisor: 10,
            }),
        };
        let device = published(&plug);
        assert_eq!(
            device.points.iter().filter(|p| &*p.key == "power").count(),
            1,
            "Metering and Electrical both know power: once"
        );
        let value = |cluster, id, data| read(&plug, cluster, id, data);
        assert_eq!(
            value(zcl::METERING, SUMMATION_DELIVERED, Data::Uint(12345))["energy"],
            Value::Float(12.345)
        );
        assert!(
            value(zcl::METERING, INSTANTANEOUS_DEMAND, Data::Int(150)).is_empty(),
            "one source for its power: Electrical"
        );
        assert_eq!(
            value(zcl::ELECTRICAL, RMS_VOLTAGE, Data::Uint(2301))["voltage"],
            Value::Float(230.1)
        );
        assert_eq!(
            value(zcl::ELECTRICAL, RMS_CURRENT, Data::Uint(250))["current"],
            Value::Float(0.25)
        );
        assert_eq!(
            value(zcl::ELECTRICAL, ACTIVE_POWER, Data::Int(575))["power"],
            Value::Float(57.5)
        );
    }

    #[test]
    fn a_plug_reports_what_changes_something() {
        let mut plug = lamp();
        plug.endpoints = vec![endpoint(1, &[zcl::ON_OFF, zcl::METERING, zcl::ELECTRICAL])];
        let tenth = Ratio {
            multiplier: 1,
            divisor: 10,
        };
        plug.scale = Scale {
            metering: Some(Ratio {
                multiplier: 1,
                divisor: 1000,
            }),
            voltage: Some(tenth),
            current: Some(Ratio {
                multiplier: 1,
                divisor: 1000,
            }),
            power: Some(tenth),
        };
        let bound = bindings(&plug);
        let of = |cluster| bound.iter().find(|b| b.cluster == cluster).unwrap();
        let metering = &of(zcl::METERING).reports;
        assert_eq!(metering.len(), 1, "no second source for its power");
        assert_eq!(metering[0].attribute, SUMMATION_DELIVERED);
        assert_eq!(metering[0].change, Some(100), "0.1 kWh");
        let electrical = &of(zcl::ELECTRICAL).reports;
        assert_eq!(electrical[0].change, Some(50), "5 W");
        assert_eq!(electrical[1].change, Some(50), "5 V");
        assert_eq!(electrical[2].change, Some(50), "50 mA");
        assert!(
            reads(&plug)
                .iter()
                .find(|plan| plan.cluster == zcl::METERING)
                .is_some_and(|plan| plan.attributes == [SUMMATION_DELIVERED])
        );

        // Metering alone: its power comes from there, in kW once scaled.
        plug.endpoints = vec![endpoint(1, &[zcl::ON_OFF, zcl::METERING])];
        let values = read(&plug, zcl::METERING, INSTANTANEOUS_DEMAND, Data::Int(150));
        assert_eq!(values["power"], Value::Float(150.0));
        let bound = bindings(&plug);
        let metering = &bound.iter().find(|b| b.cluster == zcl::METERING).unwrap();
        assert_eq!(metering.reports[1].attribute, INSTANTANEOUS_DEMAND);
        assert_eq!(metering.reports[1].change, Some(5), "5 W");

        // Electrical without a power of its own: Metering still says it.
        plug.endpoints = vec![endpoint(1, &[zcl::ON_OFF, zcl::METERING, zcl::ELECTRICAL])];
        plug.scale.power = None;
        let values = read(&plug, zcl::METERING, INSTANTANEOUS_DEMAND, Data::Int(150));
        assert_eq!(values["power"], Value::Float(150.0));

        // A threshold never overflows what its type holds.
        plug.scale.power = Some(Ratio {
            multiplier: 1,
            divisor: 10_000,
        });
        let bound = bindings(&plug);
        let electrical = &bound.iter().find(|b| b.cluster == zcl::ELECTRICAL).unwrap();
        assert_eq!(electrical.reports[0].change, Some(0x7FFF));
    }

    #[test]
    fn a_repeater_is_read_on_basic() {
        let mut repeater = lamp();
        repeater.endpoints = vec![endpoint(1, &[zcl::BASIC])];
        assert!(reads(&repeater).is_empty());
        assert_eq!(
            ping(&repeater),
            Some(ReadPlan {
                endpoint: 1,
                cluster: zcl::BASIC,
                attributes: vec![0x0000],
            })
        );
        assert_eq!(ping(&lamp()).unwrap().cluster, zcl::ON_OFF);
        repeater.endpoints.clear();
        assert_eq!(ping(&repeater), None);
    }

    #[test]
    fn colours_are_composed_once_both_shares_are_known() {
        let lamp = lamp();
        let mut color = ColorState::default();
        let mut at = |id, value| {
            decode(
                &lamp,
                &mut color,
                1,
                zcl::COLOR,
                &Attribute {
                    id,
                    value: Data::Uint(value),
                },
            )
            .into_iter()
            .collect::<HashMap<_, _>>()
        };
        let first = at(CURRENT_X, 45_874);
        assert_eq!(first["color.x"], Value::Float(0.7));
        assert!(!first.contains_key("color"), "y is not known yet");
        let second = at(CURRENT_Y, 19_660);
        assert_eq!(second["color.y"], Value::Float(0.3));
        let Value::Text(hex) = &second["color"] else {
            panic!("a colour: {second:?}");
        };
        assert!(hex.starts_with('#') && hex.len() == 7, "{hex}");
        // Unknown mode, then white: its temperature; then a colour: none.
        assert_eq!(at(COLOR_TEMPERATURE, 370)["color_temp"], Value::Int(370));
        assert_eq!(at(COLOR_MODE, 1)["color_temp"], Value::Null);
        assert_eq!(at(COLOR_TEMPERATURE, 370)["color_temp"], Value::Null);
        assert!(at(COLOR_MODE, 2).is_empty());
        assert_eq!(at(COLOR_TEMPERATURE, 370)["color_temp"], Value::Int(370));
    }

    #[test]
    fn orders_and_what_they_show() {
        let lamp = lamp();
        let sent = command(&lamp, "state", &Value::Bool(true)).unwrap();
        assert_eq!(sent.cluster, zcl::ON_OFF);
        assert_eq!(sent.endpoint, 1);
        assert_eq!(sent.request, zcl::on_off(true));
        assert_eq!(sent.effects, [("state".to_owned(), Value::Bool(true))]);

        let sent = command(&lamp, "brightness", &Value::Int(50)).unwrap();
        assert_eq!(sent.request, zcl::move_to_level_with_on_off(127, 0));
        assert_eq!(
            sent.effects,
            [
                ("brightness".to_owned(), Value::Int(50)),
                ("state".to_owned(), Value::Bool(true)),
            ]
        );
        let off = command(&lamp, "brightness", &Value::Float(0.0)).unwrap();
        assert_eq!(off.effects[1], ("state".to_owned(), Value::Bool(false)));

        let sent = command(&lamp, "color_temp", &Value::Int(370)).unwrap();
        assert_eq!(sent.cluster, zcl::COLOR);
        assert_eq!(sent.request, zcl::move_to_color_temp(370, 0));

        let red = Value::from("#ff0000");
        let sent = command(&lamp, "color", &red).unwrap();
        let (x, y) = hex_to_xy("#ff0000").unwrap();
        assert_eq!(sent.request, zcl::move_to_color(share(x), share(y), 0));
        let effects: HashMap<_, _> = sent.effects.into_iter().collect();
        assert_eq!(effects["color"], red);
        assert_eq!(effects["color_temp"], Value::Null);
    }

    #[test]
    fn orders_it_cannot_send_are_refused() {
        let lamp = lamp();
        assert_eq!(
            command(&lamp, "nothing", &Value::Bool(true)),
            Err(Refused::UnknownPoint)
        );
        assert_eq!(
            command(&lamp, "linkquality", &Value::Int(1)),
            Err(Refused::UnknownPoint)
        );
        assert_eq!(
            command(&lamp, "color.x", &Value::Float(0.1)),
            Err(Refused::UnknownPoint)
        );
        assert_eq!(
            command(&lamp, "color", &Value::from("#000000")),
            Err(Refused::BadValue),
            "black is no colour"
        );
        assert_eq!(
            command(&lamp, "state", &Value::Int(1)),
            Err(Refused::BadValue)
        );
    }

    #[test]
    fn a_thermostat() {
        let mut heater = sensor(&[zcl::THERMOSTAT], None);
        heater.capability = Some(0x8E);
        let device = published(&heater);
        assert!(device.point("system_mode").unwrap().access.write);
        let sent = command(&heater, "occupied_heating_setpoint", &Value::Float(20.5)).unwrap();
        assert_eq!(
            sent.request,
            zcl::write_attribute(HEATING_SETPOINT, zcl::INT16, &2050i16.to_le_bytes())
        );
        let sent = command(&heater, "system_mode", &Value::from("heat")).unwrap();
        assert_eq!(
            sent.request,
            zcl::write_attribute(SYSTEM_MODE, zcl::ENUM8, &[4])
        );
        assert_eq!(
            command(&heater, "system_mode", &Value::from("party")),
            Err(Refused::BadValue)
        );
        let values = read(&heater, zcl::THERMOSTAT, SYSTEM_MODE, Data::Uint(4));
        assert_eq!(values["system_mode"], Value::from("heat"));
        let values = read(&heater, zcl::THERMOSTAT, SYSTEM_MODE, Data::Uint(9));
        assert!(values.is_empty(), "no such mode on the point");
        let values = read(&heater, zcl::THERMOSTAT, LOCAL_TEMPERATURE, Data::Int(1950));
        assert_eq!(values["local_temperature"], Value::Float(19.5));
    }

    #[test]
    fn what_to_read_and_report() {
        let lamp = lamp();
        let plans = reads(&lamp);
        assert_eq!(
            plans,
            [
                ReadPlan {
                    endpoint: 1,
                    cluster: zcl::ON_OFF,
                    attributes: vec![ON_OFF_STATE],
                },
                ReadPlan {
                    endpoint: 1,
                    cluster: zcl::LEVEL,
                    attributes: vec![CURRENT_LEVEL],
                },
                ReadPlan {
                    endpoint: 1,
                    cluster: zcl::COLOR,
                    attributes: vec![COLOR_MODE, CURRENT_X, CURRENT_Y, COLOR_TEMPERATURE],
                },
            ]
        );
        let bound = bindings(&lamp);
        let clusters: Vec<_> = bound.iter().map(|b| b.cluster).collect();
        assert_eq!(clusters, [zcl::ON_OFF, zcl::LEVEL, zcl::COLOR]);
        assert_eq!(
            bound[0].reports,
            [Report {
                attribute: 0,
                data_type: zcl::BOOLEAN,
                min: 0,
                max: 3600,
                change: None,
            }]
        );
        assert_eq!(bound[1].reports[0].change, Some(1));
        assert_eq!(bound[2].reports.len(), 4);
        assert_eq!(bound[2].reports[3].attribute, COLOR_MODE);
        // A contact sensor notifies by itself: nothing to report on its zone.
        let contact = sensor(&[zcl::IAS_ZONE, zcl::POWER_CONFIG], Some(0x0015));
        let bound = bindings(&contact);
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].cluster, zcl::POWER_CONFIG);
        assert_eq!(bound[0].reports[1].min, 3600);
    }

    #[test]
    fn before_its_interview_it_is_only_there() {
        let instance = InstanceId::from("zigbee");
        let record = DeviceRecord {
            ieee: LAMP_IEEE.into(),
            ..DeviceRecord::default()
        };
        let id = DeviceId::new(&instance, LAMP_IEEE);
        let device = pending_device(&instance, &id, &record);
        assert_eq!(&*device.native_name, LAMP_IEEE);
        assert_eq!(keys(&device), ["linkquality"]);
    }
}

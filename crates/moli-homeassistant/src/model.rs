//! Pure finite adapters. Network metadata can remove rights, never add them.
use std::collections::BTreeMap;

use anyhow::{bail, ensure};
use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::{Map, Value as Json, json};

use crate::{Entity, Safety};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TemperatureUnit {
    Celsius,
    Fahrenheit,
}
impl TemperatureUnit {
    fn parse(raw: &Json) -> Option<Self> {
        match raw.as_str()? {
            "°C" => Some(Self::Celsius),
            "°F" => Some(Self::Fahrenheit),
            _ => None,
        }
    }

    pub fn from_config(raw: &Json) -> Option<Self> {
        Self::parse(raw.get("unit_system")?.get("temperature")?)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub points: Vec<PointSpec>,
    pub values: BTreeMap<String, Value>,
    entity_id: String,
    safe: bool,
    fahrenheit: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Service {
    pub path: String,
    pub body: Json,
}

impl Snapshot {
    fn add(
        &mut self,
        e: &Entity,
        key: &str,
        kind: Kind,
        meaning: (Option<Unit>, Semantic),
        writable: bool,
        value: Option<Value>,
    ) {
        let (unit, semantic) = meaning;
        let write = self.safe
            && writable
            && e.safety != Safety::Unknown
            && e.writable.iter().any(|p| p == key);
        self.points.push(PointSpec {
            key: key.into(),
            label: moli_i18n::tr(&format!("homeassistant.{key}")).into(),
            kind,
            access: Access {
                read: key != "control",
                write,
            },
            unit,
            semantic: if write && (e.safety == Safety::Machine || e.domain == "vacuum") {
                Semantic::Control
            } else {
                semantic
            },
        });
        let point = self.points.last().expect("point just inserted");
        if point.access.read {
            // A decoded snapshot is complete. Omission would preserve an old Hub
            // sample, so missing/invalid readings must explicitly become unknown.
            // Write-only controls never acquire a synthetic state.
            let value = value
                .and_then(|value| moli_core::validate(&point.kind, &value).ok())
                .unwrap_or(Value::Null);
            self.values.insert(key.into(), value);
        }
    }
}

fn numeric(min: f64, max: f64, step: Option<f64>) -> Kind {
    Kind::Numeric {
        min: Some(min),
        max: Some(max),
        step,
    }
}
fn enumeration(values: &[&str]) -> Kind {
    Kind::Enum {
        values: values.iter().map(|s| (*s).into()).collect(),
    }
}
fn number(a: &Map<String, Json>, key: &str) -> Option<f64> {
    a.get(key).and_then(Json::as_f64).filter(|v| v.is_finite())
}
fn bounded(a: &Map<String, Json>, key: &str, min: f64, max: f64) -> Option<Value> {
    number(a, key)
        .filter(|v| (min..=max).contains(v))
        .map(Value::Float)
}
fn text(a: &Map<String, Json>, key: &str) -> Option<Value> {
    a.get(key)
        .and_then(Json::as_str)
        .filter(|v| v.len() <= 256)
        .map(Value::from)
}
fn list<'a>(a: &'a Map<String, Json>, key: &str) -> Vec<&'a str> {
    let Some(values) = a
        .get(key)
        .and_then(Json::as_array)
        .filter(|v| v.len() <= 16)
    else {
        return vec![];
    };
    if values
        .iter()
        .any(|v| v.as_str().is_none_or(|s| s.len() > 64))
    {
        return vec![];
    }
    values.iter().filter_map(Json::as_str).collect()
}
fn bool_state(state: &str) -> anyhow::Result<bool> {
    match state {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => bail!("invalid binary state"),
    }
}
fn celsius(n: f64, fahrenheit: bool) -> f64 {
    if fahrenheit {
        (n - 32.0) * 5.0 / 9.0
    } else {
        n
    }
}
fn mode(ha: &str) -> Option<&'static str> {
    match ha {
        "heat" => Some("chaud"),
        "cool" => Some("froid"),
        "dry" => Some("déshumidification"),
        "fan_only" => Some("ventilation"),
        "auto" => Some("auto"),
        _ => None,
    }
}
fn ha_mode(moli: &str) -> Option<&'static str> {
    match moli {
        "chaud" => Some("heat"),
        "froid" => Some("cool"),
        "déshumidification" => Some("dry"),
        "ventilation" => Some("fan_only"),
        "auto" => Some("auto"),
        _ => None,
    }
}

pub(crate) fn decode(
    e: &Entity,
    raw: &Json,
    global_unit: Option<TemperatureUnit>,
) -> anyhow::Result<Snapshot> {
    ensure!(
        raw.get("entity_id").and_then(Json::as_str) == Some(&e.entity_id),
        "entity mismatch"
    );
    let state = raw
        .get("state")
        .and_then(Json::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or_else(|| anyhow::anyhow!("invalid entity state"))?;
    ensure!(
        !matches!(state, "unknown" | "unavailable"),
        "unavailable entity"
    );
    let a = raw
        .get("attributes")
        .and_then(Json::as_object)
        .filter(|v| v.len() <= 64)
        .ok_or_else(|| anyhow::anyhow!("invalid entity attributes"))?;
    let group = ["entity_id", "group_members"].iter().any(|key| {
        a.get(*key)
            .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|xs| !xs.is_empty()))
    });
    let mut s = Snapshot {
        points: vec![],
        values: BTreeMap::new(),
        entity_id: e.entity_id.clone(),
        safe: !group,
        fahrenheit: false,
    };
    let features = a
        .get("supported_features")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    match e.domain.as_str() {
        "sensor" => sensor(e, &mut s, state, a)?,
        "binary_sensor" => binary(e, &mut s, state, a)?,
        "light" | "switch" => light(e, &mut s, state, a)?,
        "climate" => climate(e, &mut s, state, a, features, global_unit)?,
        "cover" => cover(e, &mut s, state, a, features)?,
        "vacuum" => vacuum(e, &mut s, state, a, features)?,
        "media_player" => media(e, &mut s, state, a, features)?,
        _ => bail!("unsupported bridge domain"),
    }
    Ok(s)
}

fn light(e: &Entity, s: &mut Snapshot, state: &str, a: &Map<String, Json>) -> anyhow::Result<()> {
    s.add(
        e,
        "on",
        Kind::Binary,
        (None, Semantic::OnOff),
        true,
        Some(Value::Bool(bool_state(state)?)),
    );
    if e.domain == "light" {
        let modes = list(a, "supported_color_modes");
        let supports = modes.iter().any(|v| {
            matches!(
                *v,
                "brightness" | "color_temp" | "hs" | "xy" | "rgb" | "rgbw" | "rgbww"
            )
        });
        let value = number(a, "brightness")
            .filter(|n| (0.0..=255.0).contains(n))
            .map(|n| Value::Float(n * 100.0 / 255.0));
        if supports || value.is_some() {
            s.add(
                e,
                "brightness",
                numeric(0.0, 100.0, Some(1.0)),
                (Some(Unit::Percent), Semantic::Brightness),
                supports,
                value,
            );
        }
    }

    if e.domain == "light" {
        colours(e, s, a);
    }
    Ok(())
}

fn cover(
    e: &Entity,
    s: &mut Snapshot,
    state: &str,
    a: &Map<String, Json>,
    features: u64,
) -> anyhow::Result<()> {
    ensure!(
        matches!(state, "open" | "closed" | "opening" | "closing"),
        "invalid cover state"
    );
    s.safe &= matches!(
        a.get("device_class").and_then(Json::as_str),
        Some("awning" | "blind" | "curtain" | "shade" | "shutter")
    );
    s.add(
        e,
        "state",
        Kind::Text,
        (None, Semantic::Other),
        false,
        Some(Value::from(state)),
    );
    let commands: Vec<&str> = [(1, "open"), (2, "close"), (8, "stop")]
        .into_iter()
        .filter_map(|(bit, name)| (features & bit != 0).then_some(name))
        .collect();
    if !commands.is_empty() {
        s.add(
            e,
            "control",
            enumeration(&commands),
            (None, Semantic::Control),
            true,
            None,
        );
    }
    let position = bounded(a, "current_position", 0.0, 100.0);
    if position.is_some() || features & 4 != 0 {
        s.add(
            e,
            "percent_control",
            numeric(0.0, 100.0, Some(1.0)),
            (Some(Unit::Percent), Semantic::Control),
            features & 4 != 0,
            position,
        );
    }

    Ok(())
}

fn vacuum(
    e: &Entity,
    s: &mut Snapshot,
    state: &str,
    a: &Map<String, Json>,
    features: u64,
) -> anyhow::Result<()> {
    ensure!(
        matches!(
            state,
            "cleaning" | "docked" | "idle" | "paused" | "returning" | "error"
        ),
        "invalid vacuum state"
    );
    s.add(
        e,
        "state",
        Kind::Text,
        (None, Semantic::Other),
        false,
        Some(Value::from(state)),
    );
    let commands: Vec<&str> = [
        (8192, "start"),
        (4, "pause"),
        (8, "stop"),
        (16, "return_to_base"),
    ]
    .into_iter()
    .filter_map(|(bit, name)| (features & bit != 0).then_some(name))
    .collect();
    if !commands.is_empty() {
        s.add(
            e,
            "control",
            enumeration(&commands),
            (None, Semantic::Control),
            true,
            None,
        );
    }
    if let Some(value) = bounded(a, "battery_level", 0.0, 100.0) {
        s.add(
            e,
            "battery",
            numeric(0.0, 100.0, None),
            (Some(Unit::Percent), Semantic::Battery),
            false,
            Some(value),
        );
    }

    Ok(())
}

fn rgb(text: &str) -> Option<[u8; 3]> {
    if text.len() != 7
        || !text.starts_with('#')
        || !text.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return None;
    }
    Some([
        u8::from_str_radix(&text[1..3], 16).ok()?,
        u8::from_str_radix(&text[3..5], 16).ok()?,
        u8::from_str_radix(&text[5..7], 16).ok()?,
    ])
}

fn colours(e: &Entity, s: &mut Snapshot, a: &Map<String, Json>) {
    let modes = list(a, "supported_color_modes");
    let colour = modes
        .iter()
        .any(|mode| matches!(*mode, "rgb" | "rgbw" | "rgbww" | "hs" | "xy"));
    let value = a
        .get("rgb_color")
        .and_then(Json::as_array)
        .filter(|v| v.len() == 3)
        .and_then(|v| {
            let channel = |i: usize| u8::try_from(v[i].as_u64()?).ok();
            let (red, green, blue) = (channel(0)?, channel(1)?, channel(2)?);
            Some(Value::from(
                format!("#{red:02x}{green:02x}{blue:02x}").as_str(),
            ))
        });
    if colour || value.is_some() {
        s.add(
            e,
            "color",
            Kind::Text,
            (None, Semantic::Color),
            colour,
            value,
        );
    }
    let bounds = number(a, "min_color_temp_kelvin")
        .zip(number(a, "max_color_temp_kelvin"))
        .filter(|(min, max)| {
            *min >= 100.0
                && *max <= 100_000.0
                && min < max
                && min.fract() == 0.0
                && max.fract() == 0.0
        });
    if let Some((min, max)) = bounds {
        s.add(
            e,
            "color_temp",
            numeric(min, max, Some(1.0)),
            (Some(Unit::Other("K".into())), Semantic::ColorTemp),
            modes.contains(&"color_temp"),
            bounded(a, "color_temp_kelvin", min, max),
        );
    }
}

fn sensor(e: &Entity, s: &mut Snapshot, state: &str, a: &Map<String, Json>) -> anyhow::Result<()> {
    let unit = a
        .get("unit_of_measurement")
        .and_then(Json::as_str)
        .filter(|v| v.len() <= 32)
        .and_then(Unit::parse);
    if unit.is_some() || state.parse::<f64>().is_ok() {
        let n = state
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| anyhow::anyhow!("invalid numeric sensor"))?;
        let class = a.get("device_class").and_then(Json::as_str).unwrap_or("");
        let key = match class {
            "temperature" => "temperature",
            "power" => "power",
            "energy" => "energy",
            "humidity" => "humidity",
            "battery" => "battery",
            _ => "value",
        };
        let semantic = Semantic::infer(key, unit.as_ref());
        s.add(
            e,
            key,
            Kind::Numeric {
                min: None,
                max: None,
                step: None,
            },
            (unit, semantic),
            false,
            Some(Value::Float(n)),
        );
    } else {
        s.add(
            e,
            "state",
            Kind::Text,
            (None, Semantic::Other),
            false,
            Some(Value::from(state)),
        );
    }
    Ok(())
}
fn binary(e: &Entity, s: &mut Snapshot, state: &str, a: &Map<String, Json>) -> anyhow::Result<()> {
    let active = bool_state(state)?;
    let (key, semantic, invert) = match a.get("device_class").and_then(Json::as_str).unwrap_or("") {
        "door" | "window" | "opening" | "garage_door" => ("contact", Semantic::Contact, true),
        "motion" | "occupancy" | "presence" => ("occupancy", Semantic::Occupancy, false),
        "moisture" => ("water_leak", Semantic::WaterLeak, false),
        "smoke" => ("smoke", Semantic::Smoke, false),
        "battery" => ("battery_low", Semantic::BatteryLow, false),
        _ => ("value", Semantic::Other, false),
    };
    s.add(
        e,
        key,
        Kind::Binary,
        (None, semantic),
        false,
        Some(Value::Bool(active ^ invert)),
    );
    Ok(())
}
fn climate(
    e: &Entity,
    s: &mut Snapshot,
    state: &str,
    a: &Map<String, Json>,
    features: u64,
    global_unit: Option<TemperatureUnit>,
) -> anyhow::Result<()> {
    ensure!(
        matches!(
            state,
            "off" | "heat" | "cool" | "dry" | "fan_only" | "auto" | "heat_cool"
        ),
        "invalid climate state"
    );
    let modes = list(a, "hvac_modes");
    let values: Vec<&str> = modes.iter().filter_map(|v| mode(v)).collect();
    let power = modes.contains(&"off")
        && e.turn_on_mode
            .as_deref()
            .is_some_and(|m| modes.contains(&m));
    s.add(
        e,
        "on",
        Kind::Binary,
        (None, Semantic::OnOff),
        power,
        Some(Value::Bool(state != "off")),
    );
    if !values.is_empty() {
        s.add(
            e,
            "mode",
            enumeration(&values),
            (None, Semantic::Other),
            true,
            mode(state).map(Value::from),
        );
    }
    // HA normally reports the display unit in /api/config. An explicit
    // entity unit takes precedence, including invalid values (fail closed).
    let Some(unit) = a
        .get("temperature_unit")
        .map_or(global_unit, TemperatureUnit::parse)
    else {
        return Ok(());
    };
    s.fahrenheit = unit == TemperatureUnit::Fahrenheit;
    if let Some(n) = number(a, "current_temperature")
        .map(|n| celsius(n, s.fahrenheit))
        .filter(|n| (-100.0..=100.0).contains(n))
    {
        s.add(
            e,
            "temperature",
            numeric(-100.0, 100.0, None),
            (Some(Unit::Celsius), Semantic::Temperature),
            false,
            Some(Value::Float(n)),
        );
    }
    let bounds = number(a, "min_temp")
        .zip(number(a, "max_temp"))
        .map(|(min, max)| (celsius(min, s.fahrenheit), celsius(max, s.fahrenheit)))
        .filter(|(min, max)| min < max && *min >= -100.0 && *max <= 100.0);
    let step = number(a, "target_temp_step")
        .map(|n| if s.fahrenheit { n * 5.0 / 9.0 } else { n })
        .filter(|n| *n > 0.0 && *n <= 20.0);
    if let Some((min, max)) = bounds {
        let value = number(a, "temperature")
            .map(|n| celsius(n, s.fahrenheit))
            .filter(|n| (*n >= min) && (*n <= max))
            .map(Value::Float);
        s.add(
            e,
            "target_temperature",
            numeric(min, max, step),
            (Some(Unit::Celsius), Semantic::Temperature),
            features & 1 != 0 && state != "heat_cool" && step.is_some(),
            value,
        );
    }
    Ok(())
}
fn media(
    e: &Entity,
    s: &mut Snapshot,
    state: &str,
    a: &Map<String, Json>,
    features: u64,
) -> anyhow::Result<()> {
    ensure!(
        matches!(
            state,
            "off" | "on" | "idle" | "playing" | "paused" | "standby" | "buffering"
        ),
        "invalid media state"
    );
    s.add(
        e,
        "state",
        Kind::Text,
        (None, Semantic::Other),
        false,
        Some(Value::from(state)),
    );
    // Binary toggles need both advertised operations; partial features stay read-only.
    s.add(
        e,
        "playing",
        Kind::Binary,
        (None, Semantic::Other),
        features & 0x4001 == 0x4001,
        Some(Value::Bool(state == "playing")),
    );
    let volume = number(a, "volume_level")
        .filter(|n| (0.0..=1.0).contains(n))
        .map(|n| Value::Float(n * 100.0));
    if volume.is_some() || features & 4 != 0 {
        s.add(
            e,
            "volume",
            numeric(0.0, 100.0, None),
            (Some(Unit::Percent), Semantic::Other),
            features & 4 != 0,
            volume,
        );
    }
    let mute = a
        .get("is_volume_muted")
        .and_then(Json::as_bool)
        .map(Value::Bool);
    if mute.is_some() || features & 8 != 0 {
        s.add(
            e,
            "mute",
            Kind::Binary,
            (None, Semantic::Other),
            features & 8 != 0,
            mute,
        );
    }
    for (key, ha) in [("title", "media_title"), ("artist", "media_artist")] {
        if let Some(value) = text(a, ha) {
            s.add(
                e,
                key,
                Kind::Text,
                (None, Semantic::Other),
                false,
                Some(value),
            );
        }
    }
    Ok(())
}

// Input was validated against the 0..=100 percent point immediately below.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn brightness_byte(percent: f64) -> u8 {
    (percent * 255.0 / 100.0).round() as u8
}

pub(crate) fn command(
    e: &Entity,
    s: &Snapshot,
    key: &str,
    value: &Value,
) -> anyhow::Result<Service> {
    ensure!(
        s.entity_id == e.entity_id
            && s.safe
            && e.safety != Safety::Unknown
            && e.writable.iter().any(|p| p == key),
        "bridge write refused"
    );
    let point = s
        .points
        .iter()
        .find(|p| &*p.key == key && p.access.write)
        .ok_or_else(|| anyhow::anyhow!("read-only bridge point"))?;
    let value = moli_core::validate(&point.kind, value)?;
    if let Kind::Numeric {
        min: Some(min),
        step: Some(step),
        ..
    } = &point.kind
    {
        let n = value.as_f64().expect("numeric validated");
        let count = (n - min) / step;
        ensure!((count - count.round()).abs() < 1e-6, "value is off step");
    }
    let mut body = json!({"entity_id":e.entity_id});
    let service = service_name(e, s, key, &value, &mut body)?;

    Ok(Service {
        path: format!("/api/services/{}/{service}", e.domain),
        body,
    })
}

fn service_name(
    e: &Entity,
    s: &Snapshot,
    key: &str,
    value: &Value,
    body: &mut Json,
) -> anyhow::Result<&'static str> {
    let number = value.as_f64();
    let boolean = if let Value::Bool(b) = &value {
        Some(*b)
    } else {
        None
    };
    let text = if let Value::Text(t) = &value {
        Some(t.as_ref())
    } else {
        None
    };
    Ok(match (e.domain.as_str(), key) {
        ("switch" | "light", "on") => {
            if boolean == Some(true) {
                "turn_on"
            } else {
                "turn_off"
            }
        }
        ("light", "brightness") => {
            body["brightness"] = json!(brightness_byte(number.expect("validated")));
            "turn_on"
        }
        ("light", "color") => {
            body["rgb_color"] = json!(
                rgb(text.expect("validated")).ok_or_else(|| anyhow::anyhow!("invalid colour"))?
            );
            "turn_on"
        }
        ("light", "color_temp") => {
            body["color_temp_kelvin"] = json!(number.expect("validated"));
            "turn_on"
        }
        ("climate", "target_temperature") => {
            let n = number.expect("validated");
            body["temperature"] = json!(if s.fahrenheit {
                n * 9.0 / 5.0 + 32.0
            } else {
                n
            });
            "set_temperature"
        }
        ("climate", "mode") => {
            body["hvac_mode"] = json!(
                ha_mode(text.expect("validated"))
                    .ok_or_else(|| anyhow::anyhow!("unsupported mode"))?
            );
            "set_hvac_mode"
        }
        ("climate", "on") => {
            body["hvac_mode"] = json!(if boolean == Some(true) {
                e.turn_on_mode
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("no turn-on mode"))?
            } else {
                "off"
            });
            "set_hvac_mode"
        }
        ("cover", "percent_control") => {
            body["position"] = json!(number.expect("validated"));
            "set_cover_position"
        }
        ("cover", "control") => match text {
            Some("open") => "open_cover",
            Some("close") => "close_cover",
            Some("stop") => "stop_cover",
            _ => bail!("unsupported cover command"),
        },
        ("vacuum", "control") => match text {
            Some("start") => "start",
            Some("pause") => "pause",
            Some("stop") => "stop",
            Some("return_to_base") => "return_to_base",
            _ => bail!("unsupported vacuum command"),
        },
        ("media_player", "playing") => {
            if boolean == Some(true) {
                "media_play"
            } else {
                "media_pause"
            }
        }
        ("media_player", "volume") => {
            body["volume_level"] = json!(number.expect("validated") / 100.0);
            "volume_set"
        }
        ("media_player", "mute") => {
            body["is_volume_muted"] = json!(boolean.expect("validated"));
            "volume_mute"
        }
        _ => bail!("unsupported bridge command"),
    })
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;

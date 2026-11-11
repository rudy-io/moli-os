//! Pure translation between Zigbee2MQTT's model and Moli's. No IO.
//!
//! Z2M describes every device it supports through `exposes` in the retained
//! `bridge/devices` message. Flattening those gives us typed points for all
//! of the ~4 000 Zigbee devices Z2M knows, without one line of
//! device-specific code.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use moli_core::color::{hex_to_xy, xy_to_hex};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::{Map, Value as Json};

use crate::flame::{FLAME, Frame, NO_EFFECT};

/// Z2M `access` bitmask.
const ACCESS_STATE: u64 = 1;
const ACCESS_SET: u64 = 2;
const ACCESS_GET: u64 = 4;

/// How a point travels on the wire.
#[derive(Clone, Debug, PartialEq)]
struct Codec {
    /// Booleans travel as device-specific values (`"ON"`/`"OFF"`, `"LOCK"`…).
    binary: Option<(Json, Json)>,
    /// The device answers `{name}/get` for this point.
    gettable: bool,
    /// Enum whose values are numbers on the wire (`[0, 1, 2]`): exposed as
    /// text like every enum, converted back to numbers for the device.
    numeric_enum: bool,
    /// A dimming level the device counts from 0 to this maximum (Zigbee: 254),
    /// exposed in percent like every other driver and the dashboard do.
    level_max: Option<f64>,
    /// The colour people pick (« #rrggbb »), sent to the lamp as CIE xy and
    /// read back from its `color {x, y}`, as the Hue driver does.
    xy_hex: bool,
    /// A relay wired as a trigger (a gate, a garage door): its point is an
    /// order, never a state. The wire value that fires it (`"ON"`).
    impulse: Option<Json>,
}

/// The only value of an impulse point: one order, no state.
pub const IMPULSE: &str = "cycle";

/// One Z2M device with everything needed to decode its messages.
#[derive(Clone, Debug)]
pub struct Entry {
    pub ieee: String,
    pub friendly_name: String,
    pub device: Device,
    /// Mains-powered (router): always listening, can be read on demand.
    pub mains: bool,
    /// A colour lamp that dims: the driver can play a flame on it
    /// (`effect` = `flame`, see [`crate::flame`]).
    pub flame: bool,
    codecs: HashMap<Arc<str>, Codec>,
}

/// All devices of one Z2M bridge, indexed by IEEE address (identity) and by
/// friendly name (topic routing, may change at any time).
#[derive(Debug, Default)]
pub struct Catalog {
    entries: HashMap<String, Entry>,
    by_name: HashMap<String, String>,
}

impl Catalog {
    /// Rebuilds the catalog from a `bridge/devices` payload.
    pub fn from_bridge_devices(instance: &InstanceId, payload: &[u8]) -> serde_json::Result<Self> {
        Self::with_impulses(instance, payload, &[])
    }

    /// Like [`Self::from_bridge_devices`], with the relays the user declared
    /// as triggers (`impulse_points`: point ids such as `z2m:0x…/state_l1`).
    /// Zigbee2MQTT does not say what a relay is wired to, and a pulse relay
    /// looks like a plain switch whose state falls back to off: only the
    /// owner knows.
    pub fn with_impulses(
        instance: &InstanceId,
        payload: &[u8],
        impulses: &[String],
    ) -> serde_json::Result<Self> {
        let raw: Vec<RawDevice> = serde_json::from_slice(payload)?;
        let mut applied = HashSet::new();
        let entries: HashMap<String, Entry> = raw
            .into_iter()
            .filter(|d| d.kind != "Coordinator" && !d.disabled)
            .map(|d| {
                let entry = Entry::from_raw(instance, d, impulses, &mut applied);
                (entry.ieee.clone(), entry)
            })
            .collect();
        for id in impulses.iter().filter(|id| !applied.contains(*id)) {
            tracing::warn!(point = %id, "impulse_points: not a writable on/off point of a known device");
        }
        let by_name = entries
            .values()
            .map(|e| (e.friendly_name.clone(), e.ieee.clone()))
            .collect();
        Ok(Self { entries, by_name })
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    #[must_use]
    pub fn by_name(&self, friendly_name: &str) -> Option<&Entry> {
        self.entries.get(self.by_name.get(friendly_name)?)
    }

    #[must_use]
    pub fn by_id(&self, id: &DeviceId) -> Option<&Entry> {
        self.entries.values().find(|e| &e.device.id == id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Entry {
    fn from_raw(
        instance: &InstanceId,
        raw: RawDevice,
        impulses: &[String],
        applied: &mut HashSet<String>,
    ) -> Self {
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        let definition = raw.definition.unwrap_or_default();
        for expose in &definition.exposes {
            flatten(expose, "", "", &mut points, &mut codecs);
        }
        let id = DeviceId::new(instance, &raw.ieee_address);
        mark_impulses(&id, impulses, &mut points, &mut codecs, applied);
        mark_soil(&mut points);
        let flame = mark_flame(&mut points, &mut codecs);
        let device = Device {
            id,
            instance: instance.clone(),
            native_name: raw.friendly_name.as_str().into(),
            manufacturer: definition.vendor.or(raw.manufacturer).map(Into::into),
            model: definition.model.or(raw.model_id).map(Into::into),
            description: definition.description.map(Into::into),
            native_room: None,
            members: Vec::new(),
            points,
        };
        Self {
            ieee: raw.ieee_address,
            friendly_name: raw.friendly_name,
            device,
            mains: raw.kind == "Router",
            flame,
            codecs,
        }
    }

    /// One fade of the flame as a `{friendly_name}/set` payload: level, amber
    /// and the time the lamp takes to get there. `switch_on`: the first one
    /// also lights the lamp.
    #[must_use]
    pub fn flame_frame(&self, frame: &Frame, switch_on: bool) -> Json {
        let mut payload = match self.encode("brightness", &Value::Float(frame.brightness)) {
            Json::Object(map) => map,
            _ => Map::new(),
        };
        if switch_on && let Json::Object(state) = self.encode("state", &Value::Bool(true)) {
            payload.extend(state);
        }
        payload.insert(
            "color".into(),
            serde_json::json!({ "hue": frame.hue, "saturation": frame.saturation }),
        );
        payload.insert(
            "transition".into(),
            Json::from(frame.transition.as_secs_f64()),
        );
        Json::Object(payload)
    }

    /// The level to come back to when the flame stops, as a payload.
    #[must_use]
    pub fn flame_rest(&self, brightness: f64) -> Json {
        let mut payload = match self.encode("brightness", &Value::Float(brightness)) {
            Json::Object(map) => map,
            _ => Map::new(),
        };
        payload.insert("transition".into(), Json::from(1));
        Json::Object(payload)
    }

    /// `{name}/get` payload asking a listening device for its current
    /// values (a Zigbee read: no effect on the device). `None` for sleepy
    /// devices or when nothing is readable.
    #[must_use]
    pub fn read_request(&self) -> Option<Json> {
        if !self.mains {
            return None;
        }
        let mut request = Map::new();
        for (key, codec) in &self.codecs {
            if codec.gettable {
                let top = key.split('.').next().unwrap_or(key);
                request.insert(top.to_owned(), Json::String(String::new()));
            }
        }
        (!request.is_empty()).then_some(Json::Object(request))
    }

    /// Decodes a state message into `(key, value)` pairs for known points.
    /// Unknown properties (`update`, `last_seen`…) are ignored.
    #[must_use]
    pub fn decode(&self, payload: &Map<String, Json>) -> Vec<(Arc<str>, Value)> {
        let color_mode = payload.get("color_mode").and_then(Json::as_str);
        self.device
            .points
            .iter()
            .filter_map(|point| {
                let codec = self.codecs.get(&point.key);
                // A trigger has no state: the relay falling back to off is
                // the device finishing its pulse, not something to show.
                if codec.is_some_and(|c| c.impulse.is_some()) {
                    return None;
                }
                let json = lookup(payload, &point.key)?;
                if codec.is_some_and(|c| c.xy_hex) {
                    let axis = |a: &str| json.get(a).and_then(Json::as_f64);
                    let hex = axis("x")
                        .zip(axis("y"))
                        .and_then(|(x, y)| xy_to_hex(x, y))?;
                    return Some((point.key.clone(), Value::from(hex.as_str())));
                }
                // A lamp in colour mode keeps reporting its last white: no
                // temperature then, as with Hue, so pages show the colour.
                if point.semantic == Semantic::ColorTemp
                    && color_mode.is_some_and(|mode| mode != "color_temp")
                {
                    return Some((point.key.clone(), Value::Null));
                }
                let level = codec.and_then(|c| c.level_max).zip(json.as_f64());
                let value = if let Some((max, level)) = level {
                    Value::Int(level_to_percent(level, max))
                } else {
                    match codec.and_then(|c| c.binary.as_ref()) {
                        Some((on, _)) if json == on => Value::Bool(true),
                        Some((_, off)) if json == off => Value::Bool(false),
                        _ if codec.is_some_and(|c| c.numeric_enum) && json.is_number() => {
                            Value::Text(json.to_string().into())
                        }
                        _ => Value::from_json(json),
                    }
                };
                Some((point.key.clone(), value))
            })
            .collect()
    }

    /// Encodes a validated write as the `{friendly_name}/set` payload.
    #[must_use]
    pub fn encode(&self, key: &str, value: &Value) -> Json {
        let codec = self.codecs.get(key);
        let level = codec.and_then(|c| c.level_max).zip(value.as_f64());
        let xy = match value {
            Value::Text(hex) if codec.is_some_and(|c| c.xy_hex) => {
                Some(hex_to_xy(hex).map_or_else(
                    // Not a colour (black…): Z2M says why it refuses.
                    || serde_json::json!({ "hex": &**hex }),
                    |(x, y)| serde_json::json!({ "x": x, "y": y }),
                ))
            }
            _ => None,
        };
        let wire = if let Some(fire) = codec
            .and_then(|c| c.impulse.as_ref())
            .filter(|_| matches!(value, Value::Text(t) if &**t == IMPULSE))
        {
            fire.clone()
        } else if let Some(xy) = xy {
            xy
        } else if let Some((max, percent)) = level {
            Json::from(percent_to_level(percent, max))
        } else {
            match (codec.and_then(|c| c.binary.as_ref()), value) {
                (Some((on, _)), Value::Bool(true)) => on.clone(),
                (Some((_, off)), Value::Bool(false)) => off.clone(),
                (_, Value::Text(t)) if codec.is_some_and(|c| c.numeric_enum) => {
                    serde_json::from_str::<Json>(t)
                        .ok()
                        .filter(Json::is_number)
                        .unwrap_or_else(|| Json::String(t.to_string()))
                }
                // Z2M wants integers where the spec has an integer step.
                (_, Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9e15 =>
                {
                    #[allow(clippy::cast_possible_truncation)]
                    Json::from(*f as i64)
                }
                (_, v) => v.to_json(),
            }
        };
        // `color.x` → {"color": {"x": …}}
        key.rsplit('.').fold(wire, |inner, part| {
            let mut map = Map::new();
            map.insert(part.to_owned(), inner);
            Json::Object(map)
        })
    }
}

/// A Zigbee dimming level (0..`max`) as a whole percent. A lit level is never
/// shown as 0 %.
#[allow(clippy::cast_possible_truncation)]
fn level_to_percent(level: f64, max: f64) -> i64 {
    let percent = ((level * 100.0 / max).round() as i64).clamp(0, 100);
    if level > 0.0 { percent.max(1) } else { percent }
}

/// A percent as a Zigbee dimming level (0..`max`).
#[allow(clippy::cast_possible_truncation)]
fn percent_to_level(percent: f64, max: f64) -> i64 {
    (percent.clamp(0.0, 100.0) * max / 100.0).round() as i64
}

fn lookup<'a>(payload: &'a Map<String, Json>, key: &str) -> Option<&'a Json> {
    let mut parts = key.split('.');
    let mut current = payload.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

/// The order point of a shutter, blind or curtain: an enum that opens and
/// closes (`OPEN` / `CLOSE` / `STOP`), whatever the case it is spelled in.
fn is_shutter_order(kind: &Kind) -> bool {
    matches!(kind, Kind::Enum { values }
        if values.iter().any(|v| v.eq_ignore_ascii_case("open"))
            && values.iter().any(|v| v.eq_ignore_ascii_case("close")))
}

/// What a shutter's own points are (`group` is the enclosing expose's type).
fn shutter_semantic(group: &str, property: &str, kind: &Kind) -> Option<Semantic> {
    if is_shutter_order(kind) {
        // `state` (OPEN / CLOSE / STOP) is an order, not a switch: read as
        // one it shows « off » while the shutter is open, and a click on it
        // would send `ON`. Its opening is `position`.
        Some(Semantic::Other)
    } else if group == "cover"
        && property.starts_with("position")
        && matches!(kind, Kind::Numeric { .. })
    {
        // The opening of a shutter, blind or curtain. Never deduced from the
        // name alone: a `position` can be a servo or a playback position.
        Some(Semantic::Position)
    } else {
        None
    }
}

/// A group of exposes: a composite value or a specific type (light, cover…).
fn flatten_group<'a>(
    expose: &'a RawExpose,
    prefix: &str,
    group: &'a str,
    points: &mut Vec<PointSpec>,
    codecs: &mut HashMap<Arc<str>, Codec>,
) {
    if is_xy_color(expose) {
        push_color(expose, prefix, points, codecs);
    }
    // Composite values nest under their property; specific types
    // (light, switch, climate…) are plain groups.
    let prefix = match (&*expose.kind, &expose.property) {
        ("composite", Some(p)) => format!("{prefix}{p}."),
        _ => prefix.to_owned(),
    };
    // The enclosing group's type (`cover`, `light`…) says what a plain
    // member such as `position` is.
    let group = match &*expose.kind {
        "composite" => group,
        kind => kind,
    };
    for feature in &expose.features {
        flatten(feature, &prefix, group, points, codecs);
    }
}

fn flatten<'a>(
    expose: &'a RawExpose,
    prefix: &str,
    group: &'a str,
    points: &mut Vec<PointSpec>,
    codecs: &mut HashMap<Arc<str>, Codec>,
) {
    if !expose.features.is_empty() {
        return flatten_group(expose, prefix, group, points, codecs);
    }
    let Some(property) = &expose.property else {
        return;
    };
    let key: Arc<str> = format!("{prefix}{property}").into();
    if points.iter().any(|p| p.key == key) {
        return;
    }
    let (kind, binary) = match &*expose.kind {
        // Z2M declares `contact` upside down (`value_on: false`, `value_off: true`,
        // "closed (= true) or open (= false)"), whereas Moli's convention is
        // `true` = closed. Map the wire value straight through instead of via
        // `value_on`, or every contact sensor reads inverted.
        "binary" if &*key == "contact" => {
            (Kind::Binary, Some((Json::Bool(true), Json::Bool(false))))
        }
        "binary" => (
            Kind::Binary,
            Some((
                expose.value_on.clone().unwrap_or(Json::Bool(true)),
                expose.value_off.clone().unwrap_or(Json::Bool(false)),
            )),
        ),
        "numeric" => (
            Kind::Numeric {
                min: expose.value_min,
                max: expose.value_max,
                step: expose.value_step,
            },
            None,
        ),
        "enum" => (
            Kind::Enum {
                values: expose
                    .values
                    .iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string().into(), Into::into))
                    .collect(),
            },
            None,
        ),
        "text" => (Kind::Text, None),
        _ => return,
    };
    let mut codec = Codec {
        binary,
        gettable: expose.access & ACCESS_GET != 0,
        numeric_enum: !expose.values.is_empty() && expose.values.iter().all(Json::is_number),
        level_max: None,
        xy_hex: false,
        impulse: None,
    };
    let unit = expose.unit.as_deref().and_then(Unit::parse);
    // A calibration offset in °C is configuration, not a temperature reading.
    // A lock's state (`LOCK`/`UNLOCK`, Z2M's `lock` expose) is no switch: `true` is
    // locked, and the guard asks a person before an agent opens it.
    // Only the state itself (`state`, `state_l1`…): a plug's `child_lock` is
    // LOCK/UNLOCK too, and it locks the plug's button, not a door.
    let lock = expose.kind == "binary"
        && (&**property == "state" || property.starts_with("state_"))
        && expose.value_on.as_ref().and_then(Json::as_str) == Some("LOCK")
        && expose.value_off.as_ref().and_then(Json::as_str) == Some("UNLOCK");
    let semantic = if expose.category.as_deref() == Some("config") {
        Semantic::Config
    } else if lock {
        Semantic::Lock
    } else if let Some(semantic) = shutter_semantic(group, property, &kind) {
        semantic
    } else {
        Semantic::infer(&key, unit.as_ref())
    };
    // Zigbee dims from 0 to 254 (`value_max`); every other driver, the dashboard
    // and the agents speak percent: expose 0-100 and convert on the wire.
    let (kind, unit) = match (&kind, semantic) {
        (Kind::Numeric { max: Some(max), .. }, Semantic::Brightness) if *max > 100.0 => {
            codec.level_max = Some(*max);
            (
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                Some(Unit::Percent),
            )
        }
        _ => (kind, unit),
    };
    // Z2M converters expect composites (color {x, y}…) as a whole: writing
    // one member alone is rejected. Members are read-only until a composite
    // command exists.
    let in_composite = !prefix.is_empty();
    points.push(PointSpec {
        label: expose.label.as_deref().unwrap_or(property).into(),
        kind,
        access: Access {
            read: expose.access & ACCESS_STATE != 0,
            write: expose.access & ACCESS_SET != 0 && !in_composite,
        },
        unit,
        semantic,
        key: key.clone(),
    });
    codecs.insert(key, codec);
}

/// A lamp that takes a colour and dims can play the driver's flame: its
/// `effect` gains `flame` and `no_effect` (the point is created when the lamp
/// has none), and becomes readable, since the driver knows what it plays.
fn mark_flame(points: &mut Vec<PointSpec>, codecs: &mut HashMap<Arc<str>, Codec>) -> bool {
    let writable = |key: &str| points.iter().any(|p| &*p.key == key && p.access.write);
    let colour = writable("color") && codecs.get("color").is_some_and(|c| c.xy_hex);
    if !colour || !writable("brightness") || !writable("state") {
        return false;
    }
    let ours = [NO_EFFECT, FLAME].map(Arc::<str>::from);
    if let Some(point) = points.iter_mut().find(|p| &*p.key == "effect") {
        let Kind::Enum { values } = &mut point.kind else {
            return false;
        };
        if !point.access.write {
            return false;
        }
        for value in ours {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        point.access.read = true;
    } else {
        let key: Arc<str> = "effect".into();
        points.push(PointSpec {
            label: "Effect".into(),
            kind: Kind::Enum {
                values: ours.to_vec(),
            },
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::infer(&key, None),
            key: key.clone(),
        });
        codecs.insert(
            key,
            Codec {
                binary: None,
                gettable: false,
                numeric_enum: false,
                level_max: None,
                xy_hex: false,
                impulse: None,
            },
        );
    }
    true
}

/// Turns the relays the user declared as triggers into `cycle` orders:
/// an enum with that single value, a `Control` (an agent always asks a
/// person first), write-only, never read back. Only a plain writable on/off
/// point qualifies; anything else is left as Zigbee2MQTT exposes it.
fn mark_impulses(
    device: &DeviceId,
    impulses: &[String],
    points: &mut [PointSpec],
    codecs: &mut HashMap<Arc<str>, Codec>,
    applied: &mut HashSet<String>,
) {
    for point in points.iter_mut() {
        let id = format!("{device}/{}", point.key);
        let Some(codec) = codecs.get_mut(&point.key) else {
            continue;
        };
        let fire = match &codec.binary {
            Some((on, _))
                if impulses.contains(&id)
                    && point.kind == Kind::Binary
                    && point.access.write
                    && point.semantic == Semantic::OnOff =>
            {
                on.clone()
            }
            _ => continue,
        };
        point.kind = Kind::Enum {
            values: vec![IMPULSE.into()],
        };
        point.semantic = Semantic::Control;
        point.access = Access {
            read: false,
            write: true,
        };
        codec.impulse = Some(fire);
        codec.gettable = false;
        applied.insert(id);
    }
}

/// A plant sensor's `dry` (Tuya soil sensors): its alarm is the plant's thirst.
/// Only beside a soil moisture reading: elsewhere a bare `dry` says nothing.
fn mark_soil(points: &mut [PointSpec]) {
    if !points.iter().any(|p| p.semantic == Semantic::SoilMoisture) {
        return;
    }
    for point in points.iter_mut() {
        if &*point.key == "dry" && point.kind == Kind::Binary && !point.access.write {
            point.semantic = Semantic::SoilDry;
        }
    }
}

/// A lamp's settable `color {x, y}` composite (`color_xy`).
fn is_xy_color(expose: &RawExpose) -> bool {
    let has = |axis: &str| {
        expose
            .features
            .iter()
            .any(|f| f.property.as_deref() == Some(axis))
    };
    expose.kind == "composite"
        && expose.property.as_deref() == Some("color")
        && expose.access & ACCESS_SET != 0
        && has("x")
        && has("y")
}

/// The whole colour as one writable point, `color` (« #rrggbb »): Z2M only takes
/// the composite as a whole, and pages and agents pick colours, not CIE axes.
/// The members stay, read-only, for history.
fn push_color(
    expose: &RawExpose,
    prefix: &str,
    points: &mut Vec<PointSpec>,
    codecs: &mut HashMap<Arc<str>, Codec>,
) {
    let key: Arc<str> = format!("{prefix}color").into();
    if points.iter().any(|p| p.key == key) {
        return;
    }
    points.push(PointSpec {
        label: "Color".into(),
        kind: Kind::Text,
        access: Access {
            read: expose.access & ACCESS_STATE != 0,
            write: true,
        },
        unit: None,
        semantic: Semantic::Color,
        key: key.clone(),
    });
    codecs.insert(
        key,
        Codec {
            binary: None,
            gettable: expose.access & ACCESS_GET != 0,
            numeric_enum: false,
            level_max: None,
            xy_hex: true,
            impulse: None,
        },
    );
}

#[derive(Debug, Deserialize)]
struct RawDevice {
    ieee_address: String,
    friendly_name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    definition: Option<RawDefinition>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDefinition {
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    exposes: Vec<RawExpose>,
}

#[derive(Debug, Deserialize)]
struct RawExpose {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    property: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    access: u64,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    value_min: Option<f64>,
    #[serde(default)]
    value_max: Option<f64>,
    #[serde(default)]
    value_step: Option<f64>,
    #[serde(default)]
    value_on: Option<Json>,
    #[serde(default)]
    value_off: Option<Json>,
    #[serde(default)]
    values: Vec<Json>,
    #[serde(default)]
    features: Vec<RawExpose>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Catalog {
        Catalog::from_bridge_devices(
            &InstanceId::from("z2m"),
            include_bytes!("../../../integrations/z2m/fixtures/bridge_devices.json"),
        )
        .unwrap()
    }

    fn state(ieee: &str) -> Map<String, Json> {
        let all: Map<String, Json> = serde_json::from_slice(include_bytes!(
            "../../../integrations/z2m/fixtures/state.json"
        ))
        .unwrap();
        all[ieee].as_object().unwrap().clone()
    }

    /// A Tuya soil sensor (COOLO CS-201Z): its soil is a plant's, its `dry` the
    /// plant's thirst (#163). A bare `dry` elsewhere stays what it is.
    #[test]
    fn a_soil_sensor_is_a_plant_and_its_dry_is_thirst() {
        let numeric = |property: &str, unit: &str| json!({ "type": "numeric", "name": property, "property": property, "access": 1, "unit": unit });
        let dry = json!({ "type": "binary", "name": "dry", "property": "dry", "access": 1, "value_on": true, "value_off": false });
        let bridge = json!([
            { "ieee_address": "0x00124b00000000b1", "type": "EndDevice", "friendly_name": "plante",
              "definition": { "model": "CS-201Z", "vendor": "COOLO", "description": "Soil moisture sensor",
                "exposes": [dry, numeric("soil_moisture", "%"), numeric("temperature", "°C"), numeric("humidity", "%")] } },
            { "ieee_address": "0x00124b00000000b2", "type": "EndDevice", "friendly_name": "autre",
              "definition": { "model": "X", "vendor": "Y", "description": "Not a plant", "exposes": [dry] } }
        ]);
        let catalog = Catalog::from_bridge_devices(
            &InstanceId::from("z2m"),
            serde_json::to_vec(&bridge).unwrap().as_slice(),
        )
        .unwrap();
        let plant = catalog.by_name("plante").unwrap();
        let semantic = |key| plant.device.point(key).unwrap().semantic;
        assert_eq!(semantic("soil_moisture"), Semantic::SoilMoisture);
        assert_eq!(semantic("dry"), Semantic::SoilDry);
        assert_eq!(semantic("temperature"), Semantic::Temperature);
        let other = catalog.by_name("autre").unwrap();
        assert_eq!(other.device.point("dry").unwrap().semantic, Semantic::Other);
    }

    /// The real frames of a COOLO CS-201Z (HOBEIAN ZG-303Z) soil sensor.
    #[test]
    fn a_real_soil_sensor_reads_as_a_plant() {
        let catalog = catalog();
        let plant = catalog.by_name("0x00124b000000000b").unwrap();
        let semantic = |key| plant.device.point(key).unwrap().semantic;
        assert_eq!(semantic("soil_moisture"), Semantic::SoilMoisture);
        assert_eq!(semantic("dry"), Semantic::SoilDry);
        assert_eq!(semantic("temperature"), Semantic::Temperature);
        assert_eq!(semantic("humidity"), Semantic::Humidity);
        let values: HashMap<_, _> = plant
            .decode(&state("0x00124b000000000b"))
            .into_iter()
            .collect();
        assert_eq!(values["soil_moisture"].as_f64(), Some(54.0));
        assert_eq!(values["dry"], Value::Bool(false));
        assert_eq!(values["soil_warning"].as_f64(), Some(50.0));
    }

    #[test]
    fn real_bridge_is_fully_mapped() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 10, "coordinator excluded, 10 real devices");
        for entry in catalog.entries() {
            assert!(
                !entry.device.points.is_empty(),
                "{} has no points",
                entry.ieee
            );
            assert!(entry.device.id.as_str().starts_with("z2m:0x"));
        }
    }

    const GATE: &str = "0x00124b000000000a";

    fn gate_catalog(impulses: &[&str]) -> Catalog {
        let impulses: Vec<String> = impulses.iter().map(|id| (*id).to_owned()).collect();
        Catalog::with_impulses(
            &InstanceId::from("z2m"),
            include_bytes!("../../../integrations/z2m/fixtures/bridge_devices.json"),
            &impulses,
        )
        .unwrap()
    }

    #[test]
    fn a_two_relay_switch_is_two_plain_switches_by_default() {
        let catalog = catalog();
        let gate = catalog.by_name(GATE).unwrap();
        for key in ["state_l1", "state_l2"] {
            let point = gate.device.point(key).unwrap();
            assert_eq!(point.kind, Kind::Binary);
            assert_eq!(point.semantic, Semantic::OnOff);
        }
    }

    #[test]
    fn a_declared_relay_is_a_trigger_never_a_state() {
        let catalog = gate_catalog(&["z2m:0x00124b000000000a/state_l1"]);
        let gate = catalog.by_name(GATE).unwrap();
        let relay = gate.device.point("state_l1").unwrap();
        assert_eq!(
            relay.kind,
            Kind::Enum {
                values: vec![IMPULSE.into()]
            }
        );
        // An order to a machine: an agent always asks a person first.
        assert_eq!(relay.semantic, Semantic::Control);
        assert!(relay.access.write && !relay.access.read);
        // The other relay was not declared: it stays what Zigbee2MQTT says.
        let other = gate.device.point("state_l2").unwrap();
        assert_eq!(other.kind, Kind::Binary);
        assert_eq!(other.semantic, Semantic::OnOff);
    }

    #[test]
    fn a_trigger_fires_the_relay_and_ignores_its_fall_back() {
        let catalog = gate_catalog(&[
            "z2m:0x00124b000000000a/state_l1",
            "z2m:0x00124b000000000a/state_l2",
        ]);
        let gate = catalog.by_name(GATE).unwrap();
        assert_eq!(
            gate.encode("state_l1", &Value::from(IMPULSE)),
            json!({"state_l1": "ON"})
        );
        assert_eq!(
            gate.encode("state_l2", &Value::from(IMPULSE)),
            json!({"state_l2": "ON"})
        );
        // Real frame taken while relay 1 was pulsing: no state to show.
        let decoded: HashMap<_, _> = gate.decode(&state(GATE)).into_iter().collect();
        assert!(!decoded.contains_key("state_l1"));
        assert!(!decoded.contains_key("state_l2"));
        // The rest of the device keeps reading normally.
        assert_eq!(decoded["switch_type"], Value::from("momentary"));
    }

    #[test]
    fn a_trigger_is_never_polled() {
        let catalog = gate_catalog(&["z2m:0x00124b000000000a/state_l1"]);
        let request = catalog.by_name(GATE).unwrap().read_request().unwrap();
        assert!(request.get("state_l1").is_none());
        assert!(request.get("state_l2").is_some());
    }

    #[test]
    fn only_a_writable_switch_can_be_declared() {
        // A sensor reading, an unknown point and an unknown device change nothing.
        let catalog = gate_catalog(&[
            "z2m:0x00124b000000000a/linkquality",
            "z2m:0x00124b000000000a/inching_control_set.inching_control_1",
            "z2m:0x00124b000000000a/nope",
            "z2m:0x00124b00000000ff/state_l1",
        ]);
        let gate = catalog.by_name(GATE).unwrap();
        assert_eq!(gate.device.point("state_l1").unwrap().kind, Kind::Binary);
        assert!(matches!(
            gate.device.point("linkquality").unwrap().kind,
            Kind::Numeric { .. }
        ));
    }

    #[test]
    fn plug_points_are_typed() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000003").unwrap();
        let state = plug.device.point("state").unwrap();
        assert_eq!(state.kind, Kind::Binary);
        assert!(state.access.write);
        assert_eq!(state.semantic, Semantic::OnOff);

        let power = plug.device.point("power").unwrap();
        assert_eq!(power.unit, Some(Unit::Watt));
        assert_eq!(power.semantic, Semantic::Power);
        assert!(!power.access.write);

        let child_lock = plug.device.point("child_lock").unwrap();
        assert_eq!(child_lock.kind, Kind::Binary);
        assert!(child_lock.access.write);
    }

    #[test]
    fn real_state_messages_decode() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000009").unwrap();
        let values: HashMap<_, _> = plug
            .decode(&state("0x00124b0000000009"))
            .into_iter()
            .collect();
        assert_eq!(values["state"], Value::Bool(true));
        assert_eq!(
            values["child_lock"],
            Value::Bool(false),
            "UNLOCK is the off value"
        );
        assert_eq!(values["power"], Value::Int(1538));
        assert_eq!(values["indicator_mode"], Value::from("on"));

        let linky = catalog.by_name("0x00124b0000000001").unwrap();
        let values: HashMap<_, _> = linky
            .decode(&state("0x00124b0000000001"))
            .into_iter()
            .collect();
        assert_eq!(values["apparent_power"], Value::Int(1070));
        assert!(
            !values.contains_key("update"),
            "non-exposed objects are ignored"
        );

        let leak = catalog.by_name("0x00124b0000000007").unwrap();
        let values: HashMap<_, _> = leak
            .decode(&state("0x00124b0000000007"))
            .into_iter()
            .collect();
        assert_eq!(values["water_leak"], Value::Bool(false));
    }

    #[test]
    fn contact_is_true_when_closed() {
        // The real SNZB-04 fixture declares `value_on: false, value_off: true`.
        let catalog = catalog();
        let door = catalog.by_name("0x00124b0000000006").unwrap();
        let decode = |payload: Json| -> HashMap<_, _> {
            door.decode(payload.as_object().unwrap())
                .into_iter()
                .collect()
        };
        assert_eq!(
            decode(json!({"contact": true}))["contact"],
            Value::Bool(true),
            "Z2M reports `true` when the door is closed"
        );
        assert_eq!(
            decode(json!({"contact": false}))["contact"],
            Value::Bool(false),
            "Z2M reports `false` when the door is open"
        );
        // Other inverted-looking binaries keep going through value_on/value_off.
        assert_eq!(
            decode(json!({"battery_low": true}))["battery_low"],
            Value::Bool(true)
        );
    }

    #[test]
    fn commands_encode_to_native_values() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000003").unwrap();
        assert_eq!(
            plug.encode("state", &Value::Bool(true)),
            json!({"state": "ON"})
        );
        assert_eq!(
            plug.encode("child_lock", &Value::Bool(true)),
            json!({"child_lock": "LOCK"})
        );
        assert_eq!(
            plug.encode("indicator_mode", &Value::from("off")),
            json!({"indicator_mode": "off"})
        );

        let pir = catalog.by_name("0x00124b0000000008").unwrap();
        assert_eq!(
            pir.encode("illuminance_interval", &Value::Float(60.0)),
            json!({"illuminance_interval": 60})
        );
        assert_eq!(
            pir.device.point("illuminance_interval").unwrap().unit,
            Some(Unit::Minute)
        );
    }

    #[test]
    fn shutter_is_a_position_not_a_switch() {
        let catalog = catalog();
        let shutter = catalog.by_name("0x00124b0000000002").unwrap();
        let semantic = |key: &str| shutter.device.point(key).unwrap().semantic;

        assert_eq!(semantic("position"), Semantic::Position);
        // `state` is an order (OPEN / CLOSE / STOP), not an on/off switch.
        assert_eq!(semantic("state"), Semantic::Other);
        assert_eq!(semantic("moving"), Semantic::Other);
        assert!(
            shutter
                .device
                .points
                .iter()
                .all(|p| p.semantic != Semantic::OnOff),
            "nothing on a shutter reads as a switch"
        );

        let position = shutter.device.point("position").unwrap();
        assert!(position.access.write);
        assert_eq!(position.unit, Some(Unit::Percent));
        let Kind::Numeric { min, max, .. } = &position.kind else {
            panic!("position is numeric");
        };
        assert_eq!((*min, *max), (Some(0.0), Some(100.0)));
    }

    #[test]
    fn shutter_state_decodes_and_its_orders_encode() {
        let catalog = catalog();
        let shutter = catalog.by_name("0x00124b0000000002").unwrap();
        let values: HashMap<_, _> = shutter
            .decode(&state("0x00124b0000000002"))
            .into_iter()
            .collect();
        assert_eq!(values["position"], Value::Int(32));
        assert_eq!(values["state"], Value::from("OPEN"));
        assert_eq!(values["moving"], Value::from("STOP"));

        assert_eq!(
            shutter.encode("position", &Value::Float(50.0)),
            json!({"position": 50})
        );
        assert_eq!(
            shutter.encode("state", &Value::from("CLOSE")),
            json!({"state": "CLOSE"})
        );
    }

    #[test]
    fn config_category_wins_over_units_and_numeric_enums_round_trip() {
        let expose: RawExpose = serde_json::from_value(json!({
            "type": "climate",
            "features": [
                {"type": "numeric", "property": "local_temperature_calibration", "access": 7,
                 "unit": "°C", "category": "config"},
                {"type": "numeric", "property": "local_temperature", "access": 5, "unit": "°C"},
                {"type": "enum", "property": "mode", "access": 7, "values": [0, 1, 2]}
            ]
        }))
        .unwrap();
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        flatten(&expose, "", "", &mut points, &mut codecs);
        assert_eq!(points[0].semantic, Semantic::Config);
        assert_eq!(points[1].semantic, Semantic::Temperature);

        let entry = Entry {
            ieee: "0x2".into(),
            friendly_name: "trv".into(),
            mains: false,
            flame: false,
            device: Device {
                id: DeviceId::from("z2m:0x2"),
                instance: InstanceId::from("z2m"),
                native_name: "trv".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points,
            },
            codecs,
        };
        let decoded: HashMap<_, _> = entry
            .decode(json!({"mode": 1}).as_object().unwrap())
            .into_iter()
            .collect();
        assert_eq!(
            decoded["mode"],
            Value::from("1"),
            "matches the enum's text values"
        );
        assert_eq!(entry.encode("mode", &Value::from("2")), json!({"mode": 2}));
    }

    /// An entry built from one inline expose, as Z2M announces it.
    fn entry_of(expose: serde_json::Value) -> Entry {
        let expose: RawExpose = serde_json::from_value(expose).unwrap();
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        flatten(&expose, "", "", &mut points, &mut codecs);
        let flame = mark_flame(&mut points, &mut codecs);
        Entry {
            ieee: "0x3".into(),
            friendly_name: "bulb".into(),
            mains: true,
            flame,
            device: Device {
                id: DeviceId::from("z2m:0x3"),
                instance: InstanceId::from("z2m"),
                native_name: "bulb".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points,
            },
            codecs,
        }
    }

    fn light(brightness_max: u32) -> serde_json::Value {
        json!({"type": "light", "features": [
            {"type": "binary", "property": "state", "access": 7, "value_on": "ON", "value_off": "OFF"},
            {"type": "numeric", "property": "brightness", "access": 7,
             "value_min": 0, "value_max": brightness_max},
            {"type": "numeric", "property": "color_temp", "access": 7, "unit": "mired",
             "value_min": 153, "value_max": 500}
        ]})
    }

    #[test]
    fn a_zigbee_lock_is_a_lock_not_a_switch() {
        // Z2M's `lock` expose (zigbee-herdsman-converters `e.lock()`), as a real
        // door lock announces it in `bridge/devices`.
        let entry = entry_of(json!({
            "type": "lock",
            "features": [
                {"type": "binary", "name": "state", "label": "State", "property": "state",
                 "access": 7, "value_on": "LOCK", "value_off": "UNLOCK",
                 "description": "State of the lock"},
                {"type": "enum", "name": "lock_state", "label": "Lock state",
                 "property": "lock_state", "access": 1,
                 "values": ["not_fully_locked", "locked", "unlocked", "unknown"],
                 "description": "Actual state of the lock"}
            ]
        }));
        let state = entry
            .device
            .points
            .iter()
            .find(|p| &*p.key == "state")
            .unwrap();
        assert_eq!(state.semantic, Semantic::Lock);
        assert_eq!(state.kind, Kind::Binary);
        // `true` is locked, both ways on the wire.
        let decoded: HashMap<_, _> = entry
            .decode(json!({"state": "LOCK"}).as_object().unwrap())
            .into_iter()
            .collect();
        assert_eq!(decoded["state"], Value::Bool(true));
        assert_eq!(
            entry.encode("state", &Value::Bool(false)),
            json!({"state": "UNLOCK"})
        );
        // Whatever its name, the guard sees a lock.
        assert_eq!(
            moli_runtime::opener(&["Porte d'entrée"], &entry.device.points),
            moli_runtime::Opener::Lock
        );
        // A plain switch stays a switch.
        let plug = entry_of(json!({"type": "switch", "features": [
            {"type": "binary", "name": "state", "property": "state", "access": 7,
             "value_on": "ON", "value_off": "OFF"}]}));
        assert_eq!(plug.device.points[0].semantic, Semantic::OnOff);
        // A plug's child lock (LOCK/UNLOCK too) locks its button, not a door:
        // the plug stays a plug to the guard.
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000009").unwrap();
        let child_lock = plug.device.point("child_lock").unwrap();
        assert_ne!(child_lock.semantic, Semantic::Lock);
        assert_eq!(
            moli_runtime::opener(&["Prise salon"], &plug.device.points),
            moli_runtime::Opener::None
        );
    }

    #[test]
    fn a_position_is_an_opening_only_inside_a_cover() {
        // A servo's `position` is not a shutter.
        let servo = entry_of(json!({
            "type": "numeric", "property": "position", "access": 7,
            "unit": "%", "value_min": 0, "value_max": 100
        }));
        assert_eq!(
            servo.device.point("position").unwrap().semantic,
            Semantic::Other
        );
        // Inside a `cover` it is.
        let cover = entry_of(json!({"type": "cover", "features": [
            {"type": "enum", "property": "state", "access": 7, "values": ["OPEN", "CLOSE", "STOP"]},
            {"type": "numeric", "property": "position", "access": 7, "unit": "%",
             "value_min": 0, "value_max": 100}
        ]}));
        assert_eq!(
            cover.device.point("position").unwrap().semantic,
            Semantic::Position
        );
    }

    #[test]
    fn a_simple_motor_has_an_order_and_no_position() {
        let motor = entry_of(json!({"type": "cover", "features": [
            {"type": "enum", "property": "state", "access": 7, "values": ["OPEN", "CLOSE", "STOP"]}
        ]}));
        assert_eq!(
            motor.device.point("state").unwrap().semantic,
            Semantic::Other
        );
        assert!(
            motor
                .device
                .points
                .iter()
                .all(|p| p.semantic != Semantic::Position)
        );
    }

    #[test]
    fn zigbee_dimming_level_is_exposed_in_percent() {
        let bulb = entry_of(light(254));
        let brightness = bulb.device.point("brightness").unwrap();
        assert_eq!(brightness.semantic, Semantic::Brightness);
        assert_eq!(brightness.unit, Some(Unit::Percent));
        let Kind::Numeric { min, max, step } = &brightness.kind else {
            panic!("brightness is numeric");
        };
        assert_eq!((*min, *max, *step), (Some(0.0), Some(100.0), Some(1.0)));

        // The white temperature keeps the device's own scale.
        let Kind::Numeric { min, max, .. } = &bulb.device.point("color_temp").unwrap().kind else {
            panic!("color_temp is numeric");
        };
        assert_eq!((*min, *max), (Some(153.0), Some(500.0)));

        let read = |level: i64| {
            let values: HashMap<_, _> = bulb
                .decode(json!({"brightness": level}).as_object().unwrap())
                .into_iter()
                .collect();
            values["brightness"].clone()
        };
        assert_eq!(read(254), Value::Int(100));
        assert_eq!(read(127), Value::Int(50));
        assert_eq!(read(0), Value::Int(0));
        assert_eq!(read(1), Value::Int(1), "a lit bulb is never shown at 0 %");

        assert_eq!(
            bulb.encode("brightness", &Value::Int(100)),
            json!({"brightness": 254})
        );
        assert_eq!(
            bulb.encode("brightness", &Value::Float(50.0)),
            json!({"brightness": 127})
        );
        assert_eq!(
            bulb.encode("brightness", &Value::Int(1)),
            json!({"brightness": 3})
        );
    }

    #[test]
    fn every_whole_percent_survives_the_round_trip() {
        let bulb = entry_of(light(254));
        for percent in 1..=100_i64 {
            let wire = bulb.encode("brightness", &Value::Int(percent));
            let back: HashMap<_, _> = bulb.decode(wire.as_object().unwrap()).into_iter().collect();
            assert_eq!(back["brightness"], Value::Int(percent), "{percent} %");
        }
    }

    #[test]
    fn a_light_already_in_percent_is_left_alone() {
        let bulb = entry_of(light(100));
        let Kind::Numeric { max, .. } = &bulb.device.point("brightness").unwrap().kind else {
            panic!("brightness is numeric");
        };
        assert_eq!(*max, Some(100.0));
        let values: HashMap<_, _> = bulb
            .decode(json!({"brightness": 42}).as_object().unwrap())
            .into_iter()
            .collect();
        assert_eq!(values["brightness"], Value::Int(42));
        assert_eq!(
            bulb.encode("brightness", &Value::Int(42)),
            json!({"brightness": 42})
        );
    }

    #[test]
    fn only_listening_devices_are_read_on_demand() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000003").unwrap();
        let request = plug.read_request().unwrap();
        assert_eq!(request["state"], "");
        assert!(
            request.get("power").is_none(),
            "power is not gettable on this plug"
        );

        let smoke = catalog.by_name("0x00124b0000000004").unwrap();
        assert!(
            smoke.read_request().is_none(),
            "sleepy devices are never polled"
        );
    }

    #[test]
    fn a_colour_lamp_takes_a_colour_and_keeps_its_whites() {
        // Innr RB 287 C, as Zigbee2MQTT 2.14 exposes it.
        let lamp = entry_of(json!({"type": "light", "features": [
            {"type": "binary", "property": "state", "access": 7, "value_on": "ON", "value_off": "OFF"},
            {"type": "numeric", "property": "brightness", "access": 7, "value_min": 0, "value_max": 254},
            {"type": "numeric", "property": "color_temp", "access": 7, "unit": "mired", "value_min": 153, "value_max": 556},
            {"type": "composite", "name": "color_xy", "property": "color", "access": 7, "features": [
                {"type": "numeric", "property": "x", "access": 7},
                {"type": "numeric", "property": "y", "access": 7}
            ]},
            {"type": "composite", "name": "color_hs", "property": "color", "access": 7, "features": [
                {"type": "numeric", "property": "hue", "access": 7},
                {"type": "numeric", "property": "saturation", "access": 7}
            ]}
        ]}));
        let color = lamp.device.point("color").expect("a colour point");
        assert!(color.access.write);
        assert_eq!(color.kind, Kind::Text);
        assert_eq!(color.semantic, Semantic::Color);
        assert_eq!(
            lamp.device
                .points
                .iter()
                .filter(|p| &*p.key == "color")
                .count(),
            1,
            "one colour, whatever the number of colour spaces"
        );
        assert!(lamp.device.point("color_temp").unwrap().access.write);

        let (x, y) = hex_to_xy("#ff0000").unwrap();
        assert_eq!(
            lamp.encode("color", &Value::from("#ff0000")),
            json!({"color": {"x": x, "y": y}})
        );

        let decode = |payload: Json| -> HashMap<Arc<str>, Value> {
            lamp.decode(payload.as_object().unwrap())
                .into_iter()
                .collect()
        };
        let red = decode(json!({"state": "ON", "color_mode": "xy", "color_temp": 370,
            "color": {"x": x, "y": y, "hue": 0, "saturation": 100}}));
        assert_eq!(red["color"], Value::from(xy_to_hex(x, y).unwrap().as_str()));
        assert_eq!(red["color_temp"], Value::Null, "in colour mode, no white");
        let warm = decode(
            json!({"state": "ON", "color_mode": "color_temp", "color_temp": 370,
            "color": {"x": 0.4599, "y": 0.4106}}),
        );
        assert_eq!(warm["color_temp"], Value::Int(370));
    }

    fn colour_lamp(effect: Option<serde_json::Value>) -> Entry {
        let mut features = vec![
            json!({"type": "binary", "property": "state", "access": 7, "value_on": "ON", "value_off": "OFF"}),
            json!({"type": "numeric", "property": "brightness", "access": 7, "value_min": 0, "value_max": 254}),
            json!({"type": "composite", "name": "color_xy", "property": "color", "access": 7, "features": [
                {"type": "numeric", "property": "x", "access": 7},
                {"type": "numeric", "property": "y", "access": 7}
            ]}),
        ];
        features.extend(effect);
        entry_of(json!({"type": "light", "features": features}))
    }

    fn effects(entry: &Entry) -> Vec<&str> {
        match &entry.device.point("effect").expect("an effect point").kind {
            Kind::Enum { values } => values.iter().map(|v| &**v).collect(),
            other => panic!("effect is an enum, not {other:?}"),
        }
    }

    #[test]
    fn a_colour_lamp_can_play_the_flame() {
        // Its own Zigbee effects stay, the flame comes after them.
        let strip = colour_lamp(Some(json!(
            {"type": "enum", "property": "effect", "access": 2, "values": ["blink", "breathe"]}
        )));
        assert!(strip.flame);
        assert_eq!(effects(&strip), ["blink", "breathe", "no_effect", "flame"]);
        let effect = strip.device.point("effect").unwrap();
        assert!(effect.access.read && effect.access.write);
        // A lamp without any effect gets the point.
        let bulb = colour_lamp(None);
        assert!(bulb.flame);
        assert_eq!(effects(&bulb), ["no_effect", "flame"]);
        // A white lamp has no flame: nothing is added.
        let white = entry_of(light(254));
        assert!(!white.flame);
        assert!(white.device.point("effect").is_none());
    }

    #[test]
    fn a_fade_of_the_flame_is_one_order() {
        let strip = colour_lamp(None);
        let frame = Frame {
            brightness: 22.0,
            hue: 28,
            saturation: 90,
            transition: std::time::Duration::from_millis(1500),
        };
        assert_eq!(
            strip.flame_frame(&frame, false),
            json!({"brightness": 56, "color": {"hue": 28, "saturation": 90}, "transition": 1.5})
        );
        assert_eq!(
            strip.flame_frame(&frame, true),
            json!({"state": "ON", "brightness": 56, "color": {"hue": 28, "saturation": 90}, "transition": 1.5})
        );
        assert_eq!(
            strip.flame_rest(22.0),
            json!({"brightness": 56, "transition": 1})
        );
    }

    #[test]
    fn composite_keys_round_trip() {
        let expose: RawExpose = serde_json::from_value(json!({
            "type": "light",
            "features": [
                {"type": "binary", "property": "state", "access": 7, "value_on": "ON", "value_off": "OFF"},
                {"type": "composite", "property": "color", "features": [
                    {"type": "numeric", "property": "x", "access": 7},
                    {"type": "numeric", "property": "y", "access": 7}
                ]}
            ]
        }))
        .unwrap();
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        flatten(&expose, "", "", &mut points, &mut codecs);
        let keys: Vec<_> = points.iter().map(|p| &*p.key).collect();
        assert_eq!(keys, ["state", "color.x", "color.y"]);
        assert!(points[0].access.write);
        assert!(
            !points[1].access.write,
            "composite members are read-only: Z2M rejects partial writes"
        );

        let entry = Entry {
            ieee: "0x1".into(),
            friendly_name: "lamp".into(),
            mains: true,
            flame: false,
            device: Device {
                id: DeviceId::from("z2m:0x1"),
                instance: InstanceId::from("z2m"),
                native_name: "lamp".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points,
            },
            codecs,
        };
        assert_eq!(
            entry.encode("color.x", &Value::Float(0.3)),
            json!({"color": {"x": 0.3}})
        );
        let payload = json!({"state": "OFF", "color": {"x": 0.31, "y": 0.32}});
        let decoded: HashMap<_, _> = entry
            .decode(payload.as_object().unwrap())
            .into_iter()
            .collect();
        assert_eq!(decoded["state"], Value::Bool(false));
        assert_eq!(decoded["color.y"], Value::Float(0.32));
    }
}

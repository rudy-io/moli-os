//! Pure translation between the zwave-js-ui gateway model and Moli's. No IO.
//!
//! The gateway exposes every Z-Wave node through its `getNodes` API: a stable
//! node id (identity), a name and an optional location (topic routing only,
//! may be renamed at any time), and a `values` collection (command class,
//! endpoint, property, optional property key, metadata). Flattening that
//! through a small `(command class, property[, property key])` table gives
//! typed points for the v1 surface (on/off, level — `brightness` in percent
//! for a declared dimmer — battery, sensors, power) without one line of
//! device-specific code.
//!
//! The parser is tolerant of both the synthetic fixture layout
//! (`nodeId`/`location`/`endpointIndex`, `values` as a list,
//! `minimum`/`maximum`) and the real gateway layout (`id`/`loc`/`endpoint`,
//! `values` as a map keyed by `cc-ep-property[-key]`, `min`/`max`, numeric
//! properties for CC 112, `propertyKey` for meter values).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::Value as Json;

/// The write target of a point on the gateway:
/// `{base}/{cc}/{endpoint}/{property}/set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetTarget {
    pub cc: u8,
    pub endpoint: u8,
    pub property: &'static str,
}

/// One Z-Wave node with everything needed to decode its value topics and
/// encode its commands.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The node's stable identity (zwave node id); `None` in tolerant
    /// accumulation mode, where the node was only seen through its topics.
    pub node_id: Option<u32>,
    /// The node's current name (sanitized): topic routing only, never an
    /// identity.
    pub name: String,
    /// The node's location prefix (sanitized); empty when the node has none.
    pub loc: String,
    pub device: Device,
    pub on_target: Option<SetTarget>,
    pub level_target: Option<SetTarget>,
    /// The raw maximum of a dimmer's level (99 on a real gateway): its
    /// `brightness` is exposed in percent and converted both ways. `None`
    /// for a neutral `level`, which keeps the device's own scale.
    pub level_max: Option<f64>,
}

/// Multilevel Switch (CC 38) levels run 0–99 (zwave-js `ValueMetadata.Level`);
/// 255 is not a level but "restore the previous one".
const LEVEL_MAX: f64 = 99.0;

impl Entry {
    /// The topic base of this node: `{loc}/{name}` or just `{name}` —
    /// mirrors how the gateway builds its topics.
    #[must_use]
    pub fn base(&self) -> String {
        if self.loc.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.loc, self.name)
        }
    }
}

/// All the nodes of one gateway, indexed by device id (identity) and by
/// topic base (routing, may change at any time).
#[derive(Debug, Default)]
pub struct Catalog {
    entries: HashMap<DeviceId, Entry>,
    by_base: HashMap<String, DeviceId>,
}

impl Catalog {
    /// Rebuilds the catalog from a `getNodes` payload: either a bare node
    /// list or the gateway envelope `{success, result, …}`. Nodes without a
    /// node id, the broadcast (virtual) node, the controller (id < 2), failed
    /// nodes and nodes with no mappable point are skipped, never a parse
    /// failure (tolerant parse; the run loop falls back to topic
    /// accumulation when the API refuses).
    pub fn from_get_nodes(instance: &InstanceId, payload: &[u8]) -> serde_json::Result<Catalog> {
        let json: Json = serde_json::from_slice(payload)?;
        let nodes: Vec<Json> = match &json {
            Json::Array(nodes) => nodes.clone(),
            Json::Object(envelope) => {
                if envelope.get("success").and_then(Json::as_bool) == Some(false) {
                    return Err(refuse_err("gateway refused getNodes (success: false)"));
                }
                match envelope.get("result") {
                    Some(Json::Array(nodes)) => nodes.clone(),
                    Some(Json::Null) | None => Vec::new(),
                    Some(_) => return Err(refuse_err("getNodes result is not a node list")),
                }
            }
            _ => {
                return Err(refuse_err(
                    "getNodes payload must be a node list or a gateway envelope",
                ));
            }
        };
        let mut entries: HashMap<DeviceId, Entry> = HashMap::new();
        let mut by_base: HashMap<String, DeviceId> = HashMap::new();
        for node in &nodes {
            let Ok(raw) = RawNode::deserialize(node) else {
                continue;
            };
            // Gateway bookkeeping, never devices: the broadcast (virtual)
            // node and failed nodes.
            if raw.virtual_node || raw.failed {
                continue;
            }
            // The controller (node 1 in Z-Wave) is not a device.
            let Some(node_id) = raw.node_id.filter(|id| *id >= 2) else {
                continue;
            };
            let Some(entry) = entry_from_node(instance, &raw, node_id) else {
                continue;
            };
            let id = entry.device.id.clone();
            // First node wins a base clash: bases are routing, not identity.
            by_base.entry(entry.base()).or_insert(id.clone());
            entries.insert(id, entry);
        }
        Ok(Self { entries, by_base })
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    #[must_use]
    pub fn by_base(&self, base: &str) -> Option<&Entry> {
        let id = self.by_base.get(base)?;
        self.entries.get(id)
    }

    #[must_use]
    pub fn by_id(&self, id: &DeviceId) -> Option<&Entry> {
        self.entries.get(id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Finds the entry whose topic base is the prefix of `rest`
    /// (`{base}/{cc}/{endpoint}/{property}[/{propertyKey}]`). The base may
    /// carry a location segment, so a plain first-segment lookup would fail;
    /// when one base is a prefix of another, the longest base wins.
    #[must_use]
    pub fn entry_for_value_topic(&self, rest: &str) -> Option<&Entry> {
        let mut best: Option<(&Entry, usize)> = None;
        for entry in self.entries.values() {
            let base = entry.base();
            let is_prefix = rest
                .strip_prefix(base.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'));
            if is_prefix && best.is_none_or(|(_, len)| base.len() > len) {
                best = Some((entry, base.len()));
            }
        }
        best.map(|(entry, _)| entry)
    }

    /// Tolerant accumulation mode (the `getNodes` API is unavailable):
    /// ensures the entry named `name` carries the point mapped by
    /// `(cc, property[, property_key])` and returns it, creating the device
    /// the first time the node is seen. Unknown `(cc, property)` pairs never
    /// create anything. The identity is the node name — a late `getNodes`
    /// answer replaces those devices with the node-id-identity ones.
    pub fn upsert_accumulated(
        &mut self,
        instance: &InstanceId,
        name: &str,
        cc: u8,
        endpoint: u8,
        property: &str,
        property_key: Option<&str>,
    ) -> Option<&mut Entry> {
        let kind = map_value(cc, property, property_key)?;
        let name = sanitize(name);
        let id = DeviceId::new(instance, &name);
        let entry = self.entries.entry(id.clone()).or_insert_with(|| Entry {
            node_id: None,
            name: name.clone(),
            loc: String::new(),
            device: Device {
                id: id.clone(),
                instance: instance.clone(),
                native_name: name.clone().into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: Vec::new(),
            },
            on_target: None,
            level_target: None,
            level_max: None,
        });
        self.by_base.entry(name).or_insert(entry.device.id.clone());
        // A CC 38 `currentValue` is also the level (see `Entry::decode`): a
        // dimmer that never reports a target still gets its level point.
        let kinds: &[PointKind] = if cc == 38 && kind == PointKind::On {
            &[PointKind::On, PointKind::Level]
        } else {
            std::slice::from_ref(&kind)
        };
        for &kind in kinds {
            if !entry
                .device
                .points
                .iter()
                .any(|p| p.key.as_ref() == kind.key())
            {
                entry.device.points.push(PointSpec {
                    key: kind.key().into(),
                    label: kind.label().into(),
                    kind: kind.kind(None, None),
                    access: Access {
                        read: true,
                        write: false,
                    },
                    unit: kind.unit(),
                    // No device class is available in accumulation mode, so
                    // the level point can never be called a light: keep it
                    // neutral, under the `level` key (never `brightness`).
                    semantic: if matches!(kind, PointKind::Level) {
                        Semantic::Other
                    } else {
                        kind.semantic()
                    },
                });
            }
            match kind {
                PointKind::On if entry.on_target.is_none() => {
                    entry.on_target = Some(SetTarget {
                        cc,
                        endpoint,
                        property: switch_target_property(cc),
                    });
                }
                PointKind::Level if entry.level_target.is_none() => {
                    entry.level_target = Some(SetTarget {
                        cc,
                        endpoint,
                        property: "targetValue",
                    });
                }
                _ => {}
            }
        }
        refresh_write_access(entry);
        Some(entry)
    }
}

/// Builds a `serde_json::Error` with a custom message (serde_json exposes no
/// public constructor, so route through an `io::Error`).
fn refuse_err(message: &str) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

impl Entry {
    /// The key of this entry's CC 38 level point: `brightness` when the node
    /// is a declared dimmer (named so at catalog build time), `level`
    /// otherwise (shutters, accumulated nodes without a device class).
    fn level_point_key(&self) -> &'static str {
        if self.device.point("brightness").is_some() {
            "brightness"
        } else {
            "level"
        }
    }

    /// Decodes one value topic payload. `rest` is the full topic suffix after
    /// the base topic: `{base}/{cc}/{endpoint}/{property}` or the
    /// five-segment form with a trailing `{propertyKey}` (it decides the
    /// mapping for meter values, CC 50). The base must be this entry's base;
    /// unknown `(cc, property)` pairs and unreadable payloads are ignored
    /// (never an error).
    #[must_use]
    pub fn decode(&self, rest: &str, payload: &[u8]) -> Vec<(Arc<str>, Value)> {
        let base = self.base();
        let Some(after_base) = rest
            .strip_prefix(base.as_str())
            .and_then(|s| s.strip_prefix('/'))
        else {
            return Vec::new();
        };
        let mut parts = after_base.splitn(4, '/');
        let Some(cc) = parts.next().and_then(|s| s.parse::<u8>().ok()) else {
            return Vec::new();
        };
        // The endpoint segment must be present and numeric (otherwise the
        // topic is foreign); only the level depends on its value.
        let Some(endpoint) = parts.next().and_then(|s| s.parse::<u8>().ok()) else {
            return Vec::new();
        };
        let Some(property) = parts.next().filter(|p| !p.is_empty()) else {
            return Vec::new();
        };
        // The optional fifth segment (propertyKey) is tolerated; it decides
        // the mapping for meter values (CC 50).
        let property_key = parts.next().filter(|k| !k.is_empty());
        let Some(kind) = map_value(cc, property, property_key) else {
            return Vec::new();
        };
        // CC 38 `targetValue` is only the last order: 255 after an `on`
        // (restore the previous level), and a device that never reports a
        // target leaves it there. The level is read from `currentValue`.
        if kind == PointKind::Level {
            return Vec::new();
        }
        // A CC 38 `currentValue` is also the real level, read on the
        // endpoint it is written to (see `collect_points`).
        let level_key = (cc == 38 && self.level_target.is_some_and(|t| t.endpoint == endpoint))
            .then(|| self.level_point_key())
            .filter(|key| self.device.point(key).is_some());
        let has_point = self.device.point(kind.key()).is_some();
        if !has_point && level_key.is_none() {
            return Vec::new();
        }
        let Ok(json) = serde_json::from_slice::<Json>(payload) else {
            tracing::debug!(entry = %self.name, "unreadable zwave value payload ignored");
            return Vec::new();
        };
        // The real gateway wraps value payloads in a `{time, value}`
        // envelope; bare values (fixtures, older gateways) pass through.
        let value_json = json.get("value").unwrap_or(&json);
        let mut values = Vec::new();
        if has_point {
            let value = match (kind, value_json) {
                // CC 32/37/38 switch states: booleans pass through; CC 38
                // numbers are levels (0 = off, anything else on).
                (PointKind::On, Json::Bool(b)) => Value::Bool(*b),
                (PointKind::On, Json::Number(n)) => {
                    Value::Bool(n.as_f64().is_some_and(|v| v > 0.0))
                }
                _ => Value::from_json(value_json),
            };
            values.push((kind.key().into(), value));
        }
        // A dimmer's `brightness` is a percent; a neutral `level` keeps the
        // device's scale. `null` (level unknown) is skipped.
        if let Some((key, level)) = level_key.zip(value_json.as_f64()) {
            let value = match self.level_max {
                Some(max) => Value::Int(level_to_percent(level, max)),
                None => Value::from_json(value_json),
            };
            values.push((key.into(), value));
        }
        values
    }

    /// Encodes a validated write as `(command topic suffix, raw JSON
    /// payload)`. Only `on` (boolean) and the level (number: a dimmer's
    /// `brightness` in percent, converted to 0..`level_max`; a neutral
    /// `level` as is) are writable; everything else is refused (defensive:
    /// the hub already checks `access.write`).
    pub fn encode(&self, key: &str, value: &Value) -> anyhow::Result<(String, String)> {
        let (target, numeric) = match key {
            "on" => (self.on_target, false),
            "level" | "brightness" => (self.level_target, true),
            other => {
                return Err(anyhow::anyhow!(moli_i18n::tr!(
                    "pilotes.zwave.point_lu_seul",
                    point = other
                )));
            }
        };
        let Some(target) = target else {
            return Err(anyhow::anyhow!(moli_i18n::tr!(
                "pilotes.zwave.point_sans_cible",
                point = key
            )));
        };
        let level = self.level_max.filter(|_| numeric).zip(value.as_f64());
        let payload = if let Some((max, percent)) = level {
            // A dimmer's `brightness` is a percent; the gateway wants its
            // level (0–99).
            percent_to_level(percent, max).to_string()
        } else if numeric {
            match value {
                Value::Int(i) => i.to_string(),
                // zwave-js wants `80`, not `80.0`: exact integers stay integers.
                Value::Float(f) if f.fract() == 0.0 && f.abs() < 9e15 => {
                    #[allow(clippy::cast_possible_truncation)]
                    let i: i64 = *f as i64;
                    i.to_string()
                }
                Value::Float(f) => f.to_string(),
                _ => {
                    return Err(anyhow::anyhow!(moli_i18n::tr!(
                        "pilotes.zwave.niveau_attend_nombre"
                    )));
                }
            }
        } else {
            let Value::Bool(on) = value else {
                return Err(anyhow::anyhow!(moli_i18n::tr!(
                    "pilotes.zwave.on_attend_booleen"
                )));
            };
            // Basic (32) `currentValue` and Switch Binary (37) `targetValue`
            // take a boolean; Multilevel (38) `targetValue` takes a level
            // 0-99, or 255 to restore the previous one — `true` alone would
            // be refused at the gateway: send 0/255.
            if target.cc == 38 {
                if *on {
                    "255".to_string()
                } else {
                    "0".to_string()
                }
            } else {
                on.to_string()
            }
        };
        Ok((
            format!(
                "{}/{}/{}/{}/set",
                self.base(),
                target.cc,
                target.endpoint,
                target.property
            ),
            payload,
        ))
    }
}

/// The v1 semantic surface, mapped from a `(command class, property)` pair,
/// with an optional property key (meter values, CC 50).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointKind {
    On,
    Level,
    Battery,
    Temperature,
    Humidity,
    Power,
    Current,
    Voltage,
}

impl PointKind {
    const fn key(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Level => "level",
            Self::Battery => "battery",
            Self::Temperature => "temperature",
            Self::Humidity => "humidity",
            Self::Power => "power",
            Self::Current => "current",
            Self::Voltage => "voltage",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::On => "On",
            Self::Level => "Level",
            Self::Battery => "Battery level",
            Self::Temperature => "Temperature",
            Self::Humidity => "Humidity",
            Self::Power => "Power",
            Self::Current => "Current",
            Self::Voltage => "Voltage",
        }
    }

    fn unit(self) -> Option<Unit> {
        match self {
            Self::On => None,
            Self::Level | Self::Battery | Self::Humidity => Some(Unit::Percent),
            Self::Temperature => Some(Unit::Celsius),
            Self::Power => Some(Unit::Watt),
            Self::Current => Some(Unit::Ampere),
            Self::Voltage => Some(Unit::Volt),
        }
    }

    fn semantic(self) -> Semantic {
        match self {
            Self::On => Semantic::OnOff,
            Self::Level => Semantic::Brightness,
            Self::Battery => Semantic::Battery,
            Self::Temperature => Semantic::Temperature,
            Self::Humidity => Semantic::Humidity,
            Self::Power => Semantic::Power,
            Self::Current => Semantic::Current,
            Self::Voltage => Semantic::Voltage,
        }
    }

    fn kind(self, min: Option<f64>, max: Option<f64>) -> Kind {
        match self {
            Self::On => Kind::Binary,
            // Switch levels are bounded 0-99, batteries and relative
            // humidity 0-100 by the Z-Wave spec; the gateway bounds win when
            // given.
            Self::Level => Kind::Numeric {
                min: min.or(Some(0.0)),
                max: max.or(Some(LEVEL_MAX)),
                step: None,
            },
            Self::Battery | Self::Humidity => Kind::Numeric {
                min: min.or(Some(0.0)),
                max: max.or(Some(100.0)),
                step: None,
            },
            Self::Temperature | Self::Power | Self::Current | Self::Voltage => Kind::Numeric {
                min,
                max,
                step: None,
            },
        }
    }
}

/// The semantic of a CC 38 `level` point, from the gateway's device class:
/// `Brightness` only when the node is declared a light — generic
/// "Multilevel Switch" (0x11) with specific "Multilevel Power Switch"
/// (0x01, Light Dimmer) or "Tunable Color Switch" (0x02, Color Switch).
/// Shutter, gate and motor modules also expose CC 38 but are not lights: a
/// `Brightness` point would make the guard treat the device as a light and
/// skip its name check entirely, so unknown, absent or non-light classes
/// stay neutral (`Other`) and the name check still applies.
fn level_semantic(device_class: Option<&RawDeviceClass>) -> Semantic {
    match device_class {
        Some(dc) if dc.generic == 0x11 && matches!(dc.specific, 0x01 | 0x02) => {
            Semantic::Brightness
        }
        _ => Semantic::Other,
    }
}

/// The v1 mapping — the single source of point shapes and writeability:
/// only `on` and `level` are writable. Everything else is ignored.
fn map_value(cc: u8, property: &str, property_key: Option<&str>) -> Option<PointKind> {
    match (cc, property) {
        (32 | 37 | 38, "currentValue") => Some(PointKind::On),
        (38, "targetValue") => Some(PointKind::Level),
        // Battery: the classic CC 53, or CC 128 (`level`, newer sensors).
        (53, "batteryLevel") | (128, "level") => Some(PointKind::Battery),
        // CC 31/49 sensors: the synthetic fixture spells the properties with
        // an underscore; the real gateway spells them with a space.
        (31 | 49, "Air_temperature" | "Air temperature") => Some(PointKind::Temperature),
        (31 | 49, "Relative_humidity" | "Relative humidity") => Some(PointKind::Humidity),
        (50, "power") => Some(PointKind::Power),
        // Meter CC: the value is disambiguated by a property key — only the
        // instantaneous watt reading (key 66049) is in the v1 surface; the
        // accumulated kWh counter (key 65537) is not.
        (50, "value") => matches!(property_key, Some("66049")).then_some(PointKind::Power),
        (50, "current") => Some(PointKind::Current),
        (50, "voltage") => Some(PointKind::Voltage),
        _ => None,
    }
}

/// The write target for `on`, per command class: Basic (32) writes
/// `currentValue` directly (its only writable value, confirmed on the real
/// gateway), Switch Binary (37) and Multilevel (38) write `targetValue`.
const fn switch_target_property(cc: u8) -> &'static str {
    if cc == 32 {
        "currentValue"
    } else {
        "targetValue"
    }
}

/// Mirrors the gateway's topic sanitization: whitespace becomes `_`, and
/// characters outside `[A-Za-z0-9-_/]` plus the Latin-1 accented letters
/// (`À-Ö`, `Ø-ö`, `ø-ÿ`) are dropped. zwave-js-ui keeps these accented
/// letters in its topics, so they must survive here for bases to match.
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .filter(|&c| is_topic_char(c))
        .collect()
}

fn is_topic_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '-' | '/')
        || matches!(
            c,
            '\u{00C0}'..='\u{00D6}' | '\u{00D8}'..='\u{00F6}' | '\u{00F8}'..='\u{00FF}'
        )
}

/// A `property` is a string for most command classes but a number for CC 112
/// configuration parameters; both are keyed as strings.
fn property_as_str(property: &Json) -> Option<String> {
    match property {
        Json::String(s) => Some(s.clone()),
        Json::Number(n) => n.as_i64().map(|v| v.to_string()),
        _ => None,
    }
}

/// A property key (CC 50 meter values) is a number in the gateway payload;
/// it is compared as a string against the mapping.
fn property_key_as_str(key: &Json) -> Option<String> {
    match key {
        Json::Number(n) => n.as_u64().map(|v| v.to_string()),
        Json::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Builds one `Entry` from a `getNodes` node (tolerant: fields may be
/// missing, unknown `(cc, property)` values are skipped). Returns `None`
/// when the node has no mappable point (dead node, configuration-only
/// device): it never becomes a device.
fn entry_from_node(instance: &InstanceId, raw: &RawNode, node_id: u32) -> Option<Entry> {
    // Raw gateway name (trimmed) — kept verbatim for `native_name`, which the
    // guard reads lock/gate words from (a sanitized name would break the
    // "Gâche" → lock match). Unnamed nodes route through the gateway's own
    // fallback `nodeID_<id>` (its NODE_PREFIX).
    let raw_name = raw.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let name = raw_name
        .map(sanitize)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("nodeID_{node_id}"));
    let loc = raw
        .location
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(sanitize)
        .unwrap_or_default();

    let (points, level_target, level_max) = collect_points(raw);

    // No mappable point: the node is outside the v1 surface (dead node,
    // configuration-only device) — never a device.
    if points.is_empty() {
        return None;
    }

    let on_target = find_on_target(raw);
    let device = Device {
        id: DeviceId::new(instance, &node_id.to_string()),
        instance: instance.clone(),
        // Raw gateway name for the guard (accents/spaces intact); the
        // sanitized form stays in `Entry.name` for topic routing only.
        native_name: raw_name.unwrap_or(name.as_str()).to_owned().into(),
        manufacturer: raw
            .manufacturer
            .clone()
            .filter(|s| !s.is_empty())
            .map(Into::into),
        model: raw
            .product_label
            .clone()
            .filter(|s| !s.is_empty())
            .map(Into::into),
        description: None,
        native_room: raw
            .location
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Into::into),
        members: Vec::new(),
        points,
    };
    let mut entry = Entry {
        node_id: Some(node_id),
        name,
        loc,
        device,
        on_target,
        level_target,
        level_max,
    };
    refresh_write_access(&mut entry);
    Some(entry)
}

/// Flattens the node's values into the v1 points: in `values` order, the
/// first value that creates a key wins the point; duplicates are skipped.
/// Also returns the `level` write target (CC 38 `targetValue`) when present,
/// and the raw maximum of a dimmer's level (see `Entry::level_max`).
fn collect_points(raw: &RawNode) -> (Vec<PointSpec>, Option<SetTarget>, Option<f64>) {
    let mut points: Vec<PointSpec> = Vec::new();
    let mut level_target: Option<SetTarget> = None;
    let mut level_max: Option<f64> = None;
    for value in raw.values.iter() {
        let Some(cc) = value.command_class else {
            continue;
        };
        let Some(property) = value.property.as_ref().and_then(property_as_str) else {
            continue;
        };
        if property.is_empty() {
            continue;
        }
        let property_key = value.property_key.as_ref().and_then(property_key_as_str);
        let Some(kind) = map_value(cc, &property, property_key.as_deref()) else {
            continue;
        };
        // A dimmer's level is stored under `brightness`, not `level`: only
        // the first CC 38 `targetValue` makes the level point.
        let taken = match kind {
            PointKind::Level => level_target.is_some(),
            other => points.iter().any(|p| p.key.as_ref() == other.key()),
        };
        if taken {
            continue;
        }
        let (min, max) = value_bounds(value);
        let unit = if kind == PointKind::Temperature {
            value
                .unit
                .as_deref()
                .filter(|u| !u.is_empty())
                .or_else(|| value.metadata.as_ref().and_then(|m| m.unit.as_deref()))
                .and_then(Unit::parse)
                .or(Some(Unit::Celsius))
        } else {
            kind.unit()
        };
        // A CC 38 level is only a light level when the node's device class
        // says so (see `level_semantic`). A real dimmer is exposed under the
        // standard `brightness` key (Hue and Dirigera use the same key) so
        // the maison UI and ambiances treat it as a light; every other CC 38
        // node (shutter, gate, unknown class) keeps `level` and stays
        // neutral so the guard's name check still applies.
        let (key, label, semantic) = if matches!(kind, PointKind::Level) {
            let semantic = level_semantic(raw.device_class.as_ref());
            let is_brightness = semantic == Semantic::Brightness;
            (
                if is_brightness { "brightness" } else { "level" },
                if is_brightness { "Brightness" } else { "Level" },
                semantic,
            )
        } else {
            (kind.key(), kind.label(), kind.semantic())
        };
        // A dimmer's brightness is a percent like every other light's
        // (dashboard, ambiances, assistant): its 0-99 level is converted
        // both ways. A neutral `level` keeps the gateway's bounds.
        let kind_spec = if semantic == Semantic::Brightness {
            level_max = Some(
                max.filter(|m| *m > 0.0)
                    .map_or(LEVEL_MAX, |m| m.min(LEVEL_MAX)),
            );
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0),
            }
        } else {
            kind.kind(min, max)
        };
        points.push(PointSpec {
            key: key.into(),
            label: label.into(),
            kind: kind_spec,
            access: Access {
                read: true,
                write: false,
            },
            unit,
            semantic,
        });
        if matches!(kind, PointKind::Level) {
            level_target = Some(SetTarget {
                cc,
                endpoint: value.endpoint_index.unwrap_or(0),
                property: "targetValue",
            });
        }
    }
    (points, level_target, level_max)
}

/// `on` is commanded through the first switch command class present,
/// priority Basic (32) → Switch Binary (37) → Multilevel (38).
fn find_on_target(raw: &RawNode) -> Option<SetTarget> {
    [32u8, 37, 38].iter().find_map(|&cc| {
        raw.values
            .iter()
            .find(|v| {
                v.command_class == Some(cc)
                    && v.property.as_ref().and_then(property_as_str).as_deref()
                        == Some("currentValue")
            })
            .map(|v| SetTarget {
                cc,
                endpoint: v.endpoint_index.unwrap_or(0),
                property: switch_target_property(cc),
            })
    })
}

fn refresh_write_access(entry: &mut Entry) {
    for point in &mut entry.device.points {
        point.access.write = match &*point.key {
            "on" => entry.on_target.is_some(),
            "level" | "brightness" => entry.level_target.is_some(),
            _ => false,
        };
    }
}

/// A Multilevel Switch level (0..`max`) as a whole percent. A lit level is
/// never shown as 0 %.
///
/// 101 percents share 100 levels, so one percent cannot come back as sent.
/// Rounding down here and up in `percent_to_level` gives every percent back
/// but 99 % (read back as 100 %); rounding both ways would lose 50 %.
#[allow(clippy::cast_possible_truncation)]
fn level_to_percent(level: f64, max: f64) -> i64 {
    let percent = ((level * 100.0 / max).floor() as i64).clamp(0, 100);
    if level > 0.0 { percent.max(1) } else { percent }
}

/// A percent as a Multilevel Switch level (0..`max`), rounded up (see
/// `level_to_percent`).
#[allow(clippy::cast_possible_truncation)]
fn percent_to_level(percent: f64, max: f64) -> i64 {
    (percent.clamp(0.0, 100.0) * max / 100.0).ceil() as i64
}

/// Bounds live top-level (switch values) or in `metadata` (sensor values);
/// either location is read.
fn value_bounds(value: &RawValue) -> (Option<f64>, Option<f64>) {
    let min = value
        .minimum
        .or(value.metadata.as_ref().and_then(|m| m.minimum));
    let max = value
        .maximum
        .or(value.metadata.as_ref().and_then(|m| m.maximum));
    (min, max)
}

/// `values` is a list in the synthetic fixtures, a map keyed by topic
/// (`cc-ep-property[-key]`) in the real gateway. Both parse into the same
/// `RawValue` items; the map iterates in key order, which keeps the
/// "first winner" deterministic across runs.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawValues {
    List(Vec<RawValue>),
    Map(BTreeMap<String, RawValue>),
}

impl Default for RawValues {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl RawValues {
    fn iter(&self) -> Box<dyn Iterator<Item = &RawValue> + '_> {
        match self {
            Self::List(values) => Box::new(values.iter()),
            Self::Map(values) => Box::new(values.values()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawNode {
    /// Real gateways say `id`; the synthetic fixture says `nodeId`.
    #[serde(rename = "nodeId", alias = "id", default)]
    node_id: Option<u32>,
    #[serde(default)]
    name: Option<String>,
    /// Room prefix of the topic base; the real gateway spells it `loc`.
    #[serde(alias = "loc", default)]
    location: Option<String>,
    /// Broadcast node (id 255): gateway bookkeeping, never a device.
    #[serde(rename = "virtual", default)]
    virtual_node: bool,
    #[serde(default)]
    failed: bool,
    /// Z-Wave+ device class, raw protocol values (absent on some gateways —
    /// `level` then stays neutral, never `Brightness`).
    #[serde(rename = "deviceClass", alias = "device_class", default)]
    device_class: Option<RawDeviceClass>,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(rename = "productLabel", default)]
    product_label: Option<String>,
    #[serde(default)]
    values: RawValues,
}

#[derive(Debug, Deserialize)]
struct RawValue {
    #[serde(rename = "commandClass", default)]
    command_class: Option<u8>,
    /// Real gateways say `endpoint`; the synthetic fixture says
    /// `endpointIndex`.
    #[serde(rename = "endpointIndex", alias = "endpoint", default)]
    endpoint_index: Option<u8>,
    /// String for most command classes, number for CC 112 parameters.
    #[serde(default)]
    property: Option<Json>,
    /// Meter values (CC 50) are disambiguated by a property key.
    #[serde(rename = "propertyKey", default)]
    property_key: Option<Json>,
    #[serde(default)]
    unit: Option<String>,
    /// Real gateways spell the bounds `min`/`max`; the synthetic fixture
    /// uses `minimum`/`maximum`.
    #[serde(alias = "min", default)]
    minimum: Option<f64>,
    #[serde(alias = "max", default)]
    maximum: Option<f64>,
    #[serde(default)]
    metadata: Option<RawMetadata>,
}

/// Raw Z-Wave+ device class as reported by the gateway (`getNodes`):
/// protocol values, all optional (a missing field means "unknown" = 0).
/// Only `generic` + `specific` drive the `level` semantic — `basic`
/// (e.g. "Routing End Node") carries no dimmer information.
#[derive(Debug, Default, Deserialize)]
struct RawDeviceClass {
    #[serde(default)]
    generic: u8,
    #[serde(default)]
    specific: u8,
}

#[derive(Debug, Default, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    minimum: Option<f64>,
    #[serde(default)]
    maximum: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use serde_json::json;

    const GET_NODES: &[u8] = include_bytes!("../../../integrations/zwave/fixtures/get_nodes.json");
    const VALUE_MESSAGES: &[u8] =
        include_bytes!("../../../integrations/zwave/fixtures/value_messages.json");
    const GET_NODES_REAL: &[u8] =
        include_bytes!("../../../integrations/zwave/fixtures/get_nodes_real.json");

    fn instance() -> InstanceId {
        InstanceId::from("zwave")
    }

    fn catalog() -> Catalog {
        Catalog::from_get_nodes(&instance(), GET_NODES).unwrap()
    }

    fn decoded(catalog: &Catalog, rest: &str, payload: &str) -> Vec<(Arc<str>, Value)> {
        let base = rest.split('/').next().unwrap_or("");
        catalog
            .by_base(base)
            .map(|entry| entry.decode(rest, payload.as_bytes()))
            .unwrap_or_default()
    }

    #[test]
    fn get_nodes_builds_six_devices() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 6);
        for node_id in [2u32, 3, 5, 7, 9, 11] {
            let id = DeviceId::new(&instance(), &node_id.to_string());
            assert!(
                id.as_str().starts_with("zwave:"),
                "identity is <instance>:<nodeId> ({id})"
            );
            let entry = catalog
                .by_id(&id)
                .unwrap_or_else(|| panic!("missing device {id}"));
            assert_eq!(entry.node_id, Some(node_id));
        }
        let lamp = catalog.by_id(&DeviceId::new(&instance(), "2")).unwrap();
        assert_eq!(&*lamp.name, "bureau_lamp");
        assert_eq!(&*lamp.device.native_name, "bureau_lamp");
        assert_eq!(
            lamp.device.native_room.as_deref(),
            None,
            "the fixture carries no location: topics stay loc-less"
        );
        assert_eq!(lamp.device.manufacturer.as_deref(), Some("Synthetic"));
        assert_eq!(lamp.device.model.as_deref(), Some("ZWL-02"));
    }

    #[test]
    fn node_id_survives_rename() {
        let mut json: Json = serde_json::from_slice(GET_NODES).unwrap();
        let nodes = json["result"].as_array_mut().unwrap();
        let lamp = nodes
            .iter_mut()
            .find(|n| n["nodeId"] == json!(2u32))
            .unwrap();
        lamp["name"] = Json::String("bureau_lamp_renommee".to_owned());
        let payload = serde_json::to_vec(&json).unwrap();
        let catalog = Catalog::from_get_nodes(&instance(), &payload).unwrap();
        let lamp = catalog.by_id(&DeviceId::new(&instance(), "2")).unwrap();
        assert_eq!(&*lamp.name, "bureau_lamp_renommee");
        assert!(
            catalog.by_base("bureau_lamp").is_none(),
            "the old base no longer routes"
        );
        assert!(catalog.by_base("bureau_lamp_renommee").is_some());
    }

    #[test]
    fn points_are_typed_per_command_class() {
        let catalog = catalog();

        // bureau_lamp (CC 38): `on` binary writable.
        let lamp = catalog.by_base("bureau_lamp").unwrap();
        let on = lamp.device.point("on").unwrap();
        assert_eq!(on.kind, Kind::Binary);
        assert!(on.access.read && on.access.write);
        assert_eq!(on.semantic, Semantic::OnOff);

        // cuisine_plug (CC 32): `on` writable via 32/currentValue (boolean);
        // `power` RO.
        let plug = catalog.by_base("cuisine_plug").unwrap();
        let on = plug.device.point("on").unwrap();
        assert_eq!(on.kind, Kind::Binary);
        assert!(on.access.write, "Basic 32 is writable via currentValue/set");
        let power = plug.device.point("power").unwrap();
        assert_eq!(power.unit, Some(Unit::Watt));
        assert_eq!(power.semantic, Semantic::Power);
        assert!(power.access.read && !power.access.write);

        // garage_sensor: `temperature`/`humidity`/`battery` all read-only.
        let sensor = catalog.by_base("garage_sensor").unwrap();
        let temperature = sensor.device.point("temperature").unwrap();
        assert_eq!(
            temperature.kind,
            Kind::Numeric {
                min: None,
                max: None,
                step: None
            }
        );
        assert_eq!(temperature.unit, Some(Unit::Celsius));
        assert_eq!(temperature.semantic, Semantic::Temperature);
        assert!(!temperature.access.write);
        let humidity = sensor.device.point("humidity").unwrap();
        assert_eq!(
            humidity.kind,
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None
            }
        );
        assert_eq!(humidity.unit, Some(Unit::Percent));
        assert_eq!(humidity.semantic, Semantic::Humidity);
        assert!(!humidity.access.write);
        let battery = sensor.device.point("battery").unwrap();
        assert_eq!(
            battery.kind,
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None
            },
            "metadata bounds are read"
        );
        assert_eq!(battery.unit, Some(Unit::Percent));
        assert_eq!(battery.semantic, Semantic::Battery);
        assert!(!battery.access.write);

        // couloir_switch (CC 37): `on` writable via 37/targetValue.
        let switch = catalog.by_base("couloir_switch").unwrap();
        let on = switch.device.point("on").unwrap();
        assert_eq!(on.kind, Kind::Binary);
        assert!(
            on.access.write,
            "Switch Binary 37 is writable via targetValue/set"
        );
        assert_eq!(
            switch.on_target,
            Some(SetTarget {
                cc: 37,
                endpoint: 0,
                property: "targetValue"
            })
        );
    }

    #[test]
    fn cc38_level_key_follows_dimmer_class() {
        let catalog = catalog();

        // bureau_lamp (CC 38, dimmer class 0x11/0x01): a declared dimmer
        // exposes `brightness` (0-100 %, writable), not `level`.
        let lamp = catalog.by_base("bureau_lamp").unwrap();
        let brightness = lamp.device.point("brightness").unwrap();
        assert_eq!(
            brightness.kind,
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0)
            }
        );
        assert_eq!(brightness.unit, Some(Unit::Percent));
        assert_eq!(brightness.semantic, Semantic::Brightness);
        assert!(brightness.access.read && brightness.access.write);
        assert!(
            lamp.device.point("level").is_none(),
            "a declared dimmer exposes `brightness`, not `level`"
        );

        // kitchen_dimmer (CC 38, same dimmer class): `on` + `brightness`
        // writable.
        let dimmer = catalog.by_base("kitchen_dimmer").unwrap();
        assert!(dimmer.device.point("on").is_some_and(|p| p.access.write));
        assert!(
            dimmer
                .device
                .point("brightness")
                .is_some_and(|p| p.access.read && p.access.write)
        );

        // volet_garage (CC 38, device class "Motor Control"): the point stays
        // under `level` and stays neutral — a Brightness semantic would hide
        // the device from the guard's name check (gate/garage).
        let shutter = catalog.by_base("volet_garage").unwrap();
        assert_eq!(
            shutter.device.point("level").unwrap().semantic,
            Semantic::Other,
            "non-dimmer CC 38 must not be Brightness (guard)"
        );
        assert!(
            shutter.device.point("brightness").is_none(),
            "a motor-control CC 38 node must not expose a `brightness` point"
        );
        assert!(shutter.device.point("on").is_some_and(|p| p.access.write));
    }

    #[test]
    fn value_topics_decode_exactly() {
        let catalog = catalog();
        let messages: BTreeMap<String, String> = serde_json::from_slice(VALUE_MESSAGES).unwrap();
        assert_eq!(messages.len(), 10);
        for (rest, payload) in &messages {
            let expected = match rest.as_str() {
                // Switch state → `on`, whatever the command class (CC 38
                // multilevel: 0 = off, anything else on, and the level
                // itself; CC 32/37: the boolean passes through).
                "bureau_lamp/38/0/currentValue" => vec![
                    ("on".into(), Value::Bool(true)),
                    ("brightness".into(), Value::Int(80)),
                ],
                "cuisine_plug/32/0/currentValue" => vec![("on".into(), Value::Bool(true))],
                "cuisine_plug/50/0/power" => vec![("power".into(), Value::Float(15.2))],
                "garage_sensor/31/0/Air_temperature" => {
                    vec![("temperature".into(), Value::Float(21.5))]
                }
                "garage_sensor/49/0/Relative_humidity/0" => {
                    vec![("humidity".into(), Value::Int(45))]
                }
                "garage_sensor/53/0/batteryLevel" => vec![("battery".into(), Value::Int(87))],
                "couloir_switch/37/0/currentValue" => vec![("on".into(), Value::Bool(false))],
                "kitchen_dimmer/38/0/currentValue" => vec![
                    ("on".into(), Value::Bool(true)),
                    ("brightness".into(), Value::Int(40)),
                ],
                // CC 38 `targetValue` is the last order, not the level: never
                // read. An unknown (cc, property) is ignored.
                "bureau_lamp/38/0/targetValue" | "inconnu/99/0/quelque_chose" => Vec::new(),
                _ => unreachable!("fixture topic {rest}"),
            };
            assert_eq!(decoded(&catalog, rest, payload), expected, "{rest}");
        }
    }

    #[test]
    fn commands_encode_to_the_gateway_topics() {
        let catalog = catalog();
        let lamp = catalog.by_base("bureau_lamp").unwrap();
        assert_eq!(
            lamp.encode("on", &Value::Bool(true)).unwrap(),
            (
                "bureau_lamp/38/0/targetValue/set".to_owned(),
                "255".to_owned()
            ),
            "CC 38 on → numeric 255 (multilevel targetValue is a number)"
        );
        assert_eq!(
            lamp.encode("on", &Value::Bool(false)).unwrap(),
            (
                "bureau_lamp/38/0/targetValue/set".to_owned(),
                "0".to_owned()
            ),
            "CC 38 off → numeric 0"
        );
        assert_eq!(
            lamp.encode("brightness", &Value::Float(80.0)).unwrap(),
            (
                "bureau_lamp/38/0/targetValue/set".to_owned(),
                "80".to_owned()
            ),
            "exact integers stay integers (80, not 80.0)"
        );
        // The `level` spelling still routes to the same target (legacy key).
        assert_eq!(
            lamp.encode("level", &Value::Int(50)).unwrap(),
            (
                "bureau_lamp/38/0/targetValue/set".to_owned(),
                "50".to_owned()
            ),
            "`level` remains a valid alias for the write target"
        );
        let plug = catalog.by_base("cuisine_plug").unwrap();
        assert_eq!(
            plug.encode("on", &Value::Bool(true)).unwrap(),
            (
                "cuisine_plug/32/0/currentValue/set".to_owned(),
                "true".to_owned()
            ),
            "CC 32 writes currentValue directly (real-gateway behaviour)"
        );
        assert_eq!(
            plug.encode("on", &Value::Bool(false)).unwrap(),
            (
                "cuisine_plug/32/0/currentValue/set".to_owned(),
                "false".to_owned()
            ),
        );
        let switch = catalog.by_base("couloir_switch").unwrap();
        assert_eq!(
            switch.encode("on", &Value::Bool(false)).unwrap(),
            (
                "couloir_switch/37/0/targetValue/set".to_owned(),
                "false".to_owned()
            )
        );
        let dimmer = catalog.by_base("kitchen_dimmer").unwrap();
        assert_eq!(
            dimmer.encode("brightness", &Value::Int(40)).unwrap(),
            (
                "kitchen_dimmer/38/0/targetValue/set".to_owned(),
                "40".to_owned()
            )
        );
        let sensor = catalog.by_base("garage_sensor").unwrap();
        assert!(
            sensor.encode("battery", &Value::Int(100)).is_err(),
            "read-only points refuse writes"
        );
        assert!(
            lamp.encode("brightness", &Value::Bool(true)).is_err(),
            "brightness expects a number"
        );
        assert!(
            lamp.encode("on", &Value::Int(1)).is_err(),
            "on expects a boolean"
        );
    }

    #[test]
    fn parser_is_tolerant() {
        // success:false → explicit error (the run loop falls back to
        // accumulation mode).
        let refused = r#"{"success": false, "result": []}"#;
        assert!(Catalog::from_get_nodes(&instance(), refused.as_bytes()).is_err());

        // A bare node array is accepted as well.
        let json: Json = serde_json::from_slice(GET_NODES).unwrap();
        let bare = serde_json::to_vec(&json["result"]).unwrap();
        assert_eq!(
            Catalog::from_get_nodes(&instance(), &bare).unwrap().len(),
            6
        );

        // A node without nodeId is skipped, never a parse failure.
        let nodes: Json = json!([{ "name": "sans_id", "values": [] }]);
        let no_id = serde_json::to_vec(&nodes).unwrap();
        assert!(
            Catalog::from_get_nodes(&instance(), &no_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn switch_cc_priority_is_basic_first() {
        // A node exposing both CC 32 and CC 38: `on` is commanded through 32
        // (priority 32 → 37 → 38), even though 38 comes first in values[].
        let nodes = json!([{
            "nodeId": 42,
            "name": "double_switch",
            "location": "atelier",
            "values": [
                { "commandClass": 38, "endpointIndex": 0, "property": "currentValue",
                  "minimum": 0, "maximum": 100, "value": 50 },
                { "commandClass": 38, "endpointIndex": 0, "property": "targetValue",
                  "value": 50 },
                { "commandClass": 32, "endpointIndex": 1, "property": "currentValue",
                  "value": true }
            ]
        }]);
        let payload = serde_json::to_vec(&nodes).unwrap();
        let catalog = Catalog::from_get_nodes(&instance(), &payload).unwrap();
        let entry = catalog.by_id(&DeviceId::new(&instance(), "42")).unwrap();
        assert_eq!(
            entry.on_target,
            Some(SetTarget {
                cc: 32,
                endpoint: 1,
                property: "currentValue"
            })
        );
        assert_eq!(
            entry.level_target,
            Some(SetTarget {
                cc: 38,
                endpoint: 0,
                property: "targetValue"
            })
        );
        assert_eq!(
            entry.encode("on", &Value::Bool(true)).unwrap(),
            (
                "atelier/double_switch/32/1/currentValue/set".to_owned(),
                "true".to_owned()
            ),
            "the topic base carries the location segment"
        );
    }

    #[test]
    fn real_gateway_fixture_replays_the_live_contract() {
        let catalog = Catalog::from_get_nodes(&instance(), GET_NODES_REAL).unwrap();
        assert_eq!(
            catalog.len(),
            5,
            "only the mappable nodes (6, 7, 8, 9, 18) become devices"
        );
        for node_id in [6u32, 7, 8, 9, 18] {
            let id = DeviceId::new(&instance(), &node_id.to_string());
            assert!(catalog.by_id(&id).is_some(), "missing device {id}");
        }
        // The broadcast node (255, virtual), the controller (1) and the dead
        // node (17, no values) must not become devices.
        for excluded in [1u32, 17, 255] {
            let id = DeviceId::new(&instance(), &excluded.to_string());
            assert!(
                catalog.by_id(&id).is_none(),
                "node {excluded} (controller/dead/broadcast) must not become a device"
            );
        }

        // The relays are unnamed in the live network → the gateway's
        // `nodeID_<id>` fallback base. `on` is writable via the first
        // endpoint's `37/targetValue`; `power` (watt meter, key 66049) is
        // read-only; the kWh counter (key 65537) is out of the v1 surface.
        for base in ["nodeID_6", "nodeID_18"] {
            let relay = catalog.by_base(base).unwrap();
            assert_eq!(&*relay.name, base);
            assert_eq!(relay.device.points.len(), 2, "on + power only");
            let on = relay.device.point("on").unwrap();
            assert_eq!(on.kind, Kind::Binary);
            assert!(on.access.read && on.access.write);
            assert_eq!(
                relay.on_target,
                Some(SetTarget {
                    cc: 37,
                    endpoint: 0,
                    property: "targetValue"
                })
            );
            assert_eq!(
                relay.encode("on", &Value::Bool(true)).unwrap(),
                (format!("{base}/37/0/targetValue/set"), "true".to_owned())
            );
            let power = relay.device.point("power").unwrap();
            assert_eq!(power.unit, Some(Unit::Watt));
            assert_eq!(power.semantic, Semantic::Power);
            assert!(power.access.read && !power.access.write);
        }

        // The Fibaro sensors: CC 49 `Air temperature` (spelled with a space
        // in the real gateway) + CC 128 `level` battery. Node 8 exposes no
        // temperature value — it still becomes a device, battery only.
        for (base, with_temperature) in
            [("nodeID_7", true), ("nodeID_8", false), ("nodeID_9", true)]
        {
            let sensor = catalog.by_base(base).unwrap();
            if with_temperature {
                let temperature = sensor.device.point("temperature").unwrap();
                assert_eq!(temperature.unit, Some(Unit::Celsius));
                assert_eq!(temperature.semantic, Semantic::Temperature);
                assert!(!temperature.access.write);
            } else {
                assert!(sensor.device.point("temperature").is_none());
            }
            let battery = sensor.device.point("battery").unwrap();
            assert_eq!(
                battery.kind,
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: None
                },
                "CC 128 bounds are read (top-level min/max)"
            );
            assert_eq!(battery.unit, Some(Unit::Percent));
            assert_eq!(battery.semantic, Semantic::Battery);
            assert!(!battery.access.write);
        }
    }

    #[test]
    fn real_value_topics_decode_through_the_envelope() {
        let catalog = Catalog::from_get_nodes(&instance(), GET_NODES_REAL).unwrap();
        let relay = catalog.by_base("nodeID_6").unwrap();
        // Real gateway payloads are wrapped in `{time, value}`; the base is
        // `nodeID_<id>`.
        assert_eq!(
            relay.decode(
                "nodeID_6/37/0/currentValue",
                br#"{"time":"2026-06-24T16:20:00Z","value":true}"#
            ),
            vec![("on".into(), Value::Bool(true))]
        );
        // The meter's property-key segment (watt reading → `power`).
        assert_eq!(
            relay.decode("nodeID_6/50/0/value/66049", br#"{"value":12.3}"#),
            vec![("power".into(), Value::Float(12.3))]
        );
        // The kWh counter maps to nothing: ignored, never a parse failure.
        assert!(
            relay
                .decode("nodeID_6/50/0/value/65537", br#"{"value":42}"#)
                .is_empty()
        );
        // Bare (unwrapped) payloads still pass through.
        assert_eq!(
            relay.decode("nodeID_6/37/0/currentValue", b"false"),
            vec![("on".into(), Value::Bool(false))]
        );

        // A sensor: the spaced property name is sanitized in the topic.
        let sensor = catalog.by_base("nodeID_7").unwrap();
        assert_eq!(
            sensor.decode("nodeID_7/49/0/Air_temperature", br#"{"value":19.5}"#),
            vec![("temperature".into(), Value::Float(19.5))]
        );
        assert_eq!(
            sensor.decode("nodeID_7/128/0/level", br#"{"value":87}"#),
            vec![("battery".into(), Value::Int(87))]
        );
        // A foreign base is never decoded by this entry.
        assert!(
            relay
                .decode("nodeID_7/49/0/Air_temperature", br#"{"value":19.5}"#)
                .is_empty()
        );
    }

    #[test]
    fn location_prefixes_the_topic_base() {
        // A node with both a name and a location routes through `{loc}/{name}`.
        let nodes = json!([{
            "nodeId": 21,
            "name": "Lampe Salon",
            "loc": "Salon Est",
            "values": [
                { "commandClass": 37, "endpoint": 0, "property": "currentValue", "value": false },
                { "commandClass": 37, "endpoint": 0, "property": "targetValue", "value": false }
            ]
        }]);
        let payload = serde_json::to_vec(&nodes).unwrap();
        let catalog = Catalog::from_get_nodes(&instance(), &payload).unwrap();
        let entry = catalog.by_id(&DeviceId::new(&instance(), "21")).unwrap();
        // Whitespace → `_` (the gateway's topic sanitization).
        assert_eq!(&*entry.name, "Lampe_Salon");
        assert_eq!(&*entry.loc, "Salon_Est");
        assert_eq!(entry.base(), "Salon_Est/Lampe_Salon");
        assert!(catalog.by_base("Salon_Est/Lampe_Salon").is_some());
        assert!(catalog.by_base("Lampe_Salon").is_none());
        assert_eq!(
            entry.decode("Salon_Est/Lampe_Salon/37/0/currentValue", b"true"),
            vec![("on".into(), Value::Bool(true))]
        );
        assert_eq!(
            entry.encode("on", &Value::Bool(false)).unwrap(),
            (
                "Salon_Est/Lampe_Salon/37/0/targetValue/set".to_owned(),
                "false".to_owned()
            )
        );
        assert_eq!(
            entry.device.native_room.as_deref(),
            Some("Salon Est"),
            "the room shown in Moli keeps its raw form"
        );
    }

    #[test]
    fn value_topic_resolves_the_entry_by_base_prefix() {
        let catalog = catalog();
        let entry = catalog
            .entry_for_value_topic("bureau_lamp/38/0/currentValue")
            .unwrap();
        assert_eq!(entry.node_id, Some(2));
        // A bare base (no value segment after it) is not a value topic.
        assert!(catalog.entry_for_value_topic("bureau_lamp").is_none());
        // An unknown base resolves to nothing.
        assert!(
            catalog
                .entry_for_value_topic("inconnu/99/0/quelque_chose")
                .is_none()
        );
    }

    #[test]
    fn accented_names_survive_topic_sanitization() {
        // Review guard: the gateway keeps Latin-1 accented letters in its
        // topics; the topic base must match for value routing to work.
        let nodes = json!([{
            "nodeId": 33,
            "name": "Gâche entrée",
            "values": [
                { "commandClass": 37, "endpointIndex": 0, "property": "currentValue", "value": false },
                { "commandClass": 37, "endpointIndex": 0, "property": "targetValue", "value": false }
            ]
        }]);
        let payload = serde_json::to_vec(&nodes).unwrap();
        let catalog = Catalog::from_get_nodes(&instance(), &payload).unwrap();
        let entry = catalog.by_id(&DeviceId::new(&instance(), "33")).unwrap();
        assert_eq!(&*entry.name, "Gâche_entrée");
        assert_eq!(
            &*entry.device.native_name, "Gâche entrée",
            "native_name keeps the raw gateway name (spaces and accents intact)"
        );
        assert_eq!(
            entry.decode("Gâche_entrée/37/0/currentValue", b"true"),
            vec![("on".into(), Value::Bool(true))]
        );
        assert_eq!(
            entry.encode("on", &Value::Bool(true)).unwrap(),
            (
                "Gâche_entrée/37/0/targetValue/set".to_owned(),
                "true".to_owned()
            )
        );
    }

    #[test]
    fn level_is_brightness_only_for_dimmer_device_classes() {
        let make = |device_class: Option<Json>| -> Json {
            let mut node = json!([
                {
                "nodeId": 44,
                "name": "module_cc38",
                "values": [
                    { "commandClass": 38, "endpointIndex": 0, "property": "currentValue",
                      "minimum": 0, "maximum": 100, "value": 50 },
                    { "commandClass": 38, "endpointIndex": 0, "property": "targetValue", "value": 50 }
                ]
            }]);
            if let Some(dc) = device_class {
                node[0]["deviceClass"] = dc;
            }
            node
        };

        // Multilevel Switch / Multilevel Power Switch → a real dimmer: the
        // point is exposed under `brightness`, not `level`.
        let catalog = Catalog::from_get_nodes(
            &instance(),
            &serde_json::to_vec(&make(Some(
                json!({"basic": 4, "generic": 17, "specific": 1}),
            )))
            .unwrap(),
        )
        .unwrap();
        let dimmer = catalog.by_id(&DeviceId::new(&instance(), "44")).unwrap();
        assert_eq!(
            dimmer.device.point("brightness").unwrap().semantic,
            Semantic::Brightness
        );
        assert!(
            dimmer.device.point("level").is_none(),
            "a declared dimmer exposes `brightness`, not `level`"
        );

        // Same CC 38 on a Motor Control node: the point stays under `level`
        // and must stay neutral, otherwise the guard would treat the gate as
        // a light.
        let catalog = Catalog::from_get_nodes(
            &instance(),
            &serde_json::to_vec(&make(Some(
                json!({"basic": 4, "generic": 17, "specific": 5}),
            )))
            .unwrap(),
        )
        .unwrap();
        let motor = catalog.by_id(&DeviceId::new(&instance(), "44")).unwrap();
        assert_eq!(
            motor.device.point("level").unwrap().semantic,
            Semantic::Other,
            "shutter motor CC 38 must not be Brightness (guard)"
        );
        assert!(
            motor.device.point("brightness").is_none(),
            "a motor-control node must not expose a `brightness` point"
        );

        // No device class at all: neutral, kept under `level`.
        let catalog =
            Catalog::from_get_nodes(&instance(), &serde_json::to_vec(&make(None)).unwrap())
                .unwrap();
        let no_class = catalog.by_id(&DeviceId::new(&instance(), "44")).unwrap();
        assert_eq!(
            no_class.device.point("level").unwrap().semantic,
            Semantic::Other,
            "absent device class must not grant Brightness (guard)"
        );
        assert!(
            no_class.device.point("brightness").is_none(),
            "absent device class must not expose `brightness`"
        );
    }

    #[test]
    fn accumulated_level_point_is_neutral() {
        // Without getNodes there is no device class: an accumulated CC 38
        // `level` must never be Brightness.
        let mut catalog =
            Catalog::from_get_nodes(&instance(), &serde_json::to_vec(&json!([])).unwrap()).unwrap();
        let entry = catalog
            .upsert_accumulated(&instance(), "volet_accu", 38, 0, "targetValue", None)
            .unwrap();
        assert_eq!(
            entry.device.point("level").unwrap().semantic,
            Semantic::Other,
            "accumulation mode has no device class → neutral semantic"
        );
        let Kind::Numeric { max, .. } = &entry.device.point("level").unwrap().kind else {
            panic!("level is numeric");
        };
        assert_eq!(*max, Some(99.0), "a Multilevel Switch level runs 0-99");
    }

    #[test]
    fn an_accumulated_dimmer_reads_its_level_from_current_value() {
        // A dimmer that never reports a target: its `currentValue` alone
        // makes the `on` and `level` points, both written to `targetValue`.
        let mut catalog =
            Catalog::from_get_nodes(&instance(), &serde_json::to_vec(&json!([])).unwrap()).unwrap();
        let entry = catalog
            .upsert_accumulated(&instance(), "variateur_accu", 38, 0, "currentValue", None)
            .unwrap();
        assert!(entry.device.point("on").is_some_and(|p| p.access.write));
        assert!(entry.device.point("level").is_some_and(|p| p.access.write));
        assert_eq!(
            entry.decode("variateur_accu/38/0/currentValue", b"42"),
            vec![
                ("on".into(), Value::Bool(true)),
                ("level".into(), Value::Int(42))
            ]
        );
        assert!(
            entry
                .decode("variateur_accu/38/0/targetValue", b"255")
                .is_empty()
        );
        assert_eq!(
            entry.encode("level", &Value::Int(42)).unwrap(),
            (
                "variateur_accu/38/0/targetValue/set".to_owned(),
                "42".to_owned()
            )
        );
    }

    /// A CC 38 node shaped like the real gateway: `values` as a map, bounds
    /// spelled `min`/`max`, the zwave-js Multilevel Switch range 0-99.
    fn real_cc38(specific: u8) -> Catalog {
        let nodes = json!({"success": true, "result": [{
            "id": 5,
            "name": "Variateur salon",
            "deviceClass": {"basic": 4, "generic": 17, "specific": specific},
            "values": {
                "38-0-currentValue": { "commandClass": 38, "endpoint": 0,
                    "property": "currentValue", "min": 0, "max": 99, "value": 0 },
                "38-0-targetValue": { "commandClass": 38, "endpoint": 0,
                    "property": "targetValue", "min": 0, "max": 99, "value": 0 },
                "38-1-targetValue": { "commandClass": 38, "endpoint": 1,
                    "property": "targetValue", "min": 0, "max": 99, "value": 0 }
            }
        }]});
        Catalog::from_get_nodes(&instance(), &serde_json::to_vec(&nodes).unwrap()).unwrap()
    }

    #[test]
    fn a_real_dimmer_is_exposed_in_percent() {
        let catalog = real_cc38(0x01);
        let dimmer = catalog.by_base("Variateur_salon").unwrap();
        let brightness: Vec<_> = dimmer
            .device
            .points
            .iter()
            .filter(|p| &*p.key == "brightness")
            .collect();
        assert_eq!(
            brightness.len(),
            1,
            "one level point, from the first CC 38 target"
        );
        assert_eq!(
            brightness[0].kind,
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0)
            }
        );
        assert_eq!(brightness[0].unit, Some(Unit::Percent));
        assert_eq!(dimmer.level_target.map(|t| t.endpoint), Some(0));

        let read = |rest: &str, payload: &str| decoded(&catalog, rest, payload);
        let level = |payload: &str| {
            read("Variateur_salon/38/0/currentValue", payload)
                .into_iter()
                .find(|(key, _)| &**key == "brightness")
                .map(|(_, value)| value)
        };
        assert_eq!(
            level(r#"{"time": 1730457600000, "value": 99}"#),
            Some(Value::Int(100))
        );
        assert_eq!(level("50"), Some(Value::Int(50)));
        assert_eq!(level("98"), Some(Value::Int(98)));
        assert_eq!(
            level("1"),
            Some(Value::Int(1)),
            "a lit dimmer is never shown at 0 %"
        );
        assert_eq!(level("0"), Some(Value::Int(0)));
        assert_eq!(
            level(r#"{"time": 1730457600000, "value": null}"#),
            None,
            "unknown level"
        );
        assert!(
            read("Variateur_salon/38/0/targetValue", "255").is_empty(),
            "255 after an `on` is an order (restore the previous level), not a level"
        );
        assert!(
            read("Variateur_salon/38/1/currentValue", "10")
                .iter()
                .all(|(key, _)| &**key != "brightness"),
            "another endpoint never moves the level"
        );

        let write = |value: Value| dimmer.encode("brightness", &value).unwrap();
        let topic = "Variateur_salon/38/0/targetValue/set".to_owned();
        assert_eq!(write(Value::Float(100.0)), (topic.clone(), "99".to_owned()));
        assert_eq!(write(Value::Int(100)), (topic.clone(), "99".to_owned()));
        assert_eq!(write(Value::Float(50.0)), (topic.clone(), "50".to_owned()));
        assert_eq!(write(Value::Float(99.0)), (topic.clone(), "99".to_owned()));
        assert_eq!(write(Value::Float(1.0)), (topic.clone(), "1".to_owned()));
        assert_eq!(write(Value::Float(0.0)), (topic.clone(), "0".to_owned()));
        assert_eq!(
            dimmer.encode("on", &Value::Bool(true)).unwrap(),
            (topic, "255".to_owned()),
            "`on` still restores the previous level"
        );
        assert!(
            dimmer.encode("on", &Value::Int(1)).is_err(),
            "on expects a boolean"
        );
    }

    #[test]
    fn every_dimmer_level_survives_the_round_trip() {
        let catalog = real_cc38(0x01);
        let dimmer = catalog.by_base("Variateur_salon").unwrap();
        for raw in 0..=99_i64 {
            let percent = dimmer.decode(
                "Variateur_salon/38/0/currentValue",
                raw.to_string().as_bytes(),
            );
            let (_, percent) = percent
                .iter()
                .find(|(key, _)| &**key == "brightness")
                .unwrap();
            let (_, wire) = dimmer.encode("brightness", percent).unwrap();
            assert_eq!(wire, raw.to_string(), "level {raw} → {percent:?}");
        }
    }

    #[test]
    fn every_percent_but_99_survives_the_round_trip() {
        let catalog = real_cc38(0x01);
        let dimmer = catalog.by_base("Variateur_salon").unwrap();
        for percent in (0..=100_i64).filter(|p| *p != 99) {
            let (_, wire) = dimmer.encode("brightness", &Value::Int(percent)).unwrap();
            let back = dimmer.decode("Variateur_salon/38/0/currentValue", wire.as_bytes());
            assert!(
                back.contains(&("brightness".into(), Value::Int(percent))),
                "{percent} % → level {wire} → {back:?}"
            );
        }
    }

    #[test]
    fn a_neutral_level_keeps_the_device_scale() {
        // Motor Control (0x11/0x05): no light, no conversion.
        let catalog = real_cc38(0x05);
        let motor = catalog.by_base("Variateur_salon").unwrap();
        assert!(motor.level_max.is_none());
        let Kind::Numeric { min, max, .. } = &motor.device.point("level").unwrap().kind else {
            panic!("level is numeric");
        };
        assert_eq!((*min, *max), (Some(0.0), Some(99.0)));
        assert_eq!(
            decoded(&catalog, "Variateur_salon/38/0/currentValue", "42"),
            vec![
                ("on".into(), Value::Bool(true)),
                ("level".into(), Value::Int(42))
            ]
        );
        assert_eq!(
            motor.encode("level", &Value::Float(42.0)).unwrap(),
            (
                "Variateur_salon/38/0/targetValue/set".to_owned(),
                "42".to_owned()
            )
        );
    }
}

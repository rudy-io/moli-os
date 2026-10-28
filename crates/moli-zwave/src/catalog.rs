//! Pure translation between the zwave-js-ui gateway model and Moli's. No IO.
//!
//! The gateway exposes every Z-Wave node through its `getNodes` API: a stable
//! node id (identity), a name and an optional location (topic routing only,
//! may be renamed at any time), and a `values` collection (command class,
//! endpoint, property, optional property key, metadata). Flattening that
//! through a small `(command class, property[, property key])` table gives
//! typed points for the v1 surface (on/off, level, battery, sensors, power)
//! without one line of device-specific code.
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
}

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
        });
        self.by_base.entry(name).or_insert(entry.device.id.clone());
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
                semantic: kind.semantic(),
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
        // The endpoint segment must be present and numeric; the mapping does
        // not depend on it (a mismatch would mean a foreign topic).
        if parts.next().and_then(|s| s.parse::<u8>().ok()).is_none() {
            return Vec::new();
        }
        let Some(property) = parts.next().filter(|p| !p.is_empty()) else {
            return Vec::new();
        };
        // The optional fifth segment (propertyKey) is tolerated; it decides
        // the mapping for meter values (CC 50).
        let property_key = parts.next().filter(|k| !k.is_empty());
        let Some(kind) = map_value(cc, property, property_key) else {
            return Vec::new();
        };
        if self.device.point(kind.key()).is_none() {
            return Vec::new();
        }
        let Ok(json) = serde_json::from_slice::<Json>(payload) else {
            tracing::debug!(entry = %self.name, "unreadable zwave value payload ignored");
            return Vec::new();
        };
        // The real gateway wraps value payloads in a `{time, value}`
        // envelope; bare values (fixtures, older gateways) pass through.
        let value_json = json.get("value").unwrap_or(&json);
        let value = match (kind, value_json) {
            // CC 32/37/38 switch states: booleans pass through; CC 38
            // numbers are levels (0 = off, anything else on).
            (PointKind::On, Json::Bool(b)) => Value::Bool(*b),
            (PointKind::On, Json::Number(n)) => Value::Bool(n.as_f64().is_some_and(|v| v > 0.0)),
            _ => Value::from_json(value_json),
        };
        vec![(kind.key().into(), value)]
    }

    /// Encodes a validated write as `(command topic suffix, raw JSON
    /// payload)`. Only `on` (boolean) and `level` (number) are writable;
    /// everything else is refused (defensive: the hub already checks
    /// `access.write`).
    pub fn encode(&self, key: &str, value: &Value) -> anyhow::Result<(String, String)> {
        let (target, numeric) = match key {
            "on" => (self.on_target, false),
            "level" => (self.level_target, true),
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
        let payload = if numeric {
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
            match value {
                Value::Bool(b) => b.to_string(),
                _ => {
                    return Err(anyhow::anyhow!(moli_i18n::tr!(
                        "pilotes.zwave.on_attend_booleen"
                    )));
                }
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
            // Switch levels, batteries and relative humidity are bounded
            // 0-100 by the Z-Wave spec; the gateway bounds win when given.
            Self::Level | Self::Battery | Self::Humidity => Kind::Numeric {
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

/// The write target for `on`, per command class: Basic writes
/// `currentValue` directly, Switch Binary/Multilevel write `targetValue`.
const fn switch_target_property(cc: u8) -> &'static str {
    if cc == 32 {
        "currentValue"
    } else {
        "targetValue"
    }
}

/// Mirrors the gateway's topic sanitization: whitespace becomes `_`, and
/// characters outside `[A-Za-z0-9-_À-ÖØ-öø-ÿ/]` are dropped.
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
            '\u{00C0}'..='\u{00D6}' | '\u{00D8}' | '\u{00F6}' | '\u{00F8}' | '\u{00FF}'
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
    // Unnamed nodes route through the gateway's own fallback: `nodeID_<id>`
    // (its NODE_PREFIX).
    let name = raw
        .name
        .as_deref()
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

    let (points, level_target) = collect_points(raw);

    // No mappable point: the node is outside the v1 surface (dead node,
    // configuration-only device) — never a device.
    if points.is_empty() {
        return None;
    }

    let on_target = find_on_target(raw);
    let device = Device {
        id: DeviceId::new(instance, &node_id.to_string()),
        instance: instance.clone(),
        native_name: name.clone().into(),
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
    };
    refresh_write_access(&mut entry);
    Some(entry)
}

/// Flattens the node's values into the v1 points: in `values` order, the
/// first value that creates a key wins the point; duplicates are skipped.
/// Also returns the `level` write target (CC 38 `targetValue`) when present.
fn collect_points(raw: &RawNode) -> (Vec<PointSpec>, Option<SetTarget>) {
    let mut points: Vec<PointSpec> = Vec::new();
    let mut level_target: Option<SetTarget> = None;
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
        if points.iter().any(|p| p.key.as_ref() == kind.key()) {
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
        points.push(PointSpec {
            key: kind.key().into(),
            label: kind.label().into(),
            kind: kind.kind(min, max),
            access: Access {
                read: true,
                write: false,
            },
            unit,
            semantic: kind.semantic(),
        });
        if matches!(kind, PointKind::Level) {
            level_target = Some(SetTarget {
                cc,
                endpoint: value.endpoint_index.unwrap_or(0),
                property: "targetValue",
            });
        }
    }
    (points, level_target)
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
            "level" => entry.level_target.is_some(),
            _ => false,
        };
    }
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
    fn get_nodes_builds_five_devices() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 5);
        for node_id in [2u32, 3, 5, 7, 9] {
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

        // bureau_lamp (CC 38): `on` binary writable, `level` 0-100 % writable.
        let lamp = catalog.by_base("bureau_lamp").unwrap();
        let on = lamp.device.point("on").unwrap();
        assert_eq!(on.kind, Kind::Binary);
        assert!(on.access.read && on.access.write);
        assert_eq!(on.semantic, Semantic::OnOff);
        let level = lamp.device.point("level").unwrap();
        assert_eq!(
            level.kind,
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None
            }
        );
        assert_eq!(level.unit, Some(Unit::Percent));
        assert_eq!(level.semantic, Semantic::Brightness);
        assert!(level.access.read && level.access.write);

        // cuisine_plug (CC 32): `on` writable via 32/currentValue; `power` RO.
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

        // kitchen_dimmer (CC 38): `on` + `level` writable.
        let dimmer = catalog.by_base("kitchen_dimmer").unwrap();
        assert!(dimmer.device.point("on").is_some_and(|p| p.access.write));
        assert!(dimmer.device.point("level").is_some_and(|p| p.access.write));
    }

    #[test]
    fn value_topics_decode_exactly() {
        let catalog = catalog();
        let messages: BTreeMap<String, String> = serde_json::from_slice(VALUE_MESSAGES).unwrap();
        assert_eq!(messages.len(), 10);
        for (rest, payload) in &messages {
            let expected = match rest.as_str() {
                // Switch state → `on`, whatever the command class (CC 38
                // multilevel: 0 = off, anything else on; CC 32/37: the
                // boolean passes through).
                "bureau_lamp/38/0/currentValue" | "cuisine_plug/32/0/currentValue" => {
                    vec![("on".into(), Value::Bool(true))]
                }
                "bureau_lamp/38/0/targetValue" => vec![("level".into(), Value::Int(80))],
                "cuisine_plug/50/0/power" => vec![("power".into(), Value::Float(15.2))],
                "garage_sensor/31/0/Air_temperature" => {
                    vec![("temperature".into(), Value::Float(21.5))]
                }
                "garage_sensor/49/0/Relative_humidity/0" => {
                    vec![("humidity".into(), Value::Int(45))]
                }
                "garage_sensor/53/0/batteryLevel" => vec![("battery".into(), Value::Int(87))],
                "couloir_switch/37/0/currentValue" => vec![("on".into(), Value::Bool(false))],
                "kitchen_dimmer/38/0/targetValue" => vec![("level".into(), Value::Int(40))],
                "inconnu/99/0/quelque_chose" => Vec::new(),
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
                "true".to_owned()
            )
        );
        assert_eq!(
            lamp.encode("level", &Value::Float(80.0)).unwrap(),
            (
                "bureau_lamp/38/0/targetValue/set".to_owned(),
                "80".to_owned()
            ),
            "exact integers stay integers (80, not 80.0)"
        );
        let plug = catalog.by_base("cuisine_plug").unwrap();
        assert_eq!(
            plug.encode("on", &Value::Bool(true)).unwrap(),
            (
                "cuisine_plug/32/0/currentValue/set".to_owned(),
                "true".to_owned()
            )
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
            dimmer.encode("level", &Value::Int(40)).unwrap(),
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
            lamp.encode("level", &Value::Bool(true)).is_err(),
            "level expects a number"
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
            5
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
}

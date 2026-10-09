//! Pure translation between the Hue CLIP v2 model and Moli's. No IO.
//!
//! In CLIP v2 a physical `device` owns *services* (`light`, `button`,
//! `device_power`, `motion`, `zigbee_connectivity`…), each with its own id.
//! A Moli device is the Hue `device`; its points come from its services.
//! Events address services, so the index maps every service id back to its
//! device and to the keys it feeds.

use std::collections::HashMap;
use std::sync::Arc;

use moli_core::color::{hex_to_xy, xy_to_hex};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::{Value as Json, json};

/// What a service of a device feeds.
#[derive(Clone, Debug, PartialEq)]
enum Feed {
    Light { prefix: String },
    Button { key: Arc<str> },
    Battery,
    Connectivity,
    Motion { prefix: String },
    Temperature { prefix: String },
    LightLevel { prefix: String },
    Contact { prefix: String },
}

#[derive(Clone, Debug)]
struct Service {
    device: DeviceId,
    feed: Feed,
}

/// A write a point maps to: which resource (`light/<id>` or
/// `grouped_light/<id>`), which field.
#[derive(Clone, Debug, PartialEq)]
enum Write {
    On {
        light: String,
    },
    Brightness {
        light: String,
    },
    Mirek {
        light: String,
    },
    /// A colour given as `#rrggbb`, sent as CIE xy.
    Xy {
        light: String,
    },
    /// A lamp's own animation (« fire », « candle », « prism »…).
    Effect {
        light: String,
    },
}

/// Everything needed to route events and commands after a full load.
#[derive(Debug, Default)]
pub struct Index {
    services: HashMap<String, Service>,
    writes: HashMap<(DeviceId, Arc<str>), Write>,
}

/// Result of loading every resource of a bridge.
#[derive(Debug, Default)]
pub struct Load {
    pub devices: Vec<Device>,
    pub index: Index,
}

/// Values carried by one resource (full or partial, as in events).
#[derive(Debug, Default, PartialEq)]
pub struct Update {
    pub device: Option<DeviceId>,
    pub values: Vec<(Arc<str>, Value)>,
    pub online: Option<bool>,
    /// Values are occurrences (button presses): two identical ones in a row
    /// are two events.
    pub pulse: bool,
}

fn str_at<'a>(json: &'a Json, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(json, |j, k| j.get(k))?.as_str()
}

fn at<'a>(json: &'a Json, path: &[&str]) -> Option<&'a Json> {
    path.iter().try_fold(json, |j, k| j.get(k))
}

fn point(
    key: &str,
    label: &str,
    kind: Kind,
    write: bool,
    unit: Option<Unit>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

fn numeric(min: Option<f64>, max: Option<f64>) -> Kind {
    Kind::Numeric {
        min,
        max,
        step: None,
    }
}

/// Builds devices and the routing index from `GET /clip/v2/resource`.
pub fn load(instance: &InstanceId, resources: &[Json]) -> Load {
    let by_id: HashMap<&str, &Json> = resources
        .iter()
        .filter_map(|r| Some((r.get("id")?.as_str()?, r)))
        .collect();

    let rooms = rooms_by_device(resources);
    let mut load = Load::default();
    for raw in resources.iter().filter(|r| r["type"] == "device") {
        let Some(hue_id) = raw["id"].as_str() else {
            continue;
        };
        let id = DeviceId::new(instance, hue_id);
        let mut points = Vec::new();
        let mut counts: HashMap<&str, usize> = HashMap::new();

        for service in raw["services"].as_array().into_iter().flatten() {
            let (Some(rid), Some(rtype)) = (service["rid"].as_str(), service["rtype"].as_str())
            else {
                continue;
            };
            let Some(resource) = by_id.get(rid) else {
                continue;
            };
            let n = counts.entry(rtype).and_modify(|n| *n += 1).or_insert(1);
            // First service of a type gets bare keys; extra ones are prefixed.
            let prefix = if *n == 1 {
                String::new()
            } else {
                format!("{rtype}{n}.")
            };
            let ordinal = *n;
            let target = Target {
                device: &id,
                rid,
                resource,
                prefix,
                ordinal,
            };
            let Some(feed) = service_points(rtype, target, &mut points, &mut load.index.writes)
            else {
                continue;
            };
            load.index.services.insert(
                rid.to_owned(),
                Service {
                    device: id.clone(),
                    feed,
                },
            );
        }

        // The bridge itself and other point-less devices are not exposed.
        if points.is_empty() {
            load.index.services.retain(|_, s| s.device != id);
            continue;
        }
        load.devices.push(Device {
            id,
            instance: instance.clone(),
            native_name: str_at(raw, &["metadata", "name"])
                .unwrap_or(hue_id)
                .trim()
                .into(),
            manufacturer: str_at(raw, &["product_data", "manufacturer_name"]).map(Into::into),
            model: str_at(raw, &["product_data", "model_id"]).map(Into::into),
            description: str_at(raw, &["product_data", "product_name"]).map(Into::into),
            native_room: rooms.get(hue_id).map(|r| (*r).into()),
            members: Vec::new(),
            points,
        });
    }
    groups(instance, resources, &by_id, &mut load);
    load
}

/// Hue rooms and zones, through their `grouped_light`: on/off and
/// brightness of a whole room at once.
///
/// Rooms are writable: the guard knows them by name (a protected bedroom
/// stays protected). Zones are read-only: a zone may span several rooms,
/// a bedroom included, and must never become a way around the guard.
fn groups(
    instance: &InstanceId,
    resources: &[Json],
    by_id: &HashMap<&str, &Json>,
    load: &mut Load,
) {
    for group in resources
        .iter()
        .filter(|r| r["type"] == "room" || r["type"] == "zone")
    {
        let (Some(group_id), Some(name)) =
            (group["id"].as_str(), str_at(group, &["metadata", "name"]))
        else {
            continue;
        };
        let Some((rid, light)) = group["services"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["rtype"] == "grouped_light")
            .find_map(|s| {
                let rid = s["rid"].as_str()?;
                Some((rid, *by_id.get(rid)?))
            })
        else {
            continue;
        };
        let is_room = group["type"] == "room";
        let id = DeviceId::new(instance, group_id);
        let mut points = Vec::new();
        if light.get("on").is_some() {
            points.push(point(
                "on",
                &moli_i18n::tr!("pilotes.hue.etat"),
                Kind::Binary,
                is_room,
                None,
                Semantic::OnOff,
            ));
            if is_room {
                load.index.writes.insert(
                    (id.clone(), "on".into()),
                    Write::On {
                        light: format!("grouped_light/{rid}"),
                    },
                );
            }
        }
        if light.get("dimming").is_some() {
            let kind = numeric(Some(0.0), Some(100.0));
            points.push(point(
                "brightness",
                &moli_i18n::tr!("pilotes.hue.luminosite"),
                kind,
                is_room,
                Some(Unit::Percent),
                Semantic::Brightness,
            ));
            if is_room {
                load.index.writes.insert(
                    (id.clone(), "brightness".into()),
                    Write::Brightness {
                        light: format!("grouped_light/{rid}"),
                    },
                );
            }
        }
        if points.is_empty() {
            continue;
        }
        load.index.services.insert(
            rid.to_owned(),
            Service {
                device: id.clone(),
                feed: Feed::Light {
                    prefix: String::new(),
                },
            },
        );
        load.devices.push(Device {
            id,
            instance: instance.clone(),
            native_name: name.trim().into(),
            manufacturer: Some("Signify".into()),
            model: Some(if is_room { "room" } else { "zone" }.into()),
            description: Some(
                if is_room {
                    moli_i18n::tr!("pilotes.hue.piece")
                } else {
                    moli_i18n::tr!("pilotes.hue.zone")
                }
                .into(),
            ),
            native_room: is_room.then(|| name.trim().into()),
            // The guard checks every member's rooms: a lamp labelled
            // « Petite chambre » but in the Hue room « Salon » keeps its
            // protection when an agent targets « Salon ».
            members: group["children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["rtype"] == "device")
                .filter_map(|c| c["rid"].as_str())
                .map(|rid| DeviceId::new(instance, rid))
                .collect(),
            points,
        });
    }
}

/// Hue room of every device id.
fn rooms_by_device(resources: &[Json]) -> HashMap<&str, &str> {
    let mut rooms = HashMap::new();
    for room in resources.iter().filter(|r| r["type"] == "room") {
        let Some(name) = str_at(room, &["metadata", "name"]) else {
            continue;
        };
        for child in room["children"].as_array().into_iter().flatten() {
            if child["rtype"] == "device"
                && let Some(rid) = child["rid"].as_str()
            {
                rooms.insert(rid, name);
            }
        }
    }
    rooms
}

/// One service of a device, being translated.
struct Target<'a> {
    device: &'a DeviceId,
    rid: &'a str,
    resource: &'a Json,
    /// Empty for the first service of a type, `light2.` etc. for the next.
    prefix: String,
    /// Rank of this service among those of the same type (1-based).
    ordinal: usize,
}

/// Adds the points a service provides; `None` for services Moli ignores.
fn service_points(
    rtype: &str,
    t: Target<'_>,
    points: &mut Vec<PointSpec>,
    writes: &mut HashMap<(DeviceId, Arc<str>), Write>,
) -> Option<Feed> {
    let prefix = t.prefix;
    let sensor = |name: &str, label: String, kind: Kind, unit: Option<Unit>, semantic| {
        point(
            &format!("{prefix}{name}"),
            &label,
            kind,
            false,
            unit,
            semantic,
        )
    };
    Some(match rtype {
        "light" => {
            light_points(t.device, t.rid, t.resource, &prefix, points, writes);
            Feed::Light { prefix }
        }
        "button" => {
            let control = t.resource["metadata"]["control_id"]
                .as_u64()
                .unwrap_or(t.ordinal as u64);
            let key: Arc<str> = format!("button{control}").into();
            let values = t.resource["button"]["event_values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Json::as_str)
                .map(Into::into)
                .collect();
            let label = moli_i18n::tr!("pilotes.hue.bouton", n = control);
            points.push(point(
                &key,
                &label,
                Kind::Enum { values },
                false,
                None,
                Semantic::Other,
            ));
            Feed::Button { key }
        }
        "device_power" => {
            let kind = numeric(Some(0.0), Some(100.0));
            points.push(point(
                "battery",
                &moli_i18n::tr!("pilotes.hue.batterie"),
                kind,
                false,
                Some(Unit::Percent),
                Semantic::Battery,
            ));
            Feed::Battery
        }
        "zigbee_connectivity" => Feed::Connectivity,
        "motion" => {
            points.push(sensor(
                "motion",
                moli_i18n::tr!("pilotes.hue.mouvement"),
                Kind::Binary,
                None,
                Semantic::Occupancy,
            ));
            Feed::Motion { prefix }
        }
        "temperature" => {
            let kind = numeric(None, None);
            points.push(sensor(
                "temperature",
                moli_i18n::tr!("pilotes.hue.temperature"),
                kind,
                Some(Unit::Celsius),
                Semantic::Temperature,
            ));
            Feed::Temperature { prefix }
        }
        "light_level" => {
            let kind = numeric(Some(0.0), None);
            points.push(sensor(
                "illuminance",
                moli_i18n::tr!("pilotes.hue.eclairement"),
                kind,
                Some(Unit::Lux),
                Semantic::Illuminance,
            ));
            Feed::LightLevel { prefix }
        }
        "contact" => {
            points.push(sensor(
                "contact",
                moli_i18n::tr!("pilotes.hue.contact"),
                Kind::Binary,
                None,
                Semantic::Contact,
            ));
            Feed::Contact { prefix }
        }
        _ => return None,
    })
}

#[allow(clippy::too_many_lines)] // one block per light capability
fn light_points(
    device: &DeviceId,
    rid: &str,
    light: &Json,
    prefix: &str,
    points: &mut Vec<PointSpec>,
    writes: &mut HashMap<(DeviceId, Arc<str>), Write>,
) {
    let mut add = |key: String, spec: PointSpec, write: Option<Write>| {
        if let Some(write) = write {
            writes.insert((device.clone(), key.into()), write);
        }
        points.push(spec);
    };
    let rid = format!("light/{rid}");
    if light.get("on").is_some() {
        let key = format!("{prefix}on");
        add(
            key.clone(),
            point(
                &key,
                &moli_i18n::tr!("pilotes.hue.etat"),
                Kind::Binary,
                true,
                None,
                Semantic::OnOff,
            ),
            Some(Write::On { light: rid.clone() }),
        );
    }
    if let Some(dimming) = light.get("dimming") {
        let key = format!("{prefix}brightness");
        let min = dimming["min_dim_level"].as_f64().or(Some(0.0));
        add(
            key.clone(),
            point(
                &key,
                &moli_i18n::tr!("pilotes.hue.luminosite"),
                numeric(min, Some(100.0)),
                true,
                Some(Unit::Percent),
                Semantic::Brightness,
            ),
            Some(Write::Brightness { light: rid.clone() }),
        );
    }
    if let Some(ct) = light.get("color_temperature") {
        let key = format!("{prefix}color_temp");
        let schema = &ct["mirek_schema"];
        add(
            key.clone(),
            point(
                &key,
                &moli_i18n::tr!("pilotes.hue.temperature_couleur"),
                numeric(
                    schema["mirek_minimum"].as_f64(),
                    schema["mirek_maximum"].as_f64(),
                ),
                true,
                Some(Unit::Other("mired".into())),
                Semantic::ColorTemp,
            ),
            Some(Write::Mirek { light: rid.clone() }),
        );
    }
    if light.get("color").is_some() {
        // The colour people pick (« #rrggbb »); x and y stay for history.
        let key = format!("{prefix}color");
        add(
            key.clone(),
            point(
                &key,
                &moli_i18n::tr!("pilotes.hue.couleur"),
                Kind::Text,
                true,
                None,
                Semantic::Color,
            ),
            Some(Write::Xy { light: rid.clone() }),
        );
        for axis in ["x", "y"] {
            let key = format!("{prefix}color.{axis}");
            add(
                key.clone(),
                point(
                    &key,
                    &moli_i18n::tr!("pilotes.hue.couleur_axe", axis = axis),
                    numeric(Some(0.0), Some(1.0)),
                    false,
                    None,
                    Semantic::Color,
                ),
                None,
            );
        }
    }
    // The animations the lamp plays by itself (gen 3 bulbs).
    let effects: Vec<Arc<str>> = light["effects"]["status_values"]
        .as_array()
        .or_else(|| light["effects"]["effect_values"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| Some(Arc::from(v.as_str()?)))
        .collect();
    if effects.iter().any(|e| &**e != "no_effect") {
        let key = format!("{prefix}effect");
        add(
            key.clone(),
            point(
                &key,
                &moli_i18n::tr!("pilotes.hue.effet"),
                Kind::Enum { values: effects },
                true,
                None,
                Semantic::Other,
            ),
            Some(Write::Effect { light: rid.clone() }),
        );
    }
}

/// Light level is reported as `10000·log10(lux) + 1`.
#[allow(clippy::cast_possible_truncation)]
fn lux(light_level: f64) -> Value {
    Value::Int(10f64.powf((light_level - 1.0) / 10_000.0).round() as i64)
}

impl Index {
    /// Decodes a resource (full, or a partial event item) into values.
    /// Unknown services yield `None`.
    pub fn decode(&self, item: &Json) -> Option<Update> {
        let service = self.services.get(item.get("id")?.as_str()?)?;
        let mut update = Update {
            device: Some(service.device.clone()),
            ..Update::default()
        };
        let mut put = |key: String, value: Value| update.values.push((key.into(), value));
        match &service.feed {
            Feed::Light { prefix } => {
                if let Some(on) = at(item, &["on", "on"]).and_then(Json::as_bool) {
                    put(format!("{prefix}on"), Value::Bool(on));
                }
                if let Some(b) = at(item, &["dimming", "brightness"]).and_then(Json::as_f64) {
                    put(format!("{prefix}brightness"), Value::Float(b));
                }
                // `mirek: null` means "in color mode": a real, known state.
                if let Some(mirek) = at(item, &["color_temperature", "mirek"]) {
                    put(format!("{prefix}color_temp"), Value::from_json(mirek));
                }
                let x = at(item, &["color", "xy", "x"]).and_then(Json::as_f64);
                let y = at(item, &["color", "xy", "y"]).and_then(Json::as_f64);
                if let Some(hex) = x.zip(y).and_then(|(x, y)| xy_to_hex(x, y)) {
                    put(format!("{prefix}color"), Value::from(hex.as_str()));
                }
                for (axis, v) in [("x", x), ("y", y)] {
                    if let Some(v) = v {
                        put(format!("{prefix}color.{axis}"), Value::Float(v));
                    }
                }
                if let Some(effect) = str_at(item, &["effects", "status"]) {
                    put(format!("{prefix}effect"), Value::from(effect));
                }
            }
            Feed::Button { key } => {
                let event = str_at(item, &["button", "button_report", "event"])
                    .or_else(|| str_at(item, &["button", "last_event"]));
                if let Some(event) = event {
                    put(key.to_string(), Value::from(event));
                }
            }
            Feed::Battery => {
                if let Some(level) =
                    at(item, &["power_state", "battery_level"]).and_then(Json::as_i64)
                {
                    put("battery".into(), Value::Int(level));
                }
            }
            Feed::Connectivity => {
                if let Some(status) = item["status"].as_str() {
                    update.online = Some(status == "connected");
                }
            }
            Feed::Motion { prefix } => {
                let motion = at(item, &["motion", "motion_report", "motion"])
                    .or_else(|| at(item, &["motion", "motion"]));
                if let Some(m) = motion.and_then(Json::as_bool) {
                    put(format!("{prefix}motion"), Value::Bool(m));
                }
            }
            Feed::Temperature { prefix } => {
                let t = at(item, &["temperature", "temperature_report", "temperature"])
                    .or_else(|| at(item, &["temperature", "temperature"]));
                if let Some(t) = t.and_then(Json::as_f64) {
                    put(format!("{prefix}temperature"), Value::Float(t));
                }
            }
            Feed::LightLevel { prefix } => {
                let level = at(item, &["light", "light_level_report", "light_level"])
                    .or_else(|| at(item, &["light", "light_level"]));
                if let Some(level) = level.and_then(Json::as_f64) {
                    put(format!("{prefix}illuminance"), lux(level));
                }
            }
            Feed::Contact { prefix } => {
                if let Some(state) = str_at(item, &["contact_report", "state"]) {
                    put(format!("{prefix}contact"), Value::Bool(state == "contact"));
                }
            }
        }
        update.pulse = matches!(service.feed, Feed::Button { .. });
        Some(update)
    }

    /// The `PUT` (path, body) for a validated write, if the point is writable.
    #[must_use]
    pub fn encode(&self, device: &DeviceId, key: &str, value: &Value) -> Option<(String, Json)> {
        let write = self.writes.get(&(device.clone(), Arc::from(key)))?;
        let (light, body) = match (write, value) {
            (Write::On { light }, Value::Bool(on)) => (light, json!({ "on": { "on": on } })),
            (Write::Brightness { light }, v) => {
                (light, json!({ "dimming": { "brightness": v.as_f64()? } }))
            }
            #[allow(clippy::cast_possible_truncation)]
            (Write::Mirek { light }, v) => (
                light,
                json!({ "color_temperature": { "mirek": v.as_f64()?.round() as i64 } }),
            ),
            (Write::Xy { light }, Value::Text(hex)) => {
                let (x, y) = hex_to_xy(hex)?;
                (light, json!({ "color": { "xy": { "x": x, "y": y } } }))
            }
            (Write::Effect { light }, Value::Text(effect)) => {
                (light, json!({ "effects": { "effect": &**effect } }))
            }
            _ => return None,
        };
        Some((format!("/clip/v2/resource/{light}"), body))
    }

    /// Whether a service id is known (else an `add` event means topology changed).
    #[must_use]
    pub fn knows(&self, rid: &str) -> bool {
        self.services.contains_key(rid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Load {
        let raw: Json = serde_json::from_slice(include_bytes!(
            "../../../integrations/hue/fixtures/resources.json"
        ))
        .unwrap();
        load(&InstanceId::from("hue"), raw["data"].as_array().unwrap())
    }

    fn device<'a>(load: &'a Load, name: &str) -> &'a Device {
        load.devices
            .iter()
            .find(|d| &*d.native_name == name)
            .unwrap()
    }

    #[test]
    fn real_bridge_is_mapped() {
        let load = fixture();
        // 20 Hue devices minus the bridge itself, plus the 8 rooms.
        assert_eq!(load.devices.len(), 27);
        let rooms: Vec<_> = load
            .devices
            .iter()
            .filter(|d| d.model.as_deref() == Some("room"))
            .collect();
        assert_eq!(rooms.len(), 8);
        assert!(
            rooms
                .iter()
                .all(|r| r.native_room.as_deref() == Some(&*r.native_name)),
            "a room is its own room"
        );
        assert!(
            load.devices
                .iter()
                .all(|d| d.id.as_str().starts_with("hue:"))
        );
        assert!(load.devices.iter().all(|d| !d.points.is_empty()));
    }

    #[test]
    fn rooms_are_commandable_as_a_whole_and_zones_never_are() {
        let load = fixture();
        let room = load
            .devices
            .iter()
            .find(|d| d.model.as_deref() == Some("room") && &*d.native_name == "Petite chambre")
            .unwrap();
        let (path, body) = load
            .index
            .encode(&room.id, "on", &Value::Bool(false))
            .unwrap();
        assert!(
            path.starts_with("/clip/v2/resource/grouped_light/"),
            "{path}"
        );
        assert_eq!(body, json!({"on": {"on": false}}));

        // A synthetic zone: visible, never writable.
        let resources = vec![
            json!({"id": "z1", "type": "zone", "metadata": {"name": "Toute la maison"},
                   "services": [{"rid": "g1", "rtype": "grouped_light"}]}),
            json!({"id": "g1", "type": "grouped_light", "on": {"on": true}, "dimming": {"brightness": 50.0}}),
        ];
        let zones = load_fn(&resources);
        let zone = &zones.devices[0];
        assert!(zone.points.iter().all(|p| !p.access.write));
        assert!(
            zones
                .index
                .encode(&zone.id, "on", &Value::Bool(true))
                .is_none()
        );
        assert_eq!(zone.native_room, None);
    }

    fn load_fn(resources: &[Json]) -> Load {
        load(&InstanceId::from("hue"), resources)
    }

    #[test]
    fn dimmable_light_points_and_rooms() {
        let load = fixture();
        let lamp = device(&load, "Lampe ronde");
        let keys: Vec<&str> = lamp.points.iter().map(|p| &*p.key).collect();
        assert_eq!(
            keys,
            [
                "on",
                "brightness",
                "color_temp",
                "color",
                "color.x",
                "color.y"
            ]
        );
        assert!(lamp.point("color").unwrap().access.write);
        assert!(lamp.point("effect").is_none(), "no animations on this one");
        let globe = device(&load, "Globe");
        let Kind::Enum { values } = &globe.point("effect").unwrap().kind else {
            panic!("effects are an enum");
        };
        assert!(values.iter().any(|v| &**v == "fire"));
        let (_, body) = load
            .index
            .encode(&globe.id, "effect", &Value::from("fire"))
            .unwrap();
        assert_eq!(body, json!({"effects": {"effect": "fire"}}));
        let ct = lamp.point("color_temp").unwrap();
        assert_eq!(
            ct.kind,
            Kind::Numeric {
                min: Some(142.0),
                max: Some(500.0),
                step: None
            }
        );
        assert!(!lamp.point("color.x").unwrap().access.write);
        assert!(
            load.devices.iter().any(|d| d.native_room.is_some()),
            "rooms are suggested"
        );
    }

    #[test]
    fn dimmer_switch_has_buttons_and_battery() {
        let load = fixture();
        let dimmer = device(&load, "Hue dimmer switch 1");
        let keys: Vec<&str> = dimmer.points.iter().map(|p| &*p.key).collect();
        assert_eq!(
            keys,
            ["button1", "button2", "button3", "button4", "battery"]
        );
    }

    #[test]
    fn full_resources_decode_including_unreachable_lights() {
        let raw: Json = serde_json::from_slice(include_bytes!(
            "../../../integrations/hue/fixtures/resources.json"
        ))
        .unwrap();
        let load = fixture();
        let mut offline = 0;
        let mut values = 0;
        for item in raw["data"].as_array().unwrap() {
            if let Some(update) = load.index.decode(item) {
                values += update.values.len();
                offline += usize::from(update.online == Some(false));
            }
        }
        assert!(
            values > 40,
            "every light reports its state on load ({values})"
        );
        assert!(offline >= 1, "connectivity issues become offline devices");
    }

    #[test]
    fn events_decode_partially() {
        let load = fixture();
        let lamp = device(&load, "Lampe ronde");
        let event = json!({"id": "00000000-0000-4000-8000-00000000015b", "type": "light", "on": {"on": true}});
        let update = load.index.decode(&event).unwrap();
        assert_eq!(update.device.as_ref(), Some(&lamp.id));
        assert_eq!(update.values, vec![(Arc::from("on"), Value::Bool(true))]);

        let button = json!({"id": "00000000-0000-4000-8000-000000000102", "type": "button",
            "button": {"button_report": {"event": "short_release", "updated": "2026-10-02T20:00:00Z"}}});
        let update = load.index.decode(&button).unwrap();
        assert!(update.pulse, "button presses are occurrences");
        assert_eq!(
            update.values,
            vec![(Arc::from("button2"), Value::from("short_release"))]
        );

        assert!(
            load.index
                .decode(&json!({"id": "unknown", "type": "light"}))
                .is_none()
        );
    }

    #[test]
    fn commands_encode() {
        let load = fixture();
        let lamp = device(&load, "Lampe ronde");
        let (path, body) = load
            .index
            .encode(&lamp.id, "on", &Value::Bool(true))
            .unwrap();
        assert_eq!(
            path,
            "/clip/v2/resource/light/00000000-0000-4000-8000-00000000015b"
        );
        assert_eq!(body, json!({"on": {"on": true}}));
        let (_, body) = load
            .index
            .encode(&lamp.id, "color_temp", &Value::Float(300.4))
            .unwrap();
        assert_eq!(body, json!({"color_temperature": {"mirek": 300}}));
        assert!(
            load.index
                .encode(&lamp.id, "color.x", &Value::Float(0.3))
                .is_none()
        );
        let (_, body) = load
            .index
            .encode(&lamp.id, "color", &Value::from("#ff0000"))
            .unwrap();
        assert_eq!(body, json!({"color": {"xy": {"x": 0.7006, "y": 0.2993}}}));
        assert!(
            load.index
                .encode(&lamp.id, "color", &Value::from("rouge"))
                .is_none()
        );
        // An event carrying xy also gives the colour as people see it.
        let update = load
            .index
            .decode(&json!({
                "id": "00000000-0000-4000-8000-00000000015b",
                "color": {"xy": {"x": 0.7006, "y": 0.2993}}
            }))
            .unwrap();
        assert!(
            update.values.iter().any(
                |(k, v)| &**k == "color" && matches!(v, Value::Text(s) if s.starts_with("#ff"))
            )
        );
    }

    #[test]
    fn sensors_decode() {
        let mut load = Load::default();
        let id = DeviceId::from("hue:m");
        for (rid, feed) in [
            (
                "m",
                Feed::Motion {
                    prefix: String::new(),
                },
            ),
            (
                "t",
                Feed::Temperature {
                    prefix: String::new(),
                },
            ),
            (
                "l",
                Feed::LightLevel {
                    prefix: String::new(),
                },
            ),
            (
                "c",
                Feed::Contact {
                    prefix: String::new(),
                },
            ),
        ] {
            load.index.services.insert(
                rid.into(),
                Service {
                    device: id.clone(),
                    feed,
                },
            );
        }
        let one = |item: Json| load.index.decode(&item).unwrap().values.remove(0).1;
        assert_eq!(
            one(json!({"id": "m", "motion": {"motion_report": {"motion": true}}})),
            Value::Bool(true)
        );
        assert_eq!(
            one(json!({"id": "t", "temperature": {"temperature": 21.5}})),
            Value::Float(21.5)
        );
        assert_eq!(
            one(json!({"id": "l", "light": {"light_level": 20001.0}})),
            Value::Int(100)
        );
        assert_eq!(
            one(json!({"id": "c", "contact_report": {"state": "no_contact"}})),
            Value::Bool(false)
        );
    }
}

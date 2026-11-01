//! Pure translation between the Dirigera's model and Moli's. No IO.
//!
//! `GET /v1/home` lists `devices`. Each has a `deviceType` (what it is),
//! `attributes` (description and state in one object), what it accepts
//! (`capabilities.canReceive`), its room and `isReachable`. A Moli device is
//! a Dirigera device; its points come from the attributes it actually
//! carries: two devices of one type can differ (an old TRADFRI motion sensor
//! has no `isDetected`; a TIMMERFLOTTE shows as two devices, one per
//! measure). Events carry the same object, with only what changed.

use std::collections::HashMap;
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::{Value as Json, json};

/// How an attribute becomes a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    /// As it comes (a boolean or a number).
    Plain,
    /// `isOpen` → `contact`: true means closed, as for Hue.
    Closed,
    /// Kelvins → mireds, what Hue and the light fixtures (`moli-lights`)
    /// speak: an IKEA and a Hue lamp are set together.
    Mired,
}

/// One point fed by one attribute.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Feed {
    key: &'static str,
    attribute: &'static str,
    read: Read,
}

/// Everything known about one device after a load.
#[derive(Debug)]
struct Entry {
    id: DeviceId,
    /// What a rename or a move changes: compared with events.
    name: String,
    room: Option<String>,
    feeds: Vec<Feed>,
    /// `color` is computed from `colorHue` and `colorSaturation`.
    color: bool,
    /// Remote presses become `button`.
    button: bool,
    /// Writable point keys (the attribute is in `canReceive`).
    writes: Vec<&'static str>,
    /// Colour temperatures the lamp accepts, in kelvins (low, high).
    kelvins: Option<(f64, f64)>,
    /// Last colour seen: an event may carry the hue or the saturation alone.
    hue: Option<f64>,
    saturation: Option<f64>,
}

/// Everything needed to route events and commands after a full load.
#[derive(Debug, Default)]
pub struct Index {
    /// By Dirigera id.
    devices: HashMap<String, Entry>,
    /// Moli id → Dirigera id.
    natives: HashMap<DeviceId, String>,
}

/// A device the driver does not publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub device_type: String,
    /// Known to the driver and left out on purpose (the hub itself,
    /// repeaters, blinds…), as opposed to a type never seen before.
    pub known: bool,
}

/// Result of loading the devices of `/v1/home`.
#[derive(Debug, Default)]
pub struct Load {
    pub devices: Vec<Device>,
    pub index: Index,
    pub skipped: Vec<Skipped>,
}

/// Values carried by one device object (full, or partial as in events).
#[derive(Debug, Default, PartialEq)]
pub struct Update {
    pub device: Option<DeviceId>,
    pub values: Vec<(Arc<str>, Value)>,
    pub online: Option<bool>,
}

/// What a websocket message asks of the driver.
#[derive(Debug, PartialEq)]
pub enum Event<'a> {
    /// Values changed: decode the device object.
    State(&'a Json),
    /// A remote's button was pressed: an occurrence.
    Press(&'a Json),
    /// A device was added, removed, renamed or moved, or a room changed:
    /// load everything again.
    Reload,
    /// Nothing for Moli (scenes, pong, music…).
    Ignore,
}

enum Family {
    Light,
    Outlet,
    Sensor,
    Remote,
    /// Not published: `known` when left out on purpose.
    Skipped {
        known: bool,
    },
}

fn family(device_type: &str) -> Family {
    match device_type {
        "light" => Family::Light,
        "outlet" => Family::Outlet,
        "environmentSensor" | "motionSensor" | "occupancySensor" | "lightSensor"
        | "openCloseSensor" | "waterSensor" => Family::Sensor,
        t if t.ends_with("Controller") || t == "genericSwitch" => Family::Remote,
        // The hub itself and repeaters (nothing to show), then what v1
        // leaves for later: blinds need a position in Moli's core first.
        "gateway" | "repeater" | "blinds" | "airPurifier" | "speaker" => {
            Family::Skipped { known: true }
        }
        _ => Family::Skipped { known: false },
    }
}

/// Read-only measures (attribute, key) a device of this type may carry.
fn measures(device_type: &str) -> &'static [(&'static str, &'static str)] {
    match device_type {
        "outlet" => &[
            ("currentActivePower", "power"),
            ("currentAmps", "current"),
            ("currentVoltage", "voltage"),
            ("totalEnergyConsumed", "energy"),
        ],
        "environmentSensor" => &[
            ("currentTemperature", "temperature"),
            ("currentRH", "humidity"),
            ("currentPM25", "pm25"),
            ("vocIndex", "voc_index"),
            ("currentCO2", "co2"),
        ],
        "motionSensor" | "occupancySensor" => &[("isDetected", "motion")],
        "lightSensor" => &[("illuminance", "illuminance")],
        "openCloseSensor" => &[("isOpen", "contact")],
        "waterSensor" => &[("waterLeakDetected", "leak")],
        _ => &[],
    }
}

fn numeric(min: Option<f64>, max: Option<f64>) -> Kind {
    Kind::Numeric {
        min,
        max,
        step: None,
    }
}

/// The point a key describes (label, kind, unit, meaning), read-only.
#[allow(clippy::too_many_lines)] // one arm per point
fn spec(key: &str) -> PointSpec {
    use moli_i18n::tr;
    let (label, kind, unit, semantic) = match key {
        "on" => (
            tr!("pilotes.dirigera.etat"),
            Kind::Binary,
            None,
            Semantic::OnOff,
        ),
        "brightness" => (
            tr!("pilotes.dirigera.luminosite"),
            numeric(Some(1.0), Some(100.0)),
            Some(Unit::Percent),
            Semantic::Brightness,
        ),
        "color_temp" => (
            tr!("pilotes.dirigera.temperature_couleur"),
            numeric(None, None),
            Some(Unit::Other("mired".into())),
            Semantic::ColorTemp,
        ),
        "color" => (
            tr!("pilotes.dirigera.couleur"),
            Kind::Text,
            None,
            Semantic::Color,
        ),
        "power" => (
            tr!("pilotes.dirigera.puissance"),
            numeric(None, None),
            Some(Unit::Watt),
            Semantic::Power,
        ),
        "current" => (
            tr!("pilotes.dirigera.courant"),
            numeric(None, None),
            Some(Unit::Ampere),
            Semantic::Current,
        ),
        "voltage" => (
            tr!("pilotes.dirigera.tension"),
            numeric(None, None),
            Some(Unit::Volt),
            Semantic::Voltage,
        ),
        "energy" => (
            tr!("pilotes.dirigera.energie"),
            numeric(Some(0.0), None),
            Some(Unit::KiloWattHour),
            Semantic::Energy,
        ),
        "temperature" => (
            tr!("pilotes.dirigera.temperature"),
            numeric(None, None),
            Some(Unit::Celsius),
            Semantic::Temperature,
        ),
        "humidity" => (
            tr!("pilotes.dirigera.humidite"),
            numeric(Some(0.0), Some(100.0)),
            Some(Unit::Percent),
            Semantic::Humidity,
        ),
        "pm25" => (
            tr!("pilotes.dirigera.pm25"),
            numeric(Some(0.0), None),
            Some(Unit::Other("µg/m³".into())),
            Semantic::Other,
        ),
        "voc_index" => (
            tr!("pilotes.dirigera.voc"),
            numeric(Some(0.0), None),
            None,
            Semantic::Other,
        ),
        "co2" => (
            tr!("pilotes.dirigera.co2"),
            numeric(Some(0.0), None),
            Some(Unit::Ppm),
            Semantic::Other,
        ),
        "motion" => (
            tr!("pilotes.dirigera.mouvement"),
            Kind::Binary,
            None,
            Semantic::Occupancy,
        ),
        "illuminance" => (
            tr!("pilotes.dirigera.eclairement"),
            numeric(Some(0.0), None),
            Some(Unit::Lux),
            Semantic::Illuminance,
        ),
        "contact" => (
            tr!("pilotes.dirigera.contact"),
            Kind::Binary,
            None,
            Semantic::Contact,
        ),
        "leak" => (
            tr!("pilotes.dirigera.fuite"),
            Kind::Binary,
            None,
            Semantic::WaterLeak,
        ),
        "battery" => (
            tr!("pilotes.dirigera.batterie"),
            numeric(Some(0.0), Some(100.0)),
            Some(Unit::Percent),
            Semantic::Battery,
        ),
        "button" => (
            tr!("pilotes.dirigera.bouton"),
            Kind::Text,
            None,
            Semantic::Other,
        ),
        other => (other.to_owned(), Kind::Text, None, Semantic::Other),
    };
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

/// A colour temperature as the hub may give it: kelvins, or mireds for
/// some bounds (KAJPLATS: 153 to 454). No lamp has a kelvin value below
/// 1 000 and mireds are never above it: unambiguous.
fn kelvins(raw: f64) -> Option<f64> {
    if !raw.is_finite() || raw <= 0.0 {
        None
    } else if raw < 1000.0 {
        Some(1_000_000.0 / raw)
    } else {
        Some(raw)
    }
}

/// Kelvins → mireds, as a whole number.
#[allow(clippy::cast_possible_truncation)]
fn mireds(kelvin: f64) -> i64 {
    (1_000_000.0 / kelvin).round() as i64
}

fn text<'a>(json: &'a Json, key: &str) -> Option<&'a str> {
    json.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn strings<'a>(json: &'a Json, path: &[&str]) -> Vec<&'a str> {
    path.iter()
        .try_fold(json, |j, k| j.get(k))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
        .collect()
}

/// `singlePress` → `single`; an unknown pattern stays as it is.
fn click(pattern: &str) -> &str {
    pattern.strip_suffix("Press").unwrap_or(pattern)
}

/// Adds the point `key`, fed by `feed`, writable or not.
fn add<'p>(
    entry: &mut Entry,
    points: &'p mut Vec<PointSpec>,
    key: &'static str,
    feed: Option<(&'static str, Read)>,
    write: bool,
) -> &'p mut PointSpec {
    if write {
        entry.writes.push(key);
    }
    if let Some((attribute, read)) = feed {
        entry.feeds.push(Feed {
            key,
            attribute,
            read,
        });
    }
    let mut point = spec(key);
    point.access.write = write;
    points.push(point);
    points.last_mut().expect("just pushed")
}

/// Builds devices and the routing index from the `devices` of `/v1/home`.
pub fn load(instance: &InstanceId, raw: &[Json]) -> Load {
    let mut load = Load::default();
    for item in raw {
        let (Some(native), Some(device_type)) = (
            item["id"].as_str().filter(|id| !id.is_empty()),
            item["deviceType"].as_str(),
        ) else {
            continue;
        };
        let Some((device, entry)) = device(instance, native, device_type, item, &mut load) else {
            continue;
        };
        load.index
            .natives
            .insert(device.id.clone(), native.to_owned());
        load.index.devices.insert(native.to_owned(), entry);
        load.devices.push(device);
    }
    load
}

/// One device and its entry; `None` when it is not published.
#[allow(clippy::too_many_lines)] // one block per family
fn device(
    instance: &InstanceId,
    native: &str,
    device_type: &str,
    item: &Json,
    load: &mut Load,
) -> Option<(Device, Entry)> {
    let attributes = &item["attributes"];
    let can_receive = strings(item, &["capabilities", "canReceive"]);
    let can_send = strings(item, &["capabilities", "canSend"]);
    let has = |attribute: &str| attributes.get(attribute).is_some();
    let writable = |attribute: &str| can_receive.contains(&attribute);

    let mut entry = Entry {
        id: DeviceId::new(instance, native),
        name: text(attributes, "customName")
            .unwrap_or_default()
            .to_owned(),
        room: item["room"]["id"].as_str().map(str::to_owned),
        feeds: Vec::new(),
        color: false,
        button: false,
        writes: Vec::new(),
        kelvins: None,
        hue: None,
        saturation: None,
    };
    let mut points = Vec::new();
    match family(device_type) {
        Family::Skipped { known } => {
            load.skipped.push(Skipped {
                device_type: device_type.to_owned(),
                known,
            });
            return None;
        }
        Family::Light | Family::Outlet => {
            let light = matches!(family(device_type), Family::Light);
            if has("isOn") || writable("isOn") {
                add(
                    &mut entry,
                    &mut points,
                    "on",
                    Some(("isOn", Read::Plain)),
                    writable("isOn"),
                );
            }
            // An outlet's `lightLevel` is a leftover of the TRADFRI outlet.
            if light && (has("lightLevel") || writable("lightLevel")) {
                let write = writable("lightLevel");
                add(
                    &mut entry,
                    &mut points,
                    "brightness",
                    Some(("lightLevel", Read::Plain)),
                    write,
                );
            }
            if light && (has("colorTemperature") || writable("colorTemperature")) {
                let bounds = ["colorTemperatureMin", "colorTemperatureMax"]
                    .map(|a| attributes[a].as_f64().and_then(kelvins));
                if let [Some(a), Some(b)] = bounds {
                    entry.kelvins = Some((a.min(b), a.max(b)));
                }
                let write = writable("colorTemperature");
                let feed = Some(("colorTemperature", Read::Mired));
                let point = add(&mut entry, &mut points, "color_temp", feed, write);
                if let Some((low, high)) = entry.kelvins {
                    #[allow(clippy::cast_precision_loss)]
                    let range = numeric(Some(mireds(high) as f64), Some(mireds(low) as f64));
                    point.kind = range;
                }
            }
            if light && has("colorHue") && has("colorSaturation") {
                entry.color = true;
                let write = writable("colorHue") && writable("colorSaturation");
                add(&mut entry, &mut points, "color", None, write);
            }
        }
        Family::Sensor => {}
        Family::Remote => {
            let mut patterns: Vec<Arc<str>> = can_send
                .iter()
                .filter(|s| s.ends_with("Press"))
                .map(|s| Arc::from(click(s)))
                .collect();
            if patterns.is_empty() && matches!(device_type, "shortcutController" | "genericSwitch")
            {
                patterns = ["single", "double", "long"].map(Arc::from).to_vec();
            }
            if !patterns.is_empty() {
                entry.button = true;
                let mut point = spec("button");
                point.kind = Kind::Enum { values: patterns };
                points.push(point);
            }
        }
    }
    for &(attribute, key) in measures(device_type) {
        if has(attribute) {
            let read = if attribute == "isOpen" {
                Read::Closed
            } else {
                Read::Plain
            };
            add(&mut entry, &mut points, key, Some((attribute, read)), false);
        }
    }
    if has("batteryPercentage") {
        add(
            &mut entry,
            &mut points,
            "battery",
            Some(("batteryPercentage", Read::Plain)),
            false,
        );
    }

    // A lamp's driver, a hidden part of a product: nothing to show.
    if points.is_empty() {
        return None;
    }
    let device = Device {
        id: entry.id.clone(),
        instance: instance.clone(),
        native_name: text(attributes, "customName")
            .or_else(|| text(attributes, "model"))
            .unwrap_or(native)
            .into(),
        manufacturer: text(attributes, "manufacturer").map(Into::into),
        model: text(attributes, "model").map(Into::into),
        description: text(attributes, "productCode").map(Into::into),
        native_room: text(&item["room"], "name").map(Into::into),
        members: Vec::new(),
        points,
    };
    Some((device, entry))
}

/// `#rrggbb` → (hue in degrees, saturation 0 to 1), brightness left to
/// `brightness`. `None` for anything else, black included.
fn hex_to_hs(hex: &str) -> Option<(f64, f64)> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| {
        u8::from_str_radix(&digits[i..i + 2], 16)
            .ok()
            .map(|c| f64::from(c) / 255.0)
    };
    let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    if max <= 0.0 {
        return None;
    }
    let hue = if delta <= 0.0 {
        0.0
    } else if (max - r).abs() < f64::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let round = |v: f64, places: f64| (v * places).round() / places;
    Some((round(hue, 10.0), round(delta / max, 1000.0)))
}

/// (hue, saturation) → `#rrggbb`, at full brightness.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
fn hs_to_hex(hue: f64, saturation: f64) -> Option<String> {
    if !(hue.is_finite() && saturation.is_finite()) {
        return None;
    }
    let h = hue.rem_euclid(360.0) / 60.0;
    let c = saturation.clamp(0.0, 1.0);
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = 1.0 - c;
    let byte = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Some(format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b)))
}

impl Index {
    /// Decodes a device object (full, or a partial event) into values.
    /// Unknown devices yield `None`.
    pub fn decode(&mut self, item: &Json) -> Option<Update> {
        let entry = self.devices.get_mut(item.get("id")?.as_str()?)?;
        let attributes = &item["attributes"];
        let mut update = Update {
            device: Some(entry.id.clone()),
            online: item["isReachable"].as_bool(),
            ..Update::default()
        };
        // In colour mode the temperature is a leftover: unknown, as Hue's
        // `mirek: null`.
        let colour_mode = attributes["colorMode"].as_str();
        for feed in &entry.feeds {
            let raw = &attributes[feed.attribute];
            let value = if feed.read == Read::Mired && colour_mode == Some("color") {
                Some(Value::Null)
            } else {
                match (feed.read, raw) {
                    (Read::Plain, Json::Bool(_) | Json::Number(_)) => Some(Value::from_json(raw)),
                    (Read::Closed, Json::Bool(open)) => Some(Value::Bool(!open)),
                    (Read::Mired, Json::Number(n)) => {
                        n.as_f64().and_then(kelvins).map(|k| Value::Int(mireds(k)))
                    }
                    _ => None,
                }
            };
            if let Some(value) = value {
                update.values.push((feed.key.into(), value));
            }
        }
        if entry.color {
            let hue = attributes["colorHue"].as_f64();
            let saturation = attributes["colorSaturation"].as_f64();
            entry.hue = hue.or(entry.hue);
            entry.saturation = saturation.or(entry.saturation);
            if colour_mode == Some("temperature") {
                // A white: the hue and saturation left are a past colour's.
                update.values.push(("color".into(), Value::Null));
            } else if (hue.is_some() || saturation.is_some())
                && let Some(hex) = entry
                    .hue
                    .zip(entry.saturation)
                    .and_then(|(h, s)| hs_to_hex(h, s))
            {
                update
                    .values
                    .push(("color".into(), Value::from(hex.as_str())));
            }
        }
        Some(update)
    }

    /// A remote press: the device and the pattern (`single`, `double`,
    /// `long`).
    #[must_use]
    pub fn press(&self, data: &Json) -> Option<(DeviceId, Value)> {
        let entry = self.devices.get(data["id"].as_str()?)?;
        let pattern = data["clickPattern"].as_str()?;
        entry
            .button
            .then(|| (entry.id.clone(), Value::from(click(pattern))))
    }

    /// What a websocket message asks for.
    #[must_use]
    pub fn classify<'a>(&self, message: &'a Json) -> Event<'a> {
        let data = &message["data"];
        match message["type"].as_str().unwrap_or_default() {
            "deviceStateChanged" | "deviceConfigurationChanged" if self.reshapes(data) => {
                Event::Reload
            }
            "deviceStateChanged" | "deviceConfigurationChanged" => Event::State(data),
            "remotePressEvent" => Event::Press(data),
            "deviceAdded" | "deviceRemoved" | "roomUpdated" | "roomDeleted" => Event::Reload,
            _ => Event::Ignore,
        }
    }

    /// A rename or a move to another room: names and rooms are given at
    /// load, so load again.
    fn reshapes(&self, data: &Json) -> bool {
        let Some(entry) = data["id"].as_str().and_then(|id| self.devices.get(id)) else {
            return false;
        };
        let renamed = text(&data["attributes"], "customName").is_some_and(|n| n != entry.name);
        let moved = data
            .get("room")
            .is_some_and(|room| room["id"].as_str() != entry.room.as_deref());
        renamed || moved
    }

    /// The device to `PATCH` and its body, for a validated write; `None`
    /// when the point is not writable.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn encode(&self, device: &DeviceId, key: &str, value: &Value) -> Option<(String, Json)> {
        let native = self.natives.get(device)?;
        let entry = self.devices.get(native)?;
        if !entry.writes.contains(&key) {
            return None;
        }
        let attributes = match (key, value) {
            ("on", Value::Bool(on)) => json!({ "isOn": on }),
            ("brightness", v) => {
                json!({ "lightLevel": v.as_f64()?.round().clamp(1.0, 100.0) as i64 })
            }
            ("color_temp", v) => {
                let mut kelvin = kelvins(v.as_f64()?)?;
                if let Some((low, high)) = entry.kelvins {
                    kelvin = kelvin.clamp(low, high);
                }
                json!({ "colorTemperature": kelvin.round() as i64 })
            }
            ("color", Value::Text(hex)) => {
                let (hue, saturation) = hex_to_hs(hex)?;
                json!({ "colorHue": hue, "colorSaturation": saturation })
            }
            _ => return None,
        };
        Some((native.clone(), json!([{ "attributes": attributes }])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real hub (firmware 2.975), anonymized: 4 lamps, a MYGGSPRAY sensor
    /// (two devices), the hub itself.
    fn fixture() -> (Load, Vec<Json>) {
        let raw: Json = serde_json::from_slice(include_bytes!(
            "../../../integrations/dirigera/fixtures/home.json"
        ))
        .unwrap();
        let devices = raw["devices"].as_array().unwrap().clone();
        (load(&InstanceId::from("ikea"), &devices), devices)
    }

    fn named<'a>(load: &'a Load, name: &str) -> &'a Device {
        load.devices
            .iter()
            .find(|d| &*d.native_name == name)
            .unwrap()
    }

    fn one(item: Json) -> Load {
        load(&InstanceId::from("ikea"), &[item])
    }

    fn keys(device: &Device) -> Vec<&str> {
        device.points.iter().map(|p| &*p.key).collect()
    }

    fn light(attributes: &Json, can_receive: &[&str]) -> Json {
        json!({
            "id": "l1", "type": "light", "deviceType": "light", "isReachable": true,
            "attributes": attributes,
            "capabilities": {"canSend": [], "canReceive": can_receive},
            "room": {"id": "r1", "name": "Salon"},
        })
    }

    #[test]
    fn the_hub_is_mapped() {
        let (load, raw) = fixture();
        let names: Vec<&str> = load.devices.iter().map(|d| &*d.native_name).collect();
        assert_eq!(
            names,
            [
                "Lampe 1",
                "Luminosité 1",
                "Présence 1",
                "Lampe 2",
                "Lampe 3",
                "Lampe 4"
            ]
        );
        assert_eq!(
            load.skipped,
            vec![Skipped {
                device_type: "gateway".into(),
                known: true
            }],
            "the hub is in its own device list, never published"
        );
        // A TRADFRI driver: dimmable only.
        let driver = named(&load, "Lampe 1");
        assert_eq!(keys(driver), ["on", "brightness"]);
        assert!(driver.points.iter().all(|p| p.access.write));
        assert_eq!(driver.model.as_deref(), Some("TRADFRI Driver 30W"));
        // The hub gives « Chambre » with a trailing no-break space.
        assert_eq!(driver.native_room.as_deref(), Some("Chambre"));
        // Colour bulbs: kelvin bounds named the other way round (4000, 2202).
        for name in ["Lampe 2", "Lampe 3"] {
            let lamp = named(&load, name);
            assert_eq!(keys(lamp), ["on", "brightness", "color_temp", "color"]);
            assert_eq!(
                lamp.point("color_temp").unwrap().kind,
                numeric(Some(250.0), Some(454.0))
            );
        }
        // One MYGGSPRAY, two devices (`_1` light, `_2` presence), one battery each.
        let lux = named(&load, "Luminosité 1");
        assert_eq!(keys(lux), ["illuminance", "battery"]);
        assert_eq!(lux.native_room, None);
        assert!(lux.id.as_str().ends_with("_1"));
        let presence = named(&load, "Présence 1");
        assert_eq!(keys(presence), ["motion", "battery"]);
        assert!(presence.id.as_str().ends_with("_2"));

        let (lux_id, presence_id) = (lux.id.clone(), presence.id.clone());

        // A full load: values and reachability for every published device.
        let mut index = load.index;
        let mut offline = Vec::new();
        for item in &raw {
            if let Some(update) = index.decode(item) {
                let device = update.device.clone().unwrap();
                if update.online == Some(false) {
                    offline.push(device.clone());
                }
                if device == named_id(&raw, "Lampe 3") {
                    assert!(
                        update
                            .values
                            .contains(&(Arc::from("color_temp"), Value::Null))
                    );
                    assert!(
                        update
                            .values
                            .contains(&(Arc::from("color"), Value::from("#ffa448")))
                    );
                }
            }
        }
        assert_eq!(offline, [lux_id, presence_id]);
    }

    /// The Moli id of the fixture device with this (anonymized) name.
    fn named_id(raw: &[Json], name: &str) -> DeviceId {
        let item = raw
            .iter()
            .find(|d| d["attributes"]["customName"] == name)
            .unwrap();
        DeviceId::new(&InstanceId::from("ikea"), item["id"].as_str().unwrap())
    }

    #[test]
    fn events_seen_on_a_hub() {
        // Captured on the same hub: two lamps switched off and on from the
        // IKEA app, then Moli's own commands (30 %, green, 300 mireds) and
        // their undoing. Dimming fades: 60 comes before 30.
        let (mut load, raw) = fixture();
        let events: Vec<Json> = serde_json::from_slice(include_bytes!(
            "../../../integrations/dirigera/fixtures/events.json"
        ))
        .unwrap();
        let strip = named_id(&raw, "Lampe 3");
        let worktop = named_id(&raw, "Lampe 4");
        let mut seen: Vec<(DeviceId, Arc<str>, Value)> = Vec::new();
        for event in &events {
            match load.index.classify(event) {
                Event::State(data) => {
                    let update = load.index.decode(data).unwrap();
                    assert_eq!(update.online, Some(true));
                    let device = update.device.unwrap();
                    seen.extend(
                        update
                            .values
                            .into_iter()
                            .map(|(k, v)| (device.clone(), k, v)),
                    );
                }
                Event::Ignore => assert_eq!(event["type"], "pong"),
                other => panic!("unexpected {other:?}"),
            }
        }
        let of = |device: &DeviceId, key: &str| -> Vec<Value> {
            seen.iter()
                .filter(|(d, k, _)| d == device && &**k == key)
                .map(|(_, _, v)| v.clone())
                .collect()
        };
        assert_eq!(of(&worktop, "on"), [Value::Bool(false), Value::Bool(true)]);
        assert_eq!(
            of(&worktop, "brightness"),
            [60, 30, 37, 67].map(Value::Int).to_vec()
        );
        assert_eq!(
            of(&strip, "color"),
            [Value::from("#00ff00"), Value::Null, Value::from("#ffa449")]
        );
        assert_eq!(
            of(&strip, "color_temp"),
            [Value::Null, Value::Int(300), Value::Null]
        );
    }

    #[test]
    fn a_colour_lamp() {
        let item = light(
            &json!({"customName": " Lampe du salon ", "model": "TRADFRI bulb E27 CWS 806lm",
                   "manufacturer": "IKEA of Sweden", "productCode": "LED1924G9",
                   "isOn": false, "lightLevel": 100, "colorHue": 357.045_116_276_561_54,
                   "colorSaturation": 0.856_628_904_801_055_9, "colorTemperature": 2801,
                   "colorTemperatureMin": 4000, "colorTemperatureMax": 2202, "colorMode": "color"}),
            &[
                "customName",
                "isOn",
                "lightLevel",
                "colorTemperature",
                "colorHue",
                "colorSaturation",
            ],
        );
        let mut load = one(item.clone());
        let lamp = &load.devices[0];
        assert_eq!(lamp.id.as_str(), "ikea:l1");
        assert_eq!(&*lamp.native_name, "Lampe du salon");
        assert_eq!(lamp.native_room.as_deref(), Some("Salon"));
        assert_eq!(lamp.model.as_deref(), Some("TRADFRI bulb E27 CWS 806lm"));
        assert_eq!(keys(lamp), ["on", "brightness", "color_temp", "color"]);
        assert!(lamp.points.iter().all(|p| p.access.write));
        // Kelvin bounds named the other way round: still 250 to 454 mireds.
        assert_eq!(
            lamp.point("color_temp").unwrap().kind,
            numeric(Some(250.0), Some(454.0))
        );
        let update = load.index.decode(&item).unwrap();
        assert_eq!(update.online, Some(true));
        assert_eq!(
            update.values,
            vec![
                (Arc::from("on"), Value::Bool(false)),
                (Arc::from("brightness"), Value::Int(100)),
                (Arc::from("color_temp"), Value::Null),
                (Arc::from("color"), Value::from("#ff252f")),
            ]
        );
    }

    #[test]
    fn colour_temperature_bounds_in_mireds() {
        // KAJPLATS gives its bounds in mireds, its temperature in kelvins.
        let item = light(
            &json!({"customName": "Plafonnier", "isOn": true, "lightLevel": 40,
                   "colorTemperature": 2700, "colorTemperatureMin": 153,
                   "colorTemperatureMax": 454, "colorMode": "temperature"}),
            &["isOn", "lightLevel", "colorTemperature"],
        );
        let mut load = one(item.clone());
        let lamp = &load.devices[0];
        assert_eq!(keys(lamp), ["on", "brightness", "color_temp"]);
        assert_eq!(
            lamp.point("color_temp").unwrap().kind,
            numeric(Some(153.0), Some(454.0))
        );
        let update = load.index.decode(&item).unwrap();
        assert!(
            update
                .values
                .contains(&(Arc::from("color_temp"), Value::Int(370)))
        );
        // 300 mireds = 3 333 K; past the lamp's range, clamped to it.
        let (_, body) = load
            .index
            .encode(&lamp.id, "color_temp", &Value::Float(300.0))
            .unwrap();
        assert_eq!(body, json!([{"attributes": {"colorTemperature": 3333}}]));
        let (_, body) = load
            .index
            .encode(&lamp.id, "color_temp", &Value::Float(100.0))
            .unwrap();
        assert_eq!(body, json!([{"attributes": {"colorTemperature": 6536}}]));
    }

    #[test]
    fn commands_encode_only_what_the_device_receives() {
        let item = light(
            &json!({"customName": "Applique", "isOn": true, "lightLevel": 1}),
            &["customName", "isOn"],
        );
        let load = one(item);
        let lamp = &load.devices[0];
        assert!(lamp.point("on").unwrap().access.write);
        assert!(!lamp.point("brightness").unwrap().access.write);
        let (native, body) = load
            .index
            .encode(&lamp.id, "on", &Value::Bool(false))
            .unwrap();
        assert_eq!(native, "l1");
        assert_eq!(body, json!([{"attributes": {"isOn": false}}]));
        assert!(
            load.index
                .encode(&lamp.id, "brightness", &Value::Float(50.0))
                .is_none()
        );
        assert!(
            load.index
                .encode(&DeviceId::from("ikea:other"), "on", &Value::Bool(true))
                .is_none()
        );
    }

    #[test]
    fn colours_and_levels_encode() {
        let item = light(
            &json!({"isOn": true, "lightLevel": 50, "colorHue": 0, "colorSaturation": 0}),
            &["isOn", "lightLevel", "colorHue", "colorSaturation"],
        );
        let load = one(item);
        let id = &load.devices[0].id;
        let body = |key: &str, value: Value| load.index.encode(id, key, &value).unwrap().1;
        assert_eq!(
            body("color", Value::from("#ff0000")),
            json!([{"attributes": {"colorHue": 0.0, "colorSaturation": 1.0}}])
        );
        assert_eq!(
            body("color", Value::from("#8080ff")),
            json!([{"attributes": {"colorHue": 240.0, "colorSaturation": 0.498}}])
        );
        assert_eq!(
            body("brightness", Value::Float(49.6)),
            json!([{"attributes": {"lightLevel": 50}}])
        );
        assert!(
            load.index
                .encode(id, "color", &Value::from("rouge"))
                .is_none()
        );
        assert!(
            load.index
                .encode(id, "color", &Value::from("#000000"))
                .is_none()
        );
    }

    #[test]
    fn hue_and_saturation_round_trip() {
        for hex in [
            "#ff0000", "#00ff00", "#0000ff", "#ffffff", "#ff8000", "#8080ff",
        ] {
            let (h, s) = hex_to_hs(hex).unwrap();
            assert_eq!(hs_to_hex(h, s).as_deref(), Some(hex), "{hex}");
        }
        assert_eq!(hs_to_hex(360.0, 1.0).as_deref(), Some("#ff0000"));
    }

    #[test]
    fn events_carry_part_of_the_colour() {
        let item = light(
            &json!({"isOn": true, "colorHue": 0, "colorSaturation": 1}),
            &["isOn", "colorHue", "colorSaturation"],
        );
        let mut load = one(item.clone());
        load.index.decode(&item).unwrap();
        let update = load
            .index
            .decode(&json!({"id": "l1", "attributes": {"colorHue": 120}}))
            .unwrap();
        assert_eq!(
            update.values,
            vec![(Arc::from("color"), Value::from("#00ff00"))]
        );
        assert_eq!(update.online, None, "partial: reachability unknown");
    }

    #[test]
    fn an_outlet_with_its_meter() {
        let item = json!({
            "id": "o1", "type": "outlet", "deviceType": "outlet", "isReachable": false,
            "attributes": {"customName": "Prise", "model": "INSPELNING Smart plug",
                "isOn": true, "lightLevel": 41, "currentActivePower": 12.5,
                "currentAmps": 0.1, "currentVoltage": 230, "totalEnergyConsumed": 0.5},
            "capabilities": {"canSend": [], "canReceive": ["customName", "isOn", "lightLevel"]},
        });
        let mut load = one(item.clone());
        let plug = &load.devices[0];
        assert_eq!(keys(plug), ["on", "power", "current", "voltage", "energy"]);
        assert_eq!(plug.native_room, None);
        assert_eq!(plug.point("energy").unwrap().unit, Some(Unit::KiloWattHour));
        assert!(!plug.point("power").unwrap().access.write);
        let update = load.index.decode(&item).unwrap();
        assert_eq!(update.online, Some(false));
        assert!(
            update
                .values
                .contains(&(Arc::from("power"), Value::Float(12.5)))
        );
    }

    #[test]
    fn sensors_read_what_they_carry() {
        let sensor = |device_type: &str, attributes: Json| {
            json!({"id": "s1", "type": "sensor", "deviceType": device_type,
                   "isReachable": true, "attributes": attributes,
                   "capabilities": {"canSend": [], "canReceive": ["customName"]}})
        };
        let env = one(sensor(
            "environmentSensor",
            json!({"currentTemperature": 23, "currentRH": 74, "currentPM25": 2, "vocIndex": 59}),
        ));
        assert_eq!(
            keys(&env.devices[0]),
            ["temperature", "humidity", "pm25", "voc_index"]
        );
        // A TIMMERFLOTTE half: humidity only, and its battery.
        let half = one(sensor(
            "environmentSensor",
            json!({"currentRH": 44, "batteryPercentage": 83}),
        ));
        assert_eq!(keys(&half.devices[0]), ["humidity", "battery"]);
        // The old TRADFRI motion sensor tells nobody: nothing but its battery.
        let old = one(sensor(
            "motionSensor",
            json!({"batteryPercentage": 52, "isOn": false}),
        ));
        assert_eq!(keys(&old.devices[0]), ["battery"]);

        let item = sensor(
            "openCloseSensor",
            json!({"isOpen": true, "batteryPercentage": 92}),
        );
        let mut door = one(item.clone());
        assert_eq!(
            door.devices[0].point("contact").unwrap().semantic,
            Semantic::Contact
        );
        let update = door.index.decode(&item).unwrap();
        assert_eq!(
            update.values[0],
            (Arc::from("contact"), Value::Bool(false)),
            "open = no contact"
        );

        let item = sensor("waterSensor", json!({"waterLeakDetected": true}));
        let mut leak = one(item.clone());
        assert_eq!(
            leak.devices[0].point("leak").unwrap().semantic,
            Semantic::WaterLeak
        );
        assert_eq!(
            leak.index.decode(&item).unwrap().values,
            vec![(Arc::from("leak"), Value::Bool(true))]
        );
        // Unnamed: the model names it.
        let lux = one(sensor(
            "lightSensor",
            json!({"customName": "", "model": "VALLHORN Wireless Motion Sensor", "illuminance": 1}),
        ));
        assert_eq!(
            &*lux.devices[0].native_name,
            "VALLHORN Wireless Motion Sensor"
        );
    }

    #[test]
    fn remotes_press_buttons() {
        let item = json!({"id": "b1", "type": "controller", "deviceType": "shortcutController",
            "isReachable": true, "attributes": {"customName": "Bouton", "batteryPercentage": 1},
            "capabilities": {"canSend": ["singlePress", "doublePress", "longPress"],
                             "canReceive": ["customName"]}});
        let load = one(item);
        let remote = &load.devices[0];
        assert_eq!(keys(remote), ["button", "battery"]);
        assert_eq!(
            remote.point("button").unwrap().kind,
            Kind::Enum {
                values: ["single", "double", "long"].map(Arc::from).to_vec()
            }
        );
        let press = json!({"id": "b1", "clickPattern": "doublePress"});
        assert_eq!(
            load.index.press(&press),
            Some((remote.id.clone(), Value::from("double")))
        );
        assert_eq!(
            load.index
                .press(&json!({"id": "x", "clickPattern": "singlePress"})),
            None
        );

        // A light remote drives its lamps itself: only its battery.
        let dimmer = one(
            json!({"id": "d1", "type": "controller", "deviceType": "lightController",
            "attributes": {"batteryPercentage": 20, "isOn": false, "lightLevel": 1},
            "capabilities": {"canSend": ["isOn", "lightLevel"], "canReceive": ["customName"]}}),
        );
        assert_eq!(keys(&dimmer.devices[0]), ["battery"]);
        assert_eq!(
            dimmer
                .index
                .press(&json!({"id": "d1", "clickPattern": "singlePress"})),
            None
        );
    }

    #[test]
    fn unknown_and_unsupported_types_are_skipped() {
        let load = load(
            &InstanceId::from("ikea"),
            &[
                json!({"id": "h", "deviceType": "gateway", "attributes": {"isOn": false}}),
                json!({"id": "b", "deviceType": "blinds", "attributes": {"blindsCurrentLevel": 0}}),
                json!({"id": "x", "deviceType": "teleporter", "attributes": {"isOn": true}}),
                json!({"deviceType": "light", "attributes": {"isOn": true}}),
            ],
        );
        assert!(load.devices.is_empty());
        assert_eq!(
            load.skipped,
            vec![
                Skipped {
                    device_type: "gateway".into(),
                    known: true
                },
                Skipped {
                    device_type: "blinds".into(),
                    known: true
                },
                Skipped {
                    device_type: "teleporter".into(),
                    known: false
                },
            ]
        );
    }

    #[test]
    fn ids_with_a_slash_stay_usable() {
        let mut item = light(&json!({"isOn": true}), &["isOn"]);
        item["id"] = json!("a/b_1");
        let load = one(item);
        let lamp = &load.devices[0];
        assert_eq!(lamp.id.as_str(), "ikea:a_b_1");
        let (native, _) = load
            .index
            .encode(&lamp.id, "on", &Value::Bool(true))
            .unwrap();
        assert_eq!(native, "a/b_1", "the hub gets its own id back");
    }

    #[test]
    fn events_are_sorted_out() {
        let item = light(&json!({"customName": "Lampe", "isOn": true}), &["isOn"]);
        let load = one(item);
        let state = json!({"type": "deviceStateChanged",
            "data": {"id": "l1", "attributes": {"isOn": false}}});
        assert_eq!(load.index.classify(&state), Event::State(&state["data"]));
        let same_room = json!({"type": "deviceStateChanged",
            "data": {"id": "l1", "room": {"id": "r1", "name": "Salon"}, "attributes": {"customName": "Lampe"}}});
        assert_eq!(
            load.index.classify(&same_room),
            Event::State(&same_room["data"])
        );
        let renamed = json!({"type": "deviceConfigurationChanged",
            "data": {"id": "l1", "attributes": {"customName": "Lampe de chevet"}}});
        assert_eq!(load.index.classify(&renamed), Event::Reload);
        let moved = json!({"type": "deviceStateChanged",
            "data": {"id": "l1", "room": {"id": "r2", "name": "Chambre"}}});
        assert_eq!(load.index.classify(&moved), Event::Reload);
        for kind in ["deviceAdded", "deviceRemoved", "roomUpdated", "roomDeleted"] {
            assert_eq!(
                load.index.classify(&json!({"type": kind})),
                Event::Reload,
                "{kind}"
            );
        }
        let press = json!({"type": "remotePressEvent", "data": {"id": "b1", "clickPattern": "singlePress"}});
        assert_eq!(load.index.classify(&press), Event::Press(&press["data"]));
        for kind in ["pong", "sceneTriggered", "musicUpdated", "roomCreated"] {
            assert_eq!(
                load.index.classify(&json!({"type": kind})),
                Event::Ignore,
                "{kind}"
            );
        }
    }
}

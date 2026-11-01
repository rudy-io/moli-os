//! The house as the model reads it: the family dashboard's layout (room
//! names and aliases, hidden devices) and an inventory, one line a device.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use moli_core::{DeviceId, Kind, PointId, Value};
use moli_runtime::DeviceView;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

/// What the assistant needs from `home.json` (the rest is the dashboard's).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Layout {
    pub title: Option<String>,
    pub rooms: Vec<RoomDef>,
    pub hidden: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RoomDef {
    pub name: String,
    pub aliases: Vec<String>,
}

/// What the inventory calls a device with no room, in the house's language.
pub(crate) fn no_room() -> String {
    moli_i18n::tr!("assistant.house.no_room")
}

impl Layout {
    pub(crate) async fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        match tokio::fs::read(path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "home.json unreadable: rooms as the devices name them");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// The dashboard's name for a room (aliases resolved).
    pub(crate) fn room(&self, raw: &str) -> String {
        let wanted = norm(raw);
        self.rooms
            .iter()
            .find(|r| {
                std::iter::once(&r.name)
                    .chain(&r.aliases)
                    .any(|n| norm(n) == wanted)
            })
            .map_or_else(|| raw.trim().to_owned(), |r| r.name.clone())
    }

    pub(crate) fn room_of(&self, view: &DeviceView) -> String {
        view.label
            .room
            .as_deref()
            .or(view.device.native_room.as_deref())
            .map_or_else(no_room, |r| self.room(r))
    }

    pub(crate) fn hides(&self, id: &str) -> bool {
        self.hidden.iter().any(|h| h == id)
    }

    pub(crate) fn same_room(&self, a: &str, b: &str) -> bool {
        norm(&self.room(a)) == norm(&self.room(b))
    }
}

/// Case, accents and spacing don't count.
pub(crate) fn norm(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A light fixture: bulbs and their relay as one light (`lumieres:…`).
fn is_fixture(view: &DeviceView) -> bool {
    view.device.model.as_deref() == Some("Luminaire")
}

pub(crate) fn name_of(view: &DeviceView) -> &str {
    view.label
        .name
        .as_deref()
        .unwrap_or(&view.device.native_name)
}

/// Every visible device, by room, the dashboard's room order first:
/// `- id · name · key✎=value unit [choices], …`. Writable points first
/// (marked ✎), then a few readings.
/// For whom the inventory is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reader {
    /// The conversation: what is visible, as it is now.
    Chat,
    /// Drafting automations: every device and every point, reachable or not
    /// (an automation runs later, when it is back).
    Builder,
}

/// What the model is shown of the house: its devices by room, rooms in the
/// layout's order, and each fixture's parts (a light fixture (D12) stands for
/// its bulbs and relay: in the conversation, only the fixture shows).
type Rooms<'a> = BTreeMap<(usize, String), Vec<&'a DeviceView>>;

fn by_room<'a>(
    devices: &'a [DeviceView],
    layout: &Layout,
    reader: Reader,
) -> (Rooms<'a>, BTreeMap<&'a str, &'a str>) {
    let fixture_of: BTreeMap<&str, &str> = devices
        .iter()
        .filter(|d| is_fixture(d))
        .flat_map(|d| {
            d.device
                .members
                .iter()
                .map(move |m| (m.as_str(), name_of(d)))
        })
        .collect();
    let no_room = no_room();
    let mut rooms: BTreeMap<(usize, String), Vec<&DeviceView>> = BTreeMap::new();
    for view in devices.iter().filter(|d| {
        reader == Reader::Builder
            || !(layout.hides(d.device.id.as_str())
                || fixture_of.contains_key(d.device.id.as_str()))
    }) {
        let room = layout.room_of(view);
        let rank = layout
            .rooms
            .iter()
            .position(|r| r.name == room)
            .unwrap_or(if room == no_room {
                usize::MAX
            } else {
                usize::MAX - 1
            });
        rooms.entry((rank, room)).or_default().push(view);
    }
    (rooms, fixture_of)
}

pub(crate) fn inventory(
    devices: &[DeviceView],
    layout: &Layout,
    protected: &[String],
    reader: Reader,
) -> String {
    let (rooms, fixture_of) = by_room(devices, layout, reader);
    let mut out = String::new();
    for ((_, room), mut views) in rooms {
        views.sort_by(|a, b| name_of(a).cmp(name_of(b)));
        if protected.iter().any(|p| layout.same_room(p, &room)) {
            let _ = writeln!(
                out,
                "## {}",
                moli_i18n::tr!("assistant.house.room_protected", room = room)
            );
        } else {
            let _ = writeln!(out, "## {room}");
        }
        for view in views {
            let _ = write!(out, "- {} · {}", view.device.id, name_of(view));
            if matches!(view.device.model.as_deref(), Some("room" | "zone")) {
                let _ = write!(out, " {}", moli_i18n::tr!("assistant.house.group"));
            } else if is_fixture(view) {
                let members: Vec<&str> = view.device.members.iter().map(DeviceId::as_str).collect();
                let _ = write!(
                    out,
                    " {}",
                    moli_i18n::tr!("assistant.house.fixture", members = members.join(", "))
                );
            } else if let Some(fixture) = fixture_of.get(view.device.id.as_str()) {
                let _ = write!(
                    out,
                    " {}",
                    moli_i18n::tr!("assistant.house.member", fixture = fixture)
                );
            }
            // Its last values are a guess: never say a lamp is on when
            // nobody knows.
            if view.online == Some(false) {
                if reader == Reader::Chat {
                    let _ = writeln!(out, " · {}", moli_i18n::tr!("assistant.house.offline"));
                    continue;
                }
                let _ = write!(
                    out,
                    " {}",
                    moli_i18n::tr!("assistant.house.offline_builder")
                );
            }
            if view.camera {
                let _ = write!(out, " {}", moli_i18n::tr!("assistant.house.camera"));
            }
            let points = points_of(view, reader);
            if !points.is_empty() {
                let _ = write!(out, " · {}", points.join(", "));
            }
            out.push('\n');
        }
    }
    out
}

/// A name as one line of the prompt: a line break or another control character
/// would make lines of its own (a fake room, a fake device), so it becomes a space.
pub(crate) fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The inventory of contract v2: the same house, **nothing that moves**: no
/// value, no reachability, so that the prompt only changes when the devices,
/// their names or the rooms do (and the provider's prompt cache keeps working).
/// Devices are numbered `d1…dN` in reading order; the model writes `d3/state`
/// and the second result tells which device each number stands for.
pub(crate) fn inventory_static(
    devices: &[DeviceView],
    layout: &Layout,
    protected: &[String],
) -> (String, Vec<(String, DeviceId)>) {
    let (rooms, fixture_of) = by_room(devices, layout, Reader::Chat);
    let mut out = String::new();
    let mut aliases: Vec<(String, DeviceId)> = Vec::new();
    for ((_, room), mut views) in rooms {
        views.sort_by(|a, b| name_of(a).cmp(name_of(b)));
        let room = one_line(&room);
        if protected.iter().any(|p| layout.same_room(p, &room)) {
            let _ = writeln!(
                out,
                "## {}",
                moli_i18n::tr!("assistant.house.room_protected", room = room)
            );
        } else {
            let _ = writeln!(out, "## {room}");
        }
        for view in views {
            let alias = format!("d{}", aliases.len() + 1);
            let _ = write!(out, "- {alias} · {}", one_line(name_of(view)));
            if matches!(view.device.model.as_deref(), Some("room" | "zone")) {
                let _ = write!(out, " {}", moli_i18n::tr!("assistant.house.group"));
            } else if is_fixture(view) {
                let _ = write!(out, " {}", moli_i18n::tr!("assistant.house.fixture_short"));
            } else if let Some(fixture) = fixture_of.get(view.device.id.as_str()) {
                let _ = write!(
                    out,
                    " {}",
                    moli_i18n::tr!("assistant.house.member", fixture = fixture)
                );
            }
            if view.camera {
                let _ = write!(out, " {}", moli_i18n::tr!("assistant.house.camera"));
            }
            let points = static_points_of(view);
            if !points.is_empty() {
                let _ = write!(out, " · {}", points.join(", "));
            }
            out.push('\n');
            aliases.push((alias, view.device.id.clone()));
        }
    }
    (out, aliases)
}

/// A device's points without their values: `state✎`, `brightness✎ (0–100 %)`,
/// `mode✎ [auto|froid]`, `temperature °C`.
fn static_points_of(view: &DeviceView) -> Vec<String> {
    let writable = view.device.points.iter().filter(|p| p.access.write);
    let readings = view.device.points.iter().filter(|p| !p.access.write);
    writable
        .chain(readings)
        .filter(|p| !private(&p.key) && !setting(&p.key, p.semantic) && !noise(&p.key))
        .take(MAX_POINTS)
        .map(|p| {
            let unit = p.unit.as_ref().map(|u| u.symbol().to_owned());
            let mut text = format!("{}{}", p.key, if p.access.write { "✎" } else { "" });
            match &p.kind {
                Kind::Enum { values } if p.access.write => {
                    let shown: Vec<&str> = values.iter().take(MAX_CHOICES).map(|v| &**v).collect();
                    let _ = write!(text, " [{}]", shown.join("|"));
                }
                Kind::Numeric {
                    min: Some(min),
                    max: Some(max),
                    ..
                } if p.access.write => {
                    let _ = write!(text, " ({}–{}", trim(*min), trim(*max));
                    if let Some(unit) = &unit {
                        let _ = write!(text, " {unit}");
                    }
                    text.push(')');
                }
                // What to write there (a search to play, a sentence to say).
                Kind::Text if p.access.write && *p.label != *p.key => {
                    let _ = write!(text, " «{}»", one_line(&p.label));
                }
                _ => {
                    if let Some(unit) = &unit {
                        let _ = write!(text, " {unit}");
                    }
                }
            }
            text
        })
        .collect()
}

/// A point as the model wrote it. It sometimes names the integration rather
/// than the device (`tele/app` for `tele:192.168.1.x/app`): the one device
/// of that instance with that value, when there is exactly one.
pub(crate) fn resolve_point(written: &str, devices: &[DeviceView]) -> PointId {
    let point = PointId::from(written.trim());
    let Some((device, key)) = point.split() else {
        return point;
    };
    if devices.iter().any(|v| v.device.id == device) {
        return point;
    }
    let mut matching = devices.iter().filter(|v| {
        v.device.instance.as_str() == device.as_str()
            && v.device.points.iter().any(|p| &*p.key == key)
    });
    match (matching.next(), matching.next()) {
        (Some(only), None) => PointId::from(format!("{}/{key}", only.device.id.as_str())),
        _ => point,
    }
}

const MAX_POINTS: usize = 8;
const MAX_CHOICES: usize = 30;

/// Values the model never reads, whatever the device: where someone is to
/// the metre (a phone's position, how precise it is) and the Wi-Fi a phone
/// is on. They stay on the household's own server (presence, zones): the
/// model reads `home`, `zone`, the battery.
const PRIVATE_POINTS: [&str; 4] = ["latitude", "longitude", "accuracy", "wifi"];

/// Controls some profiles mark as configuration: the model keeps them.
const CONTROLS: [&str; 4] = ["mode", "target_temperature", "system_mode", "fan_mode"];
/// What names a device's setting (how it behaves, not what it does now): a delay, a
/// calibration, what it does at power-up, how often it reports. The model neither
/// sees nor writes them: they are the dashboard's, and a small model mistakes them
/// for orders (« minuteur de 10 minutes » → `off_on_duration`).
const SETTING_MARKS: [&str; 26] = [
    "calibration",
    "duration",
    "dimming_range",
    "power_on",
    "identify",
    "reporting",
    "report_",
    "sampling",
    "interval",
    "threshold",
    "restart",
    "selftest",
    "transition",
    "startup",
    "sensitivity",
    "detection_range",
    "led_",
    "schedule_",
    "external_trigger",
    "motor_travel",
    "heartbeat",
    "linkage",
    "spatial",
    "track_target",
    "absence_delay",
    "effect_segments",
];

/// Whether a point is a setting of the device rather than something to command or read.
pub(crate) fn setting(key: &str, semantic: moli_core::Semantic) -> bool {
    let key = key.to_ascii_lowercase();
    if CONTROLS.contains(&key.as_str()) {
        return false;
    }
    semantic == moli_core::Semantic::Config
        || key == "test"
        || SETTING_MARKS.iter().any(|m| key.contains(m))
}

/// Readings that say how the device is doing, not the house: radio, updates, power
/// cuts counted, the chip's own temperature, a colour's coordinates.
const NOISE_MARKS: [&str; 7] = [
    "linkquality",
    "rssi",
    "update",
    "power_outage",
    "device_temperature",
    "calibration",
    "color.",
];

/// Whether a value is noise to the conversation (never shown to the model, never read aloud).
pub(crate) fn noise(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    NOISE_MARKS.iter().any(|m| key.contains(m))
}

/// A point the model never reads (`PRIVATE_POINTS`): not in the inventory,
/// not in `get_device`, not in `history`, not in a « Moli écrit » prompt.
pub(crate) fn private(key: &str) -> bool {
    PRIVATE_POINTS.iter().any(|p| key.eq_ignore_ascii_case(p))
}

fn points_of(view: &DeviceView, reader: Reader) -> Vec<String> {
    let writable = view.device.points.iter().filter(|p| p.access.write);
    let readings = view
        .device
        .points
        .iter()
        .filter(|p| !p.access.write)
        .filter(|p| {
            reader == Reader::Builder
                || view
                    .state
                    .get(&p.key)
                    .is_some_and(|s| !matches!(s.value, Value::Null))
        });
    let max = if reader == Reader::Builder {
        MAX_POINTS * 2
    } else {
        MAX_POINTS
    };
    writable
        .chain(readings)
        .filter(|p| !private(&p.key))
        .take(max)
        .map(|p| {
            let value = view
                .state
                .get(&p.key)
                .map_or_else(|| "?".to_owned(), |s| show(&s.value));
            let mut text = format!("{}{}={value}", p.key, if p.access.write { "✎" } else { "" });
            if let Some(unit) = &p.unit {
                let _ = write!(text, " {}", unit.symbol());
            }
            if p.access.write {
                match &p.kind {
                    Kind::Enum { values } => {
                        let shown: Vec<&str> =
                            values.iter().take(MAX_CHOICES).map(|v| &**v).collect();
                        let _ = write!(text, " [{}]", shown.join("|"));
                    }
                    Kind::Numeric {
                        min: Some(min),
                        max: Some(max),
                        ..
                    } => {
                        let _ = write!(text, " ({}–{})", trim(*min), trim(*max));
                    }
                    // What to write there (a search to play, a sentence to say).
                    Kind::Text if *p.label != *p.key => {
                        let _ = write!(text, " «{}»", p.label);
                    }
                    _ => {}
                }
            }
            text
        })
        .collect()
}

fn show(value: &Value) -> String {
    match value {
        Value::Null => "?".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => trim(*f),
        Value::Text(t) => {
            let t: String = t.chars().take(40).collect();
            t.replace(['\n', ','], " ")
        }
    }
}

fn trim(f: f64) -> String {
    let rounded = (f * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        serde_json::from_str(
            r#"{"rooms":[{"name":"Chambre des enfants","aliases":["Petite chambre"]},{"name":"Salon"}],"hidden":["x:1"],"other":1}"#,
        )
        .unwrap()
    }

    #[test]
    fn aliases_resolve_without_accents_or_case() {
        let l = layout();
        assert_eq!(l.room("petite  CHAMBRE"), "Chambre des enfants");
        assert_eq!(l.room("Grenier"), "Grenier");
        assert!(l.same_room("Petite chambre", "chambre des enfants"));
        assert!(l.hides("x:1"));
    }

    fn view(id: &str, model: &str, members: &[&str]) -> DeviceView {
        DeviceView {
            device: std::sync::Arc::new(moli_core::Device {
                id: id.into(),
                instance: id.split(':').next().unwrap().into(),
                native_name: id.into(),
                manufacturer: None,
                model: Some(model.into()),
                description: None,
                native_room: Some("Salon".into()),
                members: members.iter().map(|m| (*m).into()).collect(),
                points: Vec::new(),
            }),
            label: moli_core::Label::default(),
            online: None,
            state: BTreeMap::new(),
            camera: false,
            printer: false,
        }
    }

    fn with_point(mut view: DeviceView, key: &str) -> DeviceView {
        let device = std::sync::Arc::make_mut(&mut view.device);
        device.points.push(moli_core::PointSpec {
            key: key.into(),
            label: key.into(),
            kind: Kind::Text,
            access: moli_core::Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: moli_core::Semantic::Other,
        });
        view
    }

    #[test]
    fn a_point_named_by_its_instance_finds_its_one_device() {
        let devices = [
            with_point(view("tele:192.168.1.x", "TV", &[]), "app"),
            with_point(view("hue:1", "LCA006", &[]), "on"),
            with_point(view("hue:2", "LCA006", &[]), "on"),
        ];
        assert_eq!(
            resolve_point("tele/app", &devices).as_str(),
            "tele:192.168.1.x/app"
        );
        // Already right, or ambiguous, or unknown: as written.
        assert_eq!(
            resolve_point("tele:192.168.1.x/app", &devices).as_str(),
            "tele:192.168.1.x/app"
        );
        assert_eq!(resolve_point("hue/on", &devices).as_str(), "hue/on");
        assert_eq!(
            resolve_point("tele/volume", &devices).as_str(),
            "tele/volume"
        );
    }

    #[test]
    fn a_fixture_stands_for_its_bulbs() {
        let devices = [
            view(
                "lumieres:suspensions",
                "Luminaire",
                &["hue:1", "hue:2", "tuya:r"],
            ),
            view("hue:1", "LCA006", &[]),
            view("hue:2", "LCA006", &[]),
            view("tuya:r", "eMylo", &[]),
            view("hue:salon", "room", &["hue:1", "hue:2"]),
            view("tele:1", "TV", &[]),
        ];
        let chat = inventory(&devices, &layout(), &[], Reader::Chat);
        assert!(chat.contains("lumieres:suspensions · lumieres:suspensions (luminaire"));
        assert!(!chat.contains("- hue:1 "), "{chat}");
        assert!(!chat.contains("- tuya:r "), "{chat}");
        assert!(chat.contains("hue:salon · hue:salon (groupe : toutes les lumières de la pièce)"));
        assert!(chat.contains("- tele:1 "));
        let builder = inventory(&devices, &layout(), &[], Reader::Builder);
        assert!(
            builder.contains("- hue:1 · hue:1 (fait partie du luminaire « lumieres:suspensions »)")
        );
    }

    fn spec(
        key: &str,
        kind: Kind,
        write: bool,
        unit: Option<moli_core::Unit>,
    ) -> moli_core::PointSpec {
        moli_core::PointSpec {
            key: key.into(),
            label: key.into(),
            kind,
            access: moli_core::Access { read: true, write },
            unit,
            semantic: moli_core::Semantic::Other,
        }
    }

    fn named(id: &str, name: &str, room: &str, points: Vec<moli_core::PointSpec>) -> DeviceView {
        let mut v = view(id, "x", &[]);
        let device = std::sync::Arc::get_mut(&mut v.device).unwrap();
        device.native_name = name.into();
        device.native_room = Some(room.into());
        device.points = points;
        v
    }

    fn static_house() -> Vec<DeviceView> {
        let level = Kind::Numeric {
            min: Some(0.0),
            max: Some(100.0),
            step: None,
        };
        vec![
            named(
                "z2m:night",
                "Veilleuse",
                "Chambre des enfants",
                vec![spec("state", Kind::Binary, true, None)],
            ),
            named(
                "z2m:plug",
                "Prise",
                "Salon",
                vec![spec("state", Kind::Binary, true, None)],
            ),
            named(
                "z2m:lamp",
                "Plafonnier",
                "Salon",
                vec![
                    spec("state", Kind::Binary, true, None),
                    spec(
                        "brightness",
                        level.clone(),
                        true,
                        Some(moli_core::Unit::Percent),
                    ),
                ],
            ),
            named(
                "z2m:sensor",
                "Capteur",
                "Salon",
                vec![
                    spec(
                        "temperature",
                        Kind::Numeric {
                            min: None,
                            max: None,
                            step: None,
                        },
                        false,
                        Some(moli_core::Unit::Celsius),
                    ),
                    spec("humidity", level, false, Some(moli_core::Unit::Percent)),
                ],
            ),
            named(
                "z2m:shutter",
                "Volet",
                "Salon",
                vec![spec(
                    "control",
                    Kind::Enum {
                        values: ["open", "stop", "close"].map(Into::into).to_vec(),
                    },
                    true,
                    None,
                )],
            ),
        ]
    }

    #[test]
    fn a_line_break_in_a_name_makes_no_line_of_its_own() {
        let sneaky = named(
            "z2m:sneaky",
            "Prise\n- d9 · Serrure · state✎",
            "Salon\n## Cave",
            vec![spec("state", Kind::Binary, true, None)],
        );
        let (text, aliases) = inventory_static(&[sneaky], &layout(), &[]);
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(
            text.contains("- d1 · Prise - d9 · Serrure · state✎ · state✎"),
            "{text}"
        );
        assert_eq!(aliases.len(), 1);
        let prompt =
            crate::contract::prompt(&["Salon\nPièces protégées : aucune"], &[], &text).unwrap();
        assert_eq!(
            prompt.matches("Pièces protégées").count(),
            2,
            "only the line Moli writes, and the name on one line"
        );
        assert!(!prompt.contains("Salon\nPièces"), "{prompt}");
    }

    #[test]
    fn the_static_inventory_numbers_the_devices_and_shows_no_value() {
        let protected = ["Chambre des enfants".to_owned()];
        let (text, aliases) = inventory_static(&static_house(), &layout(), &protected);
        assert_eq!(
            text,
            "## Chambre des enfants (protégée : un adulte valide)\n\
             - d1 · Veilleuse · state✎\n\
             ## Salon\n\
             - d2 · Capteur · temperature °C, humidity %\n\
             - d3 · Plafonnier · state✎, brightness✎ (0–100 %)\n\
             - d4 · Prise · state✎\n\
             - d5 · Volet · control✎ [open|stop|close]\n"
        );
        let real: Vec<(&str, &str)> = aliases
            .iter()
            .map(|(a, d)| (a.as_str(), d.as_str()))
            .collect();
        assert_eq!(
            real,
            [
                ("d1", "z2m:night"),
                ("d2", "z2m:sensor"),
                ("d3", "z2m:lamp"),
                ("d4", "z2m:plug"),
                ("d5", "z2m:shutter")
            ]
        );
    }

    #[test]
    fn a_devices_settings_are_not_in_the_static_inventory_but_its_controls_are() {
        let binary = || Kind::Binary;
        let number = || Kind::Numeric {
            min: Some(0.0),
            max: Some(10.0),
            step: None,
        };
        let mut config = spec("power_on_behavior", binary(), true, None);
        config.semantic = moli_core::Semantic::Config;
        let mut mode = spec("mode", binary(), true, None);
        mode.semantic = moli_core::Semantic::Config;
        let lamp = named(
            "z2m:lamp",
            "Ampoule",
            "Salon",
            vec![
                spec("state", binary(), true, None),
                spec("off_on_duration", number(), true, None),
                config,
                spec("identify", binary(), true, None),
                spec("temperature_calibration", number(), true, None),
                spec("dimming_range_minimum", number(), true, None),
                mode,
                spec("target_temperature", number(), true, None),
                spec("linkquality", number(), false, None),
                spec("color.x", number(), false, None),
                spec("power_outage_count", number(), false, None),
                spec("update_available", binary(), false, None),
                spec("device_temperature", number(), false, None),
                spec("humidity", number(), false, None),
            ],
        );
        let (text, _) = inventory_static(&[lamp], &layout(), &[]);
        assert_eq!(
            text,
            "## Salon\n- d1 · Ampoule · state✎, mode✎, target_temperature✎ (0–10), humidity\n"
        );
    }

    #[test]
    fn settings_are_told_apart_from_controls() {
        for key in [
            "off_on_duration",
            "power_on_behavior",
            "identify",
            "temperature_calibration",
            "dimming_range_maximum",
            "temp_reporting_interval",
            "restart_device",
            "selftest",
            "led_disabled_night",
        ] {
            assert!(setting(key, moli_core::Semantic::Other), "{key}");
        }
        for key in [
            "state",
            "state_l1",
            "brightness",
            "mode",
            "target_temperature",
            "system_mode",
            "occupied_heating_setpoint",
            "percent_control",
            "control",
            "volume",
            "announce",
            "power",
        ] {
            assert!(!setting(key, moli_core::Semantic::Other), "{key}");
        }
        assert!(setting("whatever", moli_core::Semantic::Config));
        assert!(
            !setting("mode", moli_core::Semantic::Config),
            "a control some profiles mark as config"
        );
    }

    #[test]
    fn the_static_inventory_does_not_move_with_the_values_or_the_reachability() {
        let protected = ["Chambre des enfants".to_owned()];
        let calm = inventory_static(&static_house(), &layout(), &protected);
        let mut busy = static_house();
        for v in &mut busy {
            v.online = Some(false);
            v.state.insert(
                "state".into(),
                moli_core::Sample {
                    value: Value::Bool(true),
                    ts: 9,
                },
            );
            v.state.insert(
                "temperature".into(),
                moli_core::Sample {
                    value: Value::Float(23.5),
                    ts: 9,
                },
            );
        }
        assert_eq!(inventory_static(&busy, &layout(), &protected), calm);
    }

    /// The inventory's words live in the catalogue: in French they read
    /// exactly as they did when they were written in the code.
    #[test]
    fn the_inventory_reads_in_french_as_it_always_did() {
        let mut offline = view("tuya:off", "eMylo", &[]);
        offline.online = Some(false);
        let mut camera = view("reolink:cour", "Argus", &[]);
        camera.camera = true;
        let mut nursery = view("hue:enfants", "Lamp", &[]);
        std::sync::Arc::get_mut(&mut nursery.device)
            .unwrap()
            .native_room = Some("petite chambre".into());
        let mut loose = view("tele:2", "TV", &[]);
        std::sync::Arc::get_mut(&mut loose.device)
            .unwrap()
            .native_room = None;
        let devices = [
            view("hue:salon", "room", &[]),
            offline,
            camera,
            nursery,
            loose,
        ];
        let protected = ["Chambre des enfants".to_owned()];
        assert_eq!(
            inventory(&devices, &layout(), &protected, Reader::Chat),
            "## Chambre des enfants (protégée : un adulte valide)\n\
             - hue:enfants · hue:enfants\n\
             ## Salon\n\
             - hue:salon · hue:salon (groupe : toutes les lumières de la pièce)\n\
             - reolink:cour · reolink:cour (caméra)\n\
             - tuya:off · tuya:off · injoignable, état inconnu\n\
             ## Sans pièce\n\
             - tele:2 · tele:2\n"
        );
        assert!(
            inventory(&devices, &layout(), &protected, Reader::Builder)
                .contains("- tuya:off · tuya:off (injoignable pour l'instant)\n")
        );
        assert_eq!(no_room(), "Sans pièce");
    }

    /// A value the device reads, and what it is now.
    fn with_reading(mut view: DeviceView, key: &str, kind: Kind, value: Value) -> DeviceView {
        let device = std::sync::Arc::make_mut(&mut view.device);
        device.points.push(moli_core::PointSpec {
            key: key.into(),
            label: key.into(),
            kind,
            access: moli_core::Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: moli_core::Semantic::Other,
        });
        view.state
            .insert(key.into(), moli_core::Sample { value, ts: 1 });
        view
    }

    /// A phone's position and its Wi-Fi stay at home, whoever reads the
    /// inventory; whether its owner is home, their zone, the battery do not.
    #[test]
    fn where_someone_is_to_the_metre_is_never_in_the_inventory() {
        let number = || Kind::Numeric {
            min: None,
            max: None,
            step: None,
        };
        let mut phone = view("phones:a", "Phone", &[]);
        for (key, kind, value) in [
            ("home", Kind::Binary, Value::Bool(false)),
            ("zone", Kind::Text, Value::Text("Travail".into())),
            ("latitude", number(), Value::Float(43.219_87)),
            ("longitude", number(), Value::Float(7.654_32)),
            ("accuracy", number(), Value::Float(12.0)),
            ("wifi", Kind::Text, Value::Text("Box-1234".into())),
            ("battery", number(), Value::Float(81.0)),
        ] {
            phone = with_reading(phone, key, kind, value);
        }
        for reader in [Reader::Chat, Reader::Builder] {
            let text = inventory(std::slice::from_ref(&phone), &layout(), &[], reader);
            for hidden in [
                "latitude",
                "longitude",
                "accuracy",
                "wifi",
                "43.2",
                "7.7",
                "Box-1234",
            ] {
                assert!(!text.contains(hidden), "{reader:?}: {hidden} in {text}");
            }
            for shown in ["home=false", "zone=Travail", "battery=81"] {
                assert!(text.contains(shown), "{reader:?}: {shown} not in {text}");
            }
        }
        assert!(private("wifi") && private("Latitude") && !private("zone"));
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(trim(21.0), "21");
        assert_eq!(trim(21.04), "21");
        assert_eq!(trim(21.25), "21.3");
        assert_eq!(norm(" Entrée "), "entree");
    }
}

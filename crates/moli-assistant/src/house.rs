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

pub(crate) fn inventory(
    devices: &[DeviceView],
    layout: &Layout,
    protected: &[String],
    reader: Reader,
) -> String {
    // A light fixture (D12) stands for its bulbs and relay: in the
    // conversation, only the fixture shows.
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

/// The house as one object: its rooms (the dashboard's order and icons),
/// each with its devices; a device sits under its parent (a camera under
/// its HomeBase) when both are in the same room. Devices with no room, or
/// in one `home.json` doesn't know, come after.
pub(crate) fn tree(
    devices: &[DeviceView],
    layout: &Layout,
    raw: &serde_json::Value,
) -> serde_json::Value {
    use serde_json::json;

    let shown: Vec<&DeviceView> = devices
        .iter()
        .filter(|d| !layout.hides(d.device.id.as_str()))
        .collect();
    let room_of: BTreeMap<&str, String> = shown
        .iter()
        .map(|d| (d.device.id.as_str(), layout.room_of(d)))
        .collect();
    let no_room = no_room();
    // Where a device sits: under its parent when it is shown, in the same room.
    let parent_of = |d: &DeviceView| -> Option<String> {
        let parent = d.device.parent.as_ref()?.as_str();
        (room_of.get(parent) == room_of.get(d.device.id.as_str())).then(|| parent.to_owned())
    };
    let mut children: BTreeMap<String, Vec<&DeviceView>> = BTreeMap::new();
    for d in &shown {
        if let Some(parent) = parent_of(d) {
            children.entry(parent).or_default().push(d);
        }
    }
    let mut by_room: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for d in shown.iter().filter(|d| parent_of(d).is_none()) {
        by_room
            .entry(room_of[d.device.id.as_str()].clone())
            .or_default()
            .push(tree_node(d, &children, 0));
    }
    let mut rooms = Vec::new();
    for def in raw["rooms"].as_array().into_iter().flatten() {
        let Some(name) = def["name"].as_str() else {
            continue;
        };
        let devices = by_room.remove(name).unwrap_or_default();
        rooms.push(json!({ "name": name, "icon": def["icon"], "devices": devices }));
    }
    let unplaced = by_room.remove(&no_room).unwrap_or_default();
    // Rooms the devices name that the dashboard doesn't know.
    for (name, devices) in by_room {
        rooms.push(json!({ "name": name, "icon": null, "devices": devices }));
    }
    json!({ "title": layout.title, "rooms": rooms, "unplaced": unplaced })
}

fn tree_node(
    d: &DeviceView,
    children: &BTreeMap<String, Vec<&DeviceView>>,
    depth: usize,
) -> serde_json::Value {
    use serde_json::json;

    let kids: Vec<_> = if depth < 4 {
        children
            .get(d.device.id.as_str())
            .map(|list| {
                list.iter()
                    .map(|k| tree_node(k, children, depth + 1))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    json!({
        "id": d.device.id,
        "name": name_of(d),
        "model": d.device.model,
        "parent": d.device.parent,
        "online": d.online,
        "children": kids,
    })
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
                parent: None,
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

    #[test]
    fn the_house_puts_a_camera_under_its_station_in_the_same_room() {
        let mut station = view("eufy:hb", "T8010", &[]);
        std::sync::Arc::make_mut(&mut station.device).native_room = Some("Entrée".into());
        let mut camera = view("eufy:cam", "T8410", &[]);
        std::sync::Arc::make_mut(&mut camera.device).parent = Some("eufy:hb".into());
        std::sync::Arc::make_mut(&mut camera.device).native_room = Some("Entrée".into());
        let mut elsewhere = view("eufy:bell", "T8210", &[]);
        std::sync::Arc::make_mut(&mut elsewhere.device).parent = Some("eufy:hb".into());
        let raw = serde_json::json!({"rooms":[{"name":"Salon","icon":"sofa"},{"name":"Entrée"}]});
        let layout: Layout = serde_json::from_value(raw.clone()).unwrap();
        let house = tree(&[station, camera, elsewhere], &layout, &raw);
        assert_eq!(house["rooms"][0]["name"], "Salon");
        assert_eq!(house["rooms"][0]["devices"][0]["id"], "eufy:bell");
        let entree = &house["rooms"][1]["devices"][0];
        assert_eq!(entree["id"], "eufy:hb");
        assert_eq!(entree["children"][0]["id"], "eufy:cam");
    }
}

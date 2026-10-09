//! The house as the model reads it: the family dashboard's layout (room
//! names and aliases, hidden devices) and an inventory, one line a device.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use moli_core::{DeviceId, Kind, Value};
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

const MAX_POINTS: usize = 8;
const MAX_CHOICES: usize = 30;

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

    #[test]
    fn numbers_read_short() {
        assert_eq!(trim(21.0), "21");
        assert_eq!(trim(21.04), "21");
        assert_eq!(trim(21.25), "21.3");
        assert_eq!(norm(" Entrée "), "entree");
    }
}

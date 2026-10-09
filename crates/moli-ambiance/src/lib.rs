//! Ambiances: a room's lights set to a look in one gesture (« Feu de
//! cheminée », « Fin de journée », « Fête »…), or to a single colour.
//!
//! `plan` is pure: for each lamp, which orders and in which order (on, its
//! own animation stopped or chosen, colour or white, brightness). `apply`
//! sends them through the hub, under the guard like any order: a protected
//! room holds them for a human.

use std::sync::Arc;

use futures::StreamExt;
use moli_core::{DeviceId, Kind, Origin, PointId, PointSpec, Semantic, Value};
use moli_runtime::{CommandError, DeviceView, Hub};
use serde::Serialize;

/// A look for a room.
#[derive(Debug)]
pub struct Ambiance {
    pub id: &'static str,
    /// The name in French, the house's fallback: what a person reads is
    /// [`Ambiance::label`] (`pilotes.ambiance.<id>` in the catalogue).
    pub name: &'static str,
    /// Icon name in the dashboard's set.
    pub icon: &'static str,
    /// Handed out to the colour lamps, one each in turn. Empty: white only.
    pub colors: &'static [&'static str],
    /// Kelvins for the lamps without colour (and for all of them when
    /// `colors` is empty).
    pub white: u32,
    /// Percent.
    pub brightness: f64,
    /// The lamp's own animation, where it has it (Hue « fire », « candle »…).
    /// Left out of the JSON when there is none.
    pub effect: Option<&'static str>,
}

pub const AMBIANCES: &[Ambiance] = &[
    Ambiance {
        id: "feu",
        name: "Feu de cheminée",
        icon: "fire",
        colors: &["#ff4500", "#ff7a00", "#ff5a1f", "#ff9a2e"],
        white: 2000,
        brightness: 55.0,
        effect: Some("fire"),
    },
    Ambiance {
        id: "bougies",
        name: "Bougies",
        icon: "candle",
        colors: &["#ff8a2a", "#ffa64d"],
        white: 2000,
        brightness: 30.0,
        effect: Some("candle"),
    },
    Ambiance {
        id: "fin-de-journee",
        name: "Fin de journée",
        icon: "sunset",
        colors: &["#ff7b39", "#ff4f6d", "#ffb347", "#d0508f"],
        white: 2300,
        brightness: 70.0,
        effect: None,
    },
    Ambiance {
        id: "cocooning",
        name: "Cocooning",
        icon: "sofa",
        colors: &["#ffb46b", "#ff9e57", "#ffc98a"],
        white: 2400,
        brightness: 45.0,
        effect: None,
    },
    Ambiance {
        id: "fete",
        name: "Fête",
        icon: "party",
        colors: &[
            "#ff2d9a", "#1f4dff", "#16d94a", "#ffd000", "#8a2be2", "#00d0ff",
        ],
        white: 4000,
        brightness: 100.0,
        effect: Some("prism"),
    },
    Ambiance {
        id: "ocean",
        name: "Océan",
        icon: "waves",
        colors: &["#0057ff", "#00b8d9", "#1a3cff", "#00d6b0"],
        white: 6500,
        brightness: 70.0,
        effect: Some("underwater"),
    },
    Ambiance {
        id: "foret",
        name: "Forêt",
        icon: "pine",
        colors: &["#2e8b57", "#7cc142", "#3fa34d", "#ffd27a"],
        white: 3500,
        brightness: 65.0,
        effect: None,
    },
    Ambiance {
        id: "aurore",
        name: "Aurore boréale",
        icon: "stars",
        colors: &["#00e0a0", "#3a7bff", "#9b4dff", "#00c8ff"],
        white: 5000,
        brightness: 60.0,
        effect: Some("cosmos"),
    },
    Ambiance {
        id: "romantique",
        name: "Romantique",
        icon: "heart",
        colors: &["#ff2d6f", "#c2185b", "#ff6f91"],
        white: 2200,
        brightness: 35.0,
        effect: None,
    },
    Ambiance {
        id: "cinema",
        name: "Cinéma",
        icon: "movie",
        colors: &["#2a2f8f", "#5b2a86"],
        white: 2200,
        brightness: 12.0,
        effect: None,
    },
    Ambiance {
        id: "lecture",
        name: "Lecture",
        icon: "book",
        colors: &[],
        white: 4000,
        brightness: 100.0,
        effect: None,
    },
    Ambiance {
        id: "veilleuse",
        name: "Veilleuse",
        icon: "moon",
        colors: &["#ff8a2a"],
        white: 2000,
        brightness: 4.0,
        effect: None,
    },
];

impl Ambiance {
    /// The name in the house's language (the French `name` when the
    /// catalogue has none).
    #[must_use]
    pub fn label(&self) -> String {
        let key = format!("pilotes.ambiance.{}", self.id.replace('-', "_"));
        let word = moli_i18n::tr(&key);
        if word == key {
            self.name.to_owned()
        } else {
            word
        }
    }
}

/// What the dashboard gets: the same fields, the name in the house's language.
impl Serialize for Ambiance {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut state = serializer.serialize_struct("Ambiance", 7)?;
        state.serialize_field("id", self.id)?;
        state.serialize_field("name", &self.label())?;
        state.serialize_field("icon", self.icon)?;
        state.serialize_field("colors", self.colors)?;
        state.serialize_field("white", &self.white)?;
        state.serialize_field("brightness", &self.brightness)?;
        match self.effect {
            Some(effect) => state.serialize_field("effect", effect)?,
            None => state.skip_field("effect")?,
        }
        state.end()
    }
}

/// The ambiance with this id.
#[must_use]
pub fn find(id: &str) -> Option<&'static Ambiance> {
    AMBIANCES.iter().find(|a| a.id == id)
}

/// What to put the lamps in.
#[derive(Clone, Copy, Debug)]
pub enum Look<'a> {
    Ambiance(&'a Ambiance),
    /// `#rrggbb` on every colour lamp; the others stay as they are.
    Color(&'a str),
    /// A white (kelvins) on every lamp that has whites.
    White(u32),
}

/// What a lamp can take, read from its points.
#[derive(Clone, Debug, PartialEq)]
pub struct Lamp {
    pub id: DeviceId,
    /// The power point and the value that means « on ».
    power: (Arc<str>, Value),
    /// The brightness point and its range.
    brightness: Option<(Arc<str>, f64, f64)>,
    color: bool,
    /// White temperature: range, and whether it counts in mireds.
    white: Option<(f64, f64, bool)>,
    effects: Vec<Arc<str>>,
    /// The animation playing now, if known.
    effect_now: Option<Arc<str>>,
}

const NO_EFFECT: &str = "no_effect";

impl Lamp {
    /// A lamp: something that switches on and dims, tints or warms. A plain
    /// switch (a socket, a TV) is not one.
    #[must_use]
    pub fn of(view: &DeviceView) -> Option<Self> {
        let d = &view.device;
        let writable = |key: &str| d.point(key).filter(|p| p.access.write);
        let power = d
            .points
            .iter()
            .filter(|p| p.access.write && p.semantic == Semantic::OnOff)
            .find_map(|p| Some((p.key.clone(), on_value(&p.kind)?)))?;
        let brightness = writable("brightness").map(|p| {
            let (min, max) = range(p);
            (p.key.clone(), min.unwrap_or(0.0), max.unwrap_or(100.0))
        });
        let color = writable("color").is_some_and(|p| p.kind == Kind::Text);
        let white = writable("color_temp").map(|p| {
            let (min, max) = range(p);
            let mireds = p.unit.as_ref().is_some_and(|u| u.symbol() == "mired")
                || max.is_some_and(|m| m <= 1000.0);
            (min.unwrap_or(0.0), max.unwrap_or(f64::MAX), mireds)
        });
        let effects = match writable("effect").map(|p| &p.kind) {
            Some(Kind::Enum { values }) => values.clone(),
            _ => Vec::new(),
        };
        if brightness.is_none() && !color && white.is_none() && effects.is_empty() {
            return None;
        }
        let effect_now = match view.state.get("effect").map(|s| &s.value) {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        };
        Some(Self {
            id: d.id.clone(),
            power,
            brightness,
            color,
            white,
            effects,
            effect_now,
        })
    }

    fn can_play(&self, effect: &str) -> bool {
        self.effects.iter().any(|e| &**e == effect)
    }

    fn playing(&self) -> bool {
        self.effect_now.as_deref().is_some_and(|e| e != NO_EFFECT)
    }
}

fn on_value(kind: &Kind) -> Option<Value> {
    match kind {
        Kind::Binary => Some(Value::Bool(true)),
        Kind::Enum { values } => values
            .iter()
            .find(|v| v.eq_ignore_ascii_case("on"))
            .map(|v| Value::Text(v.clone())),
        _ => None,
    }
}

fn range(p: &PointSpec) -> (Option<f64>, Option<f64>) {
    match p.kind {
        Kind::Numeric { min, max, .. } => (min, max),
        _ => (None, None),
    }
}

/// For each lamp (same order), its orders in sequence. Empty: the lamp
/// cannot take this look (no colour for a colour).
#[must_use]
pub fn plan(look: Look<'_>, lamps: &[Lamp]) -> Vec<Vec<(PointId, Value)>> {
    let mut tint = 0;
    lamps
        .iter()
        .map(|lamp| {
            let point = |key: &str| PointId::new(&lamp.id, key);
            let mut orders = Vec::new();
            let (key, on) = &lamp.power;
            match look {
                Look::Color(hex) => {
                    if !lamp.color {
                        return orders;
                    }
                    orders.push((point(key), on.clone()));
                    if lamp.playing() {
                        orders.push((point("effect"), Value::from(NO_EFFECT)));
                    }
                    orders.push((point("color"), Value::from(hex)));
                }
                Look::White(kelvin) => {
                    let Some(white) = lamp.white else {
                        return orders;
                    };
                    orders.push((point(key), on.clone()));
                    if lamp.playing() {
                        orders.push((point("effect"), Value::from(NO_EFFECT)));
                    }
                    orders.push((point("color_temp"), temperature(white, kelvin)));
                }
                Look::Ambiance(a) => {
                    orders.push((point(key), on.clone()));
                    let effect = a.effect.filter(|e| lamp.can_play(e));
                    if effect.is_none() && lamp.playing() {
                        orders.push((point("effect"), Value::from(NO_EFFECT)));
                    }
                    if lamp.color && !a.colors.is_empty() {
                        let hex = a.colors[tint % a.colors.len()];
                        tint += 1;
                        orders.push((point("color"), Value::from(hex)));
                    } else if let Some(white) = lamp.white {
                        orders.push((point("color_temp"), temperature(white, a.white)));
                    }
                    if let Some((key, min, max)) = &lamp.brightness {
                        // A 0–254 scale (Zigbee) as well as a percent one.
                        let v = if *max > 100.0 {
                            (a.brightness / 100.0 * max).round()
                        } else {
                            a.brightness
                        };
                        orders.push((point(key), Value::Float(v.clamp(*min, *max))));
                    }
                    if let Some(effect) = effect {
                        orders.push((point("effect"), Value::from(effect)));
                    }
                }
            }
            orders
        })
        .collect()
}

/// Kelvins in the lamp's own unit, within its range.
fn temperature((min, max, mireds): (f64, f64, bool), kelvin: u32) -> Value {
    let k = f64::from(kelvin);
    let v = if mireds { (1e6 / k).round() } else { k };
    Value::Float(v.clamp(min, max))
}

/// What happened to each lamp.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// Lamps that took the look.
    pub done: Vec<DeviceId>,
    /// Held by the guard (a protected room): a human approves each.
    pub held: Vec<Held>,
    pub failed: Vec<Failed>,
    /// Lamps that cannot take it (no colour, unreachable, not a lamp).
    pub skipped: Vec<DeviceId>,
}

#[derive(Debug, Serialize)]
pub struct Held {
    pub device: DeviceId,
    /// The order held (the one a human approves).
    pub point: PointId,
    pub approval: u64,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct Failed {
    pub device: DeviceId,
    pub error: String,
}

/// A lamp's fate: not for this look (`None`), done, or the order refused.
type Outcome = Option<Result<(), (PointId, CommandError)>>;

/// Lamps changed at once: a bridge takes about ten orders a second.
const PARALLEL: usize = 3;

/// Puts these lights (a group stands for its members) in the look. Each
/// lamp's orders go one after the other; it stops at its first refusal.
pub async fn apply(
    hub: &Hub,
    look: Look<'_>,
    targets: &[DeviceId],
    origin: Origin,
    actor: Option<String>,
) -> Report {
    let mut ids: Vec<DeviceId> = Vec::new();
    for id in targets {
        match hub.device(id) {
            Some(v) if !v.device.members.is_empty() => ids.extend(v.device.members.iter().cloned()),
            _ => ids.push(id.clone()),
        }
    }
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    let mut report = Report::default();
    let mut lamps = Vec::new();
    for id in ids {
        match hub.device(&id) {
            Some(v) if v.online != Some(false) => match Lamp::of(&v) {
                Some(lamp) => lamps.push(lamp),
                None => report.skipped.push(id),
            },
            _ => report.skipped.push(id),
        }
    }
    let plans = plan(look, &lamps);
    let actor = actor.as_ref();
    let outcomes: Vec<(DeviceId, Outcome)> = futures::stream::iter(
        lamps
            .into_iter()
            .zip(plans)
            .map(|(lamp, orders)| async move {
                if orders.is_empty() {
                    return (lamp.id, None);
                }
                for (point, value) in orders {
                    if let Err(e) = hub.command(&point, value, origin, actor.cloned()).await {
                        return (lamp.id, Some(Err((point, e))));
                    }
                }
                (lamp.id, Some(Ok(())))
            }),
    )
    .buffer_unordered(PARALLEL)
    .collect()
    .await;
    for (device, outcome) in outcomes {
        match outcome {
            None => report.skipped.push(device),
            Some(Ok(())) => report.done.push(device),
            Some(Err((point, CommandError::NeedsApproval { id, reason }))) => {
                report.held.push(Held {
                    device,
                    point,
                    approval: id,
                    reason,
                });
            }
            Some(Err((_, e))) => report.failed.push(Failed {
                device,
                error: e.to_string(),
            }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue(name: &str, color: bool, effects: &[&str], playing: Option<&str>) -> Lamp {
        Lamp {
            id: DeviceId::from(format!("hue:{name}")),
            power: ("on".into(), Value::Bool(true)),
            brightness: Some(("brightness".into(), 0.0, 100.0)),
            color,
            white: Some((153.0, 500.0, true)),
            effects: effects.iter().map(|e| Arc::from(*e)).collect(),
            effect_now: playing.map(Arc::from),
        }
    }

    fn keys(orders: &[(PointId, Value)]) -> Vec<String> {
        orders
            .iter()
            .map(|(p, v)| format!("{}={}", p.split().unwrap().1, v.to_json()))
            .collect()
    }

    #[test]
    fn every_ambiance_is_sound() {
        let mut ids = std::collections::HashSet::new();
        for a in AMBIANCES {
            assert!(ids.insert(a.id), "{} twice", a.id);
            assert!((1.0..=100.0).contains(&a.brightness), "{}", a.id);
            assert!((1500..=6500).contains(&a.white), "{}", a.id);
            for c in a.colors {
                assert!(moli_core::color::hex_to_xy(c).is_some(), "{}: {c}", a.id);
            }
        }
        assert!(find("feu").is_some() && find("nope").is_none());
    }

    #[test]
    fn fire_hands_out_colours_and_plays_where_it_can() {
        let lamps = [
            hue("globe", true, &["no_effect", "fire"], None),
            hue("spot", true, &[], None),
            hue("ambiance", false, &[], None),
        ];
        let plans = plan(Look::Ambiance(find("feu").unwrap()), &lamps);
        assert_eq!(
            keys(&plans[0]),
            [
                "on=true",
                "color=\"#ff4500\"",
                "brightness=55.0",
                "effect=\"fire\""
            ]
        );
        assert_eq!(
            keys(&plans[1]),
            ["on=true", "color=\"#ff7a00\"", "brightness=55.0"]
        );
        // White only: 2000 K is 500 mireds.
        assert_eq!(
            keys(&plans[2]),
            ["on=true", "color_temp=500.0", "brightness=55.0"]
        );
    }

    #[test]
    fn a_static_look_stops_the_animation() {
        let lamps = [hue("globe", true, &["no_effect", "fire"], Some("fire"))];
        let plans = plan(Look::Ambiance(find("lecture").unwrap()), &lamps);
        assert_eq!(
            keys(&plans[0]),
            [
                "on=true",
                "effect=\"no_effect\"",
                "color_temp=250.0",
                "brightness=100.0"
            ]
        );
        let plans = plan(Look::Color("#ff0000"), &lamps);
        assert_eq!(
            keys(&plans[0]),
            ["on=true", "effect=\"no_effect\"", "color=\"#ff0000\""]
        );
    }

    #[test]
    fn a_colour_skips_white_lamps_and_scales_follow_the_point() {
        let mut zigbee = hue("z", false, &[], None);
        zigbee.power = ("state".into(), Value::from("ON"));
        zigbee.brightness = Some(("brightness".into(), 0.0, 254.0));
        zigbee.white = Some((2200.0, 6500.0, false));
        assert!(plan(Look::Color("#00ff00"), &[zigbee.clone()])[0].is_empty());
        assert_eq!(
            keys(&plan(Look::White(6500), &[zigbee.clone()])[0]),
            ["state=\"ON\"", "color_temp=6500.0"]
        );
        let plans = plan(Look::Ambiance(find("cocooning").unwrap()), &[zigbee]);
        assert_eq!(
            keys(&plans[0]),
            ["state=\"ON\"", "color_temp=2400.0", "brightness=114.0"]
        );
    }
}

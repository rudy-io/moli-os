//! The gateway's automations, the « Planning » of the VELUX ACTIVE app: one
//! schedule of type `event` in `homesdata`. The app shows it as cards (a
//! name, days, a time of day or minutes around sunrise or sunset, an end
//! for a window that airs, and where each window or blind goes), kept in
//! `metadata.cards`; the gateway runs what the cards compile to: zones (the
//! modules and their positions at one moment), a week of `timetable`
//! entries (minutes since Monday 0 h, local time) and, for the sun, entries
//! per day (1 = Monday) with a `twilight_offset` in minutes.
//!
//! Moli shows the cards in a compact form (the point [`POINT`] on the
//! gateway, JSON text) and takes them back in the same form: each card
//! keeps what Moli does not know about it (its icon, its type), a card
//! spread over several VELUX rooms is split into one per room (the app
//! lists cards by room), and the zones and timetables are compiled again,
//! as the app compiles them.

use std::collections::BTreeMap;

use moli_core::{DeviceId, InstanceId};
use serde_json::{Map, Value as Json, json};

use crate::model::{Home, Module, Role};

/// The gateway's point: the cards, JSON text.
pub const POINT: &str = "automations";
/// Cards at most (the app shows a few per room).
const MAX_CARDS: usize = 40;
const MAX_NAME: usize = 60;
/// Minutes around sunrise or sunset the app offers.
const SUN_SPAN: i64 = 90;
const DAY: i64 = 24 * 60;

/// The gateway's point: writable, and held like a window's opening (an
/// automation opens windows: an agent asks a person first).
#[must_use]
pub fn spec() -> moli_core::PointSpec {
    moli_core::PointSpec {
        key: POINT.into(),
        label: moli_i18n::tr("pilotes.velux.planning.label").into(),
        kind: moli_core::Kind::Text,
        access: moli_core::Access {
            read: true,
            write: true,
        },
        unit: None,
        semantic: moli_core::Semantic::Control,
    }
}

/// When a card acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ref {
    /// A time of day, minutes since midnight (the cloud's `hourly`).
    Time,
    Sunrise,
    Sunset,
}

impl Ref {
    fn parse(cloud: &str) -> Option<Self> {
        match cloud {
            "hourly" | "time" => Some(Self::Time),
            "sunrise" => Some(Self::Sunrise),
            "sunset" => Some(Self::Sunset),
            _ => None,
        }
    }

    /// As the cloud says it.
    fn cloud(self) -> &'static str {
        match self {
            Self::Time => "hourly",
            Self::Sunrise => "sunrise",
            Self::Sunset => "sunset",
        }
    }

    /// As Moli says it.
    fn word(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Sunrise => "sunrise",
            Self::Sunset => "sunset",
        }
    }

    /// The minutes a card may say.
    fn span(self) -> std::ops::RangeInclusive<i64> {
        match self {
            Self::Time => 0..=DAY - 1,
            Self::Sunrise | Self::Sunset => -SUN_SPAN..=SUN_SPAN,
        }
    }
}

/// The event schedule, as read.
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    raw: Json,
}

impl Schedule {
    /// The home's event schedule, if it has one.
    #[must_use]
    pub fn find(home: &Json) -> Option<Self> {
        home["schedules"]
            .as_array()?
            .iter()
            .find(|s| s["type"] == "event" && s["id"].is_string())
            .map(|s| Self { raw: s.clone() })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        self.raw["id"].as_str().unwrap_or_default()
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.raw["name"].as_str().unwrap_or_default()
    }

    fn raw_cards(&self) -> &[Json] {
        self.raw["metadata"]["cards"]
            .as_array()
            .map_or(&[], Vec::as_slice)
    }

    /// The cards as Moli shows them: `id`, `name`, `days` (0 = Monday),
    /// `ref` (`time`, `sunrise`, `sunset`), `min` (minutes since midnight,
    /// or around the sun), `end` (a window airing until then, then closed),
    /// `set` (device → opening, 0 closed to 100 open), and `off` when the
    /// card is paused. Modules Moli does not know are left out.
    #[must_use]
    pub fn cards(&self, home: &Home, instance: &InstanceId) -> Json {
        let cards: Vec<Json> = self
            .raw_cards()
            .iter()
            .filter_map(|c| {
                let at = Ref::parse(c["begin_time_ref"].as_str()?)?;
                let mut set = Map::new();
                for m in c["modules"].as_array()? {
                    let id = m["id"].as_str()?;
                    if home.modules.iter().any(|known| known.id == id) {
                        set.insert(
                            DeviceId::new(instance, id).to_string(),
                            json!(m["target_position"].as_i64()?),
                        );
                    }
                }
                let mut card = json!({
                    "id": c["card_id"].as_i64()?,
                    "name": c["card_name"].as_str().unwrap_or_default(),
                    "days": c["days"],
                    "ref": at.word(),
                    "min": c["begin_time"].as_i64().unwrap_or(0),
                    "set": set,
                });
                if let Some(end) = c["end_time"].as_i64() {
                    card["end"] = json!(end);
                }
                if c["card_enabled"] == false {
                    card["off"] = json!(true);
                }
                Some(card)
            })
            .collect();
        Json::Array(cards)
    }

    /// The whole schedule with `wanted` (cards in [`Schedule::cards`]'s
    /// form; one without `id` is new), compiled again: what the cloud is
    /// sent. Refused, why (for the person).
    pub fn edited(
        &self,
        home: &Home,
        instance: &InstanceId,
        wanted: &Json,
    ) -> Result<Json, String> {
        let wanted = wanted
            .as_array()
            .filter(|w| w.len() <= MAX_CARDS)
            .ok_or_else(|| {
                moli_i18n::tr!("pilotes.velux.planning.trop_de_cartes", max = MAX_CARDS)
            })?;
        let existing = self.raw_cards();
        let mut next_id = existing
            .iter()
            .filter_map(|c| c["card_id"].as_i64())
            .max()
            .unwrap_or(-1)
            + 1;
        let mut cards = Vec::new();
        for card in wanted {
            let card = Card::parse(card, home, instance)?;
            let base = card
                .id
                .map(|id| {
                    existing
                        .iter()
                        .find(|c| c["card_id"] == id)
                        .cloned()
                        .ok_or_else(|| moli_i18n::tr("pilotes.velux.planning.carte_inconnue"))
                })
                .transpose()?;
            // One card per VELUX room, as the app lists them.
            let mut rooms: Vec<(Option<&str>, Moves)> = Vec::new();
            for &(module, position) in &card.set {
                let room = module.room_id.as_deref();
                match rooms.iter_mut().find(|(r, _)| *r == room) {
                    Some((_, list)) => list.push((module, position)),
                    None => rooms.push((room, vec![(module, position)])),
                }
            }
            for (i, (room, set)) in rooms.into_iter().enumerate() {
                let mut raw = base.clone().unwrap_or_else(|| json!({}));
                let id = match card.id {
                    Some(id) if i == 0 => id,
                    _ => {
                        next_id += 1;
                        next_id - 1
                    }
                };
                card.write(&mut raw, id, room, &set);
                cards.push(raw);
            }
        }
        let mut raw = self.raw.clone();
        let (timetable, zones, sunrise, sunset) = compile(&cards);
        raw["timetable"] = timetable;
        raw["zones"] = zones;
        raw["timetable_sunrise"] = sunrise;
        raw["timetable_sunset"] = sunset;
        raw["metadata"]["cards"] = Json::Array(cards);
        Ok(raw)
    }

    /// What is sent: the schedule without what only the cloud says.
    #[must_use]
    pub fn body(edited: &Json) -> Json {
        let mut body = edited.clone();
        if let Some(map) = body.as_object_mut() {
            for key in ["id", "selected", "default"] {
                map.remove(key);
            }
        }
        body
    }
}

/// Modules and where a card sends them.
type Moves<'a> = Vec<(&'a Module, i64)>;

/// A card as a person asked for it, checked.
struct Card<'a> {
    id: Option<i64>,
    name: String,
    days: Vec<i64>,
    at: Ref,
    min: i64,
    end: Option<i64>,
    set: Moves<'a>,
    on: bool,
}

impl<'a> Card<'a> {
    fn parse(card: &Json, home: &'a Home, instance: &InstanceId) -> Result<Self, String> {
        let bad = |key: &str| moli_i18n::tr!("pilotes.velux.planning.carte_invalide", champ = key);
        let name = card["name"].as_str().map(str::trim).unwrap_or_default();
        if name.is_empty() || name.chars().count() > MAX_NAME {
            return Err(bad("name"));
        }
        let mut days: Vec<i64> = card["days"]
            .as_array()
            .ok_or_else(|| bad("days"))?
            .iter()
            .map(|d| {
                d.as_i64()
                    .filter(|d| (0..7).contains(d))
                    .ok_or_else(|| bad("days"))
            })
            .collect::<Result<_, _>>()?;
        days.sort_unstable();
        days.dedup();
        if days.is_empty() {
            return Err(bad("days"));
        }
        let at = card["ref"]
            .as_str()
            .and_then(Ref::parse)
            .ok_or_else(|| bad("ref"))?;
        let min = card["min"]
            .as_i64()
            .filter(|m| at.span().contains(m))
            .ok_or_else(|| bad("min"))?;
        let end = match &card["end"] {
            Json::Null => None,
            e => Some(
                e.as_i64()
                    .filter(|e| at.span().contains(e) && *e > min)
                    .ok_or_else(|| bad("end"))?,
            ),
        };
        let mut set = Vec::new();
        for (device, position) in card["set"].as_object().ok_or_else(|| bad("set"))? {
            let module = home
                .modules
                .iter()
                .find(|m| DeviceId::new(instance, &m.id).to_string() == *device)
                .filter(|m| matches!(m.role, Role::Window | Role::Cover) && m.bridge.is_some())
                .ok_or_else(|| bad("set"))?;
            let position = position
                .as_i64()
                .filter(|p| (0..=100).contains(p))
                .ok_or_else(|| bad("set"))?;
            set.push((module, position));
        }
        if set.is_empty() {
            return Err(bad("set"));
        }
        Ok(Self {
            id: card["id"].as_i64(),
            name: name.to_owned(),
            days,
            at,
            min,
            end,
            set,
            on: card["off"] != true,
        })
    }

    /// Writes it over `raw` (the card as the cloud had it, or `{}`).
    fn write(&self, raw: &mut Json, id: i64, room: Option<&str>, set: &[(&Module, i64)]) {
        let had_end = raw.get("end_time").is_some();
        raw["card_id"] = json!(id);
        raw["card_name"] = json!(self.name);
        raw["card_enabled"] = json!(self.on);
        raw["days"] = json!(self.days);
        raw["begin_time"] = json!(self.min);
        raw["begin_time_ref"] = json!(self.at.cloud());
        raw["end_time_ref"] = json!(self.at.cloud());
        match (self.end, had_end) {
            (Some(end), _) => raw["end_time"] = json!(end),
            (None, true) => raw["end_time"] = Json::Null,
            (None, false) => {}
        }
        if let Some(room) = room {
            raw["room_id"] = json!(room);
        }
        raw["modules"] = set
            .iter()
            .map(|(m, p)| {
                json!({
                    "bridge": m.bridge,
                    "id": m.id,
                    "isBlindShutter": m.role == Role::Cover,
                    "isWindow": m.role == Role::Window,
                    "target_position": p,
                })
            })
            .collect();
        // A card the app suggested says its one position at the top too.
        if raw.get("target_position").is_some() {
            let first = set.first().map(|(_, p)| *p);
            match first.filter(|p| set.iter().all(|(_, q)| q == p)) {
                Some(p) => raw["target_position"] = json!(p),
                None => {
                    raw.as_object_mut().map(|o| o.remove("target_position"));
                }
            }
        }
    }
}

/// One moment of the week: which timetable, which day (0 = Monday), when.
type Moment = (Ref, i64, i64);

/// The cards compiled as the app does: `(timetable, zones,
/// timetable_sunrise, timetable_sunset)`. Each moment gathers the modules
/// every card sends somewhere then (in the cards' order; an airing window
/// is closed at its card's end); moments asking the same are one zone,
/// numbered as the week goes, the time of day first, then the sun.
#[must_use]
pub fn compile(cards: &[Json]) -> (Json, Json, Json, Json) {
    let mut moments: BTreeMap<Moment, Vec<Json>> = BTreeMap::new();
    for card in cards.iter().filter(|c| c["card_enabled"] != false) {
        let Some(at) = card["begin_time_ref"].as_str().and_then(Ref::parse) else {
            continue;
        };
        let begin = card["begin_time"].as_i64().unwrap_or(0);
        let end = card["end_time"].as_i64();
        let modules = card["modules"].as_array().map_or(&[][..], Vec::as_slice);
        for day in card["days"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_i64)
        {
            let mut add = |at_min: i64, module: &Json, position: &Json| {
                let list = moments.entry((at, day, at_min)).or_default();
                // Two cards at once on one module: the first one says.
                if !list.iter().any(|m| m["id"] == module["id"]) {
                    list.push(json!({
                        "id": module["id"],
                        "bridge": module["bridge"],
                        "target_position": position,
                    }));
                }
            };
            for m in modules {
                add(begin, m, &m["target_position"]);
            }
            if let Some(end) = end {
                for m in modules {
                    add(end, m, &json!(0));
                }
            }
        }
    }
    let mut zones: Vec<Vec<Json>> = Vec::new();
    let (mut timetable, mut sunrise, mut sunset) = (Vec::new(), Vec::new(), Vec::new());
    for ((at, day, minute), modules) in moments {
        let zone = zones.iter().position(|z| *z == modules).unwrap_or_else(|| {
            zones.push(modules);
            zones.len() - 1
        });
        match at {
            Ref::Time => timetable.push(json!({ "zone_id": zone, "m_offset": day * DAY + minute })),
            Ref::Sunrise | Ref::Sunset => {
                let mut entry = json!({ "zone_id": zone, "day": day + 1 });
                if minute != 0 {
                    entry["twilight_offset"] = json!(minute);
                }
                if at == Ref::Sunrise {
                    &mut sunrise
                } else {
                    &mut sunset
                }
                .push(entry);
            }
        }
    }
    let zones = zones
        .into_iter()
        .enumerate()
        .map(|(id, modules)| json!({ "id": id, "modules": modules }))
        .collect();
    (
        Json::Array(timetable),
        Json::Array(zones),
        Json::Array(sunrise),
        Json::Array(sunset),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real home, with its 13 automations (ids made up).
    fn fixture() -> (Json, Home, Schedule) {
        let text = include_str!("../../../integrations/velux/fixtures/kix300.json");
        let data: Json = serde_json::from_str(text).unwrap();
        let raw = data["homesdata"].clone();
        let home = Home::parse(&raw).unwrap();
        let schedule = Schedule::find(&raw).unwrap();
        (raw, home, schedule)
    }

    fn instance() -> InstanceId {
        InstanceId::from("velux")
    }

    #[test]
    fn the_event_schedule_is_found_not_the_algo_one() {
        let (_, _, schedule) = fixture();
        assert_eq!(schedule.name(), "VELUX event");
        assert_eq!(schedule.raw_cards().len(), 13);
    }

    #[test]
    fn compiling_the_cards_gives_back_what_the_app_compiled() {
        let (_, _, schedule) = fixture();
        let (timetable, zones, sunrise, sunset) = compile(schedule.raw_cards());
        assert_eq!(timetable, schedule.raw["timetable"]);
        assert_eq!(zones, schedule.raw["zones"]);
        assert_eq!(sunrise, schedule.raw["timetable_sunrise"]);
        assert_eq!(sunset, schedule.raw["timetable_sunset"]);
    }

    #[test]
    fn the_cards_in_their_compact_form() {
        let (_, home, schedule) = fixture();
        let cards = schedule.cards(&home, &instance());
        let cards = cards.as_array().unwrap();
        assert_eq!(cards.len(), 13);
        // « Fenêtres 70% ouvertes » : weekdays at sunset, until 30 min after.
        let airing = cards.iter().find(|c| c["id"] == 2).unwrap();
        assert_eq!(
            *airing,
            json!({
                "id": 2, "name": "Fenêtres 70% ouvertes", "days": [0, 1, 2, 3, 4],
                "ref": "sunset", "min": 0, "end": 30,
                "set": { "velux:aa:bb:cc:dd:ee:0b": 70 },
            })
        );
        // « Stores 75% fermés » : 7 h 15, weekdays.
        let morning = cards.iter().find(|c| c["id"] == 7).unwrap();
        assert_eq!(
            (morning["ref"].clone(), morning["min"].clone()),
            (json!("time"), json!(435))
        );
        assert!(Json::Array(cards.clone()).to_string().len() < moli_core::MAX_TEXT);
    }

    #[test]
    fn sent_back_unchanged_the_schedule_is_the_same() {
        let (_, home, schedule) = fixture();
        let cards = schedule.cards(&home, &instance());
        let edited = schedule.edited(&home, &instance(), &cards).unwrap();
        // The module order of a card may change (sorted by id), nothing else.
        let (t, z, r, s) = compile(schedule.raw_cards());
        assert_eq!(
            edited["timetable"].as_array().unwrap().len(),
            t.as_array().unwrap().len()
        );
        assert_eq!(
            edited["timetable_sunset"].as_array().unwrap().len(),
            s.as_array().unwrap().len()
        );
        assert_eq!(
            edited["zones"].as_array().unwrap().len(),
            z.as_array().unwrap().len()
        );
        assert_eq!(edited["timetable_sunrise"], r);
        assert_eq!(
            schedule.cards(&home, &instance()),
            Schedule {
                raw: edited.clone()
            }
            .cards(&home, &instance())
        );
        // What the app knows and Moli does not is kept.
        let suggested = edited["metadata"]["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["card_id"] == 3)
            .unwrap();
        assert_eq!(suggested["icon"], "velux-shutter-open-32");
        assert_eq!(suggested["type"], "shutter");
        let body = Schedule::body(&edited);
        assert!(body.get("id").is_none() && body.get("selected").is_none());
        assert_eq!(body["type"], "event");
    }

    #[test]
    fn a_new_card_a_moved_time_and_a_copy_to_another_room() {
        let (_, home, schedule) = fixture();
        let mut cards = schedule
            .cards(&home, &instance())
            .as_array()
            .unwrap()
            .clone();
        // 10 h 00 → 10 h 30 for « Fenêtres 10% ouvertes ».
        let airing = cards.iter_mut().find(|c| c["id"] == 9).unwrap();
        airing["min"] = json!(630);
        // A new card: the guest room's blind at 50 % at 14 h, on Sunday, and
        // the attic's blind too (another VELUX room: a card of its own).
        cards.push(json!({
            "name": "Essai", "days": [6], "ref": "time", "min": 840,
            "set": { "velux:aa:bb:cc:dd:ee:0a": 50, "velux:aa:bb:cc:dd:ee:0c": 40 },
        }));
        let edited = schedule
            .edited(&home, &instance(), &Json::Array(cards))
            .unwrap();
        let out = edited["metadata"]["cards"].as_array().unwrap();
        assert_eq!(out.len(), 15);
        let new: Vec<&Json> = out.iter().filter(|c| c["card_name"] == "Essai").collect();
        assert_eq!(new.len(), 2);
        assert_eq!(
            (new[0]["card_id"].clone(), new[1]["card_id"].clone()),
            (json!(13), json!(14))
        );
        assert_eq!(
            (new[0]["room_id"].clone(), new[1]["room_id"].clone()),
            (json!("1003"), json!("1004"))
        );
        assert_eq!(new[0]["modules"][0]["isBlindShutter"], true);
        let offsets: Vec<i64> = edited["timetable"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["m_offset"].as_i64().unwrap())
            .collect();
        assert!(offsets.contains(&630) && !offsets.contains(&600));
        assert!(offsets.contains(&(6 * DAY + 840)));
        assert!(offsets.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn a_paused_card_is_kept_but_not_compiled() {
        let (_, home, schedule) = fixture();
        let mut cards = schedule
            .cards(&home, &instance())
            .as_array()
            .unwrap()
            .clone();
        cards.retain(|c| c["id"] == 9);
        cards[0]["off"] = json!(true);
        let edited = schedule
            .edited(&home, &instance(), &Json::Array(cards))
            .unwrap();
        assert_eq!(edited["metadata"]["cards"][0]["card_enabled"], false);
        assert_eq!(edited["timetable"], json!([]));
        assert_eq!(edited["zones"], json!([]));
    }

    #[test]
    fn what_is_refused() {
        let (_, home, schedule) = fixture();
        let card = |patch: Json| {
            let mut c = json!({ "name": "x", "days": [0], "ref": "time", "min": 600,
                                "set": { "velux:aa:bb:cc:dd:ee:0a": 50 } });
            for (k, v) in patch.as_object().unwrap() {
                c[k] = v.clone();
            }
            schedule.edited(&home, &instance(), &json!([c]))
        };
        assert!(card(json!({})).is_ok());
        for bad in [
            json!({ "name": " " }),
            json!({ "days": [] }),
            json!({ "days": [7] }),
            json!({ "ref": "noon" }),
            json!({ "min": 1440 }),
            json!({ "ref": "sunset", "min": 120 }),
            json!({ "end": 500 }),
            json!({ "set": {} }),
            json!({ "set": { "velux:aa:bb:cc:dd:ee:0a": 101 } }),
            // The climate sensor does not move.
            json!({ "set": { "velux:aa:bb:cc:dd:ee:03": 50 } }),
            json!({ "set": { "velux:inconnu": 50 } }),
            json!({ "id": 99 }),
        ] {
            assert!(card(bad.clone()).is_err(), "{bad}");
        }
    }
}

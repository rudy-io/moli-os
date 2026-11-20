//! `/api/home` says which rooms are outside (`rooms[].outside`), so the
//! dashboard's « Dehors » page shows what is there without a list to keep.
//!
//! A room is outside when, in this order:
//! 1. `home.json` says so (`rooms[].outside`, `true` or `false`);
//! 2. the plan draws it, and only outside (a garden, a terrace…): drawn
//!    inside anywhere, it is inside;
//! 3. its name or one of its aliases says so ([`moli_core::outside::WORDS`]).
//!
//! A room only devices or the plan name (not in `rooms[]`) that is outside
//! is added at the end of `rooms`, `{ "name", "outside": true, "added": true }`:
//! `added` says the family did not list it, so it does not take a place in
//! the order of the rooms.

use std::collections::BTreeMap;

use moli_core::outside::{PLAN_KINDS, fold, named_outside};
use serde_json::{Map, Value, json};

/// Writes `outside` on each room of `layout`, and appends the outside rooms
/// that `plan` or `device_rooms` (as the devices name them) know and
/// `rooms[]` does not.
pub(crate) fn resolve<'a>(
    layout: &mut Map<String, Value>,
    plan: Option<&Value>,
    device_rooms: impl IntoIterator<Item = &'a str>,
) {
    let mut rooms: Vec<Value> = match layout.get("rooms") {
        Some(Value::Array(rooms)) => rooms.clone(),
        Some(_) => return, // not ours to fix
        None => Vec::new(),
    };
    // Folded name or alias → the layout's name.
    let mut named: BTreeMap<String, String> = BTreeMap::new();
    for room in &rooms {
        let Some(name) = room.get("name").and_then(Value::as_str) else {
            continue;
        };
        for n in std::iter::once(name).chain(aliases(room)) {
            named.entry(fold(n)).or_insert_with(|| name.to_owned());
        }
    }
    let canonical = |raw: &str| {
        named
            .get(&fold(raw))
            .cloned()
            .unwrap_or_else(|| raw.trim().to_owned())
    };

    // Folded room → (its name, drawn inside somewhere: true wins).
    let mut drawn: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for area in plan_rooms(plan) {
        let (Some(room), Some(kind)) = (
            area.get("room").and_then(Value::as_str),
            area.get("kind").and_then(Value::as_str),
        ) else {
            continue;
        };
        if room.trim().is_empty() {
            continue;
        }
        let inside = !PLAN_KINDS.contains(&kind);
        let name = canonical(room);
        drawn.entry(fold(&name)).or_insert((name, inside)).1 |= inside;
    }

    let verdict = |name: &str, aliases: &[&str]| match drawn.get(&fold(name)) {
        Some((_, inside)) => !inside,
        None => std::iter::once(name)
            .chain(aliases.iter().copied())
            .any(named_outside),
    };

    for room in &mut rooms {
        let Some(obj) = room.as_object_mut() else {
            continue;
        };
        let Some(name) = obj.get("name").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        if obj.get("outside").is_some_and(Value::is_boolean) {
            continue; // the family said
        }
        let aliases: Vec<&str> = obj
            .get("aliases")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let outside = verdict(&name, &aliases);
        obj.insert("outside".into(), Value::Bool(outside));
    }

    // Rooms the layout does not list: the devices' and the plan's.
    let mut extra: BTreeMap<String, String> = BTreeMap::new();
    let known: Vec<String> = device_rooms
        .into_iter()
        .map(str::to_owned)
        .chain(drawn.values().map(|(name, _)| name.clone()))
        .collect();
    for raw in &known {
        if raw.trim().is_empty() || named.contains_key(&fold(raw)) {
            continue;
        }
        let name = canonical(raw);
        extra.entry(fold(&name)).or_insert(name);
    }
    let mut added = false;
    for name in extra.into_values() {
        if verdict(&name, &[]) {
            rooms.push(json!({ "name": name, "outside": true, "added": true }));
            added = true;
        }
    }
    if added || layout.contains_key("rooms") {
        layout.insert("rooms".into(), Value::Array(rooms));
    }
}

fn aliases(room: &Value) -> impl Iterator<Item = &str> {
    room.get("aliases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn plan_rooms(plan: Option<&Value>) -> impl Iterator<Item = &Value> {
    plan.and_then(|p| p.get("floors"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("rooms").and_then(Value::as_array))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(layout: &Value, plan: Option<&Value>, devices: &[&str]) -> Value {
        let mut layout = layout.as_object().cloned().unwrap();
        resolve(&mut layout, plan, devices.iter().copied());
        Value::Object(layout)
    }

    fn outside(layout: &Value, name: &str) -> Option<bool> {
        layout["rooms"]
            .as_array()?
            .iter()
            .find(|r| r["name"] == name)?
            .get("outside")?
            .as_bool()
    }

    #[test]
    fn a_room_named_outside_is_outside() {
        let l = run(
            &json!({ "rooms": [{ "name": "Salon" }, { "name": "Extérieur" }, { "name": "Allée du garage" }] }),
            None,
            &[],
        );
        assert_eq!(outside(&l, "Salon"), Some(false));
        assert_eq!(outside(&l, "Extérieur"), Some(true));
        assert_eq!(outside(&l, "Allée du garage"), Some(true));
    }

    #[test]
    fn an_alias_says_it_too() {
        let l = run(
            &json!({ "rooms": [{ "name": "Fond", "aliases": ["Jardin"] }] }),
            None,
            &[],
        );
        assert_eq!(outside(&l, "Fond"), Some(true));
    }

    #[test]
    fn the_family_has_the_last_word() {
        let l = run(
            &json!({ "rooms": [
                { "name": "Jardin d'hiver", "outside": false },
                { "name": "Préau", "outside": true }
            ] }),
            Some(&json!({ "floors": [{ "id": "rdc", "name": "RDC", "rooms": [
                { "id": "a", "name": "a", "kind": "garden", "room": "Jardin d'hiver", "poly": [] },
                { "id": "b", "name": "b", "kind": "living", "room": "Préau", "poly": [] }
            ] }] })),
            &[],
        );
        assert_eq!(outside(&l, "Jardin d'hiver"), Some(false));
        assert_eq!(outside(&l, "Préau"), Some(true));
    }

    #[test]
    fn the_plan_comes_before_the_name() {
        let l = run(
            &json!({ "rooms": [{ "name": "Véranda du jardin" }, { "name": "Le fond" }] }),
            Some(&json!({ "floors": [{ "id": "rdc", "name": "RDC", "rooms": [
                { "id": "a", "name": "a", "kind": "veranda", "room": "Véranda du jardin", "poly": [] },
                { "id": "b", "name": "b", "kind": "garden", "room": "le  FOND", "poly": [] }
            ] }] })),
            &[],
        );
        assert_eq!(outside(&l, "Véranda du jardin"), Some(false));
        assert_eq!(outside(&l, "Le fond"), Some(true));
    }

    #[test]
    fn drawn_inside_anywhere_is_inside() {
        let l = run(
            &json!({ "rooms": [{ "name": "Garage" }] }),
            Some(&json!({ "floors": [{ "id": "rdc", "name": "RDC", "rooms": [
                { "id": "a", "name": "a", "kind": "driveway", "room": "Garage", "poly": [] },
                { "id": "b", "name": "b", "kind": "garage", "room": "Garage", "poly": [] }
            ] }] })),
            &[],
        );
        assert_eq!(outside(&l, "Garage"), Some(false));
    }

    #[test]
    fn a_room_only_devices_name_is_added_when_outside() {
        let l = run(
            &json!({ "rooms": [{ "name": "Salon", "aliases": ["Living"] }] }),
            None,
            &["Extérieur", "Living", "Bureau", "extérieur ", "Terrasse"],
        );
        let rooms = l["rooms"].as_array().unwrap();
        assert_eq!(rooms.len(), 3, "{l}");
        assert_eq!(rooms[0]["name"], "Salon");
        assert_eq!(rooms[0].get("added"), None);
        assert_eq!(rooms[1]["added"], true);
        assert_eq!(outside(&l, "Extérieur"), Some(true));
        assert_eq!(outside(&l, "Terrasse"), Some(true));
        assert_eq!(outside(&l, "Bureau"), None);
    }

    #[test]
    fn a_room_only_the_plan_draws_outside_is_added() {
        let l = run(
            &json!({}),
            Some(&json!({ "floors": [{ "id": "rdc", "name": "RDC", "rooms": [
                { "id": "a", "name": "a", "kind": "terrace", "room": "Le bout", "poly": [] }
            ] }] })),
            &[],
        );
        assert_eq!(outside(&l, "Le bout"), Some(true));
    }

    #[test]
    fn nothing_outside_leaves_the_layout_as_it_was() {
        let l = run(&json!({ "title": "Maison" }), None, &["Salon"]);
        assert_eq!(l, json!({ "title": "Maison" }));
    }
}

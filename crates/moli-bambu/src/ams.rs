//! The filament: each AMS unit (four slots) and the external spool, as
//! devices of their own (as Home Assistant's Bambu integration shows them).
//!
//! In the printer's report: `ams.ams[]` (one entry per unit: `humidity`,
//! `temp`, `tray[]` with `tray_type`, `tray_sub_brands`, `tray_color`
//! RRGGBBAA, `remain` %), `ams.tray_now` (the slot feeding the nozzle:
//! unit × 4 + slot, 254 = the external spool, 255 = none) and `vt_tray`
//! (the external spool). Field names from the documented MQTT report that
//! ha-bambulab reads.

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::DriverCtx;
use serde_json::{Map, Value as Json};

const EXTERNAL: i64 = 254;

/// Merges a report's delta into the known state, objects key by key (a
/// delta about one AMS field must not wipe the others); arrays are whole.
pub(crate) fn merge(state: &mut Map<String, Json>, delta: &Map<String, Json>) {
    for (k, v) in delta {
        match (state.get_mut(k), v) {
            (Some(Json::Object(old)), Json::Object(new)) => merge(old, new),
            _ => {
                state.insert(k.clone(), v.clone());
            }
        }
    }
}

fn number(v: &Json) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// Ids, indexes and levels: integers, sent as numbers or as text.
fn integer(v: &Json) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// One slot in words: « PLA Basic · #FFFFFF · 85 % », or « vide ».
fn slot(tray: &Json) -> String {
    let kind = tray["tray_sub_brands"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| tray["tray_type"].as_str())
        .map_or("", str::trim);
    if kind.is_empty() {
        return moli_i18n::tr!("pilotes.bambu.vide");
    }
    let mut out = kind.to_owned();
    if let Some(color) = tray["tray_color"].as_str().filter(|c| c.len() >= 6) {
        out.push_str(" · #");
        out.push_str(&color[..6].to_ascii_uppercase());
    }
    if let Some(remain) = number(&tray["remain"]).filter(|r| (0.0..=100.0).contains(r)) {
        use std::fmt::Write as _;
        let _ = write!(
            out,
            " · {}",
            moli_i18n::tr!("pilotes.bambu.pourcent", n = format!("{remain:.0}"))
        );
    }
    out
}

fn text(key: &str, label: &str) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Text,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    }
}

fn active() -> PointSpec {
    PointSpec {
        key: "active".into(),
        label: moli_i18n::tr!("pilotes.bambu.en_cours").into(),
        kind: Kind::Binary,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    }
}

fn measure(key: &str, label: &str, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: None,
            max: None,
            step: None,
        },
        access: Access {
            read: true,
            write: false,
        },
        semantic: Semantic::infer(key, unit.as_ref()),
        unit,
    }
}

/// What the report says about the filament: devices to declare and values.
pub(crate) struct Filament {
    pub devices: Vec<Device>,
    pub values: Vec<(DeviceId, &'static str, Value)>,
}

pub(crate) fn read(
    ctx: &DriverCtx,
    serial: &str,
    printer: &str,
    state: &Map<String, Json>,
) -> Filament {
    let mut devices = Vec::new();
    let mut values = Vec::new();
    let ams = &state.get("ams").cloned().unwrap_or_default();
    let now = integer(&ams["tray_now"]);
    for (n, unit) in ams["ams"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let index = integer(&unit["id"])
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(n);
        let id = ctx.device_id(&format!("{serial}_AMS_{}", index + 1));
        let trays = unit["tray"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let slots = trays.len().max(4);
        let mut points: Vec<PointSpec> = (1..=slots)
            .map(|s| {
                text(
                    SLOT_KEYS.get(s - 1).copied().unwrap_or("slot_4"),
                    &moli_i18n::tr!("pilotes.bambu.emplacement", slot = s),
                )
            })
            .collect();
        points.push(measure(
            "humidity_index",
            &moli_i18n::tr!("pilotes.bambu.humidite"),
            None,
        ));
        points.push(measure(
            "temperature",
            &moli_i18n::tr!("pilotes.bambu.temperature"),
            Some(Unit::Celsius),
        ));
        points.push(active());
        devices.push(Device {
            id: id.clone(),
            instance: ctx.instance().clone(),
            native_name: format!("{printer} · AMS {}", index + 1).into(),
            manufacturer: Some("Bambu Lab".into()),
            model: Some("AMS".into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points,
        });
        for (s, key) in SLOT_KEYS.iter().enumerate().take(slots) {
            let tray = trays
                .iter()
                .find(|t| integer(&t["id"]).and_then(|i| usize::try_from(i).ok()) == Some(s))
                .cloned()
                .unwrap_or_default();
            values.push((id.clone(), *key, Value::Text(slot(&tray).into())));
        }
        if let Some(h) = integer(&unit["humidity"]) {
            values.push((id.clone(), "humidity_index", Value::Int(h)));
        }
        if let Some(t) = number(&unit["temp"]).filter(|t| *t > 0.0) {
            values.push((
                id.clone(),
                "temperature",
                Value::Float((t * 10.0).round() / 10.0),
            ));
        }
        let mine = now
            .filter(|t| *t != EXTERNAL && (0..255).contains(t))
            .and_then(|t| usize::try_from(t).ok())
            .is_some_and(|t| t / 4 == index);
        values.push((id, "active", Value::Bool(mine)));
    }
    if let Some(spool) = state.get("vt_tray").filter(|v| v.is_object()) {
        let id = ctx.device_id(&format!("{serial}_ExternalSpool"));
        devices.push(Device {
            id: id.clone(),
            instance: ctx.instance().clone(),
            native_name: moli_i18n::tr!("pilotes.bambu.bobine_externe_nom", printer = printer)
                .into(),
            manufacturer: Some("Bambu Lab".into()),
            model: Some(moli_i18n::tr!("pilotes.bambu.bobine_externe").into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points: vec![
                text("spool", &moli_i18n::tr!("pilotes.bambu.bobine")),
                active(),
            ],
        });
        values.push((id.clone(), "spool", Value::Text(slot(spool).into())));
        values.push((id, "active", Value::Bool(now == Some(EXTERNAL))));
    }
    Filament { devices, values }
}

const SLOT_KEYS: [&str; 4] = ["slot_1", "slot_2", "slot_3", "slot_4"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slots_read_as_words() {
        assert_eq!(
            slot(
                &json!({"id": "0", "tray_type": "PLA", "tray_sub_brands": "PLA Basic", "tray_color": "FF6A13FF", "remain": 85})
            ),
            "PLA Basic · #FF6A13 · 85 %"
        );
        assert_eq!(
            slot(&json!({"id": "1", "tray_type": "PETG", "tray_color": "000000FF", "remain": -1})),
            "PETG · #000000"
        );
        assert_eq!(slot(&json!({"id": "2"})), "vide");
        assert_eq!(slot(&json!({"id": "3", "tray_type": ""})), "vide");
    }

    #[test]
    fn a_delta_keeps_what_it_does_not_say() {
        let mut state: Map<String, Json> = serde_json::from_value(json!({
            "ams": {"ams": [{"id": "0", "humidity": "4", "tray": []}], "tray_now": "255"},
            "mc_percent": 10
        }))
        .unwrap();
        let delta: Map<String, Json> =
            serde_json::from_value(json!({"ams": {"tray_now": "2"}, "mc_percent": 11})).unwrap();
        merge(&mut state, &delta);
        assert_eq!(state["ams"]["tray_now"], "2");
        assert_eq!(
            state["ams"]["ams"][0]["humidity"], "4",
            "the units are still known"
        );
        assert_eq!(state["mc_percent"], 11);
    }
}

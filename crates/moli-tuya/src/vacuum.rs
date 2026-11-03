//! Robot vacuums (category `sd`): the Rowenta and Tefal X-plorer Serie 75, and
//! the others described by the same Tuya codes.
//!
//! The Serie 75's table comes from `tefal_xplorer_vacuum.yaml` of tuya-local
//! (MIT, see NOTICE), confirmed on a real robot.
//!
//! The generic translation would hand over every data point as it is, with its
//! writable flag: on a robot that includes erasing the map and driving by hand.
//! Here only what the table names exists, under the same keys for every robot.
//! Anything that moves the robot is a `Control`: an agent asking gets a human's
//! approval first.

use moli_core::{Kind, PointSpec, Semantic, Value};
use serde_json::{Map, Value as Json};

use crate::model::{Dp, DpKind};

pub const CATEGORY: &str = "sd";

const STATE: &str = "state";
const DOCK: &str = "dock";
const FAULT: &str = "fault";
const DETAIL: &str = "detail";
/// The modes that send a robot home: `dock` says it better than `mode`.
const GO_HOME: [&str; 2] = ["chargego", "backcharge"];
const GOING_HOME: [&str; 2] = ["goto_charge", "tocharge"];
/// Modes a robot can start from a word alone: the others need a zone or a room
/// drawn on its map, which this driver does not read.
const STARTABLE: [&str; 4] = ["smart", "pose", "part", "wallfollow"];

struct Role {
    key: &'static str,
    codes: &'static [&'static str],
    label: &'static str,
    semantic: Semantic,
    write: bool,
}

const fn role(
    key: &'static str,
    codes: &'static [&'static str],
    label: &'static str,
    semantic: Semantic,
    write: bool,
) -> Role {
    Role {
        key,
        codes,
        label,
        semantic,
        write,
    }
}

const ROLES: [Role; 13] = [
    role(
        "clean",
        &["switch_go", "power_go"],
        "pilotes.tuya.robot_nettoyage",
        Semantic::Control,
        true,
    ),
    role(
        "pause",
        &["pause"],
        "pilotes.tuya.robot_pause",
        Semantic::Control,
        true,
    ),
    role(
        "mode",
        &["mode"],
        "pilotes.tuya.robot_mode",
        Semantic::Control,
        true,
    ),
    role(
        "locate",
        &["seek"],
        "pilotes.tuya.robot_localiser",
        Semantic::Other,
        true,
    ),
    role(
        STATE,
        &["status"],
        "pilotes.tuya.robot_etat",
        Semantic::Other,
        false,
    ),
    role(
        "battery",
        &[
            "residual_electricity",
            "electricity_left",
            "battery_percentage",
        ],
        "pilotes.tuya.robot_batterie",
        Semantic::Battery,
        false,
    ),
    role(
        "fan_speed",
        &["suction"],
        "pilotes.tuya.robot_aspiration",
        Semantic::Config,
        true,
    ),
    role(
        "mop",
        &["cistern"],
        "pilotes.tuya.robot_serpilliere",
        Semantic::Config,
        true,
    ),
    role(
        "volume",
        &["volume_set"],
        "pilotes.tuya.robot_volume",
        Semantic::Config,
        true,
    ),
    role(
        "clean_time",
        &["clean_time"],
        "pilotes.tuya.robot_duree",
        Semantic::Other,
        false,
    ),
    role(
        "clean_area",
        &["clean_area"],
        "pilotes.tuya.robot_surface",
        Semantic::Other,
        false,
    ),
    role(
        "edge_brush",
        &["edge_brush"],
        "pilotes.tuya.robot_brosse_laterale",
        Semantic::Other,
        false,
    ),
    role(
        "main_brush",
        &["roll_brush"],
        "pilotes.tuya.robot_brosse",
        Semantic::Other,
        false,
    ),
];

/// The bits of the Serie 75's `fault`. Other robots number theirs differently:
/// theirs are told by number.
const SERIES_75_FAULT: u32 = 122;
const FAULTS: [(u32, &str); 6] = [
    (1, "pilotes.tuya.robot_panne_batterie"),
    (5, "pilotes.tuya.robot_panne_vide"),
    (12, "pilotes.tuya.robot_panne_brosse"),
    (19, "pilotes.tuya.robot_panne_coince"),
    (20, "pilotes.tuya.robot_panne_souleve"),
    (25, "pilotes.tuya.robot_panne_retour"),
];

fn dp_of<'a>(dps: &'a [Dp], role: &Role) -> Option<&'a Dp> {
    role.codes
        .iter()
        .find_map(|code| dps.iter().find(|dp| dp.code == *code))
}

fn role_of(key: &str) -> Option<&'static Role> {
    ROLES.iter().find(|role| role.key == key)
}

fn point(key: &str, label: &str, kind: Kind, semantic: Semantic, write: bool) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: moli_i18n::tr(label).into(),
        kind,
        access: moli_core::Access { read: true, write },
        unit: None,
        semantic,
    }
}

/// The robot's own word for going home, with the data point that takes it.
fn go_home(dps: &[Dp]) -> Option<(&Dp, &'static str)> {
    let mode = dps.iter().find(|dp| dp.code == "mode")?;
    let word = GO_HOME
        .into_iter()
        .find(|word| mode.range.iter().any(|known| known == word))?;
    Some((mode, word))
}

/// What the robot shows: the table's points its data points allow.
#[must_use]
pub fn points(dps: &[Dp]) -> Vec<PointSpec> {
    let mut points: Vec<PointSpec> = ROLES
        .iter()
        .filter_map(|role| {
            let mut spec = dp_of(dps, role)?.point(false)?;
            spec.key = role.key.into();
            spec.label = moli_i18n::tr(role.label).into();
            spec.semantic = role.semantic;
            spec.access.write = role.write;
            // Only what an order can reach is offered.
            if role.key == "mode"
                && let Kind::Enum { values } = &mut spec.kind
            {
                values.retain(|mode| STARTABLE.contains(&mode.as_ref()));
            }
            Some(spec)
        })
        .collect();
    if go_home(dps).is_some() {
        points.push(point(
            DOCK,
            "pilotes.tuya.robot_retour",
            Kind::Binary,
            Semantic::Control,
            true,
        ));
    }
    if dps.iter().any(|dp| dp.code == FAULT) {
        points.push(point(
            FAULT,
            "pilotes.tuya.robot_probleme",
            Kind::Binary,
            Semantic::Alert,
            false,
        ));
        points.push(point(
            DETAIL,
            "pilotes.tuya.robot_detail",
            Kind::Text,
            Semantic::Other,
            false,
        ));
    }
    points
}

/// A fault's bits, in words: an unnamed bit is still told, by its number.
fn fault_words(bits: u64, names: &[(u32, &str)]) -> String {
    (0..64u32)
        .filter(|bit| bits & (1 << bit) != 0)
        .map(|bit| {
            names.iter().find(|(known, _)| *known == bit).map_or_else(
                || moli_i18n::tr!("pilotes.tuya.robot_panne_code", bit = bit),
                |(_, name)| moli_i18n::tr(name),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `{"8": 96, "5": "sleep"}` → `[(point key, value)]`, unknown data points skipped.
#[must_use]
pub fn decode(dps: &[Dp], raw: &Map<String, Json>) -> Vec<(String, Value)> {
    let mut values = Vec::new();
    for dp in dps {
        let Some(value) = raw.get(&dp.id.to_string()) else {
            continue;
        };
        if dp.code == FAULT {
            let Some(bits) = value.as_u64() else { continue };
            values.push((FAULT.to_owned(), Value::Bool(bits != 0)));
            let names: &[_] = if dp.id == SERIES_75_FAULT {
                &FAULTS
            } else {
                &[]
            };
            let words = fault_words(bits, names);
            values.push((DETAIL.to_owned(), Value::from(words.as_str())));
            continue;
        }
        let Some(role) = ROLES
            .iter()
            .find(|role| role.codes.contains(&dp.code.as_str()))
        else {
            continue;
        };
        if let Some(decoded) = dp.decode(false, value) {
            if role.key == STATE {
                let home =
                    matches!(&decoded, Value::Text(state) if GOING_HOME.contains(&state.as_ref()));
                values.push((DOCK.to_owned(), Value::Bool(home)));
            }
            // Going home is told by `dock`: `mode` only names what can be asked for.
            if role.key == "mode"
                && !matches!(&decoded, Value::Text(mode) if STARTABLE.contains(&mode.as_ref()))
            {
                continue;
            }
            values.push((role.key.to_owned(), decoded));
        }
    }
    values
}

fn set(dp: &Dp, value: Json, order: &mut Map<String, Json>) {
    order.insert(dp.id.to_string(), value);
}

/// The data points to send for a point set to `value` (already validated).
/// `None`: not something a robot takes, or not from a word alone.
#[must_use]
pub fn order(dps: &[Dp], key: &str, value: &Value) -> Option<Map<String, Json>> {
    let by_code = |code: &str| dps.iter().find(|dp| dp.code == code);
    let go = role_of("clean").and_then(|role| dp_of(dps, role));
    let mut order = Map::new();
    match (key, value) {
        // Going again after a pause needs the pause lifted too.
        ("clean", Value::Bool(true)) => {
            set(go?, Json::Bool(true), &mut order);
            if let Some(pause) = by_code("pause") {
                set(pause, Json::Bool(false), &mut order);
            }
        }
        ("clean" | DOCK, Value::Bool(false)) => set(go?, Json::Bool(false), &mut order),
        (DOCK, Value::Bool(true)) => {
            let (mode, word) = go_home(dps)?;
            set(mode, Json::from(word), &mut order);
        }
        ("mode", Value::Text(mode)) if STARTABLE.contains(&mode.as_ref()) => {
            set(by_code("mode")?, Json::from(mode.as_ref()), &mut order);
        }
        // A beep cannot be taken back.
        ("locate", Value::Bool(true)) => set(by_code("seek")?, Json::Bool(true), &mut order),
        ("pause" | "fan_speed" | "mop" | "volume", _) => {
            let dp = dp_of(dps, role_of(key)?)?;
            set(dp, dp.encode(false, value)?, &mut order);
        }
        _ => return None,
    }
    Some(order)
}

fn dp(id: u32, code: &str, kind: DpKind, unit: Option<&str>, range: &[&str]) -> Dp {
    Dp {
        id,
        code: code.to_owned(),
        kind,
        writable: false,
        unit: unit.map(str::to_owned),
        scale: 0,
        min: None,
        max: None,
        step: None,
        range: range.iter().map(|value| (*value).to_owned()).collect(),
    }
}

/// The data points a Serie 75 answers on the network. The cloud describes others
/// (`power_go` on 2, `mode` on 3, `seek` on 13) that it does not answer: this table
/// replaces that description.
#[must_use]
pub fn series_75() -> Vec<Dp> {
    use DpKind::{Bitmap, Boolean, Enum, Integer};
    vec![
        dp(101, "switch_go", Boolean, None, &[]),
        dp(102, "pause", Boolean, None, &[]),
        dp(
            104,
            "mode",
            Enum,
            None,
            &[
                "smart",
                "pose",
                "zone",
                "backcharge",
                "curpointing",
                "selectroom",
            ],
        ),
        dp(
            105,
            "status",
            Enum,
            None,
            &[
                "idle",
                "totaling",
                "sweep",
                "mop",
                "fault",
                "pause",
                "chargring",
                "tocharge",
                "fullcharge",
                "remotectl",
                "dormant",
                "curpointing",
                "selectroom",
            ],
        ),
        dp(106, "residual_electricity", Integer, Some("%"), &[]),
        dp(107, "clean_time", Integer, Some("s"), &[]),
        dp(108, "clean_area", Integer, Some("m²"), &[]),
        dp(
            109,
            "suction",
            Enum,
            None,
            &["quiet", "strong", "auto", "max"],
        ),
        dp(112, "seek", Boolean, None, &[]),
        dp(119, "edge_brush", Integer, Some("s"), &[]),
        dp(120, "roll_brush", Integer, Some("s"), &[]),
        dp(SERIES_75_FAULT, "fault", Bitmap, None, &[]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What a real Serie 75 answered, on its way home.
    const STATUS: &str = include_str!("../../../integrations/tuya/fixtures/robot_status.json");

    fn raw(text: &str) -> Map<String, Json> {
        let parsed: Json = serde_json::from_str(text).unwrap();
        parsed["dps"].as_object().unwrap().clone()
    }

    fn sent(order: Option<Map<String, Json>>) -> Json {
        Json::Object(order.unwrap())
    }

    fn value<'a>(values: &'a [(String, Value)], key: &str) -> &'a Value {
        &values.iter().find(|(k, _)| k == key).unwrap().1
    }

    #[test]
    fn a_real_status_reads_in_moli_words() {
        let values = decode(&series_75(), &raw(STATUS));
        assert_eq!(value(&values, "battery"), &Value::Int(91));
        assert_eq!(value(&values, "state"), &Value::from("tocharge"));
        assert_eq!(value(&values, "clean"), &Value::Bool(false));
        assert_eq!(value(&values, "dock"), &Value::Bool(true));
        // On its way home: not a mode the point offers.
        assert!(values.iter().all(|(key, _)| key != "mode"));
        assert_eq!(value(&values, "fan_speed"), &Value::from("strong"));
        assert_eq!(value(&values, "fault"), &Value::Bool(false));
        // Totals and data points outside the table are not points.
        assert!(values.iter().all(|(key, _)| key != "total_clean_area"));
    }

    #[test]
    fn a_startable_mode_is_read() {
        let values = decode(&series_75(), &raw(r#"{"dps":{"104":"smart"}}"#));
        assert_eq!(value(&values, "mode"), &Value::from("smart"));
    }

    #[test]
    fn every_value_read_has_its_point() {
        let points = points(&series_75());
        for (key, _) in decode(&series_75(), &raw(STATUS)) {
            assert!(points.iter().any(|p| p.key.as_ref() == key), "{key}");
        }
    }

    #[test]
    fn only_the_table_can_be_commanded_and_moving_is_a_control() {
        // As the cloud describes it: everything writable, map reset included.
        let mut dps = series_75();
        for dp in &mut dps {
            dp.writable = true;
        }
        dps.push(dp(13, "reset_map", DpKind::Boolean, None, &[]));
        dps.push(dp(
            111,
            "direction_control",
            DpKind::Enum,
            None,
            &["backward"],
        ));
        let points = points(&dps);
        assert!(points.iter().all(|p| !p.key.contains("reset")));
        assert!(points.iter().all(|p| p.key.as_ref() != "direction_control"));
        for key in ["clean", "pause", "dock", "mode"] {
            let point = points.iter().find(|p| p.key.as_ref() == key).unwrap();
            assert_eq!(point.semantic, Semantic::Control, "{key}");
            assert!(point.access.write, "{key}");
        }
        let mode = points.iter().find(|p| p.key.as_ref() == "mode").unwrap();
        let Kind::Enum { values } = &mode.kind else {
            panic!("mode is a choice");
        };
        assert!(values.iter().all(|m| STARTABLE.contains(&m.as_ref())));
        let battery = points.iter().find(|p| p.key.as_ref() == "battery").unwrap();
        assert_eq!(battery.semantic, Semantic::Battery);
        assert!(!battery.access.write);
    }

    #[test]
    fn orders_reach_the_robots_own_data_points() {
        let dps = series_75();
        let on = |key, value| sent(order(&dps, key, &value));
        assert_eq!(
            on("clean", Value::Bool(true)),
            json!({"101": true, "102": false})
        );
        assert_eq!(on("clean", Value::Bool(false)), json!({"101": false}));
        assert_eq!(on("pause", Value::Bool(true)), json!({"102": true}));
        assert_eq!(on("dock", Value::Bool(true)), json!({"104": "backcharge"}));
        assert_eq!(on("mode", Value::from("smart")), json!({"104": "smart"}));
        assert_eq!(on("locate", Value::Bool(true)), json!({"112": true}));
        assert_eq!(on("fan_speed", Value::from("max")), json!({"109": "max"}));
    }

    #[test]
    fn a_robot_described_by_the_cloud_goes_home_by_its_own_word() {
        let dps = [dp(4, "mode", DpKind::Enum, None, &["smart", "chargego"])];
        assert_eq!(
            sent(order(&dps, "dock", &Value::Bool(true))),
            json!({"4": "chargego"})
        );
    }

    #[test]
    fn what_needs_a_map_or_cannot_be_taken_back_is_refused() {
        let dps = series_75();
        assert!(order(&dps, "mode", &Value::from("zone")).is_none());
        assert!(order(&dps, "mode", &Value::from("selectroom")).is_none());
        assert!(order(&dps, "mode", &Value::from("backcharge")).is_none());
        assert!(order(&dps, "locate", &Value::Bool(false)).is_none());
        assert!(order(&dps, "battery", &Value::Int(100)).is_none());
        assert!(order(&dps, "reset_map", &Value::Bool(true)).is_none());
    }

    #[test]
    fn going_home_shows_and_a_fault_is_told() {
        let dps = series_75();
        // Bit 12, the main brush, bit 20, picked up.
        let values = decode(&dps, &raw(r#"{"dps":{"105":"tocharge","122":1052672}}"#));
        assert_eq!(value(&values, "dock"), &Value::Bool(true));
        assert_eq!(value(&values, "fault"), &Value::Bool(true));
        assert_eq!(
            value(&values, "detail"),
            &Value::from("brosse centrale, robot soulevé")
        );
        let unnamed = decode(&dps, &raw(r#"{"dps":{"122":8}}"#));
        assert_eq!(value(&unnamed, "detail"), &Value::from("code 3"));
    }

    #[test]
    fn another_robots_fault_bits_are_not_named_for_it() {
        let dps = [dp(28, "fault", DpKind::Bitmap, None, &[])];
        let values = decode(&dps, &raw(r#"{"dps":{"28":4096}}"#));
        assert_eq!(value(&values, "detail"), &Value::from("code 12"));
    }
}

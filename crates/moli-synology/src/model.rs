//! What DSM says, as Moli devices and points.
//!
//! Every field is optional: a DSM version that leaves one out loses that
//! point (its value goes `Null`), never the device. A value Moli does not
//! know is never guessed.

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::Value as Json;

pub(crate) type States = Vec<(&'static str, Value)>;

/// A device with the values to publish for it.
pub(crate) struct Entity {
    pub device: Device,
    pub states: States,
}

/// The NAS itself: always published, even while it sleeps (it carries the
/// order that wakes it).
pub(crate) const NAS: &str = "nas";
pub(crate) const CONTROL: &str = "control";
pub(crate) const WAKE: &str = "wake";
pub(crate) const SHUTDOWN: &str = "shutdown";
pub(crate) const REBOOT: &str = "reboot";

/// Disks and volumes in these states are fine (a unit being scrubbed or a
/// free slot is not a problem).
const FINE: [&str; 4] = ["normal", "initialized", "not_initialized", "background"];

fn num(value: &Json) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n| n.is_finite())
}

/// The longest name or word taken from the NAS.
const MAX_TEXT: usize = 80;

/// A word from the NAS (a name, an identifier, a status): a text too long or
/// holding a control character is not taken, since it ends up in identifiers,
/// names and messages.
fn text(value: &Json) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| {
        !s.is_empty() && s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control)
    })
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

fn float(n: Option<f64>) -> Value {
    n.map_or(Value::Null, |n| Value::Float(round1(n)))
}

/// A percentage, kept within 0 to 100 whatever the NAS says.
fn percentage(n: Option<f64>) -> Value {
    float(n.map(|n| n.clamp(0.0, 100.0)))
}

fn word(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.into()))
}

fn spec(
    key: &str,
    label: String,
    kind: Kind,
    unit: Option<Unit>,
    semantic: Semantic,
    write: bool,
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

fn percent(key: &str, label: String, semantic: Semantic) -> PointSpec {
    let kind = Kind::Numeric {
        min: Some(0.0),
        max: Some(100.0),
        step: None,
    };
    spec(key, label, kind, Some(Unit::Percent), semantic, false)
}

fn reading(key: &str, label: String, unit: Unit, semantic: Semantic) -> PointSpec {
    let kind = Kind::Numeric {
        min: None,
        max: None,
        step: None,
    };
    spec(key, label, kind, Some(unit), semantic, false)
}

fn words(key: &str, label: String) -> PointSpec {
    spec(key, label, Kind::Text, None, Semantic::Other, false)
}

/// The two points every part with something to watch carries: a flag (the
/// dashboard's « Parc » page lists what is wrong) and its words.
fn alert_points() -> [PointSpec; 2] {
    [
        spec(
            "problem",
            moli_i18n::tr!("pilotes.synology.probleme"),
            Kind::Binary,
            None,
            Semantic::Alert,
            false,
        ),
        words("detail", moli_i18n::tr!("pilotes.synology.detail")),
    ]
}

fn alert(reasons: &[String]) -> States {
    vec![
        ("problem", Value::Bool(!reasons.is_empty())),
        ("detail", Value::Text(reasons.join(", ").into())),
    ]
}

fn device(
    instance: &InstanceId,
    native: &str,
    name: String,
    model: Option<&str>,
    points: Vec<PointSpec>,
) -> Device {
    Device {
        id: DeviceId::new(instance, native),
        instance: instance.clone(),
        native_name: name.into(),
        manufacturer: Some("Synology".into()),
        model: model.map(Into::into),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// The NAS. `wake`: its MAC address is known, so Moli can wake it.
pub(crate) fn nas_device(instance: &InstanceId, model: Option<&str>, wake: bool) -> Device {
    let mut orders: Vec<&str> = vec![SHUTDOWN, REBOOT];
    if wake {
        orders.insert(0, WAKE);
    }
    let control = spec(
        CONTROL,
        moli_i18n::tr!("pilotes.synology.commande"),
        Kind::Enum {
            values: orders.into_iter().map(Into::into).collect(),
        },
        None,
        Semantic::Control,
        true,
    );
    let mut points = vec![
        spec(
            "up",
            moli_i18n::tr!("pilotes.synology.en_marche"),
            Kind::Binary,
            None,
            Semantic::Other,
            false,
        ),
        reading(
            "temperature",
            moli_i18n::tr!("pilotes.synology.temperature"),
            Unit::Celsius,
            Semantic::Temperature,
        ),
        percent(
            "cpu",
            moli_i18n::tr!("pilotes.synology.processeur"),
            Semantic::Other,
        ),
        percent(
            "memory",
            moli_i18n::tr!("pilotes.synology.memoire"),
            Semantic::Other,
        ),
        reading(
            "uptime",
            moli_i18n::tr!("pilotes.synology.duree_fonctionnement"),
            Unit::Hour,
            Semantic::Other,
        ),
    ];
    points.extend(alert_points());
    points.push(control);
    device(
        instance,
        NAS,
        moli_i18n::tr!("pilotes.synology.nas"),
        model,
        points,
    )
}

/// The NAS answered: `info` is `SYNO.DSM.Info`, `usage` is
/// `SYNO.Core.System.Utilization` (`Null` when it could not be read).
pub(crate) fn nas_states(info: &Json, usage: &Json) -> States {
    let cpu = &usage["cpu"];
    let load = ["user_load", "system_load", "other_load"]
        .iter()
        .filter_map(|key| num(&cpu[*key]))
        .reduce(|a, b| a + b)
        .map(|n| n.clamp(0.0, 100.0));
    let mut reasons = Vec::new();
    if info["temperature_warn"].as_bool() == Some(true) {
        reasons.push(moli_i18n::tr!("pilotes.synology.detail_temperature"));
    }
    let mut values: States = vec![
        ("up", Value::Bool(true)),
        ("temperature", float(num(&info["temperature"]))),
        ("cpu", float(load)),
        ("memory", percentage(num(&usage["memory"]["real_usage"]))),
        ("uptime", float(num(&info["uptime"]).map(|s| s / 3600.0))),
    ];
    values.extend(alert(&reasons));
    values
}

/// The NAS does not answer: nothing is known any more.
pub(crate) fn nas_states_down() -> States {
    vec![
        ("up", Value::Bool(false)),
        ("temperature", Value::Null),
        ("cpu", Value::Null),
        ("memory", Value::Null),
        ("uptime", Value::Null),
        ("problem", Value::Null),
        ("detail", Value::Null),
    ]
}

/// The model name, for the NAS device.
pub(crate) fn model(info: &Json) -> Option<&str> {
    text(&info["model"])
}

const GIB: f64 = 1_073_741_824.0;

pub(crate) fn volumes(instance: &InstanceId, storage: &Json) -> Vec<Entity> {
    let Some(list) = storage["volumes"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|volume| {
            let id = text(&volume["id"])?;
            let number = id.trim_start_matches("volume_");
            let total = num(&volume["size"]["total"]).filter(|t| *t > 0.0);
            let used = num(&volume["size"]["used"]);
            let status = text(&volume["status"]);
            let mut reasons = Vec::new();
            if let Some(status) = status.filter(|s| !FINE.contains(s)) {
                reasons.push(moli_i18n::tr!(
                    "pilotes.synology.detail_etat",
                    status = status
                ));
            }
            let mut points = vec![
                percent(
                    "used",
                    moli_i18n::tr!("pilotes.synology.occupe"),
                    Semantic::Other,
                ),
                reading(
                    "free",
                    moli_i18n::tr!("pilotes.synology.libre"),
                    Unit::Other("GB".into()),
                    Semantic::Other,
                ),
                words("status", moli_i18n::tr!("pilotes.synology.etat")),
            ];
            points.extend(alert_points());
            let mut values: States = vec![
                (
                    "used",
                    percentage(total.zip(used).map(|(t, u)| u / t * 100.0)),
                ),
                (
                    "free",
                    float(total.zip(used).map(|(t, u)| ((t - u) / GIB).max(0.0))),
                ),
                ("status", word(status)),
            ];
            values.extend(alert(&reasons));
            Some(Entity {
                device: device(
                    instance,
                    &format!("volume_{number}"),
                    moli_i18n::tr!("pilotes.synology.volume", n = number),
                    None,
                    points,
                ),
                states: values,
            })
        })
        .collect()
}

pub(crate) fn disks(instance: &InstanceId, storage: &Json) -> Vec<Entity> {
    let Some(list) = storage["disks"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|disk| {
            let id = text(&disk["id"])?;
            let status = text(&disk["status"]);
            let smart = text(&disk["smart_status"]);
            let mut reasons = Vec::new();
            if let Some(status) = status.filter(|s| !FINE.contains(s)) {
                reasons.push(moli_i18n::tr!(
                    "pilotes.synology.detail_etat",
                    status = status
                ));
            }
            if let Some(smart) = smart.filter(|s| *s != "normal") {
                reasons.push(moli_i18n::tr!(
                    "pilotes.synology.detail_smart",
                    status = smart
                ));
            }
            if disk["exceed_bad_sector_thr"].as_bool() == Some(true) {
                reasons.push(moli_i18n::tr!("pilotes.synology.detail_secteurs"));
            }
            if disk["below_remain_life_thr"].as_bool() == Some(true) {
                reasons.push(moli_i18n::tr!("pilotes.synology.detail_usure"));
            }
            let mut points = vec![
                reading(
                    "temperature",
                    moli_i18n::tr!("pilotes.synology.temperature"),
                    Unit::Celsius,
                    Semantic::Temperature,
                ),
                words("status", moli_i18n::tr!("pilotes.synology.etat")),
            ];
            points.extend(alert_points());
            let mut values: States = vec![
                ("temperature", float(num(&disk["temp"]))),
                ("status", word(status)),
            ];
            values.extend(alert(&reasons));
            Some(Entity {
                device: device(
                    instance,
                    &format!("disk_{id}"),
                    text(&disk["name"]).map_or_else(
                        || moli_i18n::tr!("pilotes.synology.disque", id = id),
                        Into::into,
                    ),
                    text(&disk["model"]),
                    points,
                ),
                states: values,
            })
        })
        .collect()
}

/// Statuses of the Network UPS Tools vocabulary that mean « not on mains
/// power, or the battery is failing ».
const UPS_TROUBLE: [&str; 6] = ["ob", "onbatt", "onbattery", "lb", "lowbatt", "rb"];

pub(crate) fn ups(instance: &InstanceId, ups: &Json) -> Option<Entity> {
    if ups.as_object().is_none_or(serde_json::Map::is_empty) || ups["enable"] == false {
        return None;
    }
    let status = text(&ups["status"]);
    let trouble = status.is_some_and(|s| {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| UPS_TROUBLE.contains(&token))
    });
    let reasons = if trouble {
        vec![moli_i18n::tr!("pilotes.synology.detail_batterie")]
    } else {
        Vec::new()
    };
    let mut points = vec![
        percent(
            "battery",
            moli_i18n::tr!("pilotes.synology.batterie"),
            Semantic::Battery,
        ),
        words("status", moli_i18n::tr!("pilotes.synology.etat")),
    ];
    points.extend(alert_points());
    let mut values: States = vec![
        ("battery", percentage(num(&ups["charge"]))),
        ("status", word(status)),
    ];
    values.extend(alert(&reasons));
    Some(Entity {
        device: device(
            instance,
            "ups",
            moli_i18n::tr!("pilotes.synology.onduleur"),
            text(&ups["model"]),
            points,
        ),
        states: values,
    })
}

/// A Hyper Backup task's last result that is not a failure.
const BACKUP_FINE: [&str; 3] = ["done", "none", "success"];

pub(crate) fn backups(instance: &InstanceId, tasks: &Json) -> Vec<Entity> {
    let Some(list) = tasks["task_list"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|task| {
            let id = match &task["task_id"] {
                Json::Number(n) => n.to_string(),
                other => text(other)?.to_owned(),
            };
            let result = text(&task["last_bkp_result"]);
            let mut reasons = Vec::new();
            if let Some(result) =
                result.filter(|r| !BACKUP_FINE.contains(&r.to_ascii_lowercase().as_str()))
            {
                reasons.push(moli_i18n::tr!(
                    "pilotes.synology.detail_resultat",
                    result = result
                ));
            }
            let mut points = vec![
                words("status", moli_i18n::tr!("pilotes.synology.etat")),
                words(
                    "last_result",
                    moli_i18n::tr!("pilotes.synology.dernier_resultat"),
                ),
                words(
                    "last_run",
                    moli_i18n::tr!("pilotes.synology.derniere_execution"),
                ),
            ];
            points.extend(alert_points());
            let mut values: States = vec![
                ("status", word(text(&task["state"]))),
                ("last_result", word(result)),
                ("last_run", word(text(&task["last_bkp_time"]))),
            ];
            values.extend(alert(&reasons));
            Some(Entity {
                device: device(
                    instance,
                    &format!("backup_{id}"),
                    text(&task["name"]).map_or_else(
                        || moli_i18n::tr!("pilotes.synology.sauvegarde", id = id.as_str()),
                        Into::into,
                    ),
                    None,
                    points,
                ),
                states: values,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(text: &str) -> Json {
        serde_json::from_str::<Json>(text).unwrap()["data"].clone()
    }
    fn instance() -> InstanceId {
        InstanceId::from("nas")
    }
    fn state<'a>(entity: &'a Entity, key: &str) -> &'a Value {
        &entity.states.iter().find(|(k, _)| *k == key).unwrap().1
    }
    fn storage() -> Json {
        fixture(include_str!(
            "../../../integrations/synology/fixtures/storage-faulty.json"
        ))
    }

    #[test]
    fn the_nas_reads_temperature_load_memory_and_uptime() {
        // A real DSM 7.4 answer (anonymised).
        let info = fixture(include_str!(
            "../../../integrations/synology/fixtures/info.json"
        ));
        let usage = fixture(include_str!(
            "../../../integrations/synology/fixtures/utilization.json"
        ));
        let states = nas_states(&info, &usage);
        let get = |k: &str| states.iter().find(|(key, _)| *key == k).unwrap().1.clone();
        assert_eq!(get("up"), Value::Bool(true));
        assert_eq!(get("temperature"), Value::Float(42.0));
        assert_eq!(get("cpu"), Value::Float(3.0));
        assert_eq!(get("memory"), Value::Float(18.0));
        assert_eq!(get("uptime"), Value::Float(359.3));
        assert_eq!(get("problem"), Value::Bool(false));
        assert_eq!(model(&info), Some("DS1019+"));
    }

    #[test]
    fn a_healthy_real_nas_has_one_volume_five_disks_no_ups_and_no_problem() {
        let storage = fixture(include_str!(
            "../../../integrations/synology/fixtures/storage.json"
        ));
        let volumes = volumes(&instance(), &storage);
        assert_eq!(volumes.len(), 1);
        assert_eq!(state(&volumes[0], "used"), &Value::Float(78.7));
        assert_eq!(state(&volumes[0], "free"), &Value::Float(3806.4));
        assert_eq!(state(&volumes[0], "problem"), &Value::Bool(false));
        let disks = disks(&instance(), &storage);
        assert_eq!(disks.len(), 5);
        assert!(
            disks
                .iter()
                .all(|d| state(d, "problem") == &Value::Bool(false))
        );
        assert_eq!(state(&disks[1], "temperature"), &Value::Float(42.0));
        // UPS not set up (`enable: false`): no device.
        let ups_off = fixture(include_str!(
            "../../../integrations/synology/fixtures/ups.json"
        ));
        assert!(ups(&instance(), &ups_off).is_none());
        let tasks = fixture(include_str!(
            "../../../integrations/synology/fixtures/backup.json"
        ));
        let backups = backups(&instance(), &tasks);
        assert_eq!(backups.len(), 1);
        assert_eq!(&*backups[0].device.id.to_string(), "nas:backup_8");
        assert_eq!(state(&backups[0], "problem"), &Value::Bool(false));
        assert_eq!(state(&backups[0], "last_result"), &Value::from("done"));
    }

    #[test]
    fn a_hot_nas_is_a_problem_and_an_unread_value_is_null_not_stale() {
        let states = nas_states(
            &json!({"temperature":78,"temperature_warn":true}),
            &Json::Null,
        );
        let get = |k: &str| states.iter().find(|(key, _)| *key == k).unwrap().1.clone();
        assert_eq!(get("problem"), Value::Bool(true));
        assert_eq!(get("cpu"), Value::Null);
        assert_eq!(get("memory"), Value::Null);
        assert_eq!(get("uptime"), Value::Null);
        assert!(
            nas_states_down()
                .iter()
                .all(|(k, v)| *k == "up" || *v == Value::Null)
        );
    }

    #[test]
    fn the_wake_order_exists_only_when_the_mac_address_is_known() {
        let orders = |wake| match &nas_device(&instance(), None, wake)
            .point(CONTROL)
            .unwrap()
            .kind
        {
            Kind::Enum { values } => values.iter().map(ToString::to_string).collect::<Vec<_>>(),
            _ => panic!("an enum"),
        };
        assert_eq!(orders(true), [WAKE, SHUTDOWN, REBOOT]);
        assert_eq!(orders(false), [SHUTDOWN, REBOOT]);
        let device = nas_device(&instance(), None, true);
        let control = device.point(CONTROL).unwrap();
        assert_eq!(control.semantic, Semantic::Control);
        assert!(control.access.write);
    }

    #[test]
    fn volumes_report_use_free_space_and_a_degraded_volume() {
        let volumes = volumes(&instance(), &storage());
        assert_eq!(volumes.len(), 2);
        let first = &volumes[0];
        assert_eq!(&*first.device.id.to_string(), "nas:volume_1");
        assert_eq!(state(first, "used"), &Value::Float(50.0));
        assert_eq!(state(first, "free"), &Value::Float(1024.0));
        assert_eq!(state(first, "problem"), &Value::Bool(false));
        let degraded = &volumes[1];
        assert_eq!(state(degraded, "problem"), &Value::Bool(true));
        assert!(matches!(state(degraded, "detail"), Value::Text(t) if t.contains("degrade")));
    }

    #[test]
    fn disks_flag_status_smart_bad_sectors_and_wear() {
        let disks = disks(&instance(), &storage());
        assert_eq!(disks.len(), 3);
        assert_eq!(state(&disks[0], "temperature"), &Value::Float(34.0));
        assert_eq!(state(&disks[0], "problem"), &Value::Bool(false));
        assert_eq!(&*disks[0].device.native_name, "Drive 1");
        assert_eq!(state(&disks[1], "problem"), &Value::Bool(true));
        assert!(matches!(state(&disks[1], "detail"), Value::Text(t)
            if t.contains("warning") && t.contains("failing")));
        // A disk past its wear threshold is a problem even with a normal status.
        assert_eq!(state(&disks[2], "problem"), &Value::Bool(true));
    }

    #[test]
    fn the_ups_is_a_problem_on_battery_and_absent_when_not_set_up() {
        let on_mains = ups(
            &instance(),
            &json!({"enable":true,"status":"OL","charge":100}),
        )
        .unwrap();
        assert_eq!(state(&on_mains, "battery"), &Value::Float(100.0));
        assert_eq!(state(&on_mains, "problem"), &Value::Bool(false));
        let on_battery = ups(
            &instance(),
            &json!({"enable":true,"status":"OB DISCHRG","charge":"87"}),
        )
        .unwrap();
        assert_eq!(state(&on_battery, "battery"), &Value::Float(87.0));
        assert_eq!(state(&on_battery, "problem"), &Value::Bool(true));
        assert!(ups(&instance(), &json!({"enable":false})).is_none());
        assert!(ups(&instance(), &json!({})).is_none());
        assert!(ups(&instance(), &Json::Null).is_none());
    }

    #[test]
    fn a_failed_backup_task_is_a_problem() {
        let tasks = fixture(include_str!(
            "../../../integrations/synology/fixtures/backup-faulty.json"
        ));
        let backups = backups(&instance(), &tasks);
        assert_eq!(backups.len(), 2);
        assert_eq!(&*backups[0].device.id.to_string(), "nas:backup_1");
        assert_eq!(state(&backups[0], "problem"), &Value::Bool(false));
        assert_eq!(
            state(&backups[0], "last_run"),
            &Value::from("2026-11-14 02:00")
        );
        assert_eq!(state(&backups[1], "problem"), &Value::Bool(true));
        assert!(matches!(state(&backups[1], "detail"), Value::Text(t) if t.contains("failed")));
    }

    #[test]
    fn nothing_readable_gives_no_device() {
        assert!(volumes(&instance(), &Json::Null).is_empty());
        assert!(disks(&instance(), &json!({"disks":[{"temp":30}]})).is_empty());
        assert!(backups(&instance(), &json!({"task_list":[{"name":"x"}]})).is_empty());
    }

    #[test]
    fn percentages_stay_within_range_and_odd_names_are_left_out() {
        let storage = json!({
            "volumes": [
                {"id": "volume_1", "status": "normal", "size": {"total": "100", "used": "250"}},
                {"id": "volume_2\u{7}", "size": {"total": "100", "used": "10"}}
            ],
            "disks": [
                {"id": "x".repeat(81), "name": "Disk"},
                {"id": "sda", "name": "Disk\n1"}
            ]
        });
        let instance = instance();
        let volumes = volumes(&instance, &storage);
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].states[0], ("used", Value::Float(100.0)));
        assert_eq!(volumes[0].states[1], ("free", Value::Float(0.0)));
        let disks = disks(&instance, &storage);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].device.id.to_string(), "nas:disk_sda");
        let down = nas_states(&json!({}), &json!({"memory": {"real_usage": 140}}));
        assert!(down.contains(&("memory", Value::Float(100.0))));
    }
}

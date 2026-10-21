//! What a push says, as Moli points: which device, which detection.
//!
//! Event codes from eufy-security-client (`CusPushEvent`, `IndoorPushEvent`,
//! `DoorbellPushEvent`): the HomeBase's cameras say `a = 1` (a security
//! event; a person when a face id `i` comes along), Wi-Fi cameras and
//! doorbells say `event_type = 31xx`. They do not overlap.

use serde_json::Value as Json;

use crate::push::Push;

/// A detection point and its label key.
pub const DETECTIONS: [(&str, &str); 7] = [
    ("motion", "pilotes.eufy.mouvement"),
    ("person", "pilotes.eufy.personne"),
    ("doorbell", "pilotes.eufy.sonnette"),
    ("animal", "pilotes.eufy.animal"),
    ("vehicle", "pilotes.eufy.vehicule"),
    ("sound", "pilotes.eufy.son"),
    ("crying", "pilotes.eufy.pleurs"),
];

const SECURITY: i64 = 1;
const MODE_SWITCH: i64 = 9;

/// The detections a model can report, from its model number.
pub fn detections(model: &str) -> &'static [&'static str] {
    let model = model.to_ascii_uppercase();
    if model.starts_with("T820") || model.starts_with("T821") || model.starts_with("T822") {
        &["motion", "person", "doorbell", "animal", "vehicle"]
    } else if model.starts_with("T840") || model.starts_with("T841") {
        &["motion", "person", "animal", "sound", "crying"]
    } else if model.starts_with("T84") || model.starts_with("T81") || model.starts_with("T816") {
        &["motion", "person"]
    } else {
        &[]
    }
}

/// Wired devices: a « battery » parameter there means nothing.
pub fn wired(model: &str) -> bool {
    let model = model.to_ascii_uppercase();
    ["T84A", "T840", "T841", "T842"]
        .iter()
        .any(|p| model.starts_with(p))
}

/// What one push changes.
#[derive(Debug, PartialEq)]
pub enum Change {
    /// A detection just happened on this device (held a few seconds).
    Detected { serial: String, key: &'static str },
    /// The station's guard mode is now this one.
    Mode { serial: String, mode: i64 },
}

fn number(value: &Json) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

pub fn read(push: &Push) -> Vec<Change> {
    let p = &push.payload;
    let serial = if push.device_sn.is_empty() {
        push.station_sn.clone()
    } else {
        push.device_sn.clone()
    };
    if serial.is_empty() {
        return Vec::new();
    }
    let code = number(&p["a"]).or_else(|| number(&p["event_type"]));
    let detected = |keys: &[&'static str]| {
        keys.iter()
            .map(|key| Change::Detected {
                serial: serial.clone(),
                key,
            })
            .collect()
    };
    match code {
        Some(SECURITY) if p.get("i").is_some_and(|i| !i.is_null()) => {
            detected(&["motion", "person"])
        }
        Some(MODE_SWITCH) => {
            let station = if push.station_sn.is_empty() {
                serial.clone()
            } else {
                push.station_sn.clone()
            };
            number(&p["arming"])
                .map(|mode| {
                    vec![Change::Mode {
                        serial: station,
                        mode,
                    }]
                })
                .unwrap_or_default()
        }
        Some(SECURITY | 3101) => detected(&["motion"]),
        Some(3102) => detected(&["person", "motion"]),
        Some(3103) => detected(&["doorbell"]),
        Some(3104) => detected(&["crying", "sound"]),
        Some(3105) => detected(&["sound"]),
        Some(3106) => detected(&["animal", "motion"]),
        Some(3107) => detected(&["vehicle", "motion"]),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn push(device: &str, payload: Json) -> Push {
        Push {
            device_type: 1,
            device_sn: device.into(),
            station_sn: "T8010P0000000000".into(),
            payload,
        }
    }

    fn keys(changes: &[Change]) -> Vec<&'static str> {
        changes
            .iter()
            .filter_map(|c| match c {
                Change::Detected { key, .. } => Some(*key),
                Change::Mode { .. } => None,
            })
            .collect()
    }

    #[test]
    fn a_homebase_camera_says_motion_and_sometimes_a_person() {
        assert_eq!(
            keys(&read(&push("T8111P0000000000", json!({ "a": 1 })))),
            ["motion"]
        );
        assert_eq!(
            keys(&read(&push(
                "T8111P0000000000",
                json!({ "a": 1, "i": "12" })
            ))),
            ["motion", "person"]
        );
    }

    #[test]
    fn wifi_cameras_and_doorbells_say_their_event_type() {
        let ring = read(&push("T8210P0000000000", json!({ "event_type": 3103 })));
        assert_eq!(
            ring,
            [Change::Detected {
                serial: "T8210P0000000000".into(),
                key: "doorbell"
            }]
        );
        assert_eq!(
            keys(&read(&push(
                "T8410P0000000000",
                json!({ "event_type": "3102" })
            ))),
            ["person", "motion"]
        );
        assert!(read(&push("T8410P0000000000", json!({ "event_type": 4242 }))).is_empty());
    }

    #[test]
    fn a_mode_switch_names_the_station() {
        assert_eq!(
            read(&push("", json!({ "a": 9, "arming": 63 }))),
            [Change::Mode {
                serial: "T8010P0000000000".into(),
                mode: 63
            }]
        );
    }

    #[test]
    fn models_say_what_they_detect() {
        assert!(detections("T8210").contains(&"doorbell"));
        assert!(detections("T8410").contains(&"crying"));
        assert_eq!(detections("T8111"), ["motion", "person"]);
        assert_eq!(detections("T84A1"), ["motion", "person"]);
        assert!(detections("T8010").is_empty());
        assert!(wired("T84A1") && wired("t8410") && !wired("T8111"));
    }
}

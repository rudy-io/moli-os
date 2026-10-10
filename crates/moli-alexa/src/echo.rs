//! Which of the account's devices are speakers Moli can drive (Echo, Echo
//! Dot, Show, multi-room groups), and how each shows in the house: the same
//! points as any speaker (`playing`, `volume`, `title`, `artist`) plus what an
//! Echo adds (play a search, say, announce, an Alexa command).

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit};
use moli_runtime::DriverCtx;
use serde_json::Value as Json;

use crate::auth::DEVICE_TYPE;

/// A speaker or a group of speakers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Echo {
    pub serial: String,
    pub device_type: String,
    /// The name given in the Alexa app.
    pub name: String,
    /// `ECHO`, `KNIGHT` (Show), `ROOK` (Spot), `WHA` (group)…
    pub family: String,
    /// A multi-room music group (its members play together).
    pub group: bool,
    /// A group's members (their serials).
    pub members: Vec<String>,
    pub online: bool,
}

/// Apps (iOS, Android, PC, Moli itself) and TV sticks are not speakers.
const NOT_SPEAKERS: [&str; 3] = [DEVICE_TYPE, "A2TF17PFR55MTB", "A1RTAM01W29CUP"];
/// Multi-room groups (`WHA`): « Speaker Group ».
const GROUP_TYPE: &str = "A3C9PE6TNYLTCH";

/// The speakers and groups in a `devices-v2` answer, in its order.
pub(crate) fn speakers(answer: &Json) -> Vec<Echo> {
    answer["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let serial = d["serialNumber"].as_str()?.to_owned();
            let device_type = d["deviceType"].as_str()?.to_owned();
            let family = d["deviceFamily"].as_str().unwrap_or_default();
            let caps: Vec<&str> = d["capabilities"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Json::as_str)
                .collect();
            let members: Vec<String> = d["clusterMembers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str().map(str::to_owned))
                .collect();
            let group = (family == "WHA" || device_type == GROUP_TYPE) && !members.is_empty();
            let speaker = caps.contains(&"AUDIO_PLAYER")
                && !NOT_SPEAKERS.contains(&device_type.as_str())
                && family != "FIRE_TV";
            (group || speaker).then(|| Echo {
                name: d["accountName"].as_str().unwrap_or(&serial).to_owned(),
                family: family.to_owned(),
                serial,
                device_type,
                group,
                members,
                online: d["online"].as_bool().unwrap_or(true),
            })
        })
        .collect()
}

fn point(key: &str, label: &str, kind: Kind, write: bool, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic: Semantic::infer(key, None),
    }
}

/// How `echo` shows in the house.
pub(crate) fn device(ctx: &DriverCtx, id: &DeviceId, echo: &Echo) -> Device {
    let mut points = vec![
        point(
            "playing",
            &moli_i18n::tr!("pilotes.alexa.lecture"),
            Kind::Binary,
            true,
            None,
        ),
        point(
            "state",
            &moli_i18n::tr!("pilotes.alexa.etat"),
            Kind::Text,
            false,
            None,
        ),
        point(
            "title",
            &moli_i18n::tr!("pilotes.alexa.titre"),
            Kind::Text,
            false,
            None,
        ),
        point(
            "artist",
            &moli_i18n::tr!("pilotes.alexa.artiste"),
            Kind::Text,
            false,
            None,
        ),
        point(
            "play",
            &moli_i18n::tr!("pilotes.alexa.jouer"),
            Kind::Text,
            true,
            None,
        ),
        point(
            "say",
            &moli_i18n::tr!("pilotes.alexa.dire"),
            Kind::Text,
            true,
            None,
        ),
        point(
            "announce",
            &moli_i18n::tr!("pilotes.alexa.annoncer"),
            Kind::Text,
            true,
            None,
        ),
        point(
            "command",
            &moli_i18n::tr!("pilotes.alexa.commande"),
            Kind::Text,
            true,
            None,
        ),
    ];
    // A group has no volume of its own: its members have.
    if !echo.group {
        points.insert(
            1,
            point(
                "volume",
                &moli_i18n::tr!("pilotes.alexa.volume"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                true,
                Some(Unit::Percent),
            ),
        );
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: echo.name.clone().into(),
        manufacturer: Some("Amazon".into()),
        model: Some(if echo.group {
            moli_i18n::tr!("pilotes.alexa.groupe").into()
        } else {
            "Echo".into()
        }),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn speakers_and_groups_but_not_apps_or_tvs() {
        let answer = json!({ "devices": [
            { "serialNumber": "G0911", "deviceType": "A3S5BH2HU6VAYF", "deviceFamily": "ECHO",
              "accountName": "Echo du salon", "online": true, "capabilities": ["AUDIO_PLAYER", "MICROPHONE"] },
            { "serialNumber": "PHONE", "deviceType": "A2IVLV5VM2W81", "deviceFamily": "VOX",
              "accountName": "Moli", "capabilities": ["AUDIO_PLAYER"] },
            { "serialNumber": "TV", "deviceType": "A2GFL5ZMWNE0PX", "deviceFamily": "FIRE_TV",
              "accountName": "Fire TV", "capabilities": ["AUDIO_PLAYER"] },
            { "serialNumber": "GRP", "deviceType": "A3C9PE6TNYLTCH", "deviceFamily": "WHA",
              "accountName": "Partout", "clusterMembers": ["G0911", "G0922"], "capabilities": [] },
            { "serialNumber": "PLUG", "deviceType": "A1", "deviceFamily": "SMART_PLUG", "capabilities": [] }
        ]});
        let found = speakers(&answer);
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Echo du salon", "Partout"]);
        assert!(!found[0].group && found[0].online);
        assert!(found[1].group);
        assert_eq!(found[1].members, ["G0911", "G0922"]);
        assert!(speakers(&json!({})).is_empty());
    }
}

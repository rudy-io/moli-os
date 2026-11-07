//! Validated decoding and scope-aware command construction.
use crate::fields::{self, Scope};
use anyhow::{Context as _, ensure};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;

pub(crate) type Entity = (Device, Vec<(String, Value)>);
pub(crate) type ZoneIds = BTreeSet<(u64, u64)>;
#[derive(Debug)]
pub(crate) struct Snapshot {
    zones: Vec<Zone>,
    identities: ZoneIds,
    incomplete_systems: BTreeSet<u64>,
}
#[derive(Debug)]
struct Zone {
    raw: Json,
    system: u64,
    id: u64,
    master: Option<u64>,
    shared: bool,
}
fn invalid() -> String {
    moli_i18n::tr!("airzone.invalid")
}
fn number(v: &Json) -> anyhow::Result<f64> {
    v.as_f64().filter(|n| n.is_finite()).context(invalid())
}
fn code(v: &Json, min: u64, max: u64) -> anyhow::Result<u64> {
    v.as_u64()
        .filter(|n| (min..=max).contains(n))
        .context(invalid())
}
fn binary(v: &Json) -> anyhow::Result<bool> {
    match v {
        Json::Bool(b) => Ok(*b),
        _ => Ok(code(v, 0, 1)? == 1),
    }
}
fn codes(v: &Json, min: u64, max: u64) -> anyhow::Result<Vec<u64>> {
    let a = v
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 32)
        .context(invalid())?;
    let values = a
        .iter()
        .map(|v| code(v, min, max))
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        values.iter().collect::<BTreeSet<_>>().len() == values.len(),
        invalid()
    );
    Ok(values)
}
impl Zone {
    fn parse(raw: &Json, system: u64, id: u64) -> anyhow::Result<Self> {
        let raw = fields::project(raw)?;
        // Invalid core measurements still isolate the zone. Absent thermostat
        // fields are legitimate on ERV and double-setpoint-only controllers.
        if let Some(value) = raw.get("on") {
            binary(value)?;
        }
        if let Some(value) = raw.get("roomTemp") {
            number(value)?;
        }
        if let Some(value) = raw.get("units") {
            code(value, 0, 1)?;
        }
        if let Some(value) = raw.get("mode") {
            code(value, 1, 7)?;
        }
        if let Some(value) = raw.get("setpoint") {
            let target = number(value)?;
            if let (Some(min), Some(max)) = (raw.get("minTemp"), raw.get("maxTemp")) {
                let min = number(min)?;
                let max = number(max)?;
                ensure!(min < max && target >= min && target <= max, invalid());
            }
        }
        if let Some(value) = raw.get("temp_step") {
            let step = number(value)?;
            ensure!(step > 0., invalid());
            if let (Some(min), Some(max)) = (raw["minTemp"].as_f64(), raw["maxTemp"].as_f64()) {
                ensure!(step <= max - min, invalid());
            }
        }
        if let Some(value) = raw.get("modes") {
            codes(value, 1, 7)?;
        }
        if let Some(value) = raw.get("erv_modes") {
            codes(value, 0, 10)?;
        }
        let master = raw
            .get("master_zoneID")
            .map(|v| code(v, 1, 32))
            .transpose()?;
        if let Some(errors) = raw.get("errors") {
            ensure!(errors.is_array(), invalid());
        }
        let shared = raw.get("modes").is_some() || raw.get("erv_modes").is_some();
        Ok(Self {
            raw,
            system,
            id,
            master,
            shared,
        })
    }
    fn native(&self, mac: &str, control: bool) -> String {
        format!(
            "{mac}_s{}_z{}{}",
            self.system,
            self.id,
            if control { "_control" } else { "" }
        )
    }
    fn entity(&self, instance: &InstanceId, mac: &str, members: Option<Vec<DeviceId>>) -> Entity {
        let control = members.is_some();
        let values = fields::points(
            if control { Scope::System } else { Scope::Zone },
            &self.raw,
            false,
        );
        let (mut points, mut states): (Vec<_>, Vec<_>) = values
            .into_iter()
            .map(|(p, v)| {
                let key = p.key.to_string();
                (p, (key, v))
            })
            .unzip();
        if !control {
            let errors = self.raw["errors"].as_array();
            points.push(PointSpec {
                key: "problem".into(),
                label: moli_i18n::tr!("airzone.problem").into(),
                kind: Kind::Binary,
                unit: None,
                semantic: Semantic::Alert,
                access: Access {
                    read: true,
                    write: false,
                },
            });
            states.push((
                "problem".into(),
                Value::Bool(errors.is_some_and(|e| !e.is_empty())),
            ));
            if let Some(errors) = errors {
                let detail = errors
                    .iter()
                    .take(8)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(240)
                    .collect::<String>();
                points.push(PointSpec {
                    key: "detail".into(),
                    label: moli_i18n::tr!("airzone.detail").into(),
                    kind: Kind::Text,
                    unit: None,
                    semantic: Semantic::Other,
                    access: Access {
                        read: true,
                        write: false,
                    },
                });
                states.push(("detail".into(), Value::Text(detail.into())));
            }
        }
        let name = self.raw["name"]
            .as_str()
            .unwrap_or("Airzone")
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect::<String>();
        let native_name = if control {
            moli_i18n::tr!("airzone.control", name = name)
        } else {
            name
        };
        (
            Device {
                id: DeviceId::new(instance, &self.native(mac, control)),
                instance: instance.clone(),
                native_name: native_name.into(),
                manufacturer: Some("Airzone".into()),
                model: Some(
                    if control {
                        "Airzone system"
                    } else {
                        "Local API"
                    }
                    .into(),
                ),
                description: None,
                native_room: None,
                members: members.unwrap_or_default(),
                points,
            },
            states,
        )
    }
}
impl Snapshot {
    pub(crate) fn parse(value: &Json) -> anyhow::Result<Self> {
        let nested = value.get("systems").is_some();
        let groups = if let Some(systems) = value.get("systems") {
            systems
                .as_array()
                .filter(|s| s.len() <= 32)
                .context(invalid())?
                .iter()
                .collect::<Vec<_>>()
        } else {
            let data = value.get("data").context(invalid())?;
            ensure!(
                data.is_object() || data.as_array().is_some_and(|a| !a.is_empty()),
                invalid()
            );
            vec![value]
        };
        let mut zones = vec![];
        let mut ids = BTreeSet::new();
        let mut incomplete_systems = BTreeSet::new();
        let mut skipped = 0;
        for group in groups {
            let data = match &group["data"] {
                Json::Object(_) => vec![&group["data"]],
                Json::Array(rows) if rows.len() <= if nested { 32 } else { 1024 } => {
                    rows.iter().collect()
                }
                _ => anyhow::bail!(invalid()),
            };
            let mut system_id = if nested {
                group.get("systemID").map(|v| code(v, 1, 32)).transpose()?
            } else {
                None
            };
            for raw in data {
                let system = code(&raw["systemID"], 1, 32)?;
                let id = code(&raw["zoneID"], 1, 32)?;
                if nested {
                    ensure!(system_id.is_none_or(|s| s == system), invalid());
                    system_id = Some(system);
                }
                ensure!(ids.insert((system, id)), invalid());
                if let Ok(zone) = Zone::parse(raw, system, id) {
                    zones.push(zone);
                } else {
                    incomplete_systems.insert(system);
                    skipped += 1;
                }
            }
        }
        if skipped > 0 {
            tracing::warn!(
                skipped_zones = skipped,
                "malformed Airzone zones ignored; shared controls disabled for affected systems"
            );
        }
        Ok(Self {
            zones,
            identities: ids,
            incomplete_systems,
        })
    }
    pub(crate) fn system_ids(&self) -> Vec<u64> {
        self.identities
            .iter()
            .map(|(system, _)| *system)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    /// Counts validated zones, keeping an isolated malformed row visible as
    /// incomplete rather than presenting a reduced inventory as complete.
    pub(crate) fn inventory_summary(&self) -> (usize, usize, bool) {
        (
            self.system_ids().len(),
            self.zones.len(),
            !self.incomplete_systems.is_empty(),
        )
    }
    pub(crate) fn scope_members(
        &self,
        instance: &InstanceId,
        mac: &str,
        system: Option<u64>,
    ) -> Option<Vec<DeviceId>> {
        if self
            .incomplete_systems
            .iter()
            .any(|s| system.is_none_or(|wanted| wanted == *s))
        {
            return None;
        }
        let members = self
            .zones
            .iter()
            .filter(|zone| system.is_none_or(|wanted| wanted == zone.system))
            .map(|zone| DeviceId::new(instance, &zone.native(mac, false)))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        (!members.is_empty()).then_some(members)
    }
    pub(crate) fn retain_known_scope(
        &mut self,
        instance: &InstanceId,
        mac: &str,
        published: &[DeviceId],
        known: &mut ZoneIds,
    ) {
        let prefix = format!("{instance}:{mac}_s");
        for device in published {
            let Some((system, zone)) = device
                .as_str()
                .strip_prefix(&prefix)
                .and_then(|n| n.split_once("_z"))
                .and_then(|(s, z)| Some((s.parse::<u64>().ok()?, z.parse::<u64>().ok()?)))
            else {
                continue;
            };
            if (1..=32).contains(&system) && (1..=32).contains(&zone) {
                known.insert((system, zone));
            }
        }
        known.extend(&self.identities);
        self.incomplete_systems.extend(
            known
                .difference(&self.identities)
                .map(|(system, _)| *system),
        );
    }
    fn members(&self, master: &Zone, instance: &InstanceId, mac: &str) -> Option<Vec<DeviceId>> {
        if self.incomplete_systems.contains(&master.system) {
            return None;
        }
        let system = self
            .zones
            .iter()
            .filter(|z| z.system == master.system)
            .collect::<Vec<_>>();
        let masters = system
            .iter()
            .filter(|z| z.shared)
            .map(|z| z.id)
            .collect::<Vec<_>>();
        if !master.shared {
            // Some systems advertise shared settings without a mode list.
            // Use every zone, never infer a narrower domain or a fake master.
            return (masters.is_empty()
                && fields::points(Scope::System, &master.raw, false)
                    .iter()
                    .any(|(point, _)| point.access.write))
            .then(|| self.scope_members(instance, mac, Some(master.system)))
            .flatten();
        }
        if system
            .iter()
            .any(|z| z.master.is_some_and(|m| !masters.contains(&m)))
            || (masters.len() > 1 && system.iter().any(|z| z.master.is_none()))
        {
            return None;
        }
        let members = system
            .iter()
            .filter(|z| z.master.map_or(masters.len() == 1, |m| m == master.id))
            .map(|z| DeviceId::new(instance, &z.native(mac, false)))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if members.is_empty()
            || !members.contains(&DeviceId::new(instance, &master.native(mac, false)))
        {
            return None;
        }
        Some(members)
    }
    pub(crate) fn entities(&self, instance: &InstanceId, mac: &str) -> Vec<Entity> {
        let mut es = self
            .zones
            .iter()
            .map(|z| z.entity(instance, mac, None))
            .collect::<Vec<_>>();
        for z in &self.zones {
            if let Some(members) = self.members(z, instance, mac) {
                es.push(z.entity(instance, mac, Some(members)));
            }
        }
        es
    }
    pub(crate) fn encode(
        &self,
        instance: &InstanceId,
        mac: &str,
        device: &DeviceId,
        key: &str,
        value: &Value,
    ) -> anyhow::Result<Json> {
        let entities = self.entities(instance, mac);
        let (d, _) = entities
            .iter()
            .find(|(d, _)| &d.id == device)
            .context(invalid())?;
        ensure!(d.point(key).is_some_and(|p| p.access.write), invalid());
        let control = !d.members.is_empty();
        let zone = self
            .zones
            .iter()
            .find(|z| DeviceId::new(instance, &z.native(mac, control)) == *device)
            .context(invalid())?;
        let (param, wire) = fields::encode(
            if control { Scope::System } else { Scope::Zone },
            &zone.raw,
            key,
            value,
        )?;
        let mut body = json!({"systemID":zone.system,"zoneID":zone.id});
        body[param] = wire;
        Ok(body)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use moli_core::{Kind, Unit};
    fn fixture() -> Json {
        serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap()
    }
    #[test]
    fn flat_broadcast_covers_multiple_systems_but_nested_parent_ids_must_match() {
        let flat =
            json!({"data":[{"systemID":1,"zoneID":1,"on":1},{"systemID":2,"zoneID":1,"on":0}]});
        assert_eq!(
            Snapshot::parse(&flat)
                .unwrap()
                .entities(&InstanceId::from("az"), "aabbccddeeff")
                .len(),
            2
        );
        assert!(
            Snapshot::parse(
                &json!({"systems":[{"systemID":2,"data":[{"systemID":1,"zoneID":1,"on":1}]}]})
            )
            .is_err()
        );
    }
    #[test]
    fn invalid_optional_humidity_does_not_remove_an_independent_zone() {
        let snapshot =
            Snapshot::parse(&json!({"data":{"systemID":1,"zoneID":1,"on":1,"humidity":null}}))
                .unwrap();
        let entities = snapshot.entities(&InstanceId::from("az"), "aabbccddeeff");
        assert_eq!(entities.len(), 1);
        assert!(entities[0].0.point("on").is_some());
        assert!(entities[0].0.point("humidity").is_none());
    }
    #[test]
    fn independently_advertised_shared_capabilities_use_whole_system_scope() {
        let snapshot = Snapshot::parse(&json!({"data":[
            {"systemID":1,"zoneID":1,"on":1,"speed":1,"speed_values":[0,1],"qAdapt":1},
            {"systemID":1,"zoneID":2,"on":0}
        ]}))
        .unwrap();
        let instance = InstanceId::from("az");
        let mac = "aabbccddeeff";
        let entities = snapshot.entities(&instance, mac);
        let control = entities
            .iter()
            .find(|(d, _)| d.id.as_str().ends_with("_control"))
            .unwrap();
        assert_eq!(control.0.members.len(), 2);
        assert_eq!(
            snapshot
                .encode(
                    &instance,
                    mac,
                    &control.0.id,
                    "fan_speed",
                    &Value::from("1")
                )
                .unwrap(),
            json!({"systemID":1,"zoneID":1,"speed":1})
        );
    }
    #[test]
    fn accepts_documented_single_system_object_and_array_envelopes() {
        let raw = fixture()["systems"][0]["data"][0].clone();
        for response in [json!({"data": raw}), json!({"data": [raw]})] {
            let snapshot = Snapshot::parse(&response).unwrap();
            let devices = snapshot.entities(&InstanceId::from("az"), "aabbccddeeff");
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].0.id.as_str(), "az:aabbccddeeff_s1_z2");
        }
    }
    #[test]
    fn double_setpoint_only_zone_keeps_both_targets_without_fake_single_target() {
        let raw = json!({"systemID":1,"zoneID":4,"on":1,"roomTemp":22,
            "double_sp":1,"coolsetpoint":25,"coolmintemp":16,"coolmaxtemp":32,
            "heatsetpoint":20,"heatmintemp":15,"heatmaxtemp":30,
            "units":0,"temp_step":0.5,"mode":7,"modes":[1,3,7]});
        let snapshot = Snapshot::parse(&json!({"systems":[{"data":[raw]}]})).unwrap();
        let devices = snapshot.entities(&InstanceId::from("az"), "aabbccddeeff");
        let (device, _) = devices
            .iter()
            .find(|(d, _)| d.id.as_str().ends_with("_z4"))
            .unwrap();
        assert!(device.point("target_temperature").is_none());
        assert!(
            device
                .points
                .iter()
                .any(|p| p.access.write && p.key.as_ref().contains("cool"))
        );
        assert!(
            device
                .points
                .iter()
                .any(|p| p.access.write && p.key.as_ref().contains("heat"))
        );
    }
    #[test]
    fn erv_only_zone_is_published_without_fake_thermostat() {
        let snapshot = Snapshot::parse(&json!({"systems":[{"data":[{
            "systemID":1,"zoneID":1,"erv_power":1,"erv_modes":[0,1,2,6],
            "erv_mode":2,"erv_speed_values":[1,3,5],"erv_speed":3
        }]}]}))
        .unwrap();
        let devices = snapshot.entities(&InstanceId::from("az"), "aabbccddeeff");
        assert_eq!(devices.len(), 2);
        assert!(
            devices
                .iter()
                .all(|(d, _)| d.point("target_temperature").is_none())
        );
        assert!(
            devices
                .iter()
                .any(|(d, _)| d.point("erv_mode").is_some_and(|p| p.access.write))
        );
    }
    #[test]
    fn controller_identity_scope_and_reported_capabilities() {
        let s = Snapshot::parse(&fixture()).unwrap();
        let es = s.entities(&InstanceId::from("az"), "aabbccddeeff");
        assert_eq!(es.len(), 5);
        let (zone, states) = &es[0];
        assert_eq!(zone.id.as_str(), "az:aabbccddeeff_s1_z2");
        assert_eq!(
            zone.point("target_temperature").unwrap().kind,
            Kind::Numeric {
                min: Some(15.),
                max: Some(30.),
                step: Some(0.5)
            }
        );
        assert!(!zone.point("mode").unwrap().access.write);
        assert!(zone.members.is_empty());
        assert!(states.contains(&("target_temperature".into(), Value::Float(22.3))));
        let control = es
            .iter()
            .find(|(d, _)| d.id.as_str() == "az:aabbccddeeff_s1_z8_control")
            .unwrap();
        assert_eq!(
            control
                .0
                .members
                .iter()
                .map(DeviceId::as_str)
                .collect::<Vec<_>>(),
            vec!["az:aabbccddeeff_s1_z2", "az:aabbccddeeff_s1_z8"]
        );
        assert!(control.0.point("on").is_none());
        assert!(control.0.point("target_temperature").is_none());
        // A reported speed remains visible even when no writable fan
        // capability was advertised (e.g. radiant-only systems).
        assert!(!control.0.point("fan_speed").unwrap().access.write);
        assert_eq!(
            control.0.point("mode").unwrap().kind,
            Kind::Enum {
                values: vec!["off".into(), "heat".into()]
            }
        );
        let f = es
            .iter()
            .find(|(d, _)| d.id.as_str() == "az:aabbccddeeff_s2_z8")
            .unwrap();
        assert_eq!(
            f.0.point("temperature").unwrap().unit,
            Some(Unit::Other("°F".into()))
        );
    }
    #[test]
    fn commands_are_individually_addressed_and_capability_checked() {
        let snapshot = Snapshot::parse(&fixture()).unwrap();
        let instance = InstanceId::from("az");
        let mac = "aabbccddeeff";
        let zone = DeviceId::from("az:aabbccddeeff_s1_z2");
        let control = DeviceId::from("az:aabbccddeeff_s1_z8_control");
        assert_eq!(
            snapshot
                .encode(&instance, mac, &zone, "on", &Value::Bool(false))
                .unwrap(),
            serde_json::json!({"systemID":1,"zoneID":2,"on":0})
        );
        assert_eq!(
            snapshot
                .encode(
                    &instance,
                    mac,
                    &zone,
                    "target_temperature",
                    &Value::Float(21.5)
                )
                .unwrap(),
            serde_json::json!({"systemID":1,"zoneID":2,"setpoint":21.5})
        );
        assert_eq!(
            snapshot
                .encode(&instance, mac, &control, "mode", &Value::from("heat"))
                .unwrap(),
            serde_json::json!({"systemID":1,"zoneID":8,"mode":3})
        );
        assert!(
            snapshot
                .encode(&instance, mac, &zone, "mode", &Value::from("heat"))
                .is_err()
        );
        assert!(
            snapshot
                .encode(&instance, mac, &control, "mode", &Value::from("cool"))
                .is_err()
        );
        assert!(
            snapshot
                .encode(
                    &instance,
                    mac,
                    &zone,
                    "target_temperature",
                    &Value::Float(31.)
                )
                .is_err()
        );
        assert!(
            snapshot
                .encode(
                    &instance,
                    mac,
                    &zone,
                    "target_temperature",
                    &Value::Float(21.3)
                )
                .is_err()
        );
        assert!(
            snapshot
                .encode(
                    &instance,
                    mac,
                    &DeviceId::from("az:unknown"),
                    "on",
                    &Value::Bool(true)
                )
                .is_err()
        );
        let fan = DeviceId::from("az:aabbccddeeff_s2_z8_control");
        assert_eq!(
            snapshot
                .encode(&instance, mac, &fan, "fan_speed", &Value::from("2"))
                .unwrap(),
            serde_json::json!({"systemID":2,"zoneID":8,"speed":2})
        );
        assert!(
            snapshot
                .encode(&instance, mac, &fan, "fan_speed", &Value::from("1"))
                .is_err()
        );
    }
    #[test]
    fn malformed_zones_are_isolated_without_shrinking_shared_control_scope() {
        for (key, bad) in [
            ("on", serde_json::json!(2)),
            ("minTemp", serde_json::json!(35)),
            ("units", serde_json::json!(7)),
            ("temp_step", serde_json::json!(0)),
            ("roomTemp", Json::Null),
        ] {
            let mut v = fixture();
            v["systems"][0]["data"][0][key] = bad;
            let snapshot = Snapshot::parse(&v).unwrap();
            let instance = InstanceId::from("az");
            let mac = "aabbccddeeff";
            let entities = snapshot.entities(&instance, mac);
            assert_eq!(
                entities
                    .iter()
                    .map(|(d, _)| d.id.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "az:aabbccddeeff_s1_z8",
                    "az:aabbccddeeff_s2_z8",
                    "az:aabbccddeeff_s2_z8_control"
                ],
                "{key}"
            );
            assert!(
                snapshot
                    .encode(
                        &instance,
                        mac,
                        &DeviceId::from("az:aabbccddeeff_s1_z2"),
                        "on",
                        &Value::Bool(false)
                    )
                    .is_err()
            );
            assert!(
                snapshot
                    .encode(
                        &instance,
                        mac,
                        &DeviceId::from("az:aabbccddeeff_s1_z8_control"),
                        "mode",
                        &Value::from("heat")
                    )
                    .is_err()
            );
            assert_eq!(
                snapshot
                    .encode(
                        &instance,
                        mac,
                        &DeviceId::from("az:aabbccddeeff_s2_z8_control"),
                        "mode",
                        &Value::from("heat")
                    )
                    .unwrap(),
                json!({"systemID":2,"zoneID":8,"mode":3})
            );
        }
    }
    #[test]
    fn malformed_envelopes_and_ambiguous_identities_fail_closed() {
        assert!(Snapshot::parse(&serde_json::json!({"data":[]})).is_err());
        for key in ["systemID", "zoneID"] {
            for bad in [json!(0), json!(33), Json::Null, json!("2")] {
                let mut v = fixture();
                v["systems"][0]["data"][0][key] = bad;
                v["systems"][0]["data"][0]["roomTemp"] = Json::Null;
                assert!(Snapshot::parse(&v).is_err(), "{key}");
            }
        }
        let mut v = fixture();
        v["systems"][0]["data"][0]["systemID"] = json!(3);
        v["systems"][0]["data"][0]["roomTemp"] = Json::Null;
        assert!(Snapshot::parse(&v).is_err());
        let mut v = fixture();
        v["systems"][0]["data"] = Json::Null;
        assert!(Snapshot::parse(&v).is_err());
        let mut v = fixture();
        let z = v["systems"][0]["data"][0].clone();
        // The malformed copy must still participate in identity validation.
        v["systems"][0]["data"][0]["roomTemp"] = Json::Null;
        v["systems"][0]["data"].as_array_mut().unwrap().push(z);
        assert!(Snapshot::parse(&v).is_err());
    }
    #[test]
    fn ambiguous_master_domains_have_no_writable_control() {
        let mut v = fixture();
        v["systems"][0]["data"][0]["modes"] = serde_json::json!([1, 3]);
        v["systems"][0]["data"][0]
            .as_object_mut()
            .unwrap()
            .remove("master_zoneID");
        let es = Snapshot::parse(&v)
            .unwrap()
            .entities(&InstanceId::from("az"), "aabbccddeeff");
        assert!(!es.iter().any(|(d,_)|d.id.as_str().contains("_s1_") && d.id.as_str().ends_with("_control")));
    }
}

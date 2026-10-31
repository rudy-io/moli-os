//! Pure, strict decoding and scope-aware command construction.
use anyhow::{Context as _, ensure};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;

pub(crate) type Entity = (Device, Vec<(String, Value)>);
#[derive(Debug)]
pub(crate) struct Snapshot {
    zones: Vec<Zone>,
}
#[derive(Debug)]
struct Zone {
    raw: Json,
    system: u64,
    id: u64,
    master: Option<u64>,
    modes: Option<Vec<u64>>,
    fan: Vec<u64>,
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
        .filter(|a| !a.is_empty() && a.len() <= 8)
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
fn text(value: &Value) -> anyhow::Result<&str> {
    if let Value::Text(s) = value {
        Ok(s)
    } else {
        anyhow::bail!(invalid())
    }
}
fn mode(code: u64) -> &'static str {
    match code {
        1 => "off",
        2 => "cool",
        3 => "heat",
        4 => "fan",
        5 => "dry",
        6 => "aux_heat",
        7 => "auto",
        _ => unreachable!("validated mode"),
    }
}
fn numeric(min: Option<f64>, max: Option<f64>, step: Option<f64>) -> Kind {
    Kind::Numeric { min, max, step }
}
fn spec(key: &str, kind: Kind, unit: Option<Unit>, write: bool) -> PointSpec {
    let semantic = match key {
        "on" => Semantic::OnOff,
        "temperature" | "target_temperature" => Semantic::Temperature,
        "humidity" => Semantic::Humidity,
        "problem" => Semantic::Alert,
        _ => Semantic::Other,
    };
    let label = match key {
        "on" => moli_i18n::tr!("airzone.on"),
        "temperature" => moli_i18n::tr!("airzone.temperature"),
        "target_temperature" => moli_i18n::tr!("airzone.target_temperature"),
        "humidity" => moli_i18n::tr!("airzone.humidity"),
        "mode" => moli_i18n::tr!("airzone.mode"),
        "fan_speed" => moli_i18n::tr!("airzone.fan_speed"),
        "problem" => moli_i18n::tr!("airzone.problem"),
        "detail" => moli_i18n::tr!("airzone.detail"),
        "air_demand" => moli_i18n::tr!("airzone.air_demand"),
        "floor_demand" => moli_i18n::tr!("airzone.floor_demand"),
        "cold_demand" => moli_i18n::tr!("airzone.cold_demand"),
        "heat_demand" => moli_i18n::tr!("airzone.heat_demand"),
        _ => unreachable!("known point"),
    };
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        unit,
        semantic,
        access: Access { read: true, write },
    }
}
impl Zone {
    fn parse(raw: &Json) -> anyhow::Result<Self> {
        let system = code(&raw["systemID"], 1, 32)?;
        let id = code(&raw["zoneID"], 1, 32)?;
        binary(&raw["on"])?;
        number(&raw["roomTemp"])?;
        let target = number(&raw["setpoint"])?;
        let min = number(&raw["minTemp"])?;
        let max = number(&raw["maxTemp"])?;
        ensure!(min < max && target >= min && target <= max, invalid());
        if let Some(step) = raw.get("temp_step") {
            let step = number(step)?;
            ensure!(step > 0. && step <= max - min, invalid());
        }
        code(&raw["units"], 0, 1)?;
        code(&raw["mode"], 1, 7)?;
        let modes = raw.get("modes").map(|v| codes(v, 1, 7)).transpose()?;
        let master = raw
            .get("master_zoneID")
            .map(|v| code(v, 1, 32))
            .transpose()?;
        for key in ["air_demand", "floor_demand", "cold_demand", "heat_demand"] {
            if let Some(v) = raw.get(key) {
                binary(v)?;
            }
        }
        if let Some(v) = raw.get("humidity") {
            let h = number(v)?;
            ensure!((0.0..=100.0).contains(&h), invalid());
        }
        if let Some(v) = raw.get("speed") {
            code(v, 0, 7)?;
        }
        let speeds = raw.get("speeds").map(|v| code(v, 0, 7)).transpose()?;
        let fan = if let Some(v) = raw.get("speed_values") {
            let values = codes(v, 0, 7)?;
            ensure!(
                speeds.is_none_or(|max| values.iter().all(|v| *v <= max)),
                invalid()
            );
            values
        } else {
            speeds
                .filter(|s| *s > 0)
                .map(|s| (0..=s).collect())
                .unwrap_or_default()
        };
        if let Some(v) = raw.get("errors") {
            ensure!(v.is_array(), invalid());
        }
        Ok(Self {
            raw: raw.clone(),
            system,
            id,
            master,
            modes,
            fan,
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
    #[allow(clippy::too_many_lines)] // one block per advertised point, with its state
    fn entity(&self, instance: &InstanceId, mac: &str, members: Option<Vec<DeviceId>>) -> Entity {
        let control = members.is_some();
        let r = &self.raw;
        let mut points = vec![];
        let mut states = vec![];
        let mut add = |p: PointSpec, value: Value| {
            states.push((p.key.to_string(), value));
            points.push(p);
        };
        if control {
            let values = self
                .modes
                .as_ref()
                .expect("master")
                .iter()
                .map(|m| mode(*m).into())
                .collect();
            add(
                spec("mode", Kind::Enum { values }, None, true),
                Value::from(mode(r["mode"].as_u64().expect("mode"))),
            );
            if !self.fan.is_empty() && self.fan.iter().any(|s| *s > 0) && r.get("speed").is_some() {
                add(
                    spec(
                        "fan_speed",
                        Kind::Enum {
                            values: self.fan.iter().map(|s| s.to_string().into()).collect(),
                        },
                        None,
                        true,
                    ),
                    Value::Text(r["speed"].as_u64().expect("speed").to_string().into()),
                );
            }
        } else {
            let unit = if r["units"] == 0 {
                Unit::Celsius
            } else {
                Unit::Other("°F".into())
            };
            add(
                spec("on", Kind::Binary, None, true),
                Value::Bool(binary(&r["on"]).expect("on")),
            );
            add(
                spec(
                    "temperature",
                    numeric(None, None, None),
                    Some(unit.clone()),
                    false,
                ),
                Value::Float(r["roomTemp"].as_f64().expect("temperature")),
            );
            add(
                spec(
                    "target_temperature",
                    numeric(
                        r["minTemp"].as_f64(),
                        r["maxTemp"].as_f64(),
                        r["temp_step"].as_f64(),
                    ),
                    Some(unit),
                    true,
                ),
                Value::Float(r["setpoint"].as_f64().expect("target")),
            );
            add(
                spec(
                    "mode",
                    Kind::Enum {
                        values: vec![mode(r["mode"].as_u64().expect("mode")).into()],
                    },
                    None,
                    false,
                ),
                Value::from(mode(r["mode"].as_u64().expect("mode"))),
            );
            if let Some(v) = r["humidity"].as_f64() {
                add(
                    spec(
                        "humidity",
                        numeric(Some(0.), Some(100.), None),
                        Some(Unit::Percent),
                        false,
                    ),
                    Value::Float(v),
                );
            }
            for key in ["air_demand", "floor_demand", "cold_demand", "heat_demand"] {
                if let Some(v) = r.get(key) {
                    add(
                        spec(key, Kind::Binary, None, false),
                        Value::Bool(binary(v).expect("demand")),
                    );
                }
            }
            let errors = r["errors"].as_array();
            add(
                spec("problem", Kind::Binary, None, false),
                Value::Bool(errors.is_some_and(|e| !e.is_empty())),
            );
            if let Some(errors) = errors {
                let text = errors
                    .iter()
                    .take(8)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(240)
                    .collect::<String>();
                add(
                    spec("detail", Kind::Text, None, false),
                    Value::Text(text.into()),
                );
            }
        }
        let name = r["name"]
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
        let systems = value["systems"]
            .as_array()
            .filter(|s| s.len() <= 32)
            .context(invalid())?;
        let mut zones = vec![];
        let mut ids = BTreeSet::new();
        for system in systems {
            let data = system["data"]
                .as_array()
                .filter(|z| z.len() <= 32)
                .context(invalid())?;
            let mut system_id = None;
            for raw in data {
                let z = Zone::parse(raw)?;
                ensure!(system_id.is_none_or(|s| s == z.system), invalid());
                system_id = Some(z.system);
                ensure!(ids.insert((z.system, z.id)), invalid());
                zones.push(z);
            }
        }
        Ok(Self { zones })
    }
    fn members(&self, master: &Zone, instance: &InstanceId, mac: &str) -> Option<Vec<DeviceId>> {
        master.modes.as_ref()?;
        let system = self
            .zones
            .iter()
            .filter(|z| z.system == master.system)
            .collect::<Vec<_>>();
        let masters = system
            .iter()
            .filter(|z| z.modes.is_some())
            .map(|z| z.id)
            .collect::<Vec<_>>();
        // Ambiguous/missing domain metadata disables shared writes, not zone writes.
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
            // Scope is a set: response ordering cannot change the guard's targets.
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
        let p = d.point(key).filter(|p| p.access.write).context(invalid())?;
        let value = moli_core::validate(&p.kind, value).map_err(|_| anyhow::anyhow!(invalid()))?;
        if let Kind::Numeric {
            min: Some(min),
            step: Some(step),
            ..
        } = p.kind
        {
            let grid = (value.as_f64().expect("numeric") - min) / step;
            ensure!((grid - grid.round()).abs() < 1e-6, invalid());
        }
        let control = !d.members.is_empty();
        let zone = self
            .zones
            .iter()
            .find(|z| DeviceId::new(instance, &z.native(mac, control)) == *device)
            .context(invalid())?;
        let (param, wire) = match key {
            "on" => ("on", json!(u8::from(value == Value::Bool(true)))),
            "target_temperature" => ("setpoint", json!(value.as_f64().context(invalid())?)),
            "mode" => {
                let text = text(&value)?;
                (
                    "mode",
                    json!((1..=7).find(|m| mode(*m) == text).context(invalid())?),
                )
            }
            "fan_speed" => ("speed", json!(text(&value)?.parse::<u64>()?)),
            _ => anyhow::bail!(invalid()),
        };
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
        assert!(control.0.point("fan_speed").is_none());
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
    fn malformed_and_duplicate_zones_are_rejected() {
        for (key, bad) in [
            ("on", serde_json::json!(2)),
            ("minTemp", serde_json::json!(35)),
            ("units", serde_json::json!(7)),
            ("systemID", serde_json::json!(0)),
            ("temp_step", serde_json::json!(0)),
            ("roomTemp", Json::Null),
        ] {
            let mut v = fixture();
            v["systems"][0]["data"][0][key] = bad;
            assert!(Snapshot::parse(&v).is_err(), "{key}");
        }
        assert!(Snapshot::parse(&serde_json::json!({"data":[]})).is_err());
        let mut v = fixture();
        let z = v["systems"][0]["data"][0].clone();
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

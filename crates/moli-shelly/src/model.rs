//! What a Shelly is made of, and what its status says (pure, no I/O).
//!
//! A Gen2+ device is a set of components named `<type>:<id>` (`switch:0`,
//! `em:0`, `emdata:0`, `em1:1`, `pm1:0`, `input:2`, `temperature:0`). The full
//! status (`Shelly.GetStatus`) and the pushed changes (`NotifyStatus`) share
//! that shape: a change carries only the fields that moved. Both are read by
//! [`readings`]; a field that is absent keeps its last value.
//!
//! Moli devices, by native id (the MAC, lower-case hex, never a name):
//! - `<mac>`: the box itself (Wi-Fi signal, firmware update, inputs, its
//!   temperature) and, on a Pro 3EM in three-phase mode, the meter;
//! - `<mac>_switch_<n>`: an output (relay), with its measures when it has them;
//! - `<mac>_em1_<n>` / `<mac>_pm1_<n>`: a single-phase meter channel.

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::Value as Json;

/// The phases of a three-phase meter, as Shelly names them.
const PHASES: [&str; 3] = ["a", "b", "c"];

/// A phase counts as wired when it sees a voltage, or once counted this much
/// energy (Wh): a phase without a clamp reads a few mA of noise, never volts.
const WIRED_ENERGY_WH: f64 = 1000.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Switch(u32),
    /// A three-phase meter (`em:<id>` + `emdata:<id>`), with its wired phases.
    Em(u32, Vec<&'static str>),
    /// A single-phase meter channel (`em1:<id>` + `em1data:<id>`).
    Em1(u32),
    /// A power meter channel (`pm1:<id>`).
    Pm1(u32),
    Input(u32),
    Temperature(u32),
}

/// One Shelly, as its first answers describe it.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Lower-case hex, no separators: the identity.
    pub mac: String,
    pub model: String,
    /// What people call it (« Shelly Pro 4PM »), from the firmware's app name.
    pub title: String,
    /// The name its owner gave it in Shelly's app, if any.
    pub name: Option<String>,
    pub parts: Vec<Part>,
    /// Which outputs (and channels) have measures: a Plus 1 has none.
    switch_measures: Vec<u32>,
    /// Names given to components in Shelly's app (`switch:0` → « Chauffe-eau »).
    names: Vec<(String, String)>,
}

fn component(key: &str) -> Option<(&str, u32)> {
    let (kind, id) = key.split_once(':')?;
    Some((kind, id.parse().ok()?))
}

/// « Pro4PM » → « Shelly Pro 4PM », « PlusPlugS » → « Shelly Plus Plug S »
/// (« Pro3EMM » is the Pro 3EM's firmware that can also run single-phase).
fn title(app: &str) -> String {
    let app = app
        .strip_suffix("EMM")
        .map_or_else(|| app.to_owned(), |a| format!("{a}EM"));
    let mut out = String::from("Shelly");
    let mut previous: Option<char> = None;
    for c in app.chars() {
        let lower_before = previous.is_some_and(|p| p.is_ascii_lowercase());
        if previous.is_none() || (lower_before && (c.is_ascii_digit() || c.is_ascii_uppercase())) {
            out.push(' ');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

fn number(v: &Json) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

impl Layout {
    /// From `Shelly.GetDeviceInfo`, `Shelly.GetConfig` and `Shelly.GetStatus`.
    #[must_use]
    pub fn read(info: &Json, config: &Json, status: &Json) -> Option<Self> {
        let mac = moli_net::lan::mac_hex(info["mac"].as_str()?)?;
        let model = info["model"]
            .as_str()
            .or_else(|| info["app"].as_str())
            .unwrap_or("Shelly")
            .to_owned();
        let title = title(info["app"].as_str().unwrap_or(""));
        let name = info["name"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .map(str::to_owned);
        let mut parts = Vec::new();
        let mut switch_measures = Vec::new();
        let mut keys: Vec<&String> = status.as_object()?.keys().collect();
        keys.sort_by_key(|k| component(k).map(|(kind, id)| (kind.to_owned(), id)));
        for key in keys {
            let Some((kind, id)) = component(key) else {
                continue;
            };
            let part = &status[key.as_str()];
            match kind {
                "switch" => {
                    if part.get("apower").is_some() {
                        switch_measures.push(id);
                    }
                    parts.push(Part::Switch(id));
                }
                "em" => {
                    let data = &status[format!("emdata:{id}").as_str()];
                    let wired = PHASES
                        .into_iter()
                        .filter(|p| {
                            number(&part[format!("{p}_voltage").as_str()]).is_some_and(|v| v > 0.0)
                                || number(&data[format!("{p}_total_act_energy").as_str()])
                                    .is_some_and(|e| e >= WIRED_ENERGY_WH)
                        })
                        .collect();
                    parts.push(Part::Em(id, wired));
                }
                "em1" => parts.push(Part::Em1(id)),
                "pm1" => parts.push(Part::Pm1(id)),
                "input" => parts.push(Part::Input(id)),
                "temperature" => parts.push(Part::Temperature(id)),
                _ => {}
            }
        }
        let names = config
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| component(key).is_some())
            .filter_map(|(key, c)| {
                let n = c["name"].as_str()?.trim();
                (!n.is_empty()).then(|| (key.clone(), n.to_owned()))
            })
            .collect();
        Some(Self {
            mac,
            model,
            title,
            name,
            parts,
            switch_measures,
            names,
        })
    }

    fn named(&self, component: &str) -> Option<&str> {
        self.names
            .iter()
            .find(|(k, _)| k == component)
            .map(|(_, n)| n.as_str())
    }

    #[must_use]
    pub fn main_native(&self) -> String {
        self.mac.clone()
    }

    #[must_use]
    pub fn switch_native(&self, id: u32) -> String {
        format!("{}_switch_{id}", self.mac)
    }

    fn channel_native(&self, kind: &str, id: u32) -> String {
        format!("{}_{kind}_{id}", self.mac)
    }

    /// The Moli devices this Shelly is, with their points.
    #[must_use]
    pub fn devices(&self, instance: &InstanceId, id: impl Fn(&str) -> DeviceId) -> Vec<Device> {
        let box_name = self.name.clone().unwrap_or_else(|| self.title.clone());
        let mut main = vec![
            spec(
                "rssi",
                "pilotes.shelly.signal",
                Unit::parse("dBm"),
                Semantic::SignalStrength,
            ),
            text("update", "pilotes.shelly.mise_a_jour"),
            binary(
                "restart_required",
                "pilotes.shelly.redemarrage",
                Semantic::Other,
            ),
        ];
        let mut others = Vec::new();
        let device = |native: String, name: String, points: Vec<PointSpec>| Device {
            id: id(&native),
            instance: instance.clone(),
            native_name: name.into(),
            manufacturer: Some("Shelly".into()),
            model: Some(self.model.as_str().into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points,
        };
        for part in &self.parts {
            match part {
                Part::Switch(n) => {
                    let mut points = vec![PointSpec {
                        access: Access {
                            read: true,
                            write: true,
                        },
                        ..binary("on", "pilotes.shelly.marche", Semantic::OnOff)
                    }];
                    if self.switch_measures.contains(n) {
                        points.extend(measures(""));
                        points.push(spec(
                            "temperature",
                            "pilotes.shelly.temperature",
                            Some(Unit::Celsius),
                            Semantic::Temperature,
                        ));
                    }
                    points.push(text("errors", "pilotes.shelly.alertes"));
                    let name = self.named(&format!("switch:{n}")).map_or_else(
                        || moli_i18n::tr!("pilotes.shelly.sortie", n = n + 1),
                        str::to_owned,
                    );
                    others.push(device(self.switch_native(*n), name, points));
                }
                Part::Em(_, wired) => {
                    main.extend(measures(""));
                    if wired.len() > 1 {
                        // Several phases: no single voltage for the whole meter.
                        main.retain(|p| {
                            !matches!(&*p.key, "voltage" | "frequency" | "power_factor")
                        });
                        for phase in wired {
                            main.extend(measures(phase));
                        }
                    }
                }
                Part::Em1(n) | Part::Pm1(n) => {
                    let kind = if matches!(part, Part::Em1(_)) {
                        "em1"
                    } else {
                        "pm1"
                    };
                    let name = self.named(&format!("{kind}:{n}")).map_or_else(
                        || moli_i18n::tr!("pilotes.shelly.voie", n = n + 1),
                        str::to_owned,
                    );
                    others.push(device(self.channel_native(kind, *n), name, measures("")));
                }
                Part::Input(n) => main.push(PointSpec {
                    label: moli_i18n::tr!("pilotes.shelly.entree", n = n + 1).into(),
                    ..binary(
                        &format!("input_{n}"),
                        "pilotes.shelly.entree",
                        Semantic::Other,
                    )
                }),
                Part::Temperature(n) => main.push(spec(
                    &if *n == 0 {
                        "temperature".to_owned()
                    } else {
                        format!("temperature_{n}")
                    },
                    "pilotes.shelly.temperature",
                    Some(Unit::Celsius),
                    Semantic::Temperature,
                )),
            }
        }
        let mut all = vec![device(self.main_native(), box_name, main)];
        all.extend(others);
        all
    }
}

/// Power, voltage… of one output or channel; `phase` prefixes keys and labels
/// (`c_power`, « Puissance (C) »). Energies are in kWh.
fn measures(phase: &str) -> Vec<PointSpec> {
    let key = |k: &str| {
        if phase.is_empty() {
            k.to_owned()
        } else {
            format!("{phase}_{k}")
        }
    };
    let mut out = vec![
        spec(
            &key("power"),
            "pilotes.shelly.puissance",
            Some(Unit::Watt),
            Semantic::Power,
        ),
        spec(
            &key("apparent_power"),
            "pilotes.shelly.puissance_apparente",
            Some(Unit::VoltAmpere),
            Semantic::ApparentPower,
        ),
        spec(
            &key("voltage"),
            "pilotes.shelly.tension",
            Some(Unit::Volt),
            Semantic::Voltage,
        ),
        spec(
            &key("current"),
            "pilotes.shelly.courant",
            Some(Unit::Ampere),
            Semantic::Current,
        ),
        spec(
            &key("power_factor"),
            "pilotes.shelly.facteur_puissance",
            None,
            Semantic::Other,
        ),
        spec(
            &key("frequency"),
            "pilotes.shelly.frequence",
            Unit::parse("Hz"),
            Semantic::Other,
        ),
        spec(
            &key("energy"),
            "pilotes.shelly.energie",
            Some(Unit::KiloWattHour),
            Semantic::Energy,
        ),
        spec(
            &key("energy_returned"),
            "pilotes.shelly.energie_restituee",
            Some(Unit::KiloWattHour),
            Semantic::Energy,
        ),
    ];
    if !phase.is_empty() {
        let tag = phase.to_ascii_uppercase();
        for p in &mut out {
            p.label = moli_i18n::tr!("pilotes.shelly.phase", label = p.label, phase = tag).into();
        }
    }
    out
}

fn spec(key: &str, label: &str, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: moli_i18n::tr!(label).into(),
        kind: Kind::Numeric {
            min: None,
            max: None,
            step: None,
        },
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

fn binary(key: &str, label: &str, semantic: Semantic) -> PointSpec {
    PointSpec {
        kind: Kind::Binary,
        ..spec(key, label, None, semantic)
    }
}

fn text(key: &str, label: &str) -> PointSpec {
    PointSpec {
        kind: Kind::Text,
        ..spec(key, label, None, Semantic::Other)
    }
}

/// Watt-hours as kWh, to the Wh.
fn kwh(v: &Json) -> Option<Value> {
    number(v).map(|wh| Value::Float((wh / 1000.0 * 1000.0).round() / 1000.0))
}

fn float(v: &Json) -> Option<Value> {
    number(v).map(Value::Float)
}

/// The values read so far, as `(native device, point key, value)`.
#[derive(Default)]
struct Sink(Vec<(String, String, Value)>);

impl Sink {
    fn put(&mut self, native: &str, key: impl Into<String>, value: Option<Value>) {
        if let Some(value) = value {
            self.0.push((native.to_owned(), key.into(), value));
        }
    }
}

/// Every value a status (full or pushed change) holds, as
/// `(native device, point key, value)`. Keys the layout did not publish are
/// ignored by the hub.
#[must_use]
pub fn readings(layout: &Layout, status: &Json) -> Vec<(String, String, Value)> {
    let mut out = Sink::default();
    let Some(map) = status.as_object() else {
        return out.0;
    };
    let main = layout.main_native();
    for (name, part) in map {
        let Some((kind, id)) = component(name) else {
            box_status(&mut out, &main, name, part);
            continue;
        };
        match kind {
            "switch" => switch(&mut out, &layout.switch_native(id), part),
            "em" => meter(&mut out, layout, id, part),
            "emdata" => meter_data(&mut out, &main, part),
            "em1" | "pm1" => channel(&mut out, &layout.channel_native(kind, id), kind, part),
            "em1data" => {
                let native = layout.channel_native("em1", id);
                out.put(&native, "energy", kwh(&part["total_act_energy"]));
                out.put(
                    &native,
                    "energy_returned",
                    kwh(&part["total_act_ret_energy"]),
                );
            }
            "input" => out.put(
                &main,
                format!("input_{id}"),
                part["state"].as_bool().map(Value::Bool),
            ),
            "temperature" => {
                let key = if id == 0 {
                    "temperature".to_owned()
                } else {
                    format!("temperature_{id}")
                };
                out.put(&main, key, float(&part["tC"]));
            }
            _ => {}
        }
    }
    out.0
}

/// `wifi` and `sys`: the box itself.
fn box_status(out: &mut Sink, main: &str, name: &str, part: &Json) {
    match name {
        "wifi" => out.put(main, "rssi", float(&part["rssi"])),
        "sys" => {
            if let Some(updates) = part.get("available_updates") {
                let stable = updates["stable"]["version"].as_str().unwrap_or("");
                out.put(main, "update", Some(Value::Text(stable.into())));
            }
            out.put(
                main,
                "restart_required",
                part["restart_required"].as_bool().map(Value::Bool),
            );
        }
        _ => {}
    }
}

fn switch(out: &mut Sink, native: &str, part: &Json) {
    out.put(native, "on", part["output"].as_bool().map(Value::Bool));
    out.put(native, "power", float(&part["apower"]));
    out.put(native, "voltage", float(&part["voltage"]));
    out.put(native, "current", float(&part["current"]));
    out.put(native, "power_factor", float(&part["pf"]));
    out.put(native, "frequency", float(&part["freq"]));
    out.put(native, "energy", kwh(&part["aenergy"]["total"]));
    out.put(
        native,
        "energy_returned",
        kwh(&part["ret_aenergy"]["total"]),
    );
    out.put(native, "temperature", float(&part["temperature"]["tC"]));
    if let Some(errors) = part.get("errors") {
        let list: Vec<&str> = errors
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .collect();
        out.put(native, "errors", Some(Value::Text(list.join(", ").into())));
    } else if part.get("output").is_some() {
        // A full switch status without `errors` means none.
        out.put(native, "errors", Some(Value::Text("".into())));
    }
}

/// `em:<id>`: a three-phase meter, on the box.
fn meter(out: &mut Sink, layout: &Layout, id: u32, part: &Json) {
    let main = layout.main_native();
    out.put(&main, "power", float(&part["total_act_power"]));
    out.put(&main, "apparent_power", float(&part["total_aprt_power"]));
    out.put(&main, "current", float(&part["total_current"]));
    let phase = |p: &str, k: &str| float(&part[format!("{p}_{k}").as_str()]);
    for p in PHASES {
        out.put(&main, format!("{p}_power"), phase(p, "act_power"));
        out.put(&main, format!("{p}_apparent_power"), phase(p, "aprt_power"));
        out.put(&main, format!("{p}_voltage"), phase(p, "voltage"));
        out.put(&main, format!("{p}_current"), phase(p, "current"));
        out.put(&main, format!("{p}_power_factor"), phase(p, "pf"));
        out.put(&main, format!("{p}_frequency"), phase(p, "freq"));
    }
    // One wired phase: the meter's voltage and frequency are its.
    if let Some(Part::Em(_, wired)) = layout
        .parts
        .iter()
        .find(|x| matches!(x, Part::Em(n, _) if *n == id))
        && let [only] = wired.as_slice()
    {
        out.put(&main, "voltage", phase(only, "voltage"));
        out.put(&main, "frequency", phase(only, "freq"));
        out.put(&main, "power_factor", phase(only, "pf"));
    }
}

/// `emdata:<id>`: the three-phase meter's energy counters (Wh), on the box.
fn meter_data(out: &mut Sink, main: &str, part: &Json) {
    out.put(main, "energy", kwh(&part["total_act"]));
    out.put(main, "energy_returned", kwh(&part["total_act_ret"]));
    for p in PHASES {
        out.put(
            main,
            format!("{p}_energy"),
            kwh(&part[format!("{p}_total_act_energy").as_str()]),
        );
        out.put(
            main,
            format!("{p}_energy_returned"),
            kwh(&part[format!("{p}_total_act_ret_energy").as_str()]),
        );
    }
}

/// `em1:<id>` / `pm1:<id>`: a single-phase meter channel.
fn channel(out: &mut Sink, native: &str, kind: &str, part: &Json) {
    let power = if kind == "em1" { "act_power" } else { "apower" };
    out.put(native, "power", float(&part[power]));
    out.put(native, "apparent_power", float(&part["aprt_power"]));
    out.put(native, "voltage", float(&part["voltage"]));
    out.put(native, "current", float(&part["current"]));
    out.put(native, "power_factor", float(&part["pf"]));
    out.put(native, "frequency", float(&part["freq"]));
    out.put(native, "energy", kwh(&part["aenergy"]["total"]));
    out.put(
        native,
        "energy_returned",
        kwh(&part["ret_aenergy"]["total"]),
    );
}

/// The output a point of this Shelly's devices drives (`switch:<n>`).
#[must_use]
pub fn switch_of(layout: &Layout, native: &str) -> Option<u32> {
    let rest = native
        .strip_prefix(layout.mac.as_str())?
        .strip_prefix("_switch_")?;
    rest.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture(name: &str) -> Json {
        let path = format!(
            "{}/../../integrations/shelly/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn layout(model: &str) -> Layout {
        Layout::read(
            &fixture(&format!("{model}-deviceinfo.json")),
            &fixture(&format!("{model}-config.json")),
            &fixture(&format!("{model}-status.json")),
        )
        .unwrap()
    }

    fn values(layout: &Layout, status: &Json) -> HashMap<(String, String), Value> {
        readings(layout, status)
            .into_iter()
            .map(|(d, k, v)| ((d, k), v))
            .collect()
    }

    #[test]
    fn a_pro_4pm_is_a_box_and_four_measured_outputs() {
        let l = layout("pro4pm");
        assert_eq!(l.mac, "aabbccddee01");
        assert_eq!(l.model, "SPSW-204PE16EU");
        assert_eq!(l.title, "Shelly Pro 4PM");
        assert_eq!(layout("pro3em").title, "Shelly Pro 3EM");
        assert_eq!(title("PlusPlugS"), "Shelly Plus Plug S");
        assert_eq!(title(""), "Shelly");
        let devices = l.devices(&InstanceId::from("relais"), |n| {
            DeviceId::new(&InstanceId::from("relais"), n)
        });
        assert_eq!(devices.len(), 5);
        let out0 = devices
            .iter()
            .find(|d| d.id.as_str() == "relais:aabbccddee01_switch_0")
            .unwrap();
        let on = out0.point("on").unwrap();
        assert!(on.access.write);
        assert_eq!(on.semantic, Semantic::OnOff);
        assert!(out0.point("power").is_some());
        assert!(out0.point("energy").is_some());
        let main = &devices[0];
        assert!(main.point("input_3").is_some());
        assert!(main.point("rssi").is_some());
        assert!(main.point("power").is_none());
    }

    #[test]
    fn a_pro_4pm_status_reads_as_values() {
        let l = layout("pro4pm");
        let v = values(&l, &fixture("pro4pm-status.json"));
        let out0 = "aabbccddee01_switch_0".to_owned();
        assert_eq!(v[&(out0.clone(), "on".into())], Value::Bool(true));
        assert_eq!(v[&(out0.clone(), "power".into())], Value::Float(1.3));
        assert_eq!(v[&(out0.clone(), "energy".into())], Value::Float(0.73));
        assert_eq!(v[&(out0.clone(), "errors".into())], Value::Text("".into()));
        assert_eq!(
            v[&("aabbccddee01".into(), "update".into())],
            Value::Text("2.1.0".into())
        );
        assert_eq!(
            v[&("aabbccddee01".into(), "input_0".into())],
            Value::Bool(false)
        );
        assert_eq!(switch_of(&l, &out0), Some(0));
        assert_eq!(switch_of(&l, "aabbccddee01"), None);
    }

    #[test]
    fn a_pro_3em_with_one_wired_phase_is_one_meter() {
        let l = layout("pro3em");
        assert_eq!(
            l.parts.iter().find(|p| matches!(p, Part::Em(..))),
            Some(&Part::Em(0, vec!["c"]))
        );
        let devices = l.devices(&InstanceId::from("pince"), |n| {
            DeviceId::new(&InstanceId::from("pince"), n)
        });
        assert_eq!(devices.len(), 1);
        let meter = &devices[0];
        for key in [
            "power",
            "voltage",
            "energy",
            "energy_returned",
            "temperature",
        ] {
            assert!(meter.point(key).is_some(), "{key}");
        }
        // One phase: no per-phase points, the meter's own are that phase.
        assert!(meter.point("c_power").is_none());
        let v = values(&l, &fixture("pro3em-status.json"));
        let main = "aabbccddee02".to_owned();
        assert_eq!(v[&(main.clone(), "power".into())], Value::Float(1216.678));
        assert_eq!(v[&(main.clone(), "voltage".into())], Value::Float(231.8));
        assert_eq!(v[&(main.clone(), "energy".into())], Value::Float(1394.189));
        assert_eq!(
            v[&(main.clone(), "energy_returned".into())],
            Value::Float(1190.614)
        );
    }

    #[test]
    fn a_pushed_change_reads_like_a_status() {
        let l = layout("pro3em");
        let notify = fixture("pro3em-notify-status.json");
        let v = values(&l, &notify[0]["params"]);
        assert_eq!(
            v[&("aabbccddee02".into(), "power".into())],
            Value::Float(917.858)
        );
        // A change without the switch's `errors` says nothing about them.
        let change = serde_json::json!({"switch:1": {"id": 1, "apower": 12.5}});
        let v = values(&layout("pro4pm"), &change);
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[&("aabbccddee01_switch_1".into(), "power".into())],
            Value::Float(12.5)
        );
    }

    #[test]
    fn all_three_phases_get_their_own_points() {
        let mut status = fixture("pro3em-status.json");
        for p in ["a", "b"] {
            status["em:0"][format!("{p}_voltage")] = serde_json::json!(230.0);
        }
        let l = Layout::read(
            &fixture("pro3em-deviceinfo.json"),
            &fixture("pro3em-config.json"),
            &status,
        )
        .unwrap();
        let devices = l.devices(&InstanceId::from("pince"), |n| {
            DeviceId::new(&InstanceId::from("pince"), n)
        });
        assert!(devices[0].point("a_power").is_some());
        assert!(devices[0].point("c_energy_returned").is_some());
    }
}

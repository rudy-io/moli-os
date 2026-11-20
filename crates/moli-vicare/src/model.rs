//! What the Viessmann cloud answers, read as Moli devices (pure, no I/O).
//!
//! An installation holds gateways, a gateway holds devices (the heating
//! appliance is `0`), and a device is a flat list of *features*
//! (`heating.circuits.1.operating.modes.active`…), each with `properties`
//! (what it reads) and `commands` (what it accepts, with the limits of each
//! parameter). A feature the appliance does not have, or a sensor that is not
//! connected (no `value`), simply gives no point.
//!
//! The appliance becomes a device `pac` (outside, return and supply
//! temperatures, compressor, COP, domestic hot water) and each heating circuit
//! a device `circuit<n>` (mode, program, set points, room temperature, pumps).
//! The limits of a set point and the modes a circuit accepts are read from the
//! commands the cloud offers, never assumed.

use std::collections::HashMap;
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::{Value as Json, json};

/// Where an installation's heating appliance lives in the cloud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub installation: String,
    pub gateway: String,
    pub device: String,
    /// The appliance's controller, as the cloud names it (`CU401B_S`).
    pub model: Option<String>,
}

impl Site {
    /// From `/equipment/installations?includeGateways=true`: the installation
    /// asked for (by default the first) and its first heating device.
    #[must_use]
    pub fn read(answer: &Json, wanted: Option<u64>) -> Option<Self> {
        answer["data"]
            .as_array()?
            .iter()
            .filter(|i| wanted.is_none_or(|w| i["id"].as_u64() == Some(w)))
            .find_map(|installation| {
                let id = installation["id"].as_u64()?;
                installation["gateways"]
                    .as_array()?
                    .iter()
                    .find_map(|gateway| {
                        let serial = gateway["serial"].as_str().filter(|s| !s.is_empty())?;
                        let device = gateway["devices"]
                            .as_array()?
                            .iter()
                            .find(|d| d["deviceType"] == "heating")?;
                        Some(Self {
                            installation: id.to_string(),
                            gateway: serial.to_owned(),
                            device: device["id"].as_str()?.to_owned(),
                            model: device["modelId"].as_str().map(str::to_owned),
                        })
                    })
            })
    }

    /// The cloud path of a command (`POST`) on one of the appliance's features.
    #[must_use]
    pub fn command_path(&self, request: &Request) -> String {
        format!(
            "{}/{}/commands/{}",
            self.features_path(),
            request.feature,
            request.action
        )
    }

    /// The cloud path of the appliance's features.
    #[must_use]
    pub fn features_path(&self) -> String {
        format!(
            "/features/installations/{}/gateways/{}/devices/{}/features",
            self.installation, self.gateway, self.device
        )
    }
}

/// The enabled features of the appliance, by name.
#[derive(Clone, Debug, Default)]
pub struct Features(HashMap<String, Json>);

impl Features {
    /// From the answer of the features path.
    #[must_use]
    pub fn read(answer: &Json) -> Self {
        Self(
            answer["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|f| f["isEnabled"].as_bool().unwrap_or(true))
                .filter_map(|f| Some((f["feature"].as_str()?.to_owned(), f.clone())))
                .collect(),
        )
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A property's value (`null` is no value: an unconnected sensor).
    fn value(&self, feature: &str, property: &str) -> Option<&Json> {
        let value = &self.0.get(feature)?["properties"][property]["value"];
        (!value.is_null()).then_some(value)
    }

    /// A command the appliance accepts now.
    fn command(&self, feature: &str, name: &str) -> Option<&Json> {
        let command = self.0.get(feature)?["commands"].get(name)?;
        command["isExecutable"]
            .as_bool()
            .unwrap_or(true)
            .then_some(command)
    }

    /// The heating circuits that are in use, by number.
    fn circuits(&self) -> Vec<u32> {
        let mut numbers: Vec<u32> = self
            .0
            .keys()
            .filter_map(|name| name.strip_prefix("heating.circuits.")?.parse().ok())
            .filter(|n| {
                self.value(&format!("heating.circuits.{n}"), "active") == Some(&json!(true))
            })
            .collect();
        numbers.sort_unstable();
        numbers
    }
}

/// How a property reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Number,
    Text,
    Bool,
    /// `"on"` / `"off"`.
    On,
}

/// How a point is written.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Order {
    /// `commands/<action>` with the number as `<param>`.
    Number {
        action: &'static str,
        param: &'static str,
    },
    /// `setMode` with one of the modes the cloud lists.
    Mode,
    /// `activate` / `deactivate`.
    Toggle,
}

/// What an order becomes in the cloud: `POST <feature>/commands/<action>`.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub feature: String,
    pub action: &'static str,
    pub body: Json,
}

#[derive(Clone, Debug, PartialEq)]
struct Point {
    spec: PointSpec,
    feature: String,
    property: &'static str,
    shape: Shape,
    order: Option<Order>,
}

impl Point {
    fn read(&self, features: &Features) -> Option<Value> {
        let value = features.value(&self.feature, self.property)?;
        match self.shape {
            Shape::Number => value.as_f64().map(Value::Float),
            Shape::Text => value.as_str().map(Value::from),
            Shape::Bool => value.as_bool().map(Value::Bool),
            Shape::On => value.as_str().map(|s| Value::Bool(s == "on")),
        }
    }

    fn request(&self, value: &Value) -> Option<Request> {
        let (action, body) = match (self.order.as_ref()?, value) {
            (Order::Number { action, param }, value) => {
                let mut body = serde_json::Map::new();
                body.insert((*param).to_owned(), json!(value.as_f64()?));
                (*action, Json::Object(body))
            }
            (Order::Mode, Value::Text(mode)) => ("setMode", json!({ "mode": &**mode })),
            (Order::Toggle, Value::Bool(true)) => ("activate", json!({})),
            (Order::Toggle, Value::Bool(false)) => ("deactivate", json!({})),
            _ => return None,
        };
        Some(Request {
            feature: self.feature.clone(),
            action,
            body,
        })
    }
}

/// A point's definition, before the cloud's limits are known.
struct Def {
    key: &'static str,
    label: String,
    feature: String,
    property: &'static str,
    shape: Shape,
    unit: Option<Unit>,
    semantic: Semantic,
}

/// A number read from the `value` property of a sensor.
fn sensor(
    key: &'static str,
    label: String,
    feature: impl Into<String>,
    unit: Option<Unit>,
    semantic: Semantic,
) -> Def {
    Def {
        key,
        label,
        feature: feature.into(),
        property: "value",
        shape: Shape::Number,
        unit,
        semantic,
    }
}

/// The limits the cloud puts on a command's parameter.
fn numeric(command: &Json, param: &str) -> Option<Kind> {
    let parameter = &command["params"][param];
    if parameter.is_null() {
        return None;
    }
    let limits = &parameter["constraints"];
    // Limits the cloud does not give are never supposed.
    Some(Kind::Numeric {
        min: Some(limits["min"].as_f64()?),
        max: Some(limits["max"].as_f64()?),
        step: limits["stepping"].as_f64(),
    })
}

/// The modes `setMode` accepts.
fn modes(command: &Json) -> Option<Kind> {
    let values: Vec<Arc<str>> = command["params"]["mode"]["constraints"]["enum"]
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(Arc::from))
        .collect();
    (!values.is_empty()).then_some(Kind::Enum { values })
}

struct Points<'a> {
    features: &'a Features,
    list: Vec<Point>,
}

impl Points<'_> {
    fn point(def: Def, kind: Kind, order: Option<Order>) -> Point {
        Point {
            spec: PointSpec {
                key: def.key.into(),
                label: def.label.into(),
                kind,
                access: Access {
                    read: true,
                    write: order.is_some(),
                },
                unit: def.unit,
                semantic: def.semantic,
            },
            feature: def.feature,
            property: def.property,
            shape: def.shape,
            order,
        }
    }

    /// A point to read, when the cloud gives its value.
    fn read(&mut self, def: Def) {
        if self.features.value(&def.feature, def.property).is_none() {
            return;
        }
        let kind = match def.shape {
            Shape::Number => Kind::Numeric {
                min: None,
                max: None,
                step: None,
            },
            Shape::Text => Kind::Text,
            Shape::Bool | Shape::On => Kind::Binary,
        };
        self.list.push(Self::point(def, kind, None));
    }

    /// A point to read and write, when the cloud gives its value and takes
    /// the command; its limits are the command's.
    fn control(&mut self, def: Def, order: Order) {
        if let Some(kind) = self.kind_of(&def, &order) {
            self.list.push(Self::point(def, kind, Some(order)));
        } else {
            // No command, or one without limits: the value stays readable.
            self.read(def);
        }
    }

    fn kind_of(&self, def: &Def, order: &Order) -> Option<Kind> {
        self.features.value(&def.feature, def.property)?;
        match order {
            Order::Number { action, param } => {
                numeric(self.features.command(&def.feature, action)?, param)
            }
            Order::Mode => modes(self.features.command(&def.feature, "setMode")?),
            Order::Toggle => {
                self.features.command(&def.feature, "activate")?;
                self.features.command(&def.feature, "deactivate")?;
                Some(Kind::Binary)
            }
        }
    }
}

/// A device and its points.
#[derive(Clone, Debug, PartialEq)]
struct Group {
    native: String,
    name: String,
    model: Option<String>,
    points: Vec<Point>,
}

/// Everything the appliance offers, as Moli devices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    groups: Vec<Group>,
}

impl Layout {
    #[must_use]
    pub fn from(features: &Features, model: Option<&str>) -> Self {
        let mut groups = vec![heat_pump(features, model)];
        groups.extend(
            features
                .circuits()
                .into_iter()
                .map(|n| circuit(features, n)),
        );
        groups.retain(|g| !g.points.is_empty());
        Self { groups }
    }

    /// Which points exist, with their limits and whether they take orders, to
    /// know when the devices must be published again.
    #[must_use]
    pub fn signature(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(|g| {
                g.points.iter().map(|p| {
                    format!(
                        "{}/{}/{:?}/{}",
                        g.native, p.spec.key, p.spec.kind, p.spec.access.write
                    )
                })
            })
            .collect()
    }

    /// The native names of the devices.
    pub fn natives(&self) -> impl Iterator<Item = &str> {
        self.groups.iter().map(|g| g.native.as_str())
    }

    #[must_use]
    pub fn devices(&self, instance: &InstanceId, id: impl Fn(&str) -> DeviceId) -> Vec<Device> {
        self.groups
            .iter()
            .map(|g| Device {
                id: id(&g.native),
                instance: instance.clone(),
                native_name: g.name.as_str().into(),
                manufacturer: Some("Viessmann".into()),
                model: g.model.as_deref().map(Into::into),
                description: None,
                native_room: None,
                members: Vec::new(),
                points: g.points.iter().map(|p| p.spec.clone()).collect(),
            })
            .collect()
    }

    /// Every value the features give, as `(native device, key, value)`.
    #[must_use]
    pub fn readings(&self, features: &Features) -> Vec<(String, String, Value)> {
        self.groups
            .iter()
            .flat_map(|g| {
                g.points.iter().filter_map(|p| {
                    Some((g.native.clone(), p.spec.key.to_string(), p.read(features)?))
                })
            })
            .collect()
    }

    /// What to send the cloud to write `value` on a point.
    #[must_use]
    pub fn request(&self, native: &str, key: &str, value: &Value) -> Option<Request> {
        self.groups
            .iter()
            .find(|g| g.native == native)?
            .points
            .iter()
            .find(|p| &*p.spec.key == key)?
            .request(value)
    }
}

fn heat_pump(features: &Features, model: Option<&str>) -> Group {
    let mut pts = Points {
        features,
        list: Vec::new(),
    };
    temperatures(&mut pts);
    compressor(&mut pts);
    hot_water(&mut pts);
    Group {
        native: "pac".into(),
        name: moli_i18n::tr!("pilotes.vicare.pompe_a_chaleur"),
        model: model.map(str::to_owned),
        points: pts.list,
    }
}

fn temperatures(pts: &mut Points<'_>) {
    pts.read(sensor(
        "outside_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_exterieure"),
        "heating.sensors.temperature.outside",
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "return_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_retour"),
        "heating.sensors.temperature.return",
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "supply_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_depart"),
        "heating.secondaryCircuit.sensors.temperature.supply",
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "source_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_source"),
        "heating.primaryCircuit.sensors.temperature.supply",
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "buffer_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_tampon"),
        "heating.bufferCylinder.sensors.temperature.top",
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "dhw_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_ecs"),
        "heating.dhw.sensors.temperature.dhwCylinder",
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
}

fn compressor(pts: &mut Points<'_>) {
    pts.read(Def {
        key: "compressor_active",
        label: moli_i18n::tr!("pilotes.vicare.compresseur_actif"),
        feature: "heating.compressors.0".into(),
        property: "active",
        shape: Shape::Bool,
        unit: None,
        semantic: Semantic::Other,
    });
    pts.read(Def {
        key: "compressor_phase",
        label: moli_i18n::tr!("pilotes.vicare.compresseur_phase"),
        feature: "heating.compressors.0".into(),
        property: "phase",
        shape: Shape::Text,
        unit: None,
        semantic: Semantic::Other,
    });
    pts.read(sensor(
        "compressor_load",
        moli_i18n::tr!("pilotes.vicare.compresseur_charge"),
        "heating.compressors.0.sensors.power",
        Some(Unit::Percent),
        Semantic::Other,
    ));
    pts.read(Def {
        key: "compressor_starts",
        label: moli_i18n::tr!("pilotes.vicare.compresseur_demarrages"),
        feature: "heating.compressors.0.statistics".into(),
        property: "starts",
        shape: Shape::Number,
        unit: None,
        semantic: Semantic::Other,
    });
    pts.read(Def {
        key: "compressor_hours",
        label: moli_i18n::tr!("pilotes.vicare.compresseur_heures"),
        feature: "heating.compressors.0.statistics".into(),
        property: "hours",
        shape: Shape::Number,
        unit: Some(Unit::Hour),
        semantic: Semantic::Other,
    });
    for (key, label, feature) in [
        (
            "cop_heating",
            moli_i18n::tr!("pilotes.vicare.cop_chauffage"),
            "heating.cop.heating",
        ),
        (
            "cop_dhw",
            moli_i18n::tr!("pilotes.vicare.cop_ecs"),
            "heating.cop.dhw",
        ),
        (
            "cop_total",
            moli_i18n::tr!("pilotes.vicare.cop_total"),
            "heating.cop.total",
        ),
    ] {
        pts.read(sensor(key, label, feature, None, Semantic::Other));
    }
}

fn hot_water(pts: &mut Points<'_>) {
    pts.read(Def {
        key: "dhw_charging",
        label: moli_i18n::tr!("pilotes.vicare.ecs_en_charge"),
        feature: "heating.dhw.charging".into(),
        property: "active",
        shape: Shape::Bool,
        unit: None,
        semantic: Semantic::Other,
    });
    for (key, label, feature) in [
        (
            "dhw_target",
            moli_i18n::tr!("pilotes.vicare.ecs_consigne"),
            "heating.dhw.temperature.main",
        ),
        (
            "dhw_target2",
            moli_i18n::tr!("pilotes.vicare.ecs_consigne2"),
            "heating.dhw.temperature.temp2",
        ),
    ] {
        pts.control(
            sensor(key, label, feature, Some(Unit::Celsius), Semantic::Config),
            Order::Number {
                action: "setTargetTemperature",
                param: "temperature",
            },
        );
    }
    pts.control(
        Def {
            key: "dhw_boost",
            label: moli_i18n::tr!("pilotes.vicare.ecs_charge_unique"),
            feature: "heating.dhw.oneTimeCharge".into(),
            property: "active",
            shape: Shape::Bool,
            unit: None,
            semantic: Semantic::Control,
        },
        Order::Toggle,
    );
}

fn circuit(features: &Features, n: u32) -> Group {
    let base = format!("heating.circuits.{n}");
    let mut pts = Points {
        features,
        list: Vec::new(),
    };
    circuit_settings(&mut pts, &base);
    circuit_readings(&mut pts, &base);
    let name = features
        .value(&base, "name")
        .and_then(Json::as_str)
        .filter(|name| !name.trim().is_empty())
        .map_or_else(
            || moli_i18n::tr!("pilotes.vicare.circuit", n = n),
            str::to_owned,
        );
    Group {
        native: format!("circuit{n}"),
        name,
        model: None,
        points: pts.list,
    }
}

fn circuit_settings(pts: &mut Points<'_>, base: &str) {
    pts.control(
        Def {
            key: "mode",
            label: moli_i18n::tr!("pilotes.vicare.mode"),
            feature: format!("{base}.operating.modes.active"),
            property: "value",
            shape: Shape::Text,
            unit: None,
            semantic: Semantic::Config,
        },
        Order::Mode,
    );
    pts.read(Def {
        key: "program",
        label: moli_i18n::tr!("pilotes.vicare.programme"),
        feature: format!("{base}.operating.programs.active"),
        property: "value",
        shape: Shape::Text,
        unit: None,
        semantic: Semantic::Other,
    });
    for (key, program, label) in [
        (
            "comfort_temperature",
            "comfort",
            moli_i18n::tr!("pilotes.vicare.consigne_confort"),
        ),
        (
            "normal_temperature",
            "normal",
            moli_i18n::tr!("pilotes.vicare.consigne_normale"),
        ),
        (
            "reduced_temperature",
            "reduced",
            moli_i18n::tr!("pilotes.vicare.consigne_reduite"),
        ),
    ] {
        pts.control(
            Def {
                key,
                label,
                feature: format!("{base}.operating.programs.{program}"),
                property: "temperature",
                shape: Shape::Number,
                unit: Some(Unit::Celsius),
                semantic: Semantic::Config,
            },
            Order::Number {
                action: "setTemperature",
                param: "targetTemperature",
            },
        );
    }
    pts.control(
        Def {
            key: "comfort_active",
            label: moli_i18n::tr!("pilotes.vicare.confort_actif"),
            feature: format!("{base}.operating.programs.comfort"),
            property: "active",
            shape: Shape::Bool,
            unit: None,
            semantic: Semantic::Control,
        },
        Order::Toggle,
    );
}

fn circuit_readings(pts: &mut Points<'_>, base: &str) {
    pts.read(sensor(
        "room_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_piece"),
        format!("{base}.sensors.temperature.room"),
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "supply_temperature",
        moli_i18n::tr!("pilotes.vicare.temperature_depart"),
        format!("{base}.sensors.temperature.supply"),
        Some(Unit::Celsius),
        Semantic::Temperature,
    ));
    pts.read(sensor(
        "flow_target",
        moli_i18n::tr!("pilotes.vicare.consigne_depart"),
        format!("{base}.temperature"),
        Some(Unit::Celsius),
        Semantic::Other,
    ));
    for (key, label, feature) in [
        (
            "circulation_pump",
            moli_i18n::tr!("pilotes.vicare.pompe_circulation"),
            format!("{base}.circulation.pump"),
        ),
        (
            "frost_protection",
            moli_i18n::tr!("pilotes.vicare.hors_gel"),
            format!("{base}.frostprotection"),
        ),
    ] {
        pts.read(Def {
            key,
            label,
            feature,
            property: "status",
            shape: Shape::On,
            unit: None,
            semantic: Semantic::Other,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Json {
        let path = format!(
            "{}/../../integrations/vicare/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn layout() -> (Features, Layout) {
        let features = Features::read(&fixture("features-heating.json"));
        let layout = Layout::from(&features, Some("CU401B_S"));
        (features, layout)
    }

    fn devices_of(layout: &Layout) -> Vec<Device> {
        let instance = InstanceId::from("pac");
        layout.devices(&instance, |native| DeviceId::new(&instance, native))
    }

    fn reading(features: &Features, layout: &Layout, native: &str, key: &str) -> Option<Value> {
        layout
            .readings(features)
            .into_iter()
            .find(|(n, k, _)| n == native && k == key)
            .map(|(_, _, v)| v)
    }

    #[test]
    fn the_site_is_the_first_heating_device() {
        let site = Site::read(&fixture("installations.json"), None).unwrap();
        assert_eq!(site.device, "0");
        assert_eq!(site.model.as_deref(), Some("CU401B_S"));
        assert_eq!(
            site.features_path(),
            "/features/installations/1234567/gateways/7777777777777777/devices/0/features"
        );
        assert_eq!(Site::read(&fixture("installations.json"), Some(1)), None);
    }

    #[test]
    fn the_appliance_and_its_circuits_are_devices() {
        let (_, layout) = layout();
        assert_eq!(
            layout.natives().collect::<Vec<_>>(),
            ["pac", "circuit0", "circuit1"]
        );
        let devices = devices_of(&layout);
        assert_eq!(devices[1].native_name.as_ref(), "Circuit radiateur");
        assert_eq!(devices[0].model.as_deref(), Some("CU401B_S"));
    }

    /// The fixture's features, as the cloud answers, with `edit` applied to
    /// the one named `feature`.
    fn edited(feature: &str, edit: impl Fn(&mut Json)) -> Features {
        let mut answer = fixture("features-heating.json");
        let list = answer["data"].as_array_mut().unwrap();
        let found = list.iter_mut().find(|f| f["feature"] == feature).unwrap();
        edit(found);
        Features::read(&answer)
    }

    #[test]
    fn a_sensor_that_is_not_connected_gives_no_point() {
        let sensor = "heating.sensors.temperature.return";
        let (_, whole) = layout();
        assert!(devices_of(&whole)[0].point("return_temperature").is_some());
        let features = edited(sensor, |f| {
            f["properties"] = json!({ "status": { "type": "string", "value": "notConnected" } });
        });
        let layout = Layout::from(&features, None);
        assert!(devices_of(&layout)[0].point("return_temperature").is_none());
    }

    #[test]
    fn a_command_without_limits_leaves_the_point_read_only() {
        let target = "heating.dhw.temperature.main";
        let (_, whole) = layout();
        let features = edited(target, |f| {
            f["commands"]["setTargetTemperature"]["params"]["temperature"]["constraints"] =
                json!({});
        });
        let bare = Layout::from(&features, Some("CU401B_S"));
        let point = devices_of(&bare)[0].point("dhw_target").cloned().unwrap();
        assert!(!point.access.write);
        // The limits are part of what is published: a change of them (or of
        // the right to order) publishes the devices again.
        assert_ne!(bare.signature(), whole.signature());
    }

    #[test]
    fn values_are_read_from_the_features() {
        let (features, layout) = layout();
        let read = |n: &str, k: &str| reading(&features, &layout, n, k);
        assert_eq!(read("pac", "outside_temperature"), Some(Value::Float(15.4)));
        assert_eq!(read("pac", "compressor_active"), Some(Value::Bool(false)));
        assert_eq!(read("pac", "compressor_phase"), Some(Value::from("off")));
        assert_eq!(read("pac", "cop_total"), Some(Value::Float(4.1)));
        assert_eq!(read("circuit0", "mode"), Some(Value::from("dhwAndHeating")));
        assert_eq!(
            read("circuit0", "circulation_pump"),
            Some(Value::Bool(true))
        );
        assert_eq!(
            read("circuit1", "circulation_pump"),
            Some(Value::Bool(false))
        );
        assert_eq!(
            read("circuit1", "room_temperature"),
            Some(Value::Float(20.9))
        );
        // No room sensor on the first circuit.
        assert_eq!(read("circuit0", "room_temperature"), None);
    }

    #[test]
    fn limits_and_modes_come_from_the_commands() {
        let (_, layout) = layout();
        let devices = devices_of(&layout);
        let target = devices[0].point("dhw_target").unwrap();
        assert!(target.access.write);
        assert_eq!(
            target.kind,
            Kind::Numeric {
                min: Some(10.0),
                max: Some(60.0),
                step: Some(1.0)
            }
        );
        let mode = devices[1].point("mode").unwrap();
        assert_eq!(
            mode.kind,
            Kind::Enum {
                values: ["dhw", "dhwAndHeating", "standby"]
                    .into_iter()
                    .map(Arc::from)
                    .collect()
            }
        );
        // Read only: nothing accepts a command for them.
        assert!(!devices[0].point("compressor_active").unwrap().access.write);
        assert!(!devices[1].point("program").unwrap().access.write);
    }

    #[test]
    fn orders_become_cloud_commands() {
        let (_, layout) = layout();
        let order = layout
            .request("pac", "dhw_target", &Value::Float(55.0))
            .unwrap();
        assert_eq!(order.feature, "heating.dhw.temperature.main");
        assert_eq!(order.action, "setTargetTemperature");
        assert_eq!(order.body, json!({ "temperature": 55.0 }));

        let order = layout
            .request("circuit1", "comfort_temperature", &Value::Int(21))
            .unwrap();
        assert_eq!(
            order.feature,
            "heating.circuits.1.operating.programs.comfort"
        );
        assert_eq!(order.action, "setTemperature");
        assert_eq!(order.body, json!({ "targetTemperature": 21.0 }));

        let order = layout
            .request("circuit0", "mode", &Value::from("standby"))
            .unwrap();
        assert_eq!(
            (order.action, order.body),
            ("setMode", json!({ "mode": "standby" }))
        );

        let on = layout
            .request("pac", "dhw_boost", &Value::Bool(true))
            .unwrap();
        let off = layout
            .request("pac", "dhw_boost", &Value::Bool(false))
            .unwrap();
        assert_eq!((on.action, off.action), ("activate", "deactivate"));
    }

    #[test]
    fn what_cannot_be_written_is_refused() {
        let (_, layout) = layout();
        assert_eq!(layout.request("pac", "cop_total", &Value::Float(1.0)), None);
        assert_eq!(
            layout.request("pac", "dhw_target", &Value::from("hot")),
            None
        );
        assert_eq!(layout.request("pac", "absent", &Value::Bool(true)), None);
        assert_eq!(
            layout.request("nowhere", "mode", &Value::from("standby")),
            None
        );
    }

    #[test]
    fn a_missing_feature_is_not_an_error() {
        let features = Features::read(&json!({ "data": [] }));
        assert!(features.is_empty());
        assert_eq!(Layout::from(&features, None), Layout::default());
        let odd = Features::read(&json!({ "data": [{ "feature": "heating.circuits.0" }, 5] }));
        assert!(Layout::from(&odd, None).signature().is_empty());
    }
}

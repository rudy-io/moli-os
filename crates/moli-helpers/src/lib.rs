//! Helpers: virtual devices the house keeps for itself, like Home
//! Assistant's `input_select` / `input_boolean` / `input_number` / `input_text`
//! (« mode de la maison », « alarme active »…).
//!
//! Each helper is a device of this instance with one writable point. A
//! command simply becomes its new value (guard and journal apply, as for any
//! device); the value survives restarts through the hub's state cache, and a
//! helper never seen before starts at its `initial` value.
//!
//! ```toml
//! [[driver]]
//! id = "maison"
//! kind = "helpers"
//! [[driver.options.helper]]
//! id = "mode_maison"
//! name = "Mode Maison"
//! kind = "select"
//! options = ["Normal", "Absent", "Nuit", "Vacances"]
//! initial = "Normal"
//! ```

use anyhow::{bail, ensure};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value, validate};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "helper")]
    pub helpers: Vec<Helper>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Helper {
    /// Stable id: the device is `<instance>:<id>`. Never rename it.
    pub id: String,
    pub name: String,
    pub kind: HelperKind,
    /// `select`: the choices.
    #[serde(default)]
    pub options: Vec<String>,
    /// `number`: bounds and step.
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    /// The value before anyone set one.
    #[serde(default)]
    pub initial: Option<toml::Value>,
    /// Room, if it belongs to one (most belong to the whole house).
    #[serde(default)]
    pub room: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelperKind {
    Select,
    Switch,
    Number,
    Text,
}

impl Helper {
    /// The point that holds its value.
    fn spec(&self) -> PointSpec {
        let (key, kind, semantic) = match self.kind {
            HelperKind::Select => (
                "state",
                Kind::Enum {
                    values: self.options.iter().map(|o| o.as_str().into()).collect(),
                },
                Semantic::Other,
            ),
            HelperKind::Switch => ("on", Kind::Binary, Semantic::OnOff),
            HelperKind::Number => (
                "value",
                Kind::Numeric {
                    min: self.min,
                    max: self.max,
                    step: self.step,
                },
                Semantic::Other,
            ),
            HelperKind::Text => ("value", Kind::Text, Semantic::Other),
        };
        PointSpec {
            key: key.into(),
            label: self.name.as_str().into(),
            kind,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic,
        }
    }

    /// The starting value, checked against the point.
    fn initial_value(&self) -> anyhow::Result<Value> {
        let raw = match (&self.initial, self.kind) {
            (Some(toml::Value::String(s)), _) => Value::Text(s.as_str().into()),
            (Some(toml::Value::Boolean(b)), _) => Value::Bool(*b),
            (Some(toml::Value::Integer(i)), _) => Value::Int(*i),
            (Some(toml::Value::Float(f)), _) => Value::Float(*f),
            (Some(other), _) => bail!("helper {}: initial {other} is not a value", self.id),
            (None, HelperKind::Select) => match self.options.first() {
                Some(first) => Value::Text(first.as_str().into()),
                None => Value::Null,
            },
            (None, HelperKind::Switch) => Value::Bool(false),
            (None, HelperKind::Number) => Value::Float(self.min.unwrap_or(0.0)),
            (None, HelperKind::Text) => Value::Text("".into()),
        };
        validate(&self.spec().kind, &raw)
            .map_err(|e| anyhow::anyhow!("helper {}: initial value: {e}", self.id))
    }
}

#[derive(Debug)]
pub struct Helpers {
    config: Config,
}

impl Helpers {
    /// Checks the configuration (ids, options, initial values).
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let mut seen = std::collections::HashSet::new();
        for h in &config.helpers {
            ensure!(
                !h.id.is_empty() && !h.id.contains([':', '/']),
                "helper id {:?}: non-empty, without ':' nor '/'",
                h.id
            );
            ensure!(seen.insert(&h.id), "helper id {:?} twice", h.id);
            ensure!(
                h.kind != HelperKind::Select || !h.options.is_empty(),
                "helper {}: a select needs options",
                h.id
            );
            h.initial_value()?;
        }
        Ok(Self { config })
    }
}

impl Driver for Helpers {
    fn kind(&self) -> &'static str {
        "helpers"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            for h in &self.config.helpers {
                let id = ctx.device_id(&h.id);
                let spec = h.spec();
                ctx.upsert_device(Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: h.name.as_str().into(),
                    manufacturer: Some("Moli".into()),
                    model: Some("Aide".into()),
                    description: None,
                    native_room: h.room.as_deref().map(Into::into),
                    members: Vec::new(),
                    points: vec![spec.clone()],
                });
                // Kept across restarts by the state cache; set only once.
                if ctx.current(&id, &spec.key).is_none() {
                    ctx.set_state(&id, &spec.key, h.initial_value()?);
                }
                ctx.set_availability(&id, true);
            }
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                // Already validated against the point by the hub.
                ctx.set_state(&command.device.id, &command.key, command.value.clone());
                command.reply(Ok(()));
            }
            Ok(())
        })
    }
}

/// Builds the device id of a helper (for tests and remaps).
#[must_use]
pub fn helper_device(instance: &moli_core::InstanceId, id: &str) -> DeviceId {
    DeviceId::new(instance, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use moli_core::{InstanceId, Origin, PointId};
    use moli_runtime::{Hub, HubOptions, spawn_driver};
    use tokio_util::sync::CancellationToken;

    fn config() -> Config {
        toml::from_str(
            r#"
            [[helper]]
            id = "mode_maison"
            name = "Mode Maison"
            kind = "select"
            options = ["Normal", "Absent", "Nuit", "Vacances"]
            initial = "Normal"
            [[helper]]
            id = "alarme_active"
            name = "Alarme Active"
            kind = "switch"
            initial = true
            [[helper]]
            id = "budget"
            name = "Budget élec mensuel"
            kind = "number"
            min = 50.0
            max = 500.0
            step = 10.0
            initial = 200.0
            "#,
        )
        .unwrap()
    }

    #[test]
    fn bad_configurations_are_refused() {
        let mut c = config();
        c.helpers[0].initial = Some(toml::Value::String("Fête".into()));
        assert!(Helpers::new(c).is_err(), "initial outside the options");
        let mut c = config();
        c.helpers[1].id = "a:b".into();
        assert!(Helpers::new(c).is_err());
        let mut c = config();
        c.helpers[0].options.clear();
        assert!(Helpers::new(c).is_err());
    }

    #[tokio::test]
    async fn helpers_start_at_their_initial_value_and_take_orders() {
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("maison"),
            Arc::new(Helpers::new(config()).unwrap()),
            cancel.clone(),
        );
        let mode = PointId::from("maison:mode_maison/state".to_owned());
        for _ in 0..100 {
            if hub.state(&mode).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            hub.state(&mode).unwrap().value,
            Value::Text("Normal".into())
        );
        assert_eq!(
            hub.state(&PointId::from("maison:alarme_active/on".to_owned()))
                .unwrap()
                .value,
            Value::Bool(true)
        );
        hub.command(&mode, Value::Text("Nuit".into()), Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(hub.state(&mode).unwrap().value, Value::Text("Nuit".into()));
        let refused = hub
            .command(&mode, Value::Text("Fête".into()), Origin::Ui, None)
            .await;
        assert!(refused.is_err(), "only the listed options");
        cancel.cancel();
    }
}

//! Airzone-only read telemetry. An unchanged sample still has a new receipt;
//! an API acknowledgement is never substituted for equipment state.
use moli_core::{Access, Kind, PointSpec, Semantic, Value, now_ms};
use moli_runtime::DriverCtx;

use crate::{api, model::Entity};

#[derive(Clone, Debug, Default)]
pub(crate) struct Progress {
    pub generation: u64,
    pub last_attempt: Option<u64>,
    pub last_response: Option<u64>,
    pub last_success: Option<u64>,
    pub last_cycle: Option<u64>,
    pub active: bool,
    pub last_error: String,
    pub poll_interval: u64,
    pub optional_systems: u64,
    pub iaq_systems: u64,
}

impl Progress {
    pub fn begin(&mut self) {
        self.last_attempt = Some(now_ms());
        self.active = true;
    }

    pub fn received(&mut self) {
        self.last_response = Some(now_ms());
    }

    pub fn core(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.last_success = Some(now_ms());
        self.last_error.clear();
    }

    pub fn failed(&mut self, error: &anyhow::Error) {
        self.active = false;
        self.last_error = match error.downcast_ref::<api::Failure>() {
            Some(api::Failure::Http(status)) => format!("http_{status}"),
            Some(api::Failure::Timeout) => "timeout".into(),
            Some(api::Failure::Transport) => "transport".into(),
            Some(api::Failure::Certificate) => "certificate".into(),
            Some(api::Failure::Identity) => "identity_changed".into(),
            Some(api::Failure::Api) => "api_rejected".into(),
            Some(api::Failure::Expired) => "expired".into(),
            _ => "invalid_response".into(),
        };
    }

    pub fn complete(&mut self) {
        self.last_cycle = Some(now_ms());
        self.active = false;
    }

    fn interval(&self, entity: &Entity) -> u64 {
        if entity.0.model.as_deref() != Some("Airzone Local API") {
            return self.poll_interval;
        }
        let candidates = if entity.0.id.as_str().contains("_iaq") {
            self.iaq_systems.max(1)
        } else if entity.0.id.as_str().ends_with("_system") {
            self.optional_systems.saturating_add(1)
        } else {
            1
        };
        self.poll_interval
            .saturating_mul(crate::resources::OPTIONAL_FAMILIES)
            .saturating_mul(candidates)
    }

    /// Append after equipment fields, so a generation event follows its values.
    pub fn append(&self, entity: &mut Entity, ctx: &DriverCtx, gateway: bool, observed: bool) {
        let received = if observed {
            number(Some(now_ms()))
        } else {
            ctx.current(&entity.0.id, "sync_last_received")
                .unwrap_or(Value::Null)
        };
        let generation = if observed {
            number(Some(self.generation))
        } else {
            ctx.current(&entity.0.id, "sync_generation")
                .unwrap_or(Value::Null)
        };
        let mut values = vec![
            ("sync_last_received", numeric(), received),
            ("sync_generation", numeric(), generation),
            (
                "sync_poll_interval",
                numeric(),
                number(Some(self.interval(entity))),
            ),
            (
                "sync_last_command_generation",
                numeric(),
                ctx.current(&entity.0.id, "sync_last_command_generation")
                    .unwrap_or(Value::Null),
            ),
        ];
        if gateway {
            values.extend([
                ("sync_last_attempt", numeric(), number(self.last_attempt)),
                ("sync_last_response", numeric(), number(self.last_response)),
                ("sync_last_success", numeric(), number(self.last_success)),
                ("sync_last_cycle", numeric(), number(self.last_cycle)),
                ("sync_in_progress", Kind::Binary, Value::Bool(self.active)),
                (
                    "sync_last_error",
                    Kind::Text,
                    Value::Text(self.last_error.clone().into()),
                ),
            ]);
        }
        for (key, kind, value) in values {
            entity.0.points.retain(|point| point.key.as_ref() != key);
            entity.1.retain(|(name, _)| name != key);
            entity.0.points.push(PointSpec {
                key: key.into(),
                label: moli_i18n::tr(&format!("airzone.{key}")).into(),
                kind,
                unit: None,
                semantic: Semantic::Other,
                access: Access {
                    read: true,
                    write: false,
                },
            });
            entity.1.push((key.into(), value));
        }
    }
}

fn numeric() -> Kind {
    Kind::Numeric {
        min: Some(0.),
        max: None,
        step: Some(1.),
    }
}

fn number(value: Option<u64>) -> Value {
    value
        .and_then(|value| i64::try_from(value).ok())
        .map_or(Value::Null, Value::Int)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_core::{Device, DeviceId};

    #[test]
    fn expected_poll_interval_accounts_for_optional_family_and_target_rotation() {
        let progress = Progress {
            poll_interval: 5,
            optional_systems: 3,
            iaq_systems: 5,
            ..Progress::default()
        };
        for (id, model, expected) in [
            ("az:aabbccddeeff_s1_z1", "Local API", 5),
            ("az:aabbccddeeff_s1_z1_control", "Airzone system", 5),
            ("az:aabbccddeeff_gateway", "Airzone Webserver", 5),
            ("az:aabbccddeeff_dhw", "Airzone Local API", 25),
            ("az:aabbccddeeff_s1_system", "Airzone Local API", 100),
            ("az:aabbccddeeff_s1_iaq1", "Airzone Local API", 125),
        ] {
            let entity = (
                Device {
                    id: DeviceId::from(id),
                    instance: "az".into(),
                    native_name: "Fixture".into(),
                    manufacturer: None,
                    model: Some(model.into()),
                    description: None,
                    native_room: None,
                    members: vec![],
                    points: vec![],
                },
                vec![],
            );
            assert_eq!(progress.interval(&entity), expected, "{id}");
        }
    }
}

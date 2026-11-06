//! Validated native bindings and one conversion for readings and numeric writes.
use moli_core::Value;
use serde_json::{Value as Json, json};

#[derive(Clone, Copy, Debug)]
pub(super) enum Family {
    Closure,
    Deployment,
}
impl Family {
    pub fn state(self) -> &'static str {
        match self {
            Self::Closure => "core:ClosureState",
            Self::Deployment => "core:DeploymentState",
        }
    }
    pub fn setter(self) -> &'static str {
        match self {
            Self::Closure => "setClosure",
            Self::Deployment => "setDeployment",
        }
    }
    pub fn convert(self, value: f64) -> Option<f64> {
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return None;
        }
        Some(match self {
            Self::Closure => 100.0 - value,
            Self::Deployment => value,
        })
    }
}
#[derive(Debug)]
enum Parameters {
    Empty,
    RtsDuration,
    Percent(Family),
    SlowClosure(Option<f64>),
}
#[derive(Debug)]
pub(super) struct Binding {
    name: &'static str,
    parameters: Parameters,
}
impl Binding {
    pub fn empty(name: &'static str) -> Self {
        Self {
            name,
            parameters: Parameters::Empty,
        }
    }
    pub fn rts(name: &'static str) -> Self {
        Self {
            name,
            parameters: Parameters::RtsDuration,
        }
    }
    pub fn percent(name: &'static str, family: Family) -> Self {
        Self {
            name,
            parameters: Parameters::Percent(family),
        }
    }
    pub fn slow_closure(target: Option<f64>) -> Self {
        Self {
            name: "setClosureAndLinearSpeed",
            parameters: Parameters::SlowClosure(target),
        }
    }
    pub fn encode(&self, value: &Value) -> Option<Json> {
        let parameters = match self.parameters {
            Parameters::Empty => json!([]),
            Parameters::RtsDuration => json!([0]),
            Parameters::Percent(family) => json!([family.convert(value.as_f64()?)?]),
            Parameters::SlowClosure(target) => {
                let opening = target.or_else(|| value.as_f64())?;
                json!([Family::Closure.convert(opening)?, "lowspeed"])
            }
        };
        Some(json!({"name":self.name,"parameters":parameters}))
    }
}

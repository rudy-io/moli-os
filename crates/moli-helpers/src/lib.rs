//! Helpers: virtual devices the house keeps for itself, like Home
//! Assistant's `input_select` / `input_boolean` / `input_number` / `input_text`
//! (« mode de la maison », « alarme active »…), plus computed aggregates
//! (the mean, minimum or maximum of the house's own points — the Jeedom
//! « capteur virtuel » equivalent).
//!
//! Each stored helper is a device of this instance with one writable point.
//! A command simply becomes its new value (guard and journal apply, as for
//! any device); the value survives restarts through the hub's state cache,
//! and a helper never seen before starts at its `initial` value.
//!
//! An `aggregate` helper is read-only and computed: it keeps the statistic
//! (`avg` by default, or `min` / `max`) of its configured source points. It
//! recomputes whenever one of them changes, after a bus lag, and on a
//! 30-second rescan (the staleness cutoff is time-based, and a source can
//! come back without an event). A source with no value, or with a last
//! reading older than `stale_s`, is left out of the statistic; when none
//! remains, the last published value is kept. The result is rounded to two
//! decimals and only published when it changed.
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
//! [[driver.options.helper]]
//! id = "temp_maison"
//! name = "Température moyenne"
//! kind = "aggregate"
//! op = "avg"          # or "min" / "max"
//! unit = "°C"
//! semantic = "temperature"
//! stale_s = 7200      # ignore a source silent for more than 2 h
//! points = ["z2m:0x00124b0000000001/temperature", "zwave:7/temperature"]
//! ```

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{bail, ensure};
use moli_core::{
    Access, Device, DeviceId, Event, Kind, PointId, Semantic, Unit, Value, now_ms, validate,
};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

/// How often an aggregate rescans the house's state: a safety net between
/// state events (staleness cutoffs are time-based, a source can come back
/// without an event, and boot order is racy).
const RESCAN_EVERY_SECS: u64 = 30;

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
    /// The value before anyone set one (stored helpers only).
    #[serde(default)]
    pub initial: Option<toml::Value>,
    /// Room, if it belongs to one (most belong to the whole house).
    #[serde(default)]
    pub room: Option<String>,
    /// `aggregate`: the source points, as `"instance:device/key"`.
    #[serde(default)]
    pub points: Vec<String>,
    /// `aggregate`: which statistic. Default `avg`.
    #[serde(default)]
    pub op: Op,
    /// `aggregate`: ignore source readings older than this many seconds.
    #[serde(default)]
    pub stale_s: Option<u64>,
    /// Unit symbol for `number` and `aggregate` points ("°C", "Wh", …).
    #[serde(default)]
    pub unit: Option<String>,
    /// Meaning of the point ("temperature", "power", …). Default `other`.
    #[serde(default)]
    pub semantic: Option<Semantic>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelperKind {
    Select,
    Switch,
    Number,
    Text,
    /// Read-only, computed from the house's own points.
    Aggregate,
}

/// Which statistic an aggregate computes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    #[default]
    Avg,
    Min,
    Max,
}

impl Helper {
    /// The point that holds its value.
    fn spec(&self) -> moli_core::PointSpec {
        let (key, kind, semantic, write) = match self.kind {
            HelperKind::Select => (
                "state",
                Kind::Enum {
                    values: self.options.iter().map(|o| o.as_str().into()).collect(),
                },
                Semantic::Other,
                true,
            ),
            HelperKind::Switch => ("on", Kind::Binary, Semantic::OnOff, true),
            HelperKind::Number => (
                "value",
                Kind::Numeric {
                    min: self.min,
                    max: self.max,
                    step: self.step,
                },
                self.semantic.unwrap_or(Semantic::Other),
                true,
            ),
            HelperKind::Text => ("value", Kind::Text, Semantic::Other, true),
            HelperKind::Aggregate => (
                "value",
                Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                self.semantic.unwrap_or(Semantic::Other),
                false,
            ),
        };
        // An aggregate or a number can carry a unit; the others cannot.
        let unit = matches!(self.kind, HelperKind::Number | HelperKind::Aggregate)
            .then(|| self.unit.as_deref().and_then(Unit::parse))
            .flatten();
        moli_core::PointSpec {
            key: key.into(),
            label: self.name.as_str().into(),
            kind,
            access: Access { read: true, write },
            unit,
            semantic,
        }
    }

    /// The starting value, checked against the point.
    fn initial_value(&self) -> anyhow::Result<Value> {
        let raw = match (&self.initial, self.kind) {
            (_, HelperKind::Aggregate) => {
                bail!(
                    "helper {}: aggregates compute their value and take no initial",
                    self.id
                )
            }
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

/// One aggregate's inputs, parsed once at configuration time.
#[derive(Debug)]
struct AggPlan {
    id: String,
    points: Vec<PointId>,
    op: Op,
    stale_s: Option<u64>,
}

#[derive(Debug)]
pub struct Helpers {
    config: Config,
    hub: Option<Hub>,
    aggregates: Vec<AggPlan>,
    /// Every source point of every aggregate (the event filter).
    tracked: HashSet<PointId>,
}

impl Helpers {
    /// Checks the configuration (ids, options, initial values, source points).
    pub fn new(config: Config, hub: Option<Hub>) -> anyhow::Result<Self> {
        let mut seen = HashSet::new();
        let mut aggregates: Vec<AggPlan> = Vec::new();
        let mut tracked: HashSet<PointId> = HashSet::new();
        for h in &config.helpers {
            ensure!(
                !h.id.is_empty() && !h.id.contains([':', '/']),
                "helper id {:?}: non-empty, without ':' nor '/'",
                h.id
            );
            ensure!(seen.insert(&h.id), "helper id {:?} twice", h.id);
            if h.kind == HelperKind::Aggregate {
                ensure!(
                    !h.points.is_empty(),
                    "helper {}: an aggregate needs at least one point",
                    h.id
                );
                ensure!(
                    hub.is_some(),
                    "helper {}: aggregates read the hub's state; this driver was built without one",
                    h.id
                );
                let mut point_seen: HashSet<&str> = HashSet::new();
                let mut points: Vec<PointId> = Vec::with_capacity(h.points.len());
                for p in &h.points {
                    ensure!(
                        PointId::from(p.as_str()).split().is_some(),
                        "helper {}: point {:?} is malformed (expected \"instance:device/key\")",
                        h.id,
                        p
                    );
                    ensure!(
                        point_seen.insert(p.as_str()),
                        "helper {}: point {:?} listed twice",
                        h.id,
                        p
                    );
                    let point = PointId::from(p.as_str());
                    tracked.insert(point.clone());
                    points.push(point);
                }
                aggregates.push(AggPlan {
                    id: h.id.clone(),
                    points,
                    op: h.op,
                    stale_s: h.stale_s,
                });
            } else {
                ensure!(
                    h.kind != HelperKind::Select || !h.options.is_empty(),
                    "helper {}: a select needs options",
                    h.id
                );
                h.initial_value()?;
            }
        }
        Ok(Self {
            config,
            hub,
            aggregates,
            tracked,
        })
    }

    /// Recomputes every aggregate from the house's current state and
    /// publishes a new value only when the rounded result changed (the hub
    /// then broadcasts it, as for any point).
    fn recompute(&self, ctx: &DriverCtx) {
        let Some(hub) = self.hub.as_ref() else {
            return;
        };
        let now = now_ms();
        for agg in &self.aggregates {
            let mut samples: Vec<f64> = Vec::new();
            for point in &agg.points {
                let Some(sample) = hub.state(point) else {
                    continue;
                };
                let Some(value) = sample.value.as_f64() else {
                    continue;
                };
                let is_stale = agg.stale_s.is_some_and(|stale_s| {
                    now.saturating_sub(sample.ts) >= stale_s.saturating_mul(1000)
                });
                if is_stale {
                    continue;
                }
                samples.push(value);
            }
            if samples.is_empty() {
                continue;
            }
            let seed = *samples.first().expect("samples is non-empty here");
            let value = match agg.op {
                Op::Avg => {
                    #[allow(clippy::cast_precision_loss)]
                    let count = samples.len() as f64;
                    samples.iter().sum::<f64>() / count
                }
                Op::Min => samples.iter().skip(1).fold(seed, |a, &b| a.min(b)),
                Op::Max => samples.iter().skip(1).fold(seed, |a, &b| a.max(b)),
            };
            // Two decimals: publish only when a reader could tell the change.
            let value = (value * 100.0).round() / 100.0;
            let id = ctx.device_id(&agg.id);
            if ctx.current(&id, "value") != Some(Value::Float(value)) {
                ctx.set_state(&id, "value", Value::Float(value));
            }
        }
    }
}

impl Driver for Helpers {
    fn kind(&self) -> &'static str {
        "helpers"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            for h in &self.config.helpers {
                let spec = h.spec();
                let id = ctx.device_id(&h.id);
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
                if h.kind != HelperKind::Aggregate {
                    // Kept across restarts by the state cache; set only once.
                    if ctx.current(&id, &spec.key).is_none() {
                        ctx.set_state(&id, &spec.key, h.initial_value()?);
                    }
                }
                ctx.set_availability(&id, true);
            }
            ctx.ready();
            if self.aggregates.is_empty() {
                while let Some(command) = ctx.next_command().await {
                    // Already validated against the point by the hub.
                    ctx.set_state(&command.device.id, &command.key, command.value.clone());
                    command.reply(Ok(()));
                }
                return Ok(());
            }
            // Subscribe before the first recompute: nothing falls in between.
            let hub = self
                .hub
                .clone()
                .expect("aggregate helpers are refused without a hub at construction");
            let mut events = hub.subscribe();
            self.recompute(ctx);
            let mut rescan = tokio::time::interval(Duration::from_secs(RESCAN_EVERY_SECS));
            rescan.tick().await; // the first tick is immediate: drop it
            loop {
                tokio::select! {
                    command = ctx.next_command() => match command {
                        Some(command) => {
                            // Already validated against the point by the hub.
                            ctx.set_state(&command.device.id, &command.key, command.value.clone());
                            command.reply(Ok(()));
                        }
                        None => return Ok(()),
                    },
                    event = events.recv() => match event {
                        Ok(Event::State { point, .. }) if self.tracked.contains(&point) => {
                            self.recompute(ctx);
                        }
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => self.recompute(ctx),
                        Err(RecvError::Closed) => return Ok(()),
                    },
                    _ = rescan.tick() => self.recompute(ctx),
                }
            }
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

    fn aggregate_helper(points: &[&str], op: &str, stale_s: Option<u64>) -> Config {
        let points_toml = points
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        toml::from_str(&format!(
            r#"
                [[helper]]
                id = "temp"
                name = "Température moyenne"
                kind = "aggregate"
                op = "{op}"
                unit = "°C"
                semantic = "temperature"
                points = [{points_toml}]
                {stale}
                "#,
            op = op,
            stale = match stale_s {
                Some(s) => format!("stale_s = {s}\n"),
                None => String::new(),
            }
        ))
        .unwrap()
    }

    /// Polls a point until it holds `want` (or gives up after 2 s).
    async fn wait_value(hub: &Hub, point: &str, want: &Value) -> bool {
        let pid = PointId::from(point);
        for _ in 0..200 {
            if hub.state(&pid).is_some_and(|s| s.value == *want) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Polls a point until any sample exists (or gives up after 2 s).
    /// The driver declares its devices and initial values asynchronously, so
    /// commands must only be sent once the point is known to the hub.
    async fn wait_state_present(hub: &Hub, point: &str) -> bool {
        let pid = PointId::from(point);
        for _ in 0..200 {
            if hub.state(&pid).is_some() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[test]
    fn bad_configurations_are_refused() {
        let mut c = config();
        c.helpers[0].initial = Some(toml::Value::String("Fête".into()));
        assert!(
            Helpers::new(c, None).is_err(),
            "initial outside the options"
        );
        let mut c = config();
        c.helpers[1].id = "a:b".into();
        assert!(Helpers::new(c, None).is_err());
        let mut c = config();
        c.helpers[0].options.clear();
        assert!(Helpers::new(c, None).is_err());
    }

    #[test]
    fn aggregate_configuration_is_checked() {
        let hub = Hub::new(HubOptions::default()).unwrap();
        let base = aggregate_helper(&["z2m:0x00124b0000000001/temperature"], "avg", None);
        // No points: refused.
        let mut c = base.clone();
        c.helpers[0].points.clear();
        assert!(Helpers::new(c, Some(hub.clone())).is_err());
        // Malformed point (no "device/key"): refused.
        let mut c = base.clone();
        c.helpers[0].points = vec!["no-slash".into()];
        assert!(Helpers::new(c, Some(hub.clone())).is_err());
        // Point listed twice: refused.
        let mut c = base.clone();
        c.helpers[0].points = vec!["z2m:0x00124b0000000001/temperature".into(); 2];
        assert!(Helpers::new(c, Some(hub.clone())).is_err());
        // Aggregates read the hub: refused without it.
        assert!(Helpers::new(base.clone(), None).is_err());
        // With a hub: accepted.
        assert!(Helpers::new(base, Some(hub)).is_ok());
    }

    #[tokio::test]
    async fn helpers_start_at_their_initial_value_and_take_orders() {
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("maison"),
            std::sync::Arc::new(Helpers::new(config(), Some(hub.clone())).unwrap()),
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

    #[tokio::test]
    async fn aggregate_helper_averages_the_source_points() {
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let sources = toml::from_str::<Config>(
            r#"
            [[helper]]
            id = "t1"
            name = "T1"
            kind = "number"
            min = 0.0
            max = 40.0
            [[helper]]
            id = "t2"
            name = "T2"
            kind = "number"
            min = 0.0
            max = 40.0
            "#,
        )
        .unwrap();
        spawn_driver(
            &hub,
            InstanceId::from("src"),
            std::sync::Arc::new(Helpers::new(sources, Some(hub.clone())).unwrap()),
            cancel.clone(),
        );
        spawn_driver(
            &hub,
            InstanceId::from("agg"),
            std::sync::Arc::new(
                Helpers::new(
                    aggregate_helper(&["src:t1/value", "src:t2/value"], "avg", None),
                    Some(hub.clone()),
                )
                .unwrap(),
            ),
            cancel.clone(),
        );
        // Number helpers publish their initial value (0.0) on startup, so the
        // aggregate has something to average from the very beginning: wait
        // for the pipeline to produce the mean of the two initial values.
        assert!(
            wait_value(&hub, "agg:temp/value", &Value::Float(0.0)).await,
            "mean of the two initial values"
        );

        hub.command(
            &PointId::from("src:t1/value"),
            Value::Float(20.0),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        hub.command(
            &PointId::from("src:t2/value"),
            Value::Float(24.0),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert!(
            wait_value(&hub, "agg:temp/value", &Value::Float(22.0)).await,
            "mean of 20 and 24"
        );
        // The point carries the configured unit and meaning, read-only.
        let view = hub
            .device(&DeviceId::from("agg:temp"))
            .expect("aggregate device published");
        let point = view
            .device
            .points
            .iter()
            .find(|p| &*p.key == "value")
            .expect("one point named value");
        assert_eq!(point.unit, Some(Unit::Celsius));
        assert_eq!(point.semantic, Semantic::Temperature);
        assert!(!point.access.write);
        cancel.cancel();
    }

    #[tokio::test]
    async fn aggregate_helper_computes_min_and_max() {
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let sources = toml::from_str::<Config>(
            r#"
            [[helper]]
            id = "a"
            name = "A"
            kind = "number"
            min = 0.0
            max = 40.0
            [[helper]]
            id = "b"
            name = "B"
            kind = "number"
            min = 0.0
            max = 40.0
            [[helper]]
            id = "c"
            name = "C"
            kind = "number"
            min = 0.0
            max = 40.0
            "#,
        )
        .unwrap();
        spawn_driver(
            &hub,
            InstanceId::from("src"),
            std::sync::Arc::new(Helpers::new(sources, Some(hub.clone())).unwrap()),
            cancel.clone(),
        );
        let agg = toml::from_str::<Config>(
            r#"
            [[helper]]
            id = "lo"
            name = "Min"
            kind = "aggregate"
            op = "min"
            points = ["src:a/value", "src:b/value", "src:c/value"]
            [[helper]]
            id = "hi"
            name = "Max"
            kind = "aggregate"
            op = "max"
            points = ["src:a/value", "src:b/value", "src:c/value"]
            "#,
        )
        .unwrap();
        spawn_driver(
            &hub,
            InstanceId::from("agg"),
            std::sync::Arc::new(Helpers::new(agg, Some(hub.clone())).unwrap()),
            cancel.clone(),
        );
        // Wait until the sources are declared before ordering them.
        for key in ["a", "b", "c"] {
            assert!(
                wait_state_present(&hub, &format!("src:{key}/value")).await,
                "source {key} declared"
            );
        }
        for (key, value) in [("a", 10.0), ("b", 30.0), ("c", 20.0)] {
            hub.command(
                &PointId::from(format!("src:{key}/value")),
                Value::Float(value),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
        }
        assert!(
            wait_value(&hub, "agg:lo/value", &Value::Float(10.0)).await,
            "min of 10, 30, 20"
        );
        assert!(
            wait_value(&hub, "agg:hi/value", &Value::Float(30.0)).await,
            "max of 10, 30, 20"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn aggregate_helper_skips_stale_sources() {
        // stale_s = 0: every reading is older than zero seconds, so nothing
        // counts and nothing is ever published.
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let sources = toml::from_str::<Config>(
            r#"
            [[helper]]
            id = "t1"
            name = "T1"
            kind = "number"
            min = 0.0
            max = 40.0
            "#,
        )
        .unwrap();
        spawn_driver(
            &hub,
            InstanceId::from("src"),
            std::sync::Arc::new(Helpers::new(sources, Some(hub.clone())).unwrap()),
            cancel.clone(),
        );
        spawn_driver(
            &hub,
            InstanceId::from("agg"),
            std::sync::Arc::new(
                Helpers::new(
                    aggregate_helper(&["src:t1/value"], "avg", Some(0)),
                    Some(hub.clone()),
                )
                .unwrap(),
            ),
            cancel.clone(),
        );
        assert!(
            wait_state_present(&hub, "src:t1/value").await,
            "source declared"
        );
        hub.command(
            &PointId::from("src:t1/value"),
            Value::Float(20.0),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            hub.state(&PointId::from("agg:temp/value")).is_none(),
            "a fully stale aggregate publishes nothing"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn aggregate_helper_ignores_points_without_a_value() {
        // The second source is a point that never reports (its device does not
        // exist): the statistic simply uses the rest.
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let sources = toml::from_str::<Config>(
            r#"
            [[helper]]
            id = "t1"
            name = "T1"
            kind = "number"
            min = 0.0
            max = 40.0
            "#,
        )
        .unwrap();
        spawn_driver(
            &hub,
            InstanceId::from("src"),
            std::sync::Arc::new(Helpers::new(sources, Some(hub.clone())).unwrap()),
            cancel.clone(),
        );
        spawn_driver(
            &hub,
            InstanceId::from("agg"),
            std::sync::Arc::new(
                Helpers::new(
                    aggregate_helper(&["src:t1/value", "ghost:missing/value"], "avg", None),
                    Some(hub.clone()),
                )
                .unwrap(),
            ),
            cancel.clone(),
        );
        assert!(
            wait_state_present(&hub, "src:t1/value").await,
            "source declared"
        );
        hub.command(
            &PointId::from("src:t1/value"),
            Value::Float(20.0),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert!(
            wait_value(&hub, "agg:temp/value", &Value::Float(20.0)).await,
            "the missing point is simply left out"
        );
        cancel.cancel();
    }
}

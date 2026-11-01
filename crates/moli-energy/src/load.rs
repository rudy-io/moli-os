//! Driven loads: a water heater (or any load that stores energy) switched by
//! a relay, run on the panels' surplus and topped up in off-peak hours.
//!
//! Deterministic (never a model in the loop): every [`TICK`] the controller
//! reads the grid's signed power, the relay and the tariff period, and
//! decides. Each load is a device `charges:<id>` with a `mode` the family
//! chooses (auto, off-peak, on, off) and what Moli is doing and why. Orders
//! go through the hub as `system` (guard and journal), like a light fixture.
//!
//! In `auto`:
//! - the relay closes when the home has sent at least the load's power to
//!   the grid for [`SUSTAINED`], and opens when it has bought more than a
//!   quarter of it for as long (never sooner than `min_on` / `min_off`);
//! - a cycle runs from the end of one off-peak stretch to the end of the
//!   next: when it is off-peak and the load ran less than `daily_minutes` in
//!   the cycle, it runs (hot water every morning, sun or not);
//! - without a grid reading (the clamp offline), it falls back to off-peak.

use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use moli_core::{
    Access, Device, DeviceId, InstanceId, Kind, Origin, PointId, PointSpec, Semantic, Unit, Value,
    now_ms,
};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub};
use serde::Deserialize;

use crate::config::period_key;

/// The instance (and the devices' prefix: `charges:<id>`).
pub const INSTANCE: &str = "charges";
const TICK: Duration = Duration::from_secs(15);
/// How long a surplus (or a purchase) must last before the relay moves.
const SUSTAINED_MS: u64 = 3 * 60_000;
const MINUTE_MS: u64 = 60_000;

pub const MODES: [&str; 4] = ["auto", "heures_creuses", "marche", "arret"];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadConfig {
    /// Device id (`charges:<id>`): never rename it.
    pub id: String,
    pub name: String,
    /// The relay's on/off point (`relais:…_switch_0/on`).
    pub switch: String,
    /// The main feed's signed power (W): positive when the home buys,
    /// negative when it sends out (a Shelly Pro 3EM's `power`).
    pub grid: String,
    /// What the load draws when it heats (W).
    pub power_w: f64,
    /// Off-peak stretches, local time (`"22:00-06:00"`). Without them, the
    /// tariff's period point says it (a period named « HC… »).
    #[serde(default)]
    pub offpeak: Vec<String>,
    /// Run at least this long per cycle; topped up in off-peak hours.
    #[serde(default)]
    pub daily_minutes: u32,
    #[serde(default = "default_min")]
    pub min_on_minutes: u32,
    #[serde(default = "default_min")]
    pub min_off_minutes: u32,
}

fn default_min() -> u32 {
    10
}

/// An off-peak stretch in minutes after local midnight; `end < start`
/// crosses midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stretch {
    start: u32,
    end: u32,
}

impl Stretch {
    pub fn parse(text: &str) -> Result<Self, String> {
        let minutes = |hm: &str| -> Option<u32> {
            let (h, m) = hm.trim().split_once(':')?;
            let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
            (h < 24 && m < 60).then_some(h * 60 + m)
        };
        let (a, b) = text
            .split_once('-')
            .ok_or_else(|| format!("{text:?}: expected \"HH:MM-HH:MM\""))?;
        match (minutes(a), minutes(b)) {
            (Some(start), Some(end)) if start != end => Ok(Self { start, end }),
            _ => Err(format!("{text:?}: expected \"HH:MM-HH:MM\"")),
        }
    }

    fn contains(self, minute: u32) -> bool {
        if self.start < self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

impl LoadConfig {
    pub fn check(&self) -> Result<(), String> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "load id {:?}: letters, digits, '_' or '-' only",
                self.id
            ));
        }
        for point in [&self.switch, &self.grid] {
            match point.split_once('/') {
                Some((device, key)) if device.contains(':') && !key.is_empty() => {}
                _ => return Err(format!("load {:?}: {point:?} is not a point id", self.id)),
            }
        }
        if !(self.power_w.is_finite() && self.power_w > 0.0) {
            return Err(format!("load {:?}: power_w must be above 0", self.id));
        }
        for s in &self.offpeak {
            Stretch::parse(s).map_err(|e| format!("load {:?}: offpeak {e}", self.id))?;
        }
        Ok(())
    }

    fn stretches(&self) -> Vec<Stretch> {
        self.offpeak
            .iter()
            .filter_map(|s| Stretch::parse(s).ok())
            .collect()
    }
}

/// What the controller sees at one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    pub now: u64,
    /// Signed power at the main feed (W), if known.
    pub grid_w: Option<f64>,
    /// The relay, if known.
    pub on: Option<bool>,
    pub offpeak: bool,
}

/// What the controller remembers between ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Memory {
    /// When the relay last moved (as seen, or as ordered).
    pub changed: u64,
    surplus_since: Option<u64>,
    buying_since: Option<u64>,
    /// Time run in the current cycle.
    pub ran_ms: u64,
    last_tick: Option<u64>,
    was_offpeak: bool,
    /// The relay as last seen (a change by hand restarts `changed`).
    on_seen: Option<bool>,
}

/// Why the load is (or is about to be) on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Forced,
    Stopped,
    OffPeak,
    Surplus,
    Buying,
    TopUp,
    Waiting,
    NoGrid,
    Holding,
}

impl Reason {
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Forced => "marche_forcee",
            Self::Stopped => "arret",
            Self::OffPeak => "heures_creuses",
            Self::Surplus => "surplus_solaire",
            Self::Buying => "achat_reseau",
            Self::TopUp => "complement_heures_creuses",
            Self::Waiting => "attente_surplus",
            Self::NoGrid => "compteur_absent",
            Self::Holding => "duree_minimale",
        }
    }
}

/// One tick: the relay state wanted (`None` = leave it), and why.
pub fn decide(
    load: &LoadConfig,
    mode: &str,
    memory: &mut Memory,
    seen: Seen,
) -> (Option<bool>, Reason) {
    // Time run, and the cycle: it ends when an off-peak stretch ends.
    if let (Some(last), Some(true)) = (memory.last_tick, seen.on) {
        memory.ran_ms += seen.now.saturating_sub(last).min(5 * MINUTE_MS);
    }
    if memory.was_offpeak && !seen.offpeak {
        memory.ran_ms = 0;
    }
    memory.last_tick = Some(seen.now);
    memory.was_offpeak = seen.offpeak;

    let wanted = |on: bool, reason| (Some(on), reason);
    match mode {
        "marche" => return wanted(true, Reason::Forced),
        "arret" => return wanted(false, Reason::Stopped),
        "heures_creuses" => return wanted(seen.offpeak, Reason::OffPeak),
        _ => {}
    }
    let on = seen.on.unwrap_or(false);
    let top_up = seen.offpeak && memory.ran_ms < u64::from(load.daily_minutes) * MINUTE_MS;
    let Some(grid) = seen.grid_w else {
        memory.surplus_since = None;
        memory.buying_since = None;
        return wanted(
            seen.offpeak && (top_up || load.daily_minutes == 0),
            Reason::NoGrid,
        );
    };
    // Surplus as if the load were off: what it draws now goes back out.
    let surplus = if on { load.power_w - grid } else { -grid };
    let held = |since: &mut Option<u64>, now: u64, true_now: bool| {
        if !true_now {
            *since = None;
            return false;
        }
        let at = *since.get_or_insert(now);
        now.saturating_sub(at) >= SUSTAINED_MS
    };
    let surplus_held = held(
        &mut memory.surplus_since,
        seen.now,
        surplus >= load.power_w * 0.9,
    );
    let buying_held = held(
        &mut memory.buying_since,
        seen.now,
        on && grid > load.power_w / 4.0,
    );
    let since_change = seen.now.saturating_sub(memory.changed);
    if on {
        if top_up {
            return (None, Reason::TopUp);
        }
        if buying_held {
            if since_change < u64::from(load.min_on_minutes) * MINUTE_MS {
                return (None, Reason::Holding);
            }
            return wanted(false, Reason::Buying);
        }
        (None, Reason::Surplus)
    } else {
        if top_up {
            return wanted(true, Reason::TopUp);
        }
        if surplus_held {
            if since_change < u64::from(load.min_off_minutes) * MINUTE_MS {
                return (None, Reason::Holding);
            }
            return wanted(true, Reason::Surplus);
        }
        (None, Reason::Waiting)
    }
}

/// The driver running every load of `[[energy.load]]`.
#[derive(Debug)]
pub struct Loads {
    hub: Hub,
    configs: Vec<LoadConfig>,
    tz: TimeZone,
    /// The tariff's period point (« HP.. » / « HC.. »), when declared.
    period: Option<String>,
}

impl Loads {
    #[must_use]
    pub fn new(hub: Hub, loads: Vec<LoadConfig>, tz: TimeZone, period: Option<String>) -> Self {
        Self {
            hub,
            configs: loads,
            tz,
            period,
        }
    }

    fn number(&self, point: &str) -> Option<f64> {
        let state = self.hub.state(&PointId::from(point.to_owned()))?;
        state.value.as_f64()
    }

    fn switch(&self, point: &str) -> Option<bool> {
        match self.hub.state(&PointId::from(point.to_owned()))?.value {
            Value::Bool(b) => Some(b),
            Value::Text(ref t) => match t.to_ascii_lowercase().as_str() {
                "on" | "true" => Some(true),
                "off" | "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    fn offpeak(&self, load: &LoadConfig, now: u64) -> bool {
        let stretches = load.stretches();
        if !stretches.is_empty() {
            let minute = i64::try_from(now)
                .ok()
                .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
                .map(|t| t.to_zoned(self.tz.clone()))
                .map_or(0, |z| {
                    u32::try_from(i32::from(z.hour()) * 60 + i32::from(z.minute())).unwrap_or(0)
                });
            return stretches.iter().any(|s| s.contains(minute));
        }
        self.period
            .as_deref()
            .and_then(|p| self.hub.state(&PointId::from(p.to_owned())))
            .is_some_and(
                |s| matches!(s.value, Value::Text(ref t) if period_key(t).starts_with("hc")),
            )
    }
}

fn device(id: DeviceId, instance: &InstanceId, load: &LoadConfig) -> Device {
    let read = Access {
        read: true,
        write: false,
    };
    Device {
        id,
        instance: instance.clone(),
        native_name: load.name.as_str().into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("serveur.energie.charge.modele").into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            PointSpec {
                key: "mode".into(),
                label: moli_i18n::tr!("serveur.energie.charge.mode").into(),
                kind: Kind::Enum {
                    values: MODES.iter().map(|m| Arc::from(*m)).collect(),
                },
                access: Access {
                    read: true,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Config,
            },
            PointSpec {
                key: "reason".into(),
                label: moli_i18n::tr!("serveur.energie.charge.raison").into(),
                kind: Kind::Text,
                access: read,
                unit: None,
                semantic: Semantic::Other,
            },
            PointSpec {
                key: "ran_minutes".into(),
                label: moli_i18n::tr!("serveur.energie.charge.duree").into(),
                kind: Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                access: read,
                unit: Some(Unit::Minute),
                semantic: Semantic::Other,
            },
        ],
    }
}

impl Driver for Loads {
    fn kind(&self) -> &'static str {
        "loads"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let ids: Vec<DeviceId> = self.configs.iter().map(|l| ctx.device_id(&l.id)).collect();
            for (id, load) in ids.iter().zip(&self.configs) {
                ctx.upsert_device(device(id.clone(), ctx.instance(), load));
                // The family's choice survives restarts (state cache).
                if ctx.current(id, "mode").is_none() {
                    ctx.set_state(id, "mode", Value::Text("auto".into()));
                }
                ctx.set_availability(id, true);
            }
            ctx.ready();
            let mut memories = vec![Memory::default(); self.configs.len()];
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    command = ctx.next_command() => {
                        let Some(command) = command else { return Ok(()) };
                        // Only `mode` is writable; the hub checked the value.
                        ctx.set_state(&command.device.id, &command.key, command.value.clone());
                        command.reply(Ok(()));
                    }
                    _ = tick.tick() => {
                        for ((id, load), memory) in ids.iter().zip(&self.configs).zip(&mut memories) {
                            self.step(ctx, id, load, memory).await;
                        }
                    }
                }
            }
        })
    }
}

impl Loads {
    async fn step(&self, ctx: &DriverCtx, id: &DeviceId, load: &LoadConfig, memory: &mut Memory) {
        let now = now_ms();
        let mode = match ctx.current(id, "mode") {
            Some(Value::Text(m)) => m.to_string(),
            _ => "auto".to_owned(),
        };
        let on = self.switch(&load.switch);
        if memory.last_tick.is_some() && on.is_some() && memory.on_seen != on {
            memory.changed = now; // moved by hand, or by the order below
        }
        memory.on_seen = on;
        let seen = Seen {
            now,
            grid_w: self.number(&load.grid),
            on,
            offpeak: self.offpeak(load, now),
        };
        let (order, reason) = decide(load, &mode, memory, seen);
        ctx.set_state(id, "reason", Value::Text(reason.key().into()));
        #[allow(clippy::cast_precision_loss)]
        ctx.set_state(
            id,
            "ran_minutes",
            Value::Float((memory.ran_ms / MINUTE_MS) as f64),
        );
        if let Some(wanted) = order
            && on != Some(wanted)
        {
            let point = PointId::from(load.switch.clone());
            let actor = format!("charge « {} »", load.name);
            match self
                .hub
                .command(&point, Value::Bool(wanted), Origin::System, Some(actor))
                .await
            {
                Ok(()) => memory.changed = now,
                Err(e) => tracing::warn!(load = load.id, error = %e, "driven load: order refused"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heater() -> LoadConfig {
        LoadConfig {
            id: "chauffe_eau".into(),
            name: "Chauffe-eau".into(),
            switch: "relais:r_switch_0/on".into(),
            grid: "pince:p/power".into(),
            power_w: 2400.0,
            offpeak: vec!["22:00-06:00".into()],
            daily_minutes: 120,
            min_on_minutes: 10,
            min_off_minutes: 10,
        }
    }

    fn seen(now_min: u64, grid: Option<f64>, on: bool, offpeak: bool) -> Seen {
        Seen {
            now: 3_600_000 * 100 + now_min * MINUTE_MS,
            grid_w: grid,
            on: Some(on),
            offpeak,
        }
    }

    #[test]
    fn a_lasting_surplus_starts_it_and_a_lasting_purchase_stops_it() {
        let load = heater();
        let mut m = Memory::default();
        // Sending 2.6 kW out: not at once, after three minutes.
        assert_eq!(
            decide(&load, "auto", &mut m, seen(0, Some(-2600.0), false, false)),
            (None, Reason::Waiting)
        );
        assert_eq!(
            decide(&load, "auto", &mut m, seen(2, Some(-2600.0), false, false)),
            (None, Reason::Waiting)
        );
        assert_eq!(
            decide(&load, "auto", &mut m, seen(3, Some(-2600.0), false, false)),
            (Some(true), Reason::Surplus)
        );
        m.changed = seen(3, None, true, false).now;
        // Heating: the feed balances around zero, it stays on.
        assert_eq!(
            decide(&load, "auto", &mut m, seen(10, Some(-100.0), true, false)),
            (None, Reason::Surplus)
        );
        // A cloud: buying 1.5 kW, for long, after the minimum run.
        assert_eq!(
            decide(&load, "auto", &mut m, seen(14, Some(1500.0), true, false)),
            (None, Reason::Surplus)
        );
        assert_eq!(
            decide(&load, "auto", &mut m, seen(17, Some(1500.0), true, false)),
            (Some(false), Reason::Buying)
        );
        // 5 min counted at most between two readings: 5 + 4 + 3.
        assert_eq!(m.ran_ms, 12 * MINUTE_MS);
    }

    #[test]
    fn a_purchase_before_the_minimum_run_waits() {
        let load = heater();
        let mut m = Memory {
            changed: seen(0, None, true, false).now,
            ..Memory::default()
        };
        decide(&load, "auto", &mut m, seen(0, Some(2000.0), true, false));
        assert_eq!(
            decide(&load, "auto", &mut m, seen(4, Some(2000.0), true, false)),
            (None, Reason::Holding)
        );
        assert_eq!(
            decide(&load, "auto", &mut m, seen(10, Some(2000.0), true, false)),
            (Some(false), Reason::Buying)
        );
    }

    #[test]
    fn off_peak_tops_up_what_the_sun_did_not_give() {
        let load = heater();
        let mut m = Memory::default();
        assert_eq!(
            decide(&load, "auto", &mut m, seen(0, Some(300.0), false, true)),
            (Some(true), Reason::TopUp)
        );
        // Two hours of running fill the day's quota: then the usual rules.
        for minute in 1..=120 {
            decide(
                &load,
                "auto",
                &mut m,
                seen(minute, Some(2700.0), true, true),
            );
        }
        assert!(m.ran_ms >= 120 * MINUTE_MS);
        let (order, _) = decide(&load, "auto", &mut m, seen(130, Some(2700.0), true, true));
        assert_eq!(order, Some(false));
        // Off-peak ends: a new cycle starts.
        decide(&load, "auto", &mut m, seen(200, Some(300.0), false, false));
        assert_eq!(m.ran_ms, 0);
    }

    #[test]
    fn without_the_clamp_it_falls_back_to_off_peak() {
        let load = heater();
        let mut m = Memory::default();
        assert_eq!(
            decide(&load, "auto", &mut m, seen(0, None, false, false)),
            (Some(false), Reason::NoGrid)
        );
        assert_eq!(
            decide(&load, "auto", &mut m, seen(1, None, false, true)),
            (Some(true), Reason::NoGrid)
        );
    }

    #[test]
    fn the_family_mode_wins() {
        let load = heater();
        let mut m = Memory::default();
        assert_eq!(
            decide(&load, "marche", &mut m, seen(0, Some(3000.0), false, false)).0,
            Some(true)
        );
        assert_eq!(
            decide(&load, "arret", &mut m, seen(1, Some(-3000.0), true, true)).0,
            Some(false)
        );
        assert_eq!(
            decide(
                &load,
                "heures_creuses",
                &mut m,
                seen(2, Some(-3000.0), false, false)
            )
            .0,
            Some(false)
        );
        assert_eq!(
            decide(
                &load,
                "heures_creuses",
                &mut m,
                seen(3, Some(3000.0), false, true)
            )
            .0,
            Some(true)
        );
    }

    #[test]
    fn stretches_cross_midnight() {
        let s = Stretch::parse("22:07-06:07").unwrap();
        assert!(s.contains(23 * 60));
        assert!(s.contains(3 * 60));
        assert!(!s.contains(12 * 60));
        assert!(Stretch::parse("25:00-06:00").is_err());
        assert!(heater().check().is_ok());
    }
}

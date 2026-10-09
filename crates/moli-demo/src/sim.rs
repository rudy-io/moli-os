//! What makes a demo house live: each read-only point gets a behaviour
//! guessed from its meaning (unit, semantic, key), and orders ripple to the
//! points that depend on them (a lamp switched on draws power, a heat pump
//! pulls the room toward its set point, a meter turns with the power).

use std::collections::HashMap;

use moli_core::{DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::DriverCtx;

use crate::Played;

/// Hour of day and whether it is daylight, in the house's time zone.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    /// 0.0 to 24.0.
    pub hour: f64,
}

impl Clock {
    #[must_use]
    pub fn now(tz: &jiff::tz::TimeZone) -> Self {
        let now = jiff::Timestamp::now().to_zoned(tz.clone());
        let hour = f64::from(now.hour())
            + f64::from(now.minute()) / 60.0
            + f64::from(now.second()) / 3600.0;
        Self { hour }
    }

    /// Between sunrise and sunset (a fixed, gentle approximation).
    #[must_use]
    pub fn daylight(self) -> bool {
        (7.25..20.0).contains(&self.hour)
    }

    /// 0 at night, up to 1 at noon.
    #[must_use]
    pub fn sun(self) -> f64 {
        if !self.daylight() {
            return 0.0;
        }
        let x = (self.hour - 7.25) / (20.0 - 7.25);
        (x * std::f64::consts::PI).sin().max(0.0)
    }

    /// A household's day: low at night, peaks morning and evening (0.5–1.4).
    #[must_use]
    pub fn household(self) -> f64 {
        let bump = |center: f64, width: f64| (-((self.hour - center) / width).powi(2)).exp();
        0.5 + 0.5 * bump(7.5, 1.2) + 0.9 * bump(20.0, 2.0) + 0.3 * bump(13.0, 1.5)
    }
}

/// A small, good-enough random source (no dependency, no cryptography).
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        #[allow(clippy::cast_precision_loss)]
        let f = (x >> 11) as f64 / (1u64 << 53) as f64;
        f
    }

    /// Uniform in [-1, 1].
    fn signed(&mut self) -> f64 {
        self.next().mul_add(2.0, -1.0)
    }

    fn chance(&mut self, p: f64) -> bool {
        self.next() < p
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Behaviour {
    /// Never moves on its own.
    Still,
    /// Wanders around its starting value: ± `span`, by `step` at most.
    Drift {
        center: f64,
        span: f64,
        step: f64,
    },
    /// A room temperature: drifts, and follows the device's set point
    /// while the device runs.
    Room {
        center: f64,
    },
    /// Power drawn: follows the device's switch, or the household's day.
    Power {
        base: f64,
        switched: bool,
    },
    /// A cumulative meter, in Wh (factor 1) or kWh (factor 0.001).
    Meter {
        factor: f64,
    },
    /// Current from the device's power (230 V).
    Current,
    /// Seconds counting up (uptime, play position).
    Counting,
    /// Light level following the sun.
    Sun {
        peak: f64,
    },
    /// Whether it is daylight.
    Daylight,
    /// A detection that comes and goes: chance per minute.
    Pulse {
        per_minute: f64,
    },
    /// A print in progress (percent), with its remaining minutes and layer.
    Progress,
    Remaining,
    Layer,
}

#[derive(Debug)]
struct Point {
    key: String,
    behaviour: Behaviour,
    /// Seconds a pulse still lasts.
    pulse_left: f64,
    /// What a meter gained but cannot show yet (below its resolution).
    carry: f64,
}

#[derive(Debug)]
struct Live {
    id: DeviceId,
    members: Vec<DeviceId>,
    specs: Vec<PointSpec>,
    points: Vec<Point>,
    /// The values as the simulation knows them (orders included).
    values: HashMap<String, Value>,
}

impl Live {
    fn num(&self, key: &str) -> Option<f64> {
        self.values.get(key).and_then(Value::as_f64)
    }

    fn flag(&self, key: &str) -> Option<bool> {
        match self.values.get(key)? {
            Value::Bool(b) => Some(*b),
            Value::Text(t) => Some(matches!(
                &**t,
                "ON" | "on" | "playing" | "PLAYING" | "printing" | "RUNNING" | "running"
            )),
            _ => None,
        }
    }

    /// The switch that says whether the device runs, if it has one.
    fn running(&self) -> Option<bool> {
        for key in ["on", "power", "state", "playing", "printing"] {
            if let Some(spec) = self.specs.iter().find(|s| &*s.key == key)
                && matches!(spec.kind, Kind::Binary)
                && let Some(on) = self.flag(key)
            {
                return Some(on);
            }
        }
        None
    }

    /// The power the device draws now, in W (its power points summed).
    fn watts(&self) -> f64 {
        self.points
            .iter()
            .filter(|p| matches!(p.behaviour, Behaviour::Power { .. }))
            .filter_map(|p| self.num(&p.key))
            .sum()
    }

    fn has(&self, key: &str) -> bool {
        self.specs.iter().any(|s| &*s.key == key)
    }
}

fn readable_number(spec: &PointSpec) -> bool {
    spec.access.read && !spec.access.write && matches!(spec.kind, Kind::Numeric { .. })
}

/// The behaviour of one point, guessed from what it means.
fn behaviour(spec: &PointSpec, start: Option<&Value>, has_switch: bool) -> Behaviour {
    let key = spec.key.to_ascii_lowercase();
    let start_num = start.and_then(Value::as_f64);
    if spec.access.write {
        return Behaviour::Still;
    }
    if matches!(spec.kind, Kind::Binary) {
        return match (spec.semantic, key.as_str()) {
            (_, "daylight") => Behaviour::Daylight,
            (_, "home" | "online" | "connected" | "line" | "charging" | "printing" | "agent") => {
                Behaviour::Still
            }
            (Semantic::Occupancy, _) | (_, "motion") => Behaviour::Pulse { per_minute: 0.08 },
            (_, "person") => Behaviour::Pulse { per_minute: 0.03 },
            (_, "animal") => Behaviour::Pulse { per_minute: 0.015 },
            (_, "vehicle") => Behaviour::Pulse { per_minute: 0.01 },
            _ => Behaviour::Still,
        };
    }
    if !readable_number(spec) {
        return Behaviour::Still;
    }
    let Some(v) = start_num else {
        return Behaviour::Still;
    };
    if key.contains("target") || key.ends_with("_total") || key.contains("total_") {
        return Behaviour::Still;
    }
    match key.as_str() {
        "progress" => return Behaviour::Progress,
        "remaining" => return Behaviour::Remaining,
        "layer" => return Behaviour::Layer,
        "uptime" | "media_position" => return Behaviour::Counting,
        _ => {}
    }
    match (spec.semantic, spec.unit.as_ref()) {
        (Semantic::Power, _) => Behaviour::Power {
            base: if v > 1.0 { v } else { 8.0 },
            switched: has_switch,
        },
        (Semantic::Energy, Some(Unit::KiloWattHour)) => Behaviour::Meter { factor: 0.001 },
        (Semantic::Energy, _) => Behaviour::Meter { factor: 1.0 },
        (Semantic::Current, _) => Behaviour::Current,
        (Semantic::Voltage, Some(Unit::Volt)) if v > 100.0 => Behaviour::Drift {
            center: v,
            span: 4.0,
            step: 0.6,
        },
        (Semantic::Temperature, _) if key == "temperature" || key.ends_with("_temperature") => {
            if key.contains("nozzle") || key.contains("bed") || key.contains("tool") {
                Behaviour::Still
            } else if key == "temperature" {
                Behaviour::Room { center: v }
            } else {
                Behaviour::Drift {
                    center: v,
                    span: 1.5,
                    step: 0.08,
                }
            }
        }
        (Semantic::Humidity, _) => Behaviour::Drift {
            center: v,
            span: 5.0,
            step: 0.3,
        },
        (Semantic::Illuminance, _) => Behaviour::Sun { peak: v.max(300.0) },
        (Semantic::SignalStrength, _) => Behaviour::Drift {
            center: v,
            span: 4.0,
            step: 1.0,
        },
        _ => match key.as_str() {
            "cpu" | "ram" | "load" | "gpu_load" | "net_down" | "net_up" | "wind_speed"
            | "wind_gusts" | "aqi" | "pm2_5" | "pm10" | "no2" | "ozone" | "orp" | "co2" | "voc"
            | "pm25" => Behaviour::Drift {
                center: v,
                span: (v.abs() * 0.25).max(0.5),
                step: (v.abs() * 0.04).max(0.05),
            },
            _ => Behaviour::Still,
        },
    }
}

#[derive(Debug)]
pub(crate) struct World {
    devices: Vec<Live>,
    index: HashMap<DeviceId, usize>,
    rng: Rng,
    tz: jiff::tz::TimeZone,
}

/// Rounded like a sensor would report it.
fn rounded(v: f64, spec: &PointSpec) -> Value {
    let step = match spec.kind {
        Kind::Numeric { step: Some(s), .. } if s > 0.0 => s,
        _ => match spec.semantic {
            Semantic::Power
            | Semantic::SignalStrength
            | Semantic::Illuminance
            | Semantic::Energy => 1.0,
            Semantic::Current => 0.001,
            _ => 0.1,
        },
    };
    let mut x = (v / step).round() * step;
    if let Kind::Numeric { min, max, .. } = spec.kind {
        if let Some(lo) = min {
            x = x.max(lo);
        }
        if let Some(hi) = max {
            x = x.min(hi);
        }
    }
    Value::Float((x * 1000.0).round() / 1000.0)
}

impl World {
    pub(crate) fn new(played: &[Played], ids: &[DeviceId], tz: jiff::tz::TimeZone) -> Self {
        let mut devices = Vec::with_capacity(played.len());
        let mut index = HashMap::new();
        for (p, id) in played.iter().zip(ids) {
            let has_switch = p.device.points.iter().any(|s| {
                matches!(s.kind, Kind::Binary)
                    && s.access.write
                    && matches!(&*s.key, "on" | "power" | "state" | "playing")
            });
            let points = p
                .device
                .points
                .iter()
                .map(|spec| Point {
                    key: spec.key.to_string(),
                    behaviour: behaviour(spec, p.state.get(&*spec.key), has_switch),
                    pulse_left: 0.0,
                    carry: 0.0,
                })
                .collect();
            let values = p
                .state
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let instance = id.instance();
            let members = p
                .device
                .members
                .iter()
                .map(|m| {
                    let s = m.as_str();
                    DeviceId::new(&instance, s.split_once(':').map_or(s, |(_, n)| n))
                })
                .collect();
            index.insert(id.clone(), devices.len());
            devices.push(Live {
                id: id.clone(),
                members,
                specs: p.device.points.clone(),
                points,
                values,
            });
        }
        #[allow(clippy::cast_sign_loss)]
        let seed = moli_core::now_ms() as u64 | 1;
        Self {
            devices,
            index,
            rng: Rng(seed),
            tz,
        }
    }

    fn set(ctx: &DriverCtx, live: &mut Live, key: &str, value: Value) {
        if live.values.get(key) == Some(&value) {
            return;
        }
        live.values.insert(key.to_owned(), value.clone());
        ctx.set_state(&live.id, key, value);
    }

    /// An order, obeyed at once, with what it implies for the device.
    pub(crate) fn obey(&mut self, ctx: &DriverCtx, id: &DeviceId, key: &str, value: &Value) {
        let Some(&i) = self.index.get(id) else {
            return;
        };
        let members = self.devices[i].members.clone();
        self.apply(ctx, i, key, value);
        // A group (a room of lamps): its members follow.
        for m in members {
            if let Some(&j) = self.index.get(&m)
                && self.devices[j].has(key)
            {
                self.apply(ctx, j, key, value);
                self.settle(ctx, j);
            }
        }
        self.settle(ctx, i);
    }

    fn apply(&mut self, ctx: &DriverCtx, i: usize, key: &str, value: &Value) {
        let live = &mut self.devices[i];
        let Some(spec) = live.specs.iter().find(|s| &*s.key == key).cloned() else {
            return;
        };
        // A pure order (a remote's key, an announcement): nothing to keep.
        if spec.access.read {
            Self::set(ctx, live, key, value.clone());
        }
        let on_key = ["on", "power", "state"]
            .into_iter()
            .find(|k| live.specs.iter().any(|s| &*s.key == *k && s.access.write));
        match (key, value) {
            ("brightness" | "color" | "color_temp", v) if v.as_f64().is_none_or(|b| b > 0.0) => {
                if let Some(k) = on_key {
                    let on = match live.specs.iter().find(|s| &*s.key == k).map(|s| &s.kind) {
                        Some(Kind::Enum { .. }) => Value::Text("ON".into()),
                        _ => Value::Bool(true),
                    };
                    Self::set(ctx, live, k, on);
                }
            }
            ("playing", Value::Bool(b)) if live.has("state") => {
                let state = if *b { "playing" } else { "paused" };
                Self::set(ctx, live, "state", Value::Text(state.into()));
            }
            ("control", Value::Text(t)) => {
                let t = t.to_ascii_lowercase();
                if t.contains("pause") {
                    Self::set(ctx, live, "printing", Value::Bool(false));
                    if live.has("state") {
                        Self::set(ctx, live, "state", Value::Text("paused".into()));
                    }
                } else if t.contains("resume") || t.contains("repr") {
                    Self::set(ctx, live, "printing", Value::Bool(true));
                    if live.has("state") {
                        Self::set(ctx, live, "state", Value::Text("printing".into()));
                    }
                } else if t.contains("stop") || t.contains("cancel") || t.contains("annul") {
                    Self::set(ctx, live, "printing", Value::Bool(false));
                    if live.has("state") {
                        Self::set(ctx, live, "state", Value::Text("idle".into()));
                    }
                }
            }
            _ => {}
        }
    }

    /// Points that follow others at once (power after a switch).
    fn settle(&mut self, ctx: &DriverCtx, i: usize) {
        let clock = Clock::now(&self.tz);
        let live = &mut self.devices[i];
        let running = live.running();
        let updates: Vec<(String, Value)> = live
            .points
            .iter()
            .filter_map(|p| match p.behaviour {
                Behaviour::Power {
                    base,
                    switched: true,
                } => {
                    let spec = live.specs.iter().find(|s| *s.key == p.key)?;
                    let w = if running.unwrap_or(true) {
                        base * live.num("brightness").map_or(1.0, |b| (b / 100.0).max(0.2))
                    } else {
                        0.3
                    };
                    Some((p.key.clone(), rounded(w, spec)))
                }
                Behaviour::Daylight => Some((p.key.clone(), Value::Bool(clock.daylight()))),
                _ => None,
            })
            .collect();
        for (k, v) in updates {
            Self::set(ctx, live, &k, v);
        }
    }

    /// One step of `dt` seconds for the whole house.
    pub(crate) fn step(&mut self, ctx: &DriverCtx, dt: f64) {
        let clock = Clock::now(&self.tz);
        for i in 0..self.devices.len() {
            self.step_device(ctx, i, dt, clock);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn step_device(&mut self, ctx: &DriverCtx, i: usize, dt: f64, clock: Clock) {
        let rng = &mut self.rng;
        let live = &mut self.devices[i];
        let running = live.running();
        let mut updates: Vec<(String, Value)> = Vec::new();
        let printing = live.flag("printing").unwrap_or(false);
        for p in &mut live.points {
            let Some(spec) = live.specs.iter().find(|s| *s.key == p.key) else {
                continue;
            };
            let now = live.values.get(&p.key).and_then(Value::as_f64);
            let next = match p.behaviour {
                Behaviour::Still
                | Behaviour::Meter { .. }
                | Behaviour::Current
                | Behaviour::Layer
                | Behaviour::Remaining => None,
                Behaviour::Drift { center, span, step } => {
                    let v = now.unwrap_or(center);
                    // Pulled back toward the centre, pushed a little at random.
                    let v = v + (center - v) * 0.05 + rng.signed() * step;
                    Some(rounded(v.clamp(center - span, center + span), spec))
                }
                Behaviour::Room { center } => {
                    let v = now.unwrap_or(center);
                    let target = match (running, live.values.get("target_temperature")) {
                        (Some(true), Some(t)) => t.as_f64().unwrap_or(center),
                        _ => center + 0.8 * (clock.sun() - 0.4),
                    };
                    let v = v + (target - v) * 0.03 * (dt / 15.0) + rng.signed() * 0.03;
                    Some(rounded(v, spec))
                }
                Behaviour::Power { base, switched } => {
                    let w = if switched {
                        if running.unwrap_or(true) {
                            base * live
                                .values
                                .get("brightness")
                                .and_then(Value::as_f64)
                                .map_or(1.0, |b| (b / 100.0).max(0.2))
                                * (1.0 + rng.signed() * 0.04)
                        } else {
                            0.3
                        }
                    } else {
                        base * clock.household() * (1.0 + rng.signed() * 0.12)
                    };
                    Some(rounded(w.max(0.0), spec))
                }
                Behaviour::Counting => {
                    if p.key == "media_position" && !running.unwrap_or(false) {
                        None
                    } else {
                        now.map(|v| rounded(v + dt, spec))
                    }
                }
                Behaviour::Sun { peak } => Some(rounded(
                    peak * clock.sun() * (1.0 + rng.signed() * 0.05) + 2.0,
                    spec,
                )),
                Behaviour::Daylight => Some(Value::Bool(clock.daylight())),
                Behaviour::Pulse { per_minute } => {
                    if p.pulse_left > 0.0 {
                        p.pulse_left -= dt;
                        if p.pulse_left <= 0.0 {
                            Some(Value::Bool(false))
                        } else {
                            None
                        }
                    } else if dt > 0.0 && rng.chance(per_minute * dt / 60.0) {
                        p.pulse_left = 30.0 + rng.next() * 60.0;
                        Some(Value::Bool(true))
                    } else {
                        None
                    }
                }
                Behaviour::Progress => {
                    if printing && dt > 0.0 {
                        // About two hours a print; a finished one starts again.
                        let v = now.unwrap_or(0.0) + dt * 100.0 / 7200.0;
                        Some(rounded(if v >= 100.0 { 1.0 } else { v }, spec))
                    } else {
                        None
                    }
                }
            };
            if let Some(v) = next {
                updates.push((p.key.clone(), v));
            }
        }
        for (k, v) in updates {
            Self::set(ctx, live, &k, v);
        }
        // Then what follows the rest: meters, currents, a print's details.
        let watts = live.watts();
        let progress = live.num("progress");
        let total_layers = live.num("total_layers");
        let has_power = live
            .points
            .iter()
            .any(|q| matches!(q.behaviour, Behaviour::Power { .. }));
        let mut follow: Vec<(String, Value)> = Vec::new();
        for p in &mut live.points {
            let Some(spec) = live.specs.iter().find(|s| *s.key == p.key) else {
                continue;
            };
            let now = live.values.get(&p.key).and_then(Value::as_f64);
            match p.behaviour {
                Behaviour::Meter { factor } => {
                    // Its own device's power, or a modest default.
                    let w = if watts > 0.0 {
                        watts
                    } else {
                        25.0 * clock.household()
                    };
                    if let Some(v) = now {
                        let exact = v + p.carry + w * dt / 3600.0 * factor;
                        let shown = rounded_meter(exact, factor);
                        p.carry = exact - shown.as_f64().unwrap_or(exact);
                        follow.push((p.key.clone(), shown));
                    }
                }
                Behaviour::Current => {
                    if has_power {
                        follow.push((p.key.clone(), rounded(watts / 230.0, spec)));
                    }
                }
                Behaviour::Remaining => {
                    if let (Some(pr), true) = (progress, printing) {
                        follow.push((p.key.clone(), rounded(((100.0 - pr) * 1.2).max(0.0), spec)));
                    }
                }
                Behaviour::Layer => {
                    if let (Some(pr), Some(total), true) = (progress, total_layers, printing) {
                        follow.push((p.key.clone(), rounded((pr / 100.0 * total).floor(), spec)));
                    }
                }
                _ => {}
            }
        }
        for (k, v) in follow {
            Self::set(ctx, live, &k, v);
        }
    }
}

fn rounded_meter(v: f64, factor: f64) -> Value {
    // Wh as whole numbers, kWh to the Wh.
    if factor < 1.0 {
        Value::Float((v * 1000.0).round() / 1000.0)
    } else {
        Value::Float(v.round())
    }
}

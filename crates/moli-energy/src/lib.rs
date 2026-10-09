//! Energy: what the home consumes, hour by hour, and what it costs.
//!
//! Meters are points the user declares in `[energy]` (Linky indexes, a
//! clamp on the main feed, one per circuit…). A recorder listens to the
//! bus, turns each new reading into kWh spread over the hours since the
//! previous one, prices it, and files it in `energy.db`, kept forever.
//! Surfaces read headline figures (`summary`) or series (`report`).

pub mod config;
mod estimate;
pub mod meter;
pub mod points;
pub mod report;
mod store;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use jiff::tz::TimeZone;
use moli_core::{Event, PointId, Unit, Value, now_ms};
use moli_runtime::Hub;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use config::{Config, MeterConfig, Reset, Role, Tariff};
use meter::{Integral, Outcome, Reading, Tracker};
pub use report::{Amount, Bucket, Period, Step};
pub use store::EnergyError;
use store::{HourRow, Store};

/// Cheap handle for the surfaces.
#[derive(Clone, Debug)]
pub struct Energy {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    store: Arc<Store>,
    config: Config,
    tz: TimeZone,
    hub: Hub,
}

/// A meter as the surfaces show it, with its live power if known.
#[derive(Clone, Debug, Serialize)]
pub struct MeterInfo {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub point: String,
    /// Power point, for live updates from the event stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_ts: Option<u64>,
    /// An appliance's circuit, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    /// Devices it feeds (their cards show its power).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub feeds: Vec<String>,
    /// Estimated from the light's model, not measured.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

/// The supplier's meter right now (`[energy.live]`).
#[derive(Clone, Debug, Serialize)]
pub struct LiveInfo {
    /// Power point, for live updates from the event stream.
    pub point: String,
    pub max: f64,
    /// From here the draw is « soutenue ».
    pub warn: f64,
    /// The point's unit (« VA » for a Linky).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub currency: String,
    pub timezone: String,
    /// First hour on record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    /// Tariff period now (« HP.. ») and its price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_now: Option<f64>,
    /// The point holding the period (its history tells the off-peak hours).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_point: Option<String>,
    pub meters: Vec<MeterInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveInfo>,
    pub today: Period,
    pub yesterday: Period,
    pub month: Period,
    pub last_month: Period,
    /// This month at the pace so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_projection: Option<Amount>,
    /// The subscription per month, for an estimated bill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_fee: Option<f64>,
    /// The point holding the monthly budget (a helper the dashboard sets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_point: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub step: Step,
    pub from: u64,
    pub to: u64,
    pub currency: String,
    pub timezone: String,
    pub meters: Vec<MeterInfo>,
    pub buckets: Vec<Bucket>,
}

impl Energy {
    /// Opens `energy.db` and starts recording. Start it before the
    /// drivers, like the history: it subscribes first.
    pub fn start(
        hub: &Hub,
        path: &Path,
        config: Config,
        cancel: CancellationToken,
    ) -> Result<(Self, JoinHandle<()>), EnergyError> {
        config.check().map_err(EnergyError::Invalid)?;
        let tz =
            TimeZone::get(&config.timezone).map_err(|e| EnergyError::Invalid(e.to_string()))?;
        let store = Arc::new(Store::open(path)?);
        // Subscribe before reading anything: no reading falls in between.
        let events = hub.subscribe();
        let last = store.last_readings()?;
        let recorder = Recorder::new(hub.clone(), config.clone(), last, Arc::clone(&store));
        let task = tokio::spawn(recorder.run(events, cancel));
        let energy = Self {
            inner: Arc::new(Inner {
                store,
                config,
                tz,
                hub: hub.clone(),
            }),
        };
        Ok((energy, task))
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    fn meters(&self) -> Vec<MeterInfo> {
        let hub = &self.inner.hub;
        self.inner
            .config
            .meters
            .iter()
            .map(|m| {
                let power = if m.estimate {
                    device_of(&m.point)
                        .and_then(|d| estimate::watts(hub, &d))
                        .map(|w| (w, now_ms()))
                } else {
                    m.power_point()
                        .and_then(|p| hub.state(&PointId::from(p.to_owned())))
                        .and_then(|s| s.value.as_f64().map(|w| (w, s.ts)))
                };
                MeterInfo {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    role: m.role,
                    point: m.point.clone(),
                    power: m.power_point().map(str::to_owned),
                    power_w: power.map(|(w, _)| w),
                    power_ts: power.map(|(_, ts)| ts),
                    within: m.within.clone(),
                    feeds: m.feeds.clone(),
                    estimated: m.estimate,
                }
            })
            .collect()
    }

    fn live(&self) -> Option<LiveInfo> {
        let live = self.inner.config.live.as_ref()?;
        let hub = &self.inner.hub;
        let point = PointId::from(live.power.clone());
        let state = hub
            .state(&point)
            .and_then(|s| s.value.as_f64().map(|v| (v, s.ts)));
        let unit = point.split().and_then(|(device, key)| {
            let unit = hub.device(&device)?.device.point(key)?.unit.clone()?;
            Some(unit.symbol().to_owned())
        });
        Some(LiveInfo {
            point: live.power.clone(),
            max: live.max,
            warn: live.warn(),
            unit,
            value: state.map(|(v, _)| v),
            ts: state.map(|(_, ts)| ts),
        })
    }

    fn period_now(&self) -> Option<String> {
        let tariff = self.inner.config.tariff.as_ref()?;
        match self
            .inner
            .hub
            .state(&PointId::from(tariff.period.clone()))?
            .value
        {
            Value::Text(s) => Some(s.to_string()),
            _ => None,
        }
    }

    pub async fn summary(&self) -> Result<Summary, EnergyError> {
        let (tz, now) = (&self.inner.tz, now_ms());
        let today = report::floor(now, Step::Day, tz)?;
        let yesterday = report::floor(today - 1, Step::Day, tz)?;
        let month = report::floor(now, Step::Month, tz)?;
        let last_month = report::floor(month - 1, Step::Month, tz)?;
        let month_end = report::next(month, Step::Month, tz)?;
        let tomorrow = report::next(today, Step::Day, tz)?;
        let from = last_month.min(yesterday);
        let store = Arc::clone(&self.inner.store);
        let (rows, since) =
            blocking(move || Ok((store.hours(from, tomorrow)?, store.first_hour()?))).await?;
        let config = &self.inner.config;
        let month_period = Period::of(config, &rows, month, month_end);
        let period = self.period_now();
        Ok(Summary {
            currency: config.currency.clone(),
            timezone: config.timezone.clone(),
            since,
            price_now: period.as_deref().and_then(|p| config.period_price(p)),
            period_point: config.tariff.as_ref().map(|t| t.period.clone()),
            period,
            meters: self.meters(),
            live: self.live(),
            today: Period::of(config, &rows, today, tomorrow),
            yesterday: Period::of(config, &rows, yesterday, today),
            month_projection: report::projection(&month_period, now, month_end),
            month: month_period,
            last_month: Period::of(config, &rows, last_month, month),
            monthly_fee: config.monthly_fee,
            budget_point: config.budget.clone(),
        })
    }

    /// The last `count` buckets of `step`, the current one included
    /// (today's hours, the last 30 days, the last 12 months…).
    pub async fn recent(&self, step: Step, count: usize) -> Result<Report, EnergyError> {
        let tz = &self.inner.tz;
        let current = report::floor(now_ms(), step, tz)?;
        let mut from = current;
        for _ in 1..count.clamp(1, report::max_count(step)) {
            let Some(before) = from.checked_sub(1) else {
                break; // 1970
            };
            from = report::floor(before, step, tz)?;
        }
        self.report(from, report::next(current, step, tz)?, step)
            .await
    }

    pub async fn report(&self, from: u64, to: u64, step: Step) -> Result<Report, EnergyError> {
        if to <= from {
            return Err(EnergyError::Invalid("`to` must be after `from`".into()));
        }
        let store = Arc::clone(&self.inner.store);
        let rows = blocking(move || store.hours(from, to)).await?;
        let config = &self.inner.config;
        Ok(Report {
            step,
            from,
            to,
            currency: config.currency.clone(),
            timezone: config.timezone.clone(),
            meters: self.meters(),
            buckets: report::buckets(&rows, from, to, step, &self.inner.tz)?,
        })
    }
}

/// The device of a point id (`device/key`).
fn device_of(point: &str) -> Option<moli_core::DeviceId> {
    PointId::from(point.to_owned()).split().map(|(d, _)| d)
}

/// One hour of history recorded elsewhere (Home Assistant statistics…).
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRow {
    pub meter: String,
    /// Start of the hour, ms since epoch (UTC, on the hour).
    pub hour: u64,
    pub kwh: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub inserted: usize,
    /// Already imported without a cost, now priced.
    pub repriced: usize,
    /// Hours already known, or recorded live by Moli.
    pub skipped: usize,
    /// Unknown meter, misaligned hour, impossible amount.
    pub invalid: usize,
}

/// Highest credible consumption of one meter in one hour.
const MAX_KWH_PER_HOUR: f64 = 500.0;

/// Files imported history into `energy.db`, priced like live readings:
/// a meter's own price, else that hour's billed price (the cost per kWh of
/// the grid meters in the same import: which Linky index moved tells « creuse »
/// from « pleine »). A Linky moves by whole kWh, so most hours show no
/// movement: those get the price learnt for that hour of the day over the
/// whole import (how often it was « creuse »).
pub fn import(
    path: &Path,
    config: &Config,
    rows: &[ImportRow],
) -> Result<ImportReport, EnergyError> {
    config.check().map_err(EnergyError::Invalid)?;
    let tz = TimeZone::get(&config.timezone).map_err(|e| EnergyError::Invalid(e.to_string()))?;
    let meters: HashMap<&str, &MeterConfig> =
        config.meters.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut report = ImportReport::default();
    let valid: Vec<(&MeterConfig, &ImportRow)> = rows
        .iter()
        .filter_map(|r| {
            let ok = r.hour % meter::HOUR_MS == 0
                && r.kwh.is_finite()
                && (0.0..=MAX_KWH_PER_HOUR).contains(&r.kwh);
            let meter = meters.get(r.meter.as_str()).filter(|_| ok);
            if meter.is_none() {
                report.invalid += 1;
            }
            meter.map(|m| (*m, r))
        })
        .collect();
    let local_hour = |hour: u64| -> usize {
        i64::try_from(hour)
            .ok()
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            .map_or(0, |t| {
                usize::try_from(t.to_zoned(tz.clone()).hour()).unwrap_or(0)
            })
    };
    // (kWh, cost) billed per hour, and per hour of the day.
    let mut billed: HashMap<u64, (f64, f64)> = HashMap::new();
    let mut by_hour_of_day = [(0.0_f64, 0.0_f64); 24];
    for (m, r) in &valid {
        if let (Role::Grid, Some(price)) = (m.role, m.price) {
            let hour = billed.entry(r.hour).or_default();
            hour.0 += r.kwh;
            hour.1 += r.kwh * price;
            let profile = &mut by_hour_of_day[local_hour(r.hour) % 24];
            profile.0 += r.kwh;
            profile.1 += r.kwh * price;
        }
    }
    let ratio = |(kwh, cost): (f64, f64)| (kwh > 0.0).then(|| cost / kwh);
    let priced: Vec<HourRow> = valid
        .iter()
        .map(|(m, r)| {
            let price = m.price.or_else(|| {
                billed
                    .get(&r.hour)
                    .copied()
                    .and_then(ratio)
                    .or_else(|| ratio(by_hour_of_day[local_hour(r.hour) % 24]))
            });
            HourRow {
                meter: m.id.clone(),
                hour: r.hour,
                kwh: r.kwh,
                cost: price.map(|p| p * r.kwh),
            }
        })
        .collect();
    let (inserted, repriced, skipped) = Store::open(path)?.import(&priced)?;
    report.inserted = inserted;
    report.repriced = repriced;
    report.skipped = skipped;
    Ok(report)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, EnergyError> + Send + 'static,
) -> Result<T, EnergyError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| EnergyError::Internal(e.to_string()))?
}

/// Tariff periods seen, in time order: what each hour was billed as.
#[derive(Debug, Default)]
struct Periods(Vec<(u64, String)>);

/// Enough to cover a month of downtime.
const PERIODS_KEPT: usize = 4_000;

impl Periods {
    /// Records a period change; an older one (backlog after a lag) is
    /// ignored. Returns whether it was taken.
    fn push(&mut self, ts: u64, period: &str) -> bool {
        if self.0.last().is_some_and(|(last, _)| ts < *last) {
            return false;
        }
        if self.0.last().is_none_or(|(_, p)| p != period) {
            self.0.push((ts, period.to_owned()));
            if self.0.len() > PERIODS_KEPT {
                self.0.remove(0);
            }
        }
        true
    }

    /// The period at `t` (the earliest known one before records began).
    fn at(&self, t: u64) -> Option<&str> {
        let i = self.0.partition_point(|(ts, _)| *ts <= t);
        self.0.get(i.saturating_sub(1)).map(|(_, p)| p.as_str())
    }

    /// Ms of `[a, b)` spent in `period`; before records began, all of it
    /// counts (unknown).
    fn time_in(&self, period: &str, a: u64, b: u64) -> u64 {
        let wanted = config::period_key(period);
        let Some(&(first, _)) = self.0.first() else {
            return b - a;
        };
        let mut eligible = b.min(first).saturating_sub(a);
        for (i, (start, p)) in self.0.iter().enumerate() {
            let end = self.0.get(i + 1).map_or(u64::MAX, |(t, _)| *t);
            let (lo, hi) = (a.max(*start), b.min(end));
            if lo < hi && config::period_key(p) == wanted {
                eligible += hi - lo;
            }
        }
        eligible
    }
}

struct Recorder {
    hub: Hub,
    config: Config,
    store: Arc<Store>,
    /// Meter index by point id.
    by_point: HashMap<String, usize>,
    trackers: HashMap<String, Tracker>,
    /// kWh per unit of each meter's point, once its device is known (kW per
    /// unit for an integrated meter).
    factors: HashMap<String, f64>,
    periods: Periods,
    /// Integrated meters (power × time), by meter id.
    integrals: HashMap<String, Integral>,
    /// When recording started: a power known from before is not counted
    /// over the time Moli was stopped.
    started: u64,
    /// Estimated lights by the devices that move them (the light, a
    /// fixture's bulbs): most events concern none, and say so at once.
    lit_by: HashMap<moli_core::DeviceId, Vec<usize>>,
}

/// How often a held power is counted, even if it does not change: the
/// hours fill as they pass.
const HOLD_TICK: std::time::Duration = std::time::Duration::from_secs(300);

impl Recorder {
    fn new(hub: Hub, config: Config, last: HashMap<String, Reading>, store: Arc<Store>) -> Self {
        let by_point = config
            .meters
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.estimate)
            .map(|(i, m)| (m.point.clone(), i))
            .collect();
        Self {
            hub,
            config,
            store,
            by_point,
            trackers: last
                .into_iter()
                .map(|(k, r)| (k, Tracker::new(r)))
                .collect(),
            factors: HashMap::new(),
            periods: Periods::default(),
            integrals: HashMap::new(),
            started: now_ms(),
            lit_by: HashMap::new(),
        }
    }

    /// Which devices move which estimated light (at start, and when a
    /// device changes: a fixture's members).
    fn index_estimates(&mut self) {
        self.lit_by.clear();
        for (i, m) in self
            .config
            .meters
            .iter()
            .enumerate()
            .filter(|(_, m)| m.estimate)
        {
            let Some(device) = device_of(&m.point) else {
                continue;
            };
            for source in estimate::sources(&self.hub, &device) {
                self.lit_by.entry(source).or_default().push(i);
            }
        }
    }

    async fn run(
        mut self,
        mut events: tokio::sync::broadcast::Receiver<Event>,
        cancel: CancellationToken,
    ) {
        self.read_present().await;
        let mut tick = tokio::time::interval(HOLD_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => self.hold_powers(None).await,
                event = events.recv() => match event {
                    Ok(Event::Availability { device, online }) => {
                        if !online {
                            // Offline: what was held counts until now, nothing after.
                            self.hold_powers(Some(&device)).await;
                        }
                        // A light without power draws nothing; back, it draws again.
                        self.on_estimate(&device, now_ms()).await;
                    }
                    Ok(Event::State { point, value, ts }) => {
                        self.on_state(point.as_str(), &value, ts).await;
                        if let Some(device) = device_of(point.as_str()) {
                            self.on_estimate(&device, ts).await;
                        }
                    }
                    Ok(Event::DeviceUpserted { device }) => {
                        if self.config.meters.iter().any(|m| m.estimate) {
                            self.index_estimates();
                        }
                        // Its units may have changed: look them up again.
                        let prefix = format!("{}/", device.id);
                        let meters = &self.config.meters;
                        self.factors.retain(|id, _| {
                            meters.iter().any(|m| &m.id == id && !m.point.starts_with(&prefix))
                        });
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(missed)) => {
                        // Counters are cumulative: the present says it all.
                        tracing::warn!(missed, "energy recorder lagged, re-reading the meters");
                        self.read_present().await;
                    }
                    Err(RecvError::Closed) => break,
                },
            }
        }
    }

    /// The tariff's and the meters' current values (at start, after a lag,
    /// once the tariff is first known).
    async fn read_present(&mut self) {
        let tariff = self.config.tariff.as_ref().map(|t| t.period.clone());
        let points: Vec<String> = tariff
            .into_iter()
            .chain(self.config.meters.iter().map(|m| m.point.clone()))
            .collect();
        for point in points {
            if let Some(sample) = self.hub.state(&PointId::from(point.clone())) {
                self.on_state(&point, &sample.value, sample.ts).await;
            }
        }
        self.index_estimates();
        let lights: Vec<moli_core::DeviceId> = self
            .config
            .meters
            .iter()
            .filter(|m| m.estimate)
            .filter_map(|m| device_of(&m.point))
            .collect();
        for device in lights {
            self.on_estimate(&device, now_ms()).await;
        }
    }

    /// A light's estimated power, counted over time like an integrated
    /// meter's (when its state, or one of its fixture's bulbs, moved).
    async fn on_estimate(&mut self, moved: &moli_core::DeviceId, ts: u64) {
        let Some(indexes) = self.lit_by.get(moved) else {
            return; // no light moved
        };
        if self.config.tariff.is_some() && self.periods.0.is_empty() {
            return; // no price yet: a later state files it
        }
        let meters: Vec<MeterConfig> = indexes
            .iter()
            .map(|&i| self.config.meters[i].clone())
            .collect();
        for meter in meters {
            let Some(w) = device_of(&meter.point).and_then(|d| estimate::watts(&self.hub, &d))
            else {
                continue;
            };
            let started = self.started;
            let next = self
                .integral(&meter.id)
                .observe(Some(w / 1000.0), ts, started);
            self.file(&meter, next).await;
        }
    }

    async fn on_state(&mut self, point: &str, value: &Value, ts: u64) {
        if self
            .config
            .tariff
            .as_ref()
            .is_some_and(|t| t.period == point)
        {
            if let Value::Text(period) = value {
                let first = self.periods.0.is_empty();
                if self.periods.push(ts, period) && first {
                    // Readings that waited for a price can be filed now.
                    self.read_meters().await;
                }
            }
            return;
        }
        self.on_meter(point, value, ts).await;
    }

    async fn read_meters(&mut self) {
        let points: Vec<String> = self.config.meters.iter().map(|m| m.point.clone()).collect();
        for point in points {
            if let Some(sample) = self.hub.state(&PointId::from(point.clone())) {
                self.on_meter(&point, &sample.value, sample.ts).await;
            }
        }
    }

    async fn on_meter(&mut self, point: &str, value: &Value, ts: u64) {
        let Some(&index) = self.by_point.get(point) else {
            return;
        };
        let Some(raw) = value.as_f64() else {
            return; // unavailable: nothing to count
        };
        let Some(factor) = self.factor(index) else {
            return;
        };
        let meter = self.config.meters[index].clone();
        if self.config.tariff.is_some() && meter.price.is_none() && self.periods.0.is_empty() {
            // No price yet: counters are cumulative, file it later.
            return;
        }
        let started = self.started;
        let next = if meter.integrate {
            self.integral(&meter.id)
                .observe(Some(raw * factor), ts, started)
        } else {
            Reading {
                kwh: raw * factor,
                ts,
            }
        };
        self.file(&meter, next).await;
    }

    /// An integrated meter's running energy (from the last filed reading).
    fn integral(&mut self, id: &str) -> &mut Integral {
        let filed = self.trackers.get(id).map_or(0.0, |t| t.last.kwh);
        self.integrals
            .entry(id.to_owned())
            .or_insert_with(|| Integral::new(filed))
    }

    /// Counts every held power until now (all meters, or one device's going
    /// offline: then nothing more is held).
    async fn hold_powers(&mut self, offline: Option<&moli_core::DeviceId>) {
        let now = now_ms();
        let meters: Vec<MeterConfig> = self
            .config
            .meters
            .iter()
            .filter(|m| m.integrate || m.estimate)
            .filter(|m| {
                offline.is_none_or(|d| {
                    PointId::from(m.point.clone())
                        .split()
                        .is_some_and(|(device, _)| &device == d)
                })
            })
            .cloned()
            .collect();
        for meter in meters {
            let started = self.started;
            let integral = self.integral(&meter.id);
            let Some((kw, _)) = integral.held else {
                continue;
            };
            let held = offline.is_none().then_some(kw);
            let next = integral.observe(held, now, started);
            self.file(&meter, next).await;
        }
    }

    /// A meter's new reading (a counter's, or an integral's), filed.
    async fn file(&mut self, meter: &MeterConfig, next: Reading) {
        let Some(&tracker) = self.trackers.get(&meter.id) else {
            // First reading ever: the reference for the next ones.
            self.commit(&meter.id, Tracker::new(next), Vec::new()).await;
            return;
        };
        let mut tracker = tracker;
        match tracker.observe(next, meter.reset, self.config.max_power_kw) {
            Outcome::Hold => {
                // Remember a suspect reading (memory only).
                self.trackers.insert(meter.id.clone(), tracker);
            }
            Outcome::Rebase => {
                tracing::warn!(
                    meter = meter.id,
                    kwh = next.kwh,
                    "energy counter jumped to a new baseline: followed, nothing counted"
                );
                self.commit(&meter.id, tracker, Vec::new()).await;
            }
            Outcome::Consumed { from, kwh } => {
                let slices = self.slices(meter, kwh, from, next.ts);
                self.commit(&meter.id, tracker, slices).await;
            }
        }
    }

    /// Spreads `kwh` over `[from, to)` and prices each hour.
    fn slices(&self, meter: &MeterConfig, kwh: f64, from: u64, to: u64) -> Vec<HourRow> {
        let pieces = match &meter.period {
            Some(period) => {
                meter::spread_weighted(kwh, from, to, |a, b| self.periods.time_in(period, a, b))
            }
            None => meter::spread(kwh, from, to),
        };
        pieces
            .into_iter()
            .map(|(hour, kwh)| {
                // The price in the middle of the slice's own time.
                let middle = hour.max(from) / 2 + (hour + meter::HOUR_MS).min(to) / 2;
                let price = meter.price.or_else(|| {
                    self.periods
                        .at(middle)
                        .and_then(|p| self.config.period_price(p))
                });
                HourRow {
                    meter: meter.id.clone(),
                    hour,
                    kwh,
                    cost: price.map(|p| p * kwh),
                }
            })
            .collect()
    }

    /// Files a reading; memory follows only once it is on disk, so a failed
    /// write is carried by the next reading instead of being lost.
    async fn commit(&mut self, meter: &str, tracker: Tracker, slices: Vec<HourRow>) {
        let (store, id, last) = (Arc::clone(&self.store), meter.to_owned(), tracker.last);
        match blocking(move || store.record(&id, last, &slices)).await {
            Ok(()) => {
                self.trackers.insert(meter.to_owned(), tracker);
            }
            Err(e) => tracing::warn!(
                error = %e,
                meter,
                "energy reading not written, carried by the next one"
            ),
        }
    }

    /// kWh per unit of the meter's point (Wh → 0.001). Unknown until the
    /// driver published the device; an energy-less unit is refused (looked
    /// up again when the device changes).
    fn factor(&mut self, index: usize) -> Option<f64> {
        let meter = &self.config.meters[index];
        if let Some(&f) = self.factors.get(&meter.id) {
            return f.is_finite().then_some(f);
        }
        let point = PointId::from(meter.point.clone());
        let (device, key) = point.split()?;
        let view = self.hub.device(&device)?;
        let factor = match (meter.integrate, &view.device.point(key)?.unit) {
            (false, Some(Unit::WattHour)) | (true, Some(Unit::Watt)) => 0.001,
            (false, Some(Unit::KiloWattHour)) => 1.0,
            (_, other) => {
                tracing::warn!(meter = meter.id, unit = ?other, integrate = meter.integrate, "energy meter point is not in Wh or kWh (W to integrate): ignored");
                f64::NAN
            }
        };
        self.factors.insert(meter.id.clone(), factor);
        factor.is_finite().then_some(factor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = meter::HOUR_MS;

    #[test]
    fn imports_price_unbilled_hours_by_hour_of_day() {
        let dir = std::env::temp_dir().join(format!("moli-energy-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config: Config = serde_json::from_value(serde_json::json!({
            "meter": [
                { "id": "hc", "name": "HC", "point": "z:l/1", "role": "grid", "price": 0.1 },
                { "id": "hp", "name": "HP", "point": "z:l/2", "role": "grid", "price": 0.2 },
                { "id": "general", "name": "G", "point": "p:e/1", "role": "total" },
            ]
        }))
        .unwrap();
        let day = 24 * H;
        let r = |meter: &str, hour: u64, kwh: f64| ImportRow {
            meter: meter.into(),
            hour,
            kwh,
        };
        let rows = [
            r("hc", 3 * H, 1.0),            // 03:00 UTC is « creuse »…
            r("general", 3 * H, 1.0),       // …billed that very hour
            r("general", day + 3 * H, 2.0), // the Linky did not move: learnt
            r("hp", 15 * H, 1.0),
            r("general", 9 * H, 1.0),     // nothing known at 09:00
            r("nope", 3 * H, 1.0),        // unknown meter
            r("general", 3 * H + 5, 1.0), // not on the hour
        ];
        let path = dir.join("energy.db");
        let report = import(&path, &config, &rows).unwrap();
        assert_eq!((report.inserted, report.invalid), (5, 2));
        let costs: HashMap<(String, u64), Option<f64>> = Store::open(&path)
            .unwrap()
            .hours(0, 10 * day)
            .unwrap()
            .into_iter()
            .map(|row| ((row.meter, row.hour), row.cost))
            .collect();
        let cost = |m: &str, h: u64| costs[&(m.to_owned(), h)];
        assert_eq!(cost("general", 3 * H), Some(0.1));
        assert_eq!(cost("general", day + 3 * H), Some(0.2));
        assert_eq!(cost("general", 9 * H), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn periods_answer_what_each_moment_was() {
        let mut p = Periods::default();
        assert_eq!(p.time_in("HC", 0, H), H, "unknown: all eligible");
        assert!(p.push(10 * H, "HP.."));
        assert!(p.push(22 * H, "HC.."));
        assert!(!p.push(21 * H, "HP.."), "backlog after a lag: ignored");
        assert_eq!(p.at(23 * H), Some("HC.."));
        assert_eq!(p.at(12 * H), Some("HP.."));
        assert_eq!(p.at(H), Some("HP.."), "before records: the earliest");
        // 20:00 → 24:00: two hours of each.
        assert_eq!(p.time_in("hc", 20 * H, 24 * H), 2 * H);
        assert_eq!(p.time_in("HP", 20 * H, 24 * H), 2 * H);
        // Before 10:00 nothing is known: eligible.
        assert_eq!(p.time_in("HC", 8 * H, 11 * H), 2 * H);
    }
}

//! Energy: what the home consumes, hour by hour, and what it costs.
//!
//! Meters are points the user declares in `[energy]` (Linky indexes, a
//! clamp on the main feed, one per circuit…). A recorder listens to the
//! bus, turns each new reading into kWh spread over the hours since the
//! previous one, prices it, and files it in `energy.db`, kept forever.
//! Surfaces read headline figures (`summary`) or series (`report`).

pub mod config;
mod estimate;
pub mod load;
pub mod meter;
pub mod points;
mod price;
pub mod report;
mod store;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use jiff::tz::TimeZone;
use moli_core::{Event, PointId, Unit, Value, now_ms};
use moli_runtime::Hub;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use config::{Battery, Config, MeterConfig, Reset, Role, Tariff};
use meter::{Integral, Outcome, Reading, Tracker};
pub use price::HousePrice;
use price::HousePrices;
pub use report::{Amount, Bucket, Period, SolarShare, Step};
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
    /// The house's price of a kWh, shared with the recorder.
    prices: HousePrices,
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

/// The power right now, with panels (W).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Flows {
    /// What the panels make (the `solar` meters' power).
    pub production_w: f64,
    /// At the main feed, signed: positive when the home buys, negative when
    /// it sends out (the `total` meter's power, a clamp that sees both ways).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid_w: Option<f64>,
    /// The home battery, signed: positive while it charges, negative while
    /// it gives back (`[energy.battery] power`, else the `charge` /
    /// `discharge` meters' powers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_w: Option<f64>,
    /// What the home uses: bought + made, less what the battery takes,
    /// plus what it gives back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumption_w: Option<f64>,
}

impl Flows {
    /// From the meters' live powers and the battery's (`None` without a
    /// solar power).
    #[must_use]
    pub fn of(meters: &[MeterInfo], battery_w: Option<f64>) -> Option<Self> {
        let solar: Vec<f64> = meters
            .iter()
            .filter(|m| m.role == Role::Solar)
            .filter_map(|m| m.power_w)
            .collect();
        if solar.is_empty() {
            return None;
        }
        // A sleeping inverter may read a few negative watts: none made.
        let production_w = solar.iter().map(|w| w.max(0.0)).sum::<f64>();
        let grid_w = meters
            .iter()
            .find(|m| m.role == Role::Total)
            .and_then(|m| m.power_w);
        Some(Self {
            production_w,
            grid_w,
            battery_w,
            consumption_w: grid_w.map(|g| (g + production_w - battery_w.unwrap_or(0.0)).max(0.0)),
        })
    }
}

/// The home battery right now (`[energy.battery]`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BatteryInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soc_point: Option<String>,
    /// State of charge, in %.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soc: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soc_ts: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_point: Option<String>,
    /// Signed like `Flows::battery_w`: positive while it charges. Without
    /// `power_point`, from the `charge` / `discharge` meters (their `power`
    /// in `meters`, which live views read themselves).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_ts: Option<u64>,
    /// The power point reads positive while discharging: live views that
    /// read it themselves flip it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub discharge_positive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity_kwh: Option<f64>,
    /// What the charge holds now (kWh), with a capacity and a charge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_kwh: Option<f64>,
}

impl BatteryInfo {
    /// From the configuration and the points' current values (value, ts).
    #[must_use]
    pub fn of(
        config: &config::Battery,
        soc: Option<(f64, u64)>,
        power: Option<(f64, u64)>,
    ) -> Self {
        // A charge outside 0–100 % is a bad reading, not a state.
        let soc = soc.filter(|(v, _)| v.is_finite() && (0.0..=100.0).contains(v));
        let power = power
            .filter(|(w, _)| w.is_finite())
            .map(|(w, ts)| (if config.discharge_positive { -w } else { w }, ts));
        Self {
            soc_point: config.soc.clone(),
            soc: soc.map(|(v, _)| v),
            soc_ts: soc.map(|(_, ts)| ts),
            power_point: config.power.clone(),
            power_w: power.map(|(w, _)| w),
            power_ts: power.map(|(_, ts)| ts),
            discharge_positive: config.discharge_positive,
            capacity_kwh: config.capacity_kwh,
            stored_kwh: config
                .capacity_kwh
                .zip(soc)
                .map(|(c, (v, _))| (c * v).round() / 100.0),
        }
    }

    /// Without a `power` point, the power from the `charge` / `discharge`
    /// meters' own power points instead ([`meters_battery_power`]).
    #[must_use]
    pub fn or_meters(mut self, meters: &[MeterInfo]) -> Self {
        if self.power_point.is_none()
            && let Some((w, ts)) = meters_battery_power(meters)
        {
            self.power_w = Some(w);
            self.power_ts = Some(ts);
        }
        self
    }
}

/// The battery's power from the `charge` / `discharge` meters' power
/// points, like `solar` gives the production: what goes in less what comes
/// out (W, positive while charging). For a battery that says both as
/// positive powers (a Jackery's `charge_power` and `discharge_power`, its
/// meters integrated). Each point once (two indexes may share one); `None`
/// when none of them reads.
#[must_use]
pub fn meters_battery_power(meters: &[MeterInfo]) -> Option<(f64, u64)> {
    let mut seen = HashSet::new();
    let mut out: Option<(f64, u64)> = None;
    for m in meters {
        let sign = match m.role {
            Role::Charge => 1.0,
            Role::Discharge => -1.0,
            _ => continue,
        };
        let (Some(point), Some(w)) = (&m.power, m.power_w) else {
            continue;
        };
        if !w.is_finite() || !seen.insert(point.as_str()) {
            continue;
        }
        // A battery at rest may read a few negative watts: nothing moves.
        let (sum, ts) = out.unwrap_or((0.0, 0));
        out = Some((sum + sign * w.max(0.0), ts.max(m.power_ts.unwrap_or(0))));
    }
    out
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
    /// Where the power goes right now, with solar meters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flows: Option<Flows>,
    /// The home battery right now, with `[energy.battery]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatteryInfo>,
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
    /// The house's price of a kWh now (chosen from the dashboard), for the
    /// meters without a price of their own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub house_price: Option<f64>,
    /// Every price of the house, oldest first, with when it began.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub house_prices: Vec<HousePrice>,
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
        let sources: Vec<(String, String)> = config
            .meters
            .iter()
            .map(|m| (m.id.clone(), m.point.clone()))
            .collect();
        for meter in store.follow_sources(&sources)? {
            tracing::warn!(
                meter,
                "energy meter reads another point: starting from its first reading, nothing counted"
            );
        }
        let last = store.last_readings()?;
        let suspects = store.suspects()?;
        let prices = HousePrices::new(store.house_prices()?);
        let recorder = Recorder::new(hub.clone(), config.clone(), last, Arc::clone(&store))
            .with_suspects(&suspects)
            .with_prices(prices.clone());
        let task = tokio::spawn(recorder.run(events, cancel));
        let energy = Self {
            inner: Arc::new(Inner {
                store,
                config,
                tz,
                hub: hub.clone(),
                prices,
            }),
        };
        Ok((energy, task))
    }

    /// Every price of the house, oldest first.
    #[must_use]
    pub fn house_prices(&self) -> Vec<HousePrice> {
        self.inner.prices.all()
    }

    /// A new price of the house, from now on: the hours already filed keep
    /// theirs. Returns the price it replaces.
    pub async fn set_house_price(&self, price: f64) -> Result<Option<f64>, EnergyError> {
        config::check_price(price).map_err(EnergyError::Invalid)?;
        let now = now_ms();
        let before = self.inner.prices.at(now);
        let new = HousePrice { since: now, price };
        let store = Arc::clone(&self.inner.store);
        blocking(move || store.add_house_price(new)).await?;
        self.inner.prices.push(new);
        Ok(before)
    }

    /// Every price of the house at once, as edited from the dashboard (a
    /// past price corrected, one added with its date, one removed). The hours
    /// already filed from the first change on take the new prices, for the
    /// meters the house's price prices (see [`Self::house_priced`]).
    /// Returns how many hours were priced again.
    pub async fn set_house_prices(&self, prices: Vec<HousePrice>) -> Result<usize, EnergyError> {
        if prices.is_empty() {
            return Err(EnergyError::Invalid(
                "keep at least one price: without any, every hour would lose its cost".into(),
            ));
        }
        let mut new = prices;
        new.sort_by_key(|p| p.since);
        for p in &new {
            config::check_price(p.price).map_err(EnergyError::Invalid)?;
        }
        if new.windows(2).any(|w| w[0].since == w[1].since) {
            return Err(EnergyError::Invalid(
                "two prices begin at the same moment".into(),
            ));
        }
        let old = self.inner.prices.all();
        let Some(from) = price::first_change(&old, &new) else {
            return Ok(0);
        };
        let meters = self.house_priced();
        let store = Arc::clone(&self.inner.store);
        let kept = new.clone();
        let repriced = blocking(move || store.replace_house_prices(&kept, &meters, from)).await?;
        self.inner.prices.replace(new);
        Ok(repriced)
    }

    /// The meters priced by the house's price: they consume, have no price
    /// of their own, and the house has no tariff periods (whose prices come
    /// first, and whose past periods are not kept).
    fn house_priced(&self) -> Vec<String> {
        let config = &self.inner.config;
        if config.tariff.is_some() {
            return Vec::new();
        }
        config
            .meters
            .iter()
            .filter(|m| m.role.consumes() && m.price.is_none() && m.price_point.is_none())
            .map(|m| m.id.clone())
            .collect()
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
                        .and_then(|d| current_estimate(hub, &d))
                        .map(|w| (w, now_ms()))
                } else {
                    m.power_point()
                        .and_then(|p| hub.current_state(&PointId::from(p.to_owned())))
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
            .current_state(&point)
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

    /// With `[energy.battery]`, or `charge` / `discharge` meters that say
    /// their power (then without a charge nor a capacity).
    fn battery(&self, meters: &[MeterInfo]) -> Option<BatteryInfo> {
        let by_meters = config::Battery::default();
        let battery = match &self.inner.config.battery {
            Some(battery) => battery,
            None if meters
                .iter()
                .any(|m| matches!(m.role, Role::Charge | Role::Discharge) && m.power.is_some()) =>
            {
                &by_meters
            }
            None => return None,
        };
        let hub = &self.inner.hub;
        let read = |point: &Option<String>| {
            point
                .as_ref()
                .and_then(|p| hub.current_state(&PointId::from(p.clone())))
                .and_then(|s| s.value.as_f64().map(|v| (v, s.ts)))
        };
        Some(BatteryInfo::of(battery, read(&battery.soc), read(&battery.power)).or_meters(meters))
    }

    fn period_now(&self) -> Option<String> {
        let tariff = self.inner.config.tariff.as_ref()?;
        match self
            .inner
            .hub
            .current_state(&PointId::from(tariff.period.clone()))?
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
        let meters = self.meters();
        let battery = self.battery(&meters);
        Ok(Summary {
            currency: config.currency.clone(),
            timezone: config.timezone.clone(),
            since,
            price_now: period
                .as_deref()
                .and_then(|p| config.period_price(p))
                .or_else(|| self.inner.prices.at(now)),
            period_point: config.tariff.as_ref().map(|t| t.period.clone()),
            period,
            flows: Flows::of(&meters, battery.as_ref().and_then(|b| b.power_w)),
            battery,
            meters,
            live: self.live(),
            today: Period::of(config, &rows, today, tomorrow),
            yesterday: Period::of(config, &rows, yesterday, today),
            month_projection: report::projection(
                &month_period,
                pace_until(config, &rows, month, now),
                month_end,
            ),
            month: month_period,
            last_month: Period::of(config, &rows, last_month, month),
            monthly_fee: config.monthly_fee,
            budget_point: config.budget.clone(),
            house_price: self.inner.prices.at(now),
            house_prices: self.inner.prices.all(),
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

// Retained bulb settings are useful to display, but must not restart an
// integral when a driver republishes its definition before its first reading.
fn current_estimate(hub: &Hub, device: &moli_core::DeviceId) -> Option<f64> {
    for source in estimate::sources(hub, device) {
        let Some(view) = hub.device(&source) else {
            continue;
        };
        // Keep the existing zero-power convention for an explicitly offline
        // bulb; fixtures derive their power from their members, not their own state.
        if view.online == Some(false)
            || view.device.model.as_deref() == Some("Luminaire")
            || estimate::watts(hub, &source).is_none()
        {
            continue;
        }
        for point in &view.device.points {
            if matches!(
                point.semantic,
                moli_core::Semantic::OnOff
                    | moli_core::Semantic::Brightness
                    | moli_core::Semantic::Color
                    | moli_core::Semantic::ColorTemp
            ) && view.state.contains_key(&point.key)
                && hub
                    .current_state(&PointId::new(&source, &point.key))
                    .is_none()
            {
                return None;
            }
        }
    }
    estimate::watts(hub, device)
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

/// A meter's own price: its `price_point`'s value (`supplier`) plus
/// `price_extra` when the point has one (a price above 5 per kWh is a
/// misreading), else its fixed `price`.
fn own_price(meter: &MeterConfig, supplier: Option<f64>) -> Option<f64> {
    supplier
        .filter(|v| v.is_finite() && (0.0..5.0).contains(v))
        .map(|v| v + meter.price_extra.unwrap_or(0.0))
        .or(meter.price)
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
    let store = Store::open(path)?;
    let house = store.house_prices()?;
    let (priced, invalid) = price_rows(config, &tz, &house, rows);
    let mut report = ImportReport {
        invalid,
        ..ImportReport::default()
    };
    let (inserted, repriced, skipped) = store.import(&priced)?;
    report.inserted = inserted;
    report.repriced = repriced;
    report.skipped = skipped;
    Ok(report)
}

/// The rows that make sense, each with its price, and how many did not.
fn price_rows(
    config: &Config,
    tz: &TimeZone,
    house: &[HousePrice],
    rows: &[ImportRow],
) -> (Vec<HourRow>, usize) {
    let meters: HashMap<&str, &MeterConfig> =
        config.meters.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut invalid = 0;
    let valid: Vec<(&MeterConfig, &ImportRow)> = rows
        .iter()
        .filter_map(|r| {
            let ok = r.hour % meter::HOUR_MS == 0
                && r.kwh.is_finite()
                && (0.0..=MAX_KWH_PER_HOUR).contains(&r.kwh);
            let meter = meters.get(r.meter.as_str()).filter(|_| ok);
            if meter.is_none() {
                invalid += 1;
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
            // Production is never billed; exports earn their own price only.
            let price = m.price.or_else(|| {
                m.role.consumes().then_some(())?;
                billed
                    .get(&r.hour)
                    .copied()
                    .and_then(ratio)
                    .or_else(|| ratio(by_hour_of_day[local_hour(r.hour) % 24]))
                    // The house's price in the middle of the hour.
                    .or_else(|| price::at(house, r.hour + meter::HOUR_MS / 2))
            });
            HourRow {
                meter: m.id.clone(),
                hour: r.hour,
                kwh: r.kwh,
                cost: price.map(|p| p * r.kwh),
            }
        })
        .collect();
    (priced, invalid)
}

/// How far the month's figures reach. Meters that report a day late have nothing
/// yet for the last hours: counted as empty, they would make the pace too low.
fn pace_until(config: &Config, rows: &[HourRow], month: u64, now: u64) -> u64 {
    let mut main = config
        .meters
        .iter()
        .filter(|m| matches!(m.role, Role::Grid | Role::Total))
        .peekable();
    if main.peek().is_none() || !main.clone().all(|m| m.history) {
        return now;
    }
    rows.iter()
        .filter(|r| r.hour >= month && main.clone().any(|m| m.id == r.meter))
        .map(|r| r.hour + meter::HOUR_MS)
        .max()
        .unwrap_or(month)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, EnergyError> + Send + 'static,
) -> Result<T, EnergyError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| EnergyError::Internal(e.to_string()))?
}

/// One entry of a `history` point: an hour, or a whole day (from its start
/// to the next day's, 23 to 25 hours) when the service gives only the day.
#[derive(Deserialize)]
#[serde(untagged)]
enum Reported {
    Hour { hour: u64, kwh: f64 },
    Day { day: u64, to: u64, kwh: f64 },
}

/// The hours to file: those reported, and each whole day spread evenly over
/// its hours when the store has none of them for this meter.
fn reported_rows(
    store: &Store,
    meter: &str,
    reported: Vec<Reported>,
) -> Result<Vec<ImportRow>, EnergyError> {
    let mut rows = Vec::new();
    for entry in reported {
        match entry {
            Reported::Hour { hour, kwh } => rows.push(ImportRow {
                meter: meter.to_owned(),
                hour,
                kwh,
            }),
            Reported::Day { day, to, kwh } => {
                let hours = to.saturating_sub(day) / meter::HOUR_MS;
                let whole =
                    to > day && (to - day) % meter::HOUR_MS == 0 && (23..=25).contains(&hours);
                if !whole || !kwh.is_finite() || kwh < 0.0 {
                    continue;
                }
                if store.hours(day, to)?.iter().any(|r| r.meter == meter) {
                    continue;
                }
                #[allow(clippy::cast_precision_loss)] // 23 to 25
                let share = kwh / hours as f64;
                rows.extend((0..hours).map(|i| ImportRow {
                    meter: meter.to_owned(),
                    hour: day + i * meter::HOUR_MS,
                    kwh: share,
                }));
            }
        }
    }
    Ok(rows)
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
    /// The house's price of a kWh, for the meters without one.
    prices: HousePrices,
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
            prices: HousePrices::default(),
        }
    }

    /// Suspect readings kept from before a restart.
    fn with_suspects(mut self, suspects: &HashMap<String, (Reading, u32)>) -> Self {
        for (meter, tracker) in &mut self.trackers {
            if let Some(&suspect) = suspects.get(meter) {
                *tracker = Tracker::resumed(tracker.last, Some(suspect));
            }
        }
        self
    }

    fn with_prices(mut self, prices: HousePrices) -> Self {
        self.prices = prices;
        self
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
                    Ok(Event::State {
                        point, value, ts, ..
                    }) => {
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
            if let Some(sample) = self.hub.current_state(&PointId::from(point.clone())) {
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
            let Some(w) = device_of(&meter.point).and_then(|d| current_estimate(&self.hub, &d))
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
            if let Some(sample) = self.hub.current_state(&PointId::from(point.clone())) {
                self.on_meter(&point, &sample.value, sample.ts).await;
            }
        }
    }

    async fn on_meter(&mut self, point: &str, value: &Value, ts: u64) {
        let Some(&index) = self.by_point.get(point) else {
            return;
        };
        if self.config.meters[index].history {
            self.on_history(index, value).await;
            return;
        }
        let Some(raw) = value.as_f64() else {
            return; // unavailable: nothing to count
        };
        let Some(factor) = self.factor(index) else {
            return;
        };
        let meter = self.config.meters[index].clone();
        if meter.role.consumes()
            && self.config.tariff.is_some()
            && meter.price.is_none()
            && meter.price_point.is_none()
            && self.periods.0.is_empty()
        {
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

    /// Hours a service reported late, filed as they are. The same hours can
    /// come back (a restart, the next report): what is filed stays.
    async fn on_history(&mut self, index: usize, value: &Value) {
        let Value::Text(text) = value else {
            return; // unavailable: nothing to file
        };
        let meter = &self.config.meters[index];
        let Ok(reported) = serde_json::from_str::<Vec<Reported>>(text) else {
            tracing::warn!(meter = meter.id, "energy history unreadable");
            return;
        };
        let id = meter.id.clone();
        let store = Arc::clone(&self.store);
        // A whole day (a service that gave only the day's total) is spread
        // over its hours only when none of them is filed yet: hours a curve
        // gave, even in part, are never topped up.
        let rows = blocking(move || reported_rows(&store, &id, reported)).await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(error = %error, "energy history not read");
                return;
            }
        };
        let Ok(tz) = TimeZone::get(&self.config.timezone) else {
            return; // refused when the recorder started
        };
        let (priced, invalid) = price_rows(&self.config, &tz, &self.prices.all(), &rows);
        if invalid > 0 {
            tracing::warn!(meter = meter.id, invalid, "energy history: hours refused");
        }
        let store = Arc::clone(&self.store);
        let filed = blocking(move || store.import(&priced)).await;
        if let Err(error) = filed {
            tracing::warn!(error = %error, "energy history not filed");
        }
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
                // A new suspect reading, or one more behind it, is kept on
                // disk too: a restart must not forget a jump already seen.
                let suspect = tracker.suspect();
                if suspect != self.trackers.get(&meter.id).and_then(Tracker::suspect) {
                    let (store, id) = (Arc::clone(&self.store), meter.id.clone());
                    if let Err(e) = blocking(move || store.hold(&id, suspect)).await {
                        tracing::warn!(error = %e, meter = meter.id, "suspect energy reading not kept");
                    }
                }
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

    /// A meter's own price now (see [`own_price`]).
    fn own_price(&self, meter: &MeterConfig) -> Option<f64> {
        let supplier = meter
            .price_point
            .as_ref()
            .and_then(|p| self.hub.current_state(&PointId::from(p.clone())))
            .and_then(|s| s.value.as_f64());
        own_price(meter, supplier)
    }

    /// Spreads `kwh` over `[from, to)` and prices each hour.
    fn slices(&self, meter: &MeterConfig, kwh: f64, from: u64, to: u64) -> Vec<HourRow> {
        let own = self.own_price(meter);
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
                let price = own.or_else(|| {
                    meter.role.consumes().then_some(())?;
                    self.periods
                        .at(middle)
                        .and_then(|p| self.config.period_price(p))
                        .or_else(|| self.prices.at(middle))
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
        if let Some(f) = meter.kwh_per_unit {
            // Said by the configuration (a gas index in m³): no unit to look up.
            self.factors.insert(meter.id.clone(), f);
            return Some(f);
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

    #[test]
    fn the_battery_takes_its_share_of_the_live_power() {
        let meter = |role: Role, power_w: Option<f64>| MeterInfo {
            id: format!("{role:?}"),
            name: String::new(),
            role,
            point: "a:b/c".into(),
            power: None,
            power_w,
            power_ts: None,
            within: None,
            feeds: Vec::new(),
            estimated: false,
        };
        let meters = [
            meter(Role::Solar, Some(3000.0)),
            meter(Role::Total, Some(-200.0)),
        ];
        // 3 kW made, 200 W sent out, 1 kW into the battery: 1.8 kW used.
        let f = Flows::of(&meters, Some(1000.0)).unwrap();
        assert_eq!(f.battery_w, Some(1000.0));
        assert_eq!(f.consumption_w, Some(1800.0));
        // At night it gives back: 500 W bought + 700 W from the battery.
        let night = [
            meter(Role::Solar, Some(0.0)),
            meter(Role::Total, Some(500.0)),
        ];
        assert_eq!(
            Flows::of(&night, Some(-700.0)).unwrap().consumption_w,
            Some(1200.0)
        );
        assert_eq!(Flows::of(&night, None).unwrap().consumption_w, Some(500.0));

        let config = Battery {
            soc: Some("bms:b/soc".into()),
            power: Some("bms:b/power".into()),
            discharge_positive: false,
            capacity_kwh: Some(10.0),
        };
        let b = BatteryInfo::of(&config, Some((45.0, 1)), Some((-300.0, 2)));
        assert_eq!(
            (b.soc, b.power_w, b.stored_kwh),
            (Some(45.0), Some(-300.0), Some(4.5))
        );
        // An inverter that counts what it gives the home: flipped.
        let flipped = Battery {
            discharge_positive: true,
            ..config.clone()
        };
        assert_eq!(
            BatteryInfo::of(&flipped, None, Some((300.0, 2))).power_w,
            Some(-300.0)
        );
        // A charge outside 0–100 % is a bad reading.
        let bad = BatteryInfo::of(&config, Some((140.0, 1)), None);
        assert_eq!((bad.soc, bad.stored_kwh), (None, None));
    }

    #[test]
    fn the_battery_power_comes_from_its_meters_without_a_power_point() {
        let meter = |id: &str, role: Role, power: Option<(&str, f64, u64)>| MeterInfo {
            id: id.into(),
            name: String::new(),
            role,
            point: "a:b/c".into(),
            power: power.map(|(p, _, _)| p.into()),
            power_w: power.map(|(_, w, _)| w),
            power_ts: power.map(|(_, _, ts)| ts),
            within: None,
            feeds: Vec::new(),
            estimated: false,
        };
        // Two positive powers (integrated meters): 1.2 kW in, nothing out.
        let meters = [
            meter("pv", Role::Solar, Some(("inv:i/power", 2000.0, 5))),
            meter(
                "bat-in",
                Role::Charge,
                Some(("jk:b/charge_power", 1200.0, 7)),
            ),
            meter(
                "bat-out",
                Role::Discharge,
                Some(("jk:b/discharge_power", 0.0, 6)),
            ),
        ];
        assert_eq!(meters_battery_power(&meters), Some((1200.0, 7)));
        let b = BatteryInfo::of(&Battery::default(), None, None).or_meters(&meters);
        assert_eq!((b.power_w, b.power_ts), (Some(1200.0), Some(7)));
        // At night it gives back 700 W; a few negative watts at rest are none.
        let night = [
            meter("bat-in", Role::Charge, Some(("jk:b/charge_power", -3.0, 9))),
            meter(
                "bat-out",
                Role::Discharge,
                Some(("jk:b/discharge_power", 700.0, 9)),
            ),
        ];
        assert_eq!(meters_battery_power(&night), Some((-700.0, 9)));
        // Two indexes on one power point count it once.
        let shared = [
            meter("in-a", Role::Charge, Some(("bms:b/in", 500.0, 1))),
            meter("in-b", Role::Charge, Some(("bms:b/in", 500.0, 1))),
            meter("out", Role::Discharge, Some(("bms:b/out", 0.0, 1))),
        ];
        assert_eq!(meters_battery_power(&shared), Some((500.0, 1)));
        // Nothing reads yet, or only indexes: no power.
        let silent = [
            meter("bat-in", Role::Charge, None),
            meter("bat-out", Role::Discharge, None),
        ];
        assert_eq!(meters_battery_power(&silent), None);
        // A `power` point of its own wins, even when it does not read.
        let config = Battery {
            power: Some("bms:b/power".into()),
            ..Battery::default()
        };
        let own = BatteryInfo::of(&config, None, Some((-300.0, 2))).or_meters(&meters);
        assert_eq!(own.power_w, Some(-300.0));
        assert_eq!(
            BatteryInfo::of(&config, None, None)
                .or_meters(&meters)
                .power_w,
            None
        );
    }

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
    fn a_late_meter_paces_the_month_on_what_it_has_reported() {
        let row = |meter: &str, hour| HourRow {
            meter: meter.to_owned(),
            hour,
            kwh: 1.0,
            cost: None,
        };
        let meter = |id: &str, role: &str, late: bool| {
            format!(
                "[[meter]]\nid = \"{id}\"\nname = \"x\"\npoint = \"a:b/c\"\nrole = \"{role}\"\nhistory = {late}\n"
            )
        };
        let config = |text: String| toml::from_str::<Config>(&text).unwrap();
        let late = config(meter("linky", "grid", true));
        let rows = [
            row("linky", 100 * H),
            row("linky", 130 * H),
            row("old", 90 * H),
        ];
        assert_eq!(pace_until(&late, &rows, 100 * H, 150 * H), 131 * H);
        // Nothing reported this month: no pace at all, not a pace of zero.
        assert_eq!(pace_until(&late, &rows[2..], 100 * H, 150 * H), 100 * H);
        // A meter read live says the present.
        let live = config(format!(
            "{}{}",
            meter("linky", "grid", true),
            meter("em", "total", false)
        ));
        assert_eq!(pace_until(&live, &rows, 100 * H, 150 * H), 150 * H);
        assert_eq!(
            pace_until(&config(String::new()), &rows, 100 * H, 150 * H),
            150 * H
        );
    }

    #[tokio::test]
    async fn a_whole_day_fills_only_a_day_without_any_hour() {
        let config: Config = toml::from_str(
            "[[meter]]\nid = \"linky\"\nname = \"Linky\"\npoint = \"enedis:linky/hours\"\nrole = \"grid\"\nhistory = true\nprice = 0.5\n",
        )
        .unwrap();
        let hub = Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let store = Arc::new(Store::memory());
        let mut recorder = Recorder::new(hub, config, HashMap::new(), Arc::clone(&store));
        let point = "enedis:linky/hours";
        let text = |json: String| Value::Text(json.into());
        // The curve gave two hours of the first day, then stopped.
        let curve = format!(
            r#"[{{"hour":{},"kwh":1.0}},{{"hour":{},"kwh":1.0}}]"#,
            24 * H,
            25 * H
        );
        recorder.on_meter(point, &text(curve), 30 * H).await;
        // Then the daily totals: that day (left as it is), the next one (spread),
        // one of 22 hours (refused), and a negative one (refused).
        let days = format!(
            r#"[{{"day":{},"to":{},"kwh":48.0}},{{"day":{},"to":{},"kwh":12.0}},{{"day":{},"to":{},"kwh":5.0}},{{"day":{},"to":{},"kwh":-1.0}}]"#,
            24 * H,
            48 * H,
            48 * H,
            72 * H,
            72 * H,
            94 * H,
            96 * H,
            120 * H
        );
        recorder.on_meter(point, &text(days.clone()), 80 * H).await;
        // The same again: nothing counts twice.
        recorder.on_meter(point, &text(days), 81 * H).await;
        let rows = store.hours(0, 200 * H).unwrap();
        let first: f64 = rows.iter().filter(|r| r.hour < 48 * H).map(|r| r.kwh).sum();
        let second: Vec<_> = rows
            .iter()
            .filter(|r| (48 * H..72 * H).contains(&r.hour))
            .collect();
        assert!((first - 2.0).abs() < 1e-9, "{first}");
        assert_eq!(second.len(), 24);
        assert!(
            second
                .iter()
                .all(|r| (r.kwh - 0.5).abs() < 1e-9 && r.cost == Some(0.25))
        );
        assert!(rows.iter().all(|r| r.hour < 72 * H));
    }

    #[tokio::test]
    async fn a_late_report_files_each_hour_once_at_its_place() {
        let config: Config = toml::from_str(
            "[[meter]]\nid = \"linky\"\nname = \"Linky\"\npoint = \"enedis:linky/hours\"\nrole = \"grid\"\nhistory = true\nprice = 0.5\n",
        )
        .unwrap();
        let hub = Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let store = Arc::new(Store::memory());
        let mut recorder = Recorder::new(hub, config, HashMap::new(), Arc::clone(&store));
        let point = "enedis:linky/hours";
        let text = |json: String| Value::Text(json.into());
        let first = format!(
            r#"[{{"hour":{},"kwh":2.0}},{{"hour":{},"kwh":1.0}}]"#,
            10 * H,
            11 * H
        );
        recorder.on_meter(point, &text(first.clone()), 30 * H).await;
        // The same report again, then a new hour, an impossible one, and nonsense.
        recorder.on_meter(point, &text(first), 31 * H).await;
        let more = format!(
            r#"[{{"hour":{},"kwh":4.0}},{{"hour":{},"kwh":-1.0}}]"#,
            12 * H,
            13 * H
        );
        recorder.on_meter(point, &text(more), 32 * H).await;
        recorder.on_meter(point, &text("nope".into()), 33 * H).await;
        recorder.on_meter(point, &Value::Null, 34 * H).await;
        let rows = store.hours(0, 100 * H).unwrap();
        let filed: Vec<(u64, f64, Option<f64>)> =
            rows.iter().map(|r| (r.hour, r.kwh, r.cost)).collect();
        assert_eq!(
            filed,
            vec![
                (10 * H, 2.0, Some(1.0)),
                (11 * H, 1.0, Some(0.5)),
                (12 * H, 4.0, Some(2.0)),
            ]
        );
    }

    #[tokio::test]
    async fn late_gas_hours_are_filed_with_their_own_price_only() {
        let config: Config = toml::from_str(
            "[[meter]]\nid = \"linky\"\nname = \"Linky\"\npoint = \"enedis:linky/hours\"\nrole = \"grid\"\nhistory = true\nprice = 0.5\n\
             [[meter]]\nid = \"gaz\"\nname = \"Gaz\"\npoint = \"grdf:gazpar/hours\"\nrole = \"gas\"\nhistory = true\nprice = 0.1\n\
             [[meter]]\nid = \"gaz2\"\nname = \"Gaz sans prix\"\npoint = \"grdf2:gazpar/hours\"\nrole = \"gas\"\nhistory = true\n",
        )
        .unwrap();
        let hub = Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let store = Arc::new(Store::memory());
        let mut recorder = Recorder::new(hub, config, HashMap::new(), Arc::clone(&store));
        let text = |json: String| Value::Text(json.into());
        let hour = |kwh: f64| format!(r#"[{{"hour":{},"kwh":{kwh}}}]"#, 10 * H);
        recorder
            .on_meter("enedis:linky/hours", &text(hour(1.0)), 30 * H)
            .await;
        recorder
            .on_meter("grdf:gazpar/hours", &text(hour(4.0)), 30 * H)
            .await;
        recorder
            .on_meter("grdf2:gazpar/hours", &text(hour(2.0)), 30 * H)
            .await;
        let rows = store.hours(0, 100 * H).unwrap();
        let cost = |id: &str| rows.iter().find(|r| r.meter == id).map(|r| (r.kwh, r.cost));
        assert_eq!(cost("gaz"), Some((4.0, Some(0.4))));
        // Never the electricity's price.
        assert_eq!(cost("gaz2"), Some((2.0, None)));
    }

    #[tokio::test]
    async fn the_house_price_prices_the_meters_without_one_from_its_time() {
        let config: Config = toml::from_str(
            "[[meter]]\nid = \"plug\"\nname = \"Plug\"\npoint = \"t:p/e\"\nrole = \"total\"\n\
             [[meter]]\nid = \"own\"\nname = \"Own\"\npoint = \"t:o/e\"\nrole = \"total\"\nprice = 0.5\n\
             [[meter]]\nid = \"pv\"\nname = \"PV\"\npoint = \"t:s/e\"\nrole = \"solar\"\n",
        )
        .unwrap();
        let hub = Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let recorder = Recorder::new(
            hub,
            config.clone(),
            HashMap::new(),
            Arc::new(Store::memory()),
        )
        .with_prices(HousePrices::new(vec![HousePrice {
            since: 2 * H,
            price: 0.2,
        }]));
        let costs = |id: &str| -> Vec<Option<f64>> {
            let meter = config.meters.iter().find(|m| m.id == id).unwrap();
            recorder
                .slices(meter, 3.0, H, 4 * H)
                .iter()
                .map(|s| s.cost.map(|c| (c * 1000.0).round() / 1000.0))
                .collect()
        };
        // Before the price began, the hour has none; then 1 kWh × 0.2.
        assert_eq!(costs("plug"), vec![None, Some(0.2), Some(0.2)]);
        // A meter's own price comes first; the panels are never billed.
        assert_eq!(costs("own"), vec![Some(0.5); 3]);
        assert_eq!(costs("pv"), vec![None; 3]);
        // A late report: each hour at the price of its own time.
        let rows = [3 * H / 2 - H / 2, 2 * H].map(|hour| ImportRow {
            meter: "plug".into(),
            hour,
            kwh: 1.0,
        });
        let tz = TimeZone::UTC;
        let (priced, _) = price_rows(&config, &tz, &recorder.prices.all(), &rows);
        let priced: Vec<Option<f64>> = priced.iter().map(|r| r.cost).collect();
        assert_eq!(priced, vec![None, Some(0.2)]);
    }

    #[test]
    fn a_supplier_price_plus_what_it_leaves_out() {
        let config: Config = toml::from_str(
            "[[meter]]\nid = \"day\"\nname = \"Jour\"\npoint = \"p1:m/day\"\nrole = \"grid\"\n\
             price = 0.3\nprice_point = \"engie:c/electricity_offtake_peak\"\nprice_extra = 0.11692\n\
             [[meter]]\nid = \"out\"\nname = \"Injection\"\npoint = \"p1:m/out\"\nrole = \"export\"\n\
             price_point = \"engie:c/electricity_injection_peak\"\n",
        )
        .unwrap();
        config.check().unwrap();
        let (day, out) = (&config.meters[0], &config.meters[1]);
        let near = |p: Option<f64>, v: f64| (p.unwrap() - v).abs() < 1e-9;
        assert!(near(own_price(day, Some(0.25009)), 0.36701));
        // No value yet, or a misreading: the fixed price.
        assert!(near(own_price(day, None), 0.3));
        assert!(near(own_price(day, Some(25.0)), 0.3));
        assert!(near(own_price(out, Some(0.04635)), 0.04635));
        assert_eq!(own_price(out, None), None);
        // `price_extra` needs a point to add to.
        let bad: Config = toml::from_str(
            "[[meter]]\nid = \"x\"\nname = \"x\"\npoint = \"a:b/c\"\nrole = \"grid\"\nprice_extra = 0.1\n",
        )
        .unwrap();
        assert!(bad.check().is_err());
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

//! The « Énergie » device: the headline figures as points, for automations
//! and agents (Home Assistant's « Coût mois », « Projection facture fin de
//! mois »…). Moli runs it by itself, as instance [`INSTANCE`], whenever
//! `[energy]` exists: nothing to declare.

use std::time::Duration;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointId, PointSpec, Semantic, Unit};
use moli_core::{Value, now_ms};
use moli_runtime::{BoxFuture, Driver, DriverCtx};

use crate::{Energy, Summary};

/// The instance (and so the device's prefix: `energie:maison/…`).
pub const INSTANCE: &str = "energie";
const DEVICE: &str = "maison";
/// Hourly figures: every 5 minutes is plenty, and costs nothing.
const EVERY: Duration = Duration::from_secs(300);
const HOUR_MS: u64 = 3_600_000;
/// Before this, a month's pace says little: Home Assistant waited for the
/// 6th before alerting on the budget.
const SETTLED_MS: u64 = 5 * 24 * HOUR_MS;

#[derive(Debug)]
pub struct Points {
    energy: Energy,
}

impl Points {
    #[must_use]
    pub fn new(energy: Energy) -> Self {
        Self { energy }
    }

    /// The monthly budget, as its point holds it now.
    fn budget(&self) -> Budget {
        let Some(point) = &self.energy.inner.config.budget else {
            return Budget::Undeclared;
        };
        self.energy
            .inner
            .hub
            .state(&PointId::from(point.clone()))
            .and_then(|s| s.value.as_f64())
            .map_or(Budget::Unknown, Budget::Is)
    }

    async fn refresh(&self, ctx: &DriverCtx, id: &DeviceId) {
        match self.energy.summary().await {
            Ok(summary) => {
                let fee = self.energy.inner.config.monthly_fee;
                for (key, value) in readings(&summary, fee, self.budget(), now_ms()) {
                    ctx.set_state(id, key, value);
                }
                ctx.set_availability(id, true);
            }
            Err(e) => {
                tracing::warn!(error = %e, "energy figures unavailable");
                ctx.set_availability(id, false);
            }
        }
    }
}

impl Driver for Points {
    fn kind(&self) -> &'static str {
        "energy"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id(DEVICE);
            let config = &self.energy.inner.config;
            ctx.upsert_device(device(&id, ctx.instance(), config));
            ctx.ready();
            loop {
                self.refresh(ctx, &id).await;
                tokio::select! {
                    command = ctx.next_command() => match command {
                        // Read-only points: the hub never sends any.
                        Some(command) => {
                            command.reply(Err(moli_i18n::tr!("serveur.energie.lecture_seule")));
                        }
                        None => return Ok(()),
                    },
                    () = tokio::time::sleep(EVERY) => {}
                }
            }
        })
    }
}

fn device(id: &DeviceId, instance: &InstanceId, config: &crate::Config) -> Device {
    let money = || Unit::parse(&config.currency).unwrap_or_else(|| Unit::Other("€".into()));
    let mut points = vec![
        number(
            "today_kwh",
            &moli_i18n::tr!("serveur.energie.point.today_kwh"),
            Unit::KiloWattHour,
        ),
        number(
            "today_cost",
            &moli_i18n::tr!("serveur.energie.point.today_cost"),
            money(),
        ),
        number(
            "yesterday_kwh",
            &moli_i18n::tr!("serveur.energie.point.yesterday_kwh"),
            Unit::KiloWattHour,
        ),
        number(
            "yesterday_cost",
            &moli_i18n::tr!("serveur.energie.point.yesterday_cost"),
            money(),
        ),
        number(
            "month_kwh",
            &moli_i18n::tr!("serveur.energie.point.month_kwh"),
            Unit::KiloWattHour,
        ),
        number(
            "month_cost",
            &moli_i18n::tr!("serveur.energie.point.month_cost"),
            money(),
        ),
        number(
            "projection_kwh",
            &moli_i18n::tr!("serveur.energie.point.projection_kwh"),
            Unit::KiloWattHour,
        ),
        number(
            "bill_projection",
            &moli_i18n::tr!("serveur.energie.point.bill_projection"),
            money(),
        ),
    ];
    if config.budget.is_some() {
        points.push(PointSpec {
            key: "over_budget".into(),
            label: moli_i18n::tr!("serveur.energie.point.over_budget").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Other,
        });
    }
    Device {
        id: id.clone(),
        instance: instance.clone(),
        native_name: moli_i18n::tr!("serveur.energie.appareil").into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("serveur.energie.modele").into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// Never `Semantic::Energy`: these are summaries, not meters to add up.
fn number(key: &str, label: &str, unit: Unit) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: None,
            max: None,
            step: None,
        },
        access: Access {
            read: true,
            write: false,
        },
        unit: Some(unit),
        semantic: Semantic::Other,
    }
}

#[derive(Clone, Copy, Debug)]
enum Budget {
    /// No `budget` in `[energy]`: no `over_budget` point.
    Undeclared,
    /// Its point holds no number (yet): never « over ».
    Unknown,
    Is(f64),
}

/// The points' values.
fn readings(
    summary: &Summary,
    fee: Option<f64>,
    budget: Budget,
    now: u64,
) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    let mut put = |kwh_key, cost_key, kwh: f64, cost: Option<f64>| {
        out.push((kwh_key, Value::Float(round2(kwh))));
        if let Some(cost) = cost {
            out.push((cost_key, Value::Float(round2(cost))));
        }
    };
    put(
        "today_kwh",
        "today_cost",
        summary.today.total.kwh,
        summary.today.total.cost,
    );
    put(
        "yesterday_kwh",
        "yesterday_cost",
        summary.yesterday.total.kwh,
        summary.yesterday.total.cost,
    );
    put(
        "month_kwh",
        "month_cost",
        summary.month.total.kwh,
        summary.month.total.cost,
    );
    let bill = summary.month_projection.as_ref().and_then(|p| {
        out.push(("projection_kwh", Value::Float(round2(p.kwh))));
        p.cost.map(|c| c + fee.unwrap_or(0.0))
    });
    if let Some(bill) = bill {
        out.push(("bill_projection", Value::Float(round2(bill))));
    }
    let budget = match budget {
        Budget::Undeclared => return out,
        Budget::Unknown => None,
        Budget::Is(b) => Some(b),
    };
    let settled = now.saturating_sub(summary.month.from) >= SETTLED_MS;
    let over = settled && bill.zip(budget).is_some_and(|(bill, budget)| bill > budget);
    out.push(("over_budget", Value::Bool(over)));
    out
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Amount, Period};

    fn period(from: u64, kwh: f64, cost: Option<f64>) -> Period {
        Period {
            from,
            to: from + 30 * 24 * HOUR_MS,
            total: Amount { kwh, cost },
            meters: std::collections::BTreeMap::new(),
            unmeasured: None,
        }
    }

    fn summary(month_from: u64, projection: Option<Amount>) -> Summary {
        Summary {
            currency: "€".into(),
            timezone: "Europe/Paris".into(),
            since: Some(0),
            period: None,
            price_now: None,
            period_point: None,
            meters: Vec::new(),
            live: None,
            today: period(month_from, 9.876, Some(1.5)),
            yesterday: period(month_from, 12.0, None),
            month: period(month_from, 120.0, Some(20.0)),
            last_month: period(0, 0.0, None),
            month_projection: projection,
            monthly_fee: Some(19.56),
            budget_point: None,
        }
    }

    fn get<'a>(out: &'a [(&str, Value)], key: &str) -> Option<&'a Value> {
        out.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    #[test]
    fn the_bill_adds_the_subscription_to_the_pace() {
        let month = 1_000 * HOUR_MS;
        let s = summary(
            month,
            Some(Amount {
                kwh: 360.0,
                cost: Some(60.0),
            }),
        );
        let now = month + 6 * 24 * HOUR_MS;
        let out = readings(&s, Some(19.56), Budget::Is(70.0), now);
        assert_eq!(get(&out, "today_kwh"), Some(&Value::Float(9.88)));
        assert_eq!(get(&out, "today_cost"), Some(&Value::Float(1.5)));
        assert_eq!(get(&out, "yesterday_cost"), None, "no price, no cost");
        assert_eq!(get(&out, "bill_projection"), Some(&Value::Float(79.56)));
        assert_eq!(get(&out, "over_budget"), Some(&Value::Bool(true)));
        let out = readings(&s, Some(19.56), Budget::Is(80.0), now);
        assert_eq!(get(&out, "over_budget"), Some(&Value::Bool(false)));
    }

    #[test]
    fn no_alarm_too_early_without_a_budget_or_a_pace() {
        let month = 1_000 * HOUR_MS;
        let s = summary(
            month,
            Some(Amount {
                kwh: 360.0,
                cost: Some(600.0),
            }),
        );
        let early = month + 2 * 24 * HOUR_MS;
        let out = readings(&s, None, Budget::Is(70.0), early);
        assert_eq!(
            get(&out, "over_budget"),
            Some(&Value::Bool(false)),
            "too early"
        );
        let late = month + 10 * 24 * HOUR_MS;
        let out = readings(&s, None, Budget::Undeclared, late);
        assert_eq!(get(&out, "over_budget"), None, "no budget declared");
        let out = readings(&s, None, Budget::Unknown, late);
        assert_eq!(
            get(&out, "over_budget"),
            Some(&Value::Bool(false)),
            "budget unknown"
        );
        let out = readings(&summary(month, None), None, Budget::Is(70.0), late);
        assert_eq!(get(&out, "bill_projection"), None);
        assert_eq!(get(&out, "over_budget"), Some(&Value::Bool(false)));
    }
}

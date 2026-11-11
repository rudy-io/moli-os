//! ENGIE's answers decoded, pure (no I/O). Field names as the ENGIE Smart
//! app's API sends them (documented by aioengiebelgium's parsers); anything
//! missing or malformed is left out rather than guessed.

use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::Value as Json;

fn text(v: &Json) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn number(v: &Json) -> Option<f64> {
    match v {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

fn time(v: &Json) -> Option<Timestamp> {
    v.as_str()?.parse().ok()
}

fn date(v: &Json) -> Option<Date> {
    let s = v.as_str()?;
    s.get(..10)?.parse().ok()
}

fn items(v: &Json) -> impl Iterator<Item = &Json> {
    v.as_array().into_iter().flatten()
}

/// A contract of the account (`businessAgreement`): what prices and peaks
/// are asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agreement {
    pub number: String,
    pub address: Option<String>,
}

/// The customer account and its active contracts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// `customerAccountNumber`: what the vehicles and sessions are asked by.
    pub number: Option<String>,
    pub agreements: Vec<Agreement>,
}

/// `GET …/customer-account-relations`.
#[must_use]
pub fn account(json: &Json) -> Account {
    let mut account = Account::default();
    for relation in items(&json["items"]) {
        let ca = &relation["customerAccount"];
        let Some(number) = text(&ca["customerAccountNumber"]) else {
            continue;
        };
        account.number.get_or_insert(number);
        for ag in items(&ca["businessAgreements"]) {
            if ag["active"] != Json::Bool(true) {
                continue;
            }
            let Some(number) = text(&ag["businessAgreementNumber"]) else {
                continue;
            };
            account.agreements.push(Agreement {
                number,
                address: address(&ag["consumptionAddress"]),
            });
        }
    }
    account
}

fn address(a: &Json) -> Option<String> {
    let street = [&a["street"], &a["houseNumber"]]
        .into_iter()
        .filter_map(text)
        .collect::<Vec<_>>()
        .join(" ");
    let city = text(&a["city"]).or_else(|| text(&a["municipality"]));
    let parts: Vec<String> = [Some(street).filter(|s| !s.is_empty()), city]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// One price of a contract: which energy, which way, which time slot.
#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    /// The point: `electricity_offtake_peak`, `gas_offtake`…
    pub key: String,
    /// € per kWh, VAT included.
    pub value: f64,
}

/// The slot's name in a point key: `S_TOU1_OFFTAKE_HIGH_LOAD_HOURS` → `peak`;
/// a single all-day price → empty. `None` for the blended `EN` code, which
/// Home Assistant leaves out too (it is not a price one pays).
fn slot(raw: &str) -> Option<String> {
    let upper = raw.to_ascii_uppercase();
    let rest = ["OFFTAKE_", "INJECTION_"]
        .iter()
        .find_map(|d| upper.find(d).map(|at| &upper[at + d.len()..]))
        .unwrap_or(&upper);
    Some(match rest {
        "EN" => return None,
        "HIGH_LOAD_HOURS" => "peak".to_owned(),
        "LOW_LOAD_HOURS" => "offpeak".to_owned(),
        "" | "TOTAL_HOURS" => String::new(),
        other => other.to_ascii_lowercase(),
    })
}

/// `GET …/supplier-energy-prices`, with what each EAN carries (`ELECTRICITY`,
/// `GAS`, from its service point): the prices of the period covering `today`.
#[must_use]
pub fn prices(json: &Json, today: Date, energy_of: impl Fn(&str) -> Option<String>) -> Vec<Price> {
    let mut out = Vec::new();
    for item in items(&json["items"]) {
        let ean = text(&item["ean"]).unwrap_or_default();
        let energy =
            energy_of(&ean).map_or_else(|| "electricity".to_owned(), |e| e.to_ascii_lowercase());
        let current = items(&item["prices"]).find(|p| {
            date(&p["from"]).is_none_or(|from| from <= today)
                && date(&p["to"]).is_none_or(|to| today <= to)
        });
        let Some(period) = current else { continue };
        let configs = &period["proportionalPriceConfigurations"];
        for direction in ["offtake", "injection"] {
            for s in items(&configs[direction]) {
                let Some(code) = text(&s["timeOfUseSlotCode"]) else {
                    continue;
                };
                let Some(slot) = slot(&code) else { continue };
                let Some(value) =
                    number(&s["priceValueInEuroPerKWH"]).or_else(|| number(&s["priceValue"]))
                else {
                    continue;
                };
                let key = if slot.is_empty() {
                    format!("{energy}_{direction}")
                } else {
                    format!("{energy}_{direction}_{slot}")
                };
                if !out.iter().any(|p: &Price| p.key == key) {
                    out.push(Price { key, value });
                }
            }
        }
    }
    out
}

/// `GET …/service-points/{ean}`: `ELECTRICITY` or `GAS`.
#[must_use]
pub fn division(json: &Json) -> Option<String> {
    text(&json["division"])
}

/// A quarter-hour peak of the capacity tariff.
#[derive(Clone, Debug, PartialEq)]
pub struct Peak {
    pub kw: f64,
    pub kwh: f64,
    pub start: Timestamp,
}

/// The month's peaks: the month's own (none yet on its first day), and the
/// day's highest so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peaks {
    pub month: Option<Peak>,
    pub previous_month: Option<Peak>,
    pub today: Option<Peak>,
}

fn peak(v: &Json) -> Option<Peak> {
    Some(Peak {
        kw: number(&v["peakKW"])?,
        kwh: number(&v["peakKWh"]).unwrap_or(0.0),
        start: time(&v["start"])?,
    })
}

/// `GET …/energy-insights/peaks?year=&month=`; `today_start` is the start of
/// the day in the house's zone.
#[must_use]
pub fn peaks(json: &Json, today_start: Timestamp) -> Peaks {
    Peaks {
        month: peak(&json["peakOfTheMonth"]),
        previous_month: peak(&json["previousPeakOfTheMonth"]),
        today: items(&json["dailyPeaks"])
            .filter_map(peak)
            .filter(|p| p.start >= today_start)
            .max_by(|a, b| a.kw.total_cmp(&b.kw)),
    }
}

/// A vehicle linked to the account's smart charging.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vehicle {
    pub id: u64,
    pub name: String,
    pub model: Option<String>,
    pub brand: Option<String>,
    pub reachable: Option<bool>,
    /// `unplugged`, `plugged_in`, `charging`… (lowercase, as ENGIE says it).
    pub status: Option<String>,
    pub battery: Option<f64>,
    pub range_km: Option<f64>,
    pub power_kw: Option<f64>,
}

/// `GET …/customer-accounts/{can}/vehicles`: the active ones.
#[must_use]
pub fn vehicles(json: &Json) -> Vec<Vehicle> {
    items(&json["items"])
        .filter(|v| v["active"] != Json::Bool(false))
        .filter_map(|v| {
            let id = v["id"].as_u64()?;
            let state = &v["chargeState"];
            let brand = text(&v["brandName"]);
            let model = text(&v["modelName"]);
            let name = text(&v["displayName"])
                .or_else(|| {
                    Some(
                        [brand.clone(), model.clone()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                    .filter(|n| !n.is_empty())
                })
                .unwrap_or_else(|| format!("{id}"));
            Some(Vehicle {
                id,
                name,
                model,
                brand,
                reachable: v["reachable"].as_bool(),
                status: text(&state["status"]).map(|s| s.to_ascii_lowercase()),
                battery: number(&state["batteryLevel"]),
                range_km: number(&state["range"]),
                power_kw: number(&state["chargePower"]),
            })
        })
        .collect()
}

/// The last charging session of a vehicle.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub kwh: Option<f64>,
    pub cost: Option<f64>,
    /// What smart charging earned on it, in €.
    pub reward: Option<f64>,
    pub status: Option<String>,
    /// `on_target`, `off_target`: whether the car was ready in time.
    pub outcome: Option<String>,
}

/// `GET …/vehicles/{id}/charging-sessions/latest`.
#[must_use]
pub fn session(json: &Json) -> Option<Session> {
    let s = json.get("session").filter(|s| s.is_object())?;
    Some(Session {
        kwh: number(&s["totalConsumptionKwh"]),
        cost: number(&s["cost"]),
        reward: number(&s["reward"]),
        status: text(&s["status"]).map(|v| v.to_ascii_lowercase()),
        outcome: text(&s["smartChargeOutcome"]["state"]).map(|v| v.to_ascii_lowercase()),
    })
}

/// What the current session aims for: when the car leaves, how full, and
/// whether it charges at once instead of at the cheapest hours.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub departure: Option<Timestamp>,
    pub target: Option<u8>,
    pub direct: Option<bool>,
}

/// `GET`/`PUT …/charging-sessions/latest/charge-settings`.
#[must_use]
pub fn settings(json: &Json) -> Settings {
    Settings {
        departure: time(&json["departureTimeOverride"]),
        target: json["targetBatteryLevel"]
            .as_u64()
            .and_then(|t| u8::try_from(t).ok())
            .filter(|t| *t <= 100),
        direct: json["direct"].as_bool(),
    }
}

/// The body of a `PUT …/latest/charge-settings`: all three, always.
#[must_use]
pub fn settings_body(departure: Timestamp, target: u8, direct: bool) -> Json {
    serde_json::json!({
        "departureTimeOverride": departure.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "targetBatteryLevel": target,
        "direct": direct,
    })
}

/// What smart charging earned: this month, and over all the months asked.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rewards {
    pub month: f64,
    pub total: f64,
    pub sessions: u64,
}

/// `GET …/charging-sessions-summary?granularity=MONTHLY`; `month_start` is
/// the start of the current month.
#[must_use]
pub fn rewards(json: &Json, month_start: Timestamp) -> Rewards {
    let mut r = Rewards::default();
    for entry in items(&json["items"]) {
        let reward = number(&entry["reward"]).unwrap_or(0.0);
        r.total += reward;
        r.sessions += entry["sessionCount"].as_u64().unwrap_or(0);
        if time(&entry["start"]).is_some_and(|t| t >= month_start) {
            r.month += reward;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Json {
        let path = format!(
            "{}/../../integrations/engie/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_account_and_its_active_contracts() {
        let a = account(&fixture("customer-account-relations"));
        assert_eq!(a.number.as_deref(), Some("000000000001"));
        assert_eq!(
            a.agreements,
            [Agreement {
                number: "000000000002".into(),
                address: Some("Rue Exemple 1, Exempleville".into()),
            }]
        );
    }

    #[test]
    fn prices_of_the_current_period_by_slot() {
        let today: Date = "2026-11-11".parse().unwrap();
        let energy = |ean: &str| {
            Some(
                if ean.ends_with('9') {
                    "GAS"
                } else {
                    "ELECTRICITY"
                }
                .into(),
            )
        };
        let p = prices(&fixture("supplier-energy-prices"), today, energy);
        let get = |k: &str| p.iter().find(|p| p.key == k).map(|p| p.value);
        assert_eq!(get("electricity_offtake_peak"), Some(0.1452));
        assert_eq!(get("electricity_offtake_offpeak"), Some(0.1189));
        assert_eq!(get("electricity_injection_peak"), Some(0.0251));
        assert_eq!(get("gas_offtake"), Some(0.0613));
        assert_eq!(get("electricity_offtake_en"), None, "EN is left out");
        // October's period is over: its prices are not today's.
        assert!(p.iter().all(|p| (p.value - 0.99).abs() > 1e-9));
        assert_eq!(p.len(), 5);
    }

    #[test]
    fn slots_are_named_like_home_assistants() {
        assert_eq!(
            slot("S_TOU1_OFFTAKE_HIGH_LOAD_HOURS").as_deref(),
            Some("peak")
        );
        assert_eq!(slot("INJECTION_LOW_LOAD_HOURS").as_deref(), Some("offpeak"));
        assert_eq!(
            slot("OFFTAKE_SUPEROFFPEAK").as_deref(),
            Some("superoffpeak")
        );
        assert_eq!(slot("TOTAL_HOURS").as_deref(), Some(""));
        assert_eq!(slot("OFFTAKE_EN"), None);
    }

    #[test]
    fn the_months_peak_and_todays() {
        let today: Timestamp = "2026-11-10T23:00:00Z".parse().unwrap();
        let p = peaks(&fixture("peaks"), today);
        let month = p.month.unwrap();
        assert_eq!((month.kw, month.kwh), (4.23, 1.0575));
        assert_eq!(p.today.unwrap().kw, 2.1);
        assert_eq!(p.previous_month.unwrap().kw, 5.02);
        assert_eq!(peaks(&serde_json::json!({}), today), Peaks::default());
    }

    #[test]
    fn vehicles_sessions_and_settings() {
        let v = vehicles(&fixture("vehicles"));
        assert_eq!(v.len(), 1, "the inactive one is left out");
        let car = &v[0];
        assert_eq!((car.id, car.name.as_str()), (101, "Ma voiture"));
        assert_eq!(car.status.as_deref(), Some("plugged_in"));
        assert_eq!((car.battery, car.power_kw), (Some(62.0), Some(0.0)));

        let s = session(&fixture("latest-session")).unwrap();
        assert_eq!((s.kwh, s.reward), (Some(18.4), Some(0.87)));
        assert_eq!(s.outcome.as_deref(), Some("on_target"));
        assert_eq!(session(&serde_json::json!({})), None);

        let set = settings(&fixture("charge-settings"));
        assert_eq!((set.target, set.direct), (Some(80), Some(false)));
        let departure = set.departure.unwrap();
        assert_eq!(
            settings_body(departure, 90, true),
            serde_json::json!({
                "departureTimeOverride": "2026-11-12T06:30:00Z",
                "targetBatteryLevel": 90,
                "direct": true,
            })
        );
    }

    #[test]
    fn rewards_this_month_and_in_all() {
        let month: Timestamp = "2026-10-31T23:00:00Z".parse().unwrap();
        let r = rewards(&fixture("sessions-summary"), month);
        assert!((r.total - 14.25).abs() < 1e-9);
        assert!((r.month - 2.5).abs() < 1e-9);
        assert_eq!(r.sessions, 31);
    }
}

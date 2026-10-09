//! `[energy]` in `moli.toml`: which points are energy meters, what a kWh
//! costs. Edited by humans; no surface changes it.

use std::collections::{BTreeMap, HashSet};

use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Days and months are cut in this zone (IANA name).
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_currency")]
    pub currency: String,
    /// The most a meter can draw (kW): a reading implying more is a bad
    /// one. 36 kVA is the largest French residential contract.
    #[serde(default = "default_max_power")]
    pub max_power_kw: f64,
    /// The subscription, per month (taxes included): what the bill adds to
    /// the kWh. Only the « Énergie » device's estimated bill uses it.
    #[serde(default)]
    pub monthly_fee: Option<f64>,
    /// Point holding the monthly budget (a helper, in the currency): the
    /// « Énergie » device says when the estimated bill goes over it.
    #[serde(default)]
    pub budget: Option<String>,
    /// Price by tariff period, for meters without their own price.
    #[serde(default)]
    pub tariff: Option<Tariff>,
    /// What the supplier's meter draws right now, for the live gauge.
    #[serde(default)]
    pub live: Option<Live>,
    #[serde(default, rename = "meter")]
    pub meters: Vec<MeterConfig>,
}

fn default_timezone() -> String {
    "Europe/Paris".into()
}

fn default_currency() -> String {
    "€".into()
}

fn default_max_power() -> f64 {
    36.0
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tariff {
    /// Point holding the current period (Linky PTEC: « HP.. », « HC.. »).
    pub period: String,
    /// Price per kWh by period name; names compare without dots, spaces
    /// or case (« HP.. » is « hp »).
    pub prices: BTreeMap<String, f64>,
}

/// The live draw at the supplier's meter (Linky PAPP, in VA).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Live {
    /// Instantaneous power point (W or VA).
    pub power: String,
    /// Full scale, in the point's unit: the contract (9 kVA → 9000).
    pub max: f64,
    /// From here the draw is « soutenue » (the gauge turns orange, then red
    /// at twice as much). By default a quarter of `max`.
    #[serde(default)]
    pub warn: Option<f64>,
}

impl Live {
    #[must_use]
    pub fn warn(&self) -> f64 {
        self.warn.unwrap_or(self.max / 4.0)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterConfig {
    /// Storage key: never rename it (history is filed under it).
    pub id: String,
    pub name: String,
    /// A cumulative counter, in Wh or kWh (an index, or a counter that
    /// restarts each day or month); with `integrate`, a power point (W or
    /// kW) whose energy Moli adds up itself.
    pub point: String,
    pub role: Role,
    /// Fixed price per kWh (a Linky « heures creuses » index).
    #[serde(default)]
    pub price: Option<f64>,
    /// Instantaneous power point (W), for the live view.
    #[serde(default)]
    pub power: Option<String>,
    #[serde(default)]
    pub reset: Reset,
    /// Tariff period this counter runs in (a Linky « HC » index only moves
    /// in « heures creuses »): its energy is spread over that time only.
    #[serde(default)]
    pub period: Option<String>,
    /// `point` is a power: energy = power × time, held between readings.
    /// For plugs whose own counter cannot be trusted (a Tuya `add_ele`
    /// restarts at every report).
    #[serde(default)]
    pub integrate: bool,
    /// An appliance's circuit (a `circuit` or `total` meter id), when it is
    /// known: shown inside it. Without it, it is part of what no circuit
    /// explains.
    #[serde(default)]
    pub within: Option<String>,
    /// Devices this meter feeds (a circuit behind a clim): their cards show
    /// its power. Shared with others when it feeds more than them.
    #[serde(default)]
    pub feeds: Vec<String>,
    /// An appliance that measures nothing: a smart light, whose power Moli
    /// estimates from its model's measured profile and its state (`point`:
    /// any point of the light, or of a fixture `lumieres:…`). Shown « ≈ ».
    #[serde(default)]
    pub estimate: bool,
}

impl MeterConfig {
    /// The point that says the power now, for the live views.
    #[must_use]
    pub fn power_point(&self) -> Option<&str> {
        self.power
            .as_deref()
            .or_else(|| self.integrate.then_some(self.point.as_str()))
    }
}

/// What a meter measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Billed by the supplier (Linky indexes): the reference for kWh and cost.
    Grid,
    /// The whole home, measured finely (a clamp on the main feed).
    Total,
    /// One circuit of the panel: part of the total.
    Circuit,
    /// One appliance (a plug, a machine's own meter): shown on its own,
    /// never added to the total nor taken from what circuits leave over.
    Appliance,
}

/// What a decrease of the counter means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reset {
    /// A fall to under half is a restart (daily or monthly counter, device
    /// reboot); a smaller fall is a bad reading, ignored.
    #[default]
    Auto,
    /// The counter never restarts (an index): every fall is ignored.
    Never,
}

impl Config {
    pub fn check(&self) -> Result<(), String> {
        TimeZone::get(&self.timezone).map_err(|e| format!("timezone {:?}: {e}", self.timezone))?;
        let mut ids = HashSet::new();
        for m in &self.meters {
            if m.id.is_empty()
                || !m
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!(
                    "meter id {:?}: letters, digits, '_' or '-' only",
                    m.id
                ));
            }
            if !ids.insert(&m.id) {
                return Err(format!("duplicate meter id {:?}", m.id));
            }
            for point in std::iter::once(&m.point).chain(&m.power) {
                check_point(point).map_err(|e| format!("meter {:?}: {e}", m.id))?;
            }
            if let Some(bad) = m.feeds.iter().find(|d| !d.contains(':') || d.contains('/')) {
                return Err(format!(
                    "meter {:?}: feeds {bad:?} is not a device id",
                    m.id
                ));
            }
            if let Some(within) = &m.within {
                let inside = self
                    .meters
                    .iter()
                    .any(|o| &o.id == within && matches!(o.role, Role::Circuit | Role::Total));
                if m.role != Role::Appliance || !inside {
                    return Err(format!(
                        "meter {:?}: `within` is for an appliance, naming a circuit or total meter",
                        m.id
                    ));
                }
            }
            if m.estimate && (m.role != Role::Appliance || m.integrate || m.power.is_some()) {
                return Err(format!(
                    "meter {:?}: `estimate` is for an appliance, without `integrate` nor `power`",
                    m.id
                ));
            }
            if let Some(price) = m.price {
                check_price(price).map_err(|e| format!("meter {:?}: {e}", m.id))?;
            }
        }
        if let Some(live) = &self.live {
            check_point(&live.power).map_err(|e| format!("[energy.live] {e}"))?;
            if !(live.max.is_finite() && live.max > 0.0) {
                return Err(format!("[energy.live] max {} must be above 0", live.max));
            }
            if let Some(warn) = live.warn
                && !(warn.is_finite() && warn > 0.0 && warn <= live.max)
            {
                return Err(format!(
                    "[energy.live] warn {warn} must be above 0 and at most max"
                ));
            }
        }
        if let Some(fee) = self.monthly_fee {
            check_price(fee).map_err(|e| format!("monthly_fee: {e}"))?;
        }
        if let Some(budget) = &self.budget {
            check_point(budget).map_err(|e| format!("budget: {e}"))?;
        }
        if let Some(t) = &self.tariff {
            check_point(&t.period).map_err(|e| format!("[energy.tariff] {e}"))?;
            for (period, price) in &t.prices {
                check_price(*price).map_err(|e| format!("[energy.tariff] {period}: {e}"))?;
            }
        }
        Ok(())
    }

    /// The price of a kWh in this period, if the tariff knows it.
    #[must_use]
    pub fn period_price(&self, period: &str) -> Option<f64> {
        let wanted = period_key(period);
        self.tariff
            .as_ref()?
            .prices
            .iter()
            .find(|(name, _)| period_key(name) == wanted)
            .map(|(_, price)| *price)
    }
}

/// Period names compare without dots, spaces or case.
#[must_use]
pub fn period_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn check_point(point: &str) -> Result<(), String> {
    match point.split_once('/') {
        Some((device, key)) if device.contains(':') && !key.is_empty() => Ok(()),
        _ => Err(format!("{point:?} is not a point id (instance:device/key)")),
    }
}

fn check_price(price: f64) -> Result<(), String> {
    if price.is_finite() && price >= 0.0 {
        Ok(())
    } else {
        Err(format!("price {price} must be a positive number"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<(), String> {
        toml::from_str::<Config>(text)
            .map_err(|e| e.to_string())?
            .check()
    }

    #[test]
    fn checks_ids_points_prices_and_zone() {
        let ok = r#"
            [tariff]
            period = "z2m:linky/ptec"
            prices = { "HP" = 0.17, "HC" = 0.13 }
            [[meter]]
            id = "hc"
            name = "Heures creuses"
            point = "z2m:linky/tier1"
            role = "grid"
            price = 0.13
        "#;
        parse(ok).unwrap();
        assert!(parse(&ok.replace("\"hc\"", "\"h c\"")).is_err());
        assert!(parse(&ok.replace("z2m:linky/tier1", "tier1")).is_err());
        assert!(parse(&ok.replace("price = 0.13", "price = -1.0")).is_err());
        assert!(parse(&format!("timezone = \"Mars/Base\"\n{ok}")).is_err());
        assert!(parse(&ok.replace("role = \"grid\"", "role = \"solar\"")).is_err());
        let live = "[live]\npower = \"z2m:linky/apparent_power\"\nmax = 9000\n";
        parse(&format!("{live}{ok}")).unwrap();
        assert!(parse(&format!("{}{ok}", live.replace("9000", "0"))).is_err());
        assert!(parse(&format!("{}{ok}", live.replace("z2m:linky/", ""))).is_err());
        parse(&format!("{live}warn = 2000\n{ok}")).unwrap();
        assert!(parse(&format!("{live}warn = 9500\n{ok}")).is_err());
        let plug = "[[meter]]\nid = \"tv\"\nname = \"Télé\"\npoint = \"tuya:plug/cur_power\"\nrole = \"appliance\"\nintegrate = true\n";
        parse(&format!("{ok}{plug}")).unwrap();
        assert!(
            parse(&format!("{ok}{plug}within = \"hc\"\n")).is_err(),
            "inside a grid meter"
        );
        let circuit = "[[meter]]\nid = \"c3\"\nname = \"Circuit 3\"\npoint = \"em:x/e3\"\nrole = \"circuit\"\n";
        parse(&format!("{ok}{circuit}{plug}within = \"c3\"\n")).unwrap();
        let lamp = "[[meter]]\nid = \"lampe\"\nname = \"Lampe\"\npoint = \"hue:l/on\"\nrole = \"appliance\"\nestimate = true\n";
        parse(&format!("{ok}{lamp}")).unwrap();
        assert!(parse(&format!("{ok}{}", lamp.replace("appliance", "circuit"))).is_err());
        assert!(parse(&format!("{ok}{lamp}integrate = true\n")).is_err());
        let bill = "monthly_fee = 19.56\nbudget = \"maison:budget/value\"\n";
        parse(&format!("{bill}{ok}")).unwrap();
        assert!(parse(&format!("{}{ok}", bill.replace("19.56", "-1.0"))).is_err());
        assert!(parse(&format!("{}{ok}", bill.replace("maison:", ""))).is_err());
    }

    #[test]
    fn period_names_ignore_dots_and_case() {
        let c: Config = toml::from_str(
            "[tariff]\nperiod = \"a:b/c\"\nprices = { \"HP\" = 0.17, \"HC\" = 0.13 }",
        )
        .unwrap();
        assert_eq!(c.period_price("HP.."), Some(0.17));
        assert_eq!(c.period_price(" hc "), Some(0.13));
        assert_eq!(c.period_price("BASE"), None);
    }
}

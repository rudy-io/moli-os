//! The house's price of a kWh, chosen from the dashboard (no meter, or a
//! price that changes every month): every price with the moment it began,
//! kept in `energy.db`. A meter with its own `price`, or a tariff period's
//! price, comes first; this one prices the rest. An hour keeps the price of
//! its time: a new price only counts for the hours that follow.

use std::sync::{Arc, PoisonError, RwLock};

use serde::Serialize;

/// One price of the house, from `since` (ms, UTC) on.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HousePrice {
    pub since: u64,
    pub price: f64,
}

/// Every price of the house, oldest first; shared by the recorder and the
/// surfaces, so a new price counts at once, without a restart.
#[derive(Clone, Debug, Default)]
pub(crate) struct HousePrices(Arc<RwLock<Vec<HousePrice>>>);

impl HousePrices {
    pub(crate) fn new(mut prices: Vec<HousePrice>) -> Self {
        prices.sort_by_key(|p| p.since);
        Self(Arc::new(RwLock::new(prices)))
    }

    pub(crate) fn all(&self) -> Vec<HousePrice> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The price at `ts`: the last one begun by then.
    pub(crate) fn at(&self, ts: u64) -> Option<f64> {
        at(&self.0.read().unwrap_or_else(PoisonError::into_inner), ts)
    }

    pub(crate) fn push(&self, price: HousePrice) {
        let mut prices = self.0.write().unwrap_or_else(PoisonError::into_inner);
        prices.retain(|p| p.since < price.since);
        prices.push(price);
    }
}

/// The price at `ts` in `prices` (oldest first).
pub(crate) fn at(prices: &[HousePrice], ts: u64) -> Option<f64> {
    prices
        .iter()
        .take_while(|p| p.since <= ts)
        .last()
        .map(|p| p.price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hour_keeps_the_price_of_its_time() {
        let prices = HousePrices::new(vec![
            HousePrice {
                since: 2_000,
                price: 0.25,
            },
            HousePrice {
                since: 1_000,
                price: 0.20,
            },
        ]);
        assert_eq!(prices.at(999), None);
        assert_eq!(prices.at(1_000), Some(0.20));
        assert_eq!(prices.at(1_999), Some(0.20));
        assert_eq!(prices.at(5_000), Some(0.25));
        prices.push(HousePrice {
            since: 3_000,
            price: 0.30,
        });
        assert_eq!(prices.at(2_500), Some(0.25));
        assert_eq!(prices.at(3_000), Some(0.30));
    }
}

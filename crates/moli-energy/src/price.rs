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

    /// Every price at once (the edited list).
    pub(crate) fn replace(&self, mut prices: Vec<HousePrice>) {
        prices.sort_by_key(|p| p.since);
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = prices;
    }

    pub(crate) fn push(&self, price: HousePrice) {
        let mut prices = self.0.write().unwrap_or_else(PoisonError::into_inner);
        prices.retain(|p| p.since < price.since);
        prices.push(price);
    }
}

/// The first moment the price differs between two lists (oldest first):
/// the hours before it keep their cost. `None` when nothing changes.
pub(crate) fn first_change(old: &[HousePrice], new: &[HousePrice]) -> Option<u64> {
    // The price only changes at a moment one of the lists begins a price.
    let mut moments: Vec<u64> = old.iter().chain(new).map(|p| p.since).collect();
    moments.sort_unstable();
    moments.dedup();
    moments.into_iter().find(|&t| at(old, t) != at(new, t))
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

    fn p(since: u64, price: f64) -> HousePrice {
        HousePrice { since, price }
    }

    #[test]
    fn the_first_change_is_where_the_prices_part() {
        let old = [p(10, 0.20), p(30, 0.25)];
        assert_eq!(first_change(&old, &old), None);
        // A price corrected in the past.
        assert_eq!(first_change(&old, &[p(10, 0.20), p(30, 0.24)]), Some(30));
        // One added before everything.
        assert_eq!(
            first_change(&old, &[p(0, 0.18), p(10, 0.20), p(30, 0.25)]),
            Some(0)
        );
        // One removed: from its moment, the one before goes on.
        assert_eq!(first_change(&old, &[p(10, 0.20)]), Some(30));
        // The same price moved to another day.
        assert_eq!(first_change(&old, &[p(10, 0.20), p(20, 0.25)]), Some(20));
        assert_eq!(first_change(&[], &[p(5, 0.2)]), Some(5));
    }

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

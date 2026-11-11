//! Basket options: one store, another store, or a split between stores.
//!
//! Pure calculation, no I/O. The caller builds one [`Line`] per open shopping
//! list item (with the *fresh* price per store, `None` when unknown) and the
//! ordering [`Terms`] of each store, then calls [`compute`].

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One open shopping-list item, priced in every store that has it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// Id of the shopping-list item.
    pub item: i64,
    /// Product name (display only).
    pub product: String,
    /// Wanted quantity.
    pub qty: f64,
    /// Store this item can only be bought from (e.g. a capsule maker's own shop).
    pub fixed_provider: Option<String>,
    /// Unit price in cents per store; `None` (or absent) when the product is
    /// unavailable or has no fresh price there.
    pub prices: BTreeMap<String, Option<i64>>,
}

/// Ordering conditions of one store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terms {
    pub provider: String,
    /// Minimum order, fees excluded, in cents.
    pub min_order_cents: i64,
    pub fee_cents: i64,
}

/// One way to buy the list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// A store id for a single basket, `"split"` for the split basket.
    pub key: String,
    /// Store id -> item ids bought there.
    pub groups: BTreeMap<String, Vec<i64>>,
    pub total_cents: i64,
    /// Items this choice cannot buy.
    pub missing: Vec<i64>,
    /// Every group reaches its store's minimum order.
    pub valid: bool,
}

/// The options offered to the household.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BasketOptions {
    /// One basket per store, in store id order.
    pub singles: Vec<Choice>,
    pub split: Option<Choice>,
    /// `"split"` or the key of the best single basket (empty with no store).
    pub recommended: String,
    pub saving_cents: i64,
    pub saving_ratio: f64,
    /// Items priced in no store: set aside, they do not skew the comparison.
    pub nowhere: Vec<i64>,
    /// Items that can only be bought from a store outside the comparison.
    pub elsewhere: Vec<i64>,
}

/// Slack for comparing a total with `best × (1 − threshold)` in floating point,
/// so that "exactly 30 %" counts (cents stay far below 2^53).
const EPSILON: f64 = 1e-6;

/// Line cost in cents: quantity × unit price, half away from zero.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn cost(qty: f64, price: i64) -> i64 {
    (qty * price as f64).round() as i64
}

/// Unit price of a line in one store: `None` when unknown there, or when the
/// line is pinned to another store.
fn price_in(line: &Line, provider: &str) -> Option<i64> {
    if line
        .fixed_provider
        .as_deref()
        .is_some_and(|f| f != provider)
    {
        return None;
    }
    line.prices.get(provider).copied().flatten()
}

/// Compute the single baskets, the split basket and the recommendation.
///
/// `threshold` is the saving ratio (e.g. `0.30`) the split must reach over the
/// best single basket to be recommended.
#[must_use]
pub fn compute(lines: &[Line], terms: &[Terms], threshold: f64) -> BasketOptions {
    // Stores in id order, so every tie breaks the same way.
    let terms: BTreeMap<&str, &Terms> = terms.iter().map(|t| (t.provider.as_str(), t)).collect();

    let mut elsewhere = Vec::new();
    let mut nowhere = Vec::new();
    let mut kept: Vec<&Line> = Vec::new();
    for line in lines {
        if line
            .fixed_provider
            .as_deref()
            .is_some_and(|f| !terms.contains_key(f))
        {
            elsewhere.push(line.item);
        } else if terms.keys().all(|p| price_in(line, p).is_none()) {
            nowhere.push(line.item);
        } else {
            kept.push(line);
        }
    }

    let singles: Vec<Choice> = terms.values().map(|t| single_basket(&kept, t)).collect();
    let best = best_single(&singles);
    let split = split_basket(&kept, &terms);

    let mut recommended = best.map(|b| b.key.clone()).unwrap_or_default();
    let (mut saving_cents, mut saving_ratio) = (0, 0.0);
    if let (Some(best), Some(split)) = (best, &split) {
        saving_cents = best.total_cents - split.total_cents;
        #[allow(clippy::cast_precision_loss)]
        if best.total_cents > 0 {
            saving_ratio = saving_cents as f64 / best.total_cents as f64;
        }
        #[allow(clippy::cast_precision_loss)]
        let limit = best.total_cents as f64 * (1.0 - threshold) + EPSILON;
        #[allow(clippy::cast_precision_loss)]
        if split.total_cents as f64 <= limit && split.missing.len() <= best.missing.len() {
            "split".clone_into(&mut recommended);
        }
    }

    BasketOptions {
        singles,
        split,
        recommended,
        saving_cents,
        saving_ratio,
        nowhere,
        elsewhere,
    }
}

/// Everything bought from one store; what it lacks goes to `missing`.
fn single_basket(lines: &[&Line], terms: &Terms) -> Choice {
    let mut items = Vec::new();
    let mut missing = Vec::new();
    let mut goods = 0;
    for line in lines {
        match price_in(line, &terms.provider) {
            Some(price) => {
                items.push(line.item);
                goods += cost(line.qty, price);
            }
            None => missing.push(line.item),
        }
    }
    Choice {
        key: terms.provider.clone(),
        groups: BTreeMap::from([(terms.provider.clone(), items)]),
        total_cents: goods + terms.fee_cents,
        missing,
        valid: goods >= terms.min_order_cents,
    }
}

/// Fewest missing items, then lowest total, among the valid baskets; among all
/// of them when none is valid. The first store (id order) wins a tie.
fn best_single(singles: &[Choice]) -> Option<&Choice> {
    let rank = |c: &&Choice| (c.missing.len(), c.total_cents);
    singles
        .iter()
        .filter(|c| c.valid)
        .min_by_key(rank)
        .or_else(|| singles.iter().min_by_key(rank))
}

/// Every item at its cheapest store, dissolving the groups too small for their
/// store's minimum order. `None` when fewer than two stores remain.
fn split_basket(lines: &[&Line], terms: &BTreeMap<&str, &Terms>) -> Option<Choice> {
    if terms.len() < 2 {
        return None;
    }
    let mut dissolved: BTreeSet<&str> = BTreeSet::new();
    loop {
        let mut groups: BTreeMap<&str, Vec<&Line>> = BTreeMap::new();
        let mut missing = Vec::new();
        for line in lines {
            // Cheapest store not dissolved; the first (id order) wins a tie.
            let cheapest = terms
                .keys()
                .filter(|p| !dissolved.contains(**p))
                .filter_map(|p| price_in(line, p).map(|price| (price, *p)))
                .min();
            match cheapest {
                Some((_, provider)) => groups.entry(provider).or_default().push(line),
                None => missing.push(line.item),
            }
        }
        if groups.len() < 2 {
            return None;
        }

        let goods = |provider: &str, group: &[&Line]| -> i64 {
            group
                .iter()
                .filter_map(|l| price_in(l, provider).map(|price| cost(l.qty, price)))
                .sum()
        };
        let sizes: BTreeMap<&str, i64> = groups.iter().map(|(p, g)| (*p, goods(p, g))).collect();
        // Only a group under its own minimum can be dissolved: the smallest of
        // them (lowest id on a tie) goes, and its items move on.
        let smallest_short = sizes
            .iter()
            .filter(|(p, g)| **g < terms[**p].min_order_cents)
            .min_by_key(|(_, g)| **g)
            .map(|(p, _)| *p);
        if let Some(provider) = smallest_short {
            dissolved.insert(provider);
            continue;
        }

        let total_cents = sizes
            .iter()
            .map(|(p, g)| *g + terms[p].fee_cents)
            .sum::<i64>();
        return Some(Choice {
            key: "split".to_owned(),
            groups: groups
                .into_iter()
                .map(|(p, g)| (p.to_owned(), g.iter().map(|l| l.item).collect()))
                .collect(),
            total_cents,
            missing,
            valid: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(min_a: i64, fee_a: i64, min_b: i64, fee_b: i64) -> Vec<Terms> {
        vec![
            Terms {
                provider: "A".into(),
                min_order_cents: min_a,
                fee_cents: fee_a,
            },
            Terms {
                provider: "B".into(),
                min_order_cents: min_b,
                fee_cents: fee_b,
            },
        ]
    }

    fn free_terms() -> Vec<Terms> {
        terms(0, 0, 0, 0)
    }

    fn line(item: i64, a: Option<i64>, b: Option<i64>) -> Line {
        Line {
            item,
            product: format!("p{item}"),
            qty: 1.0,
            fixed_provider: None,
            prices: BTreeMap::from([("A".to_string(), a), ("B".to_string(), b)]),
        }
    }

    fn single<'a>(o: &'a BasketOptions, key: &str) -> &'a Choice {
        o.singles.iter().find(|c| c.key == key).expect("single")
    }

    /// Items `1` (x) and `2` (y) priced in A and B.
    fn xy(ax: i64, ay: i64, bx: i64, by: i64) -> Vec<Line> {
        vec![line(1, Some(ax), Some(bx)), line(2, Some(ay), Some(by))]
    }

    #[test]
    fn small_gap_keeps_one_basket() {
        let o = compute(&xy(1000, 500, 400, 600), &free_terms(), 0.30);
        assert_eq!(single(&o, "A").total_cents, 1500);
        assert_eq!(single(&o, "B").total_cents, 1000);
        assert_eq!(o.split.as_ref().expect("split").total_cents, 900);
        assert_eq!(o.recommended, "B");
        assert_eq!(o.saving_cents, 100);
        assert!((o.saving_ratio - 0.1).abs() < 1e-9);
    }

    #[test]
    fn big_gap_splits() {
        let o = compute(&xy(1000, 200, 200, 1000), &free_terms(), 0.30);
        assert_eq!(single(&o, "A").total_cents, 1200);
        assert_eq!(single(&o, "B").total_cents, 1200);
        let split = o.split.as_ref().expect("split");
        assert_eq!(split.total_cents, 400);
        assert_eq!(split.groups["A"], vec![2]);
        assert_eq!(split.groups["B"], vec![1]);
        assert!(split.valid);
        assert_eq!(o.recommended, "split");
        assert_eq!(o.saving_cents, 800);
    }

    #[test]
    fn exactly_thirty_percent_splits() {
        // Singles 1000 and 1000, split 200 + 200 + 300 = 700.
        let lines = vec![
            line(1, Some(200), Some(500)),
            line(2, Some(500), Some(200)),
            line(3, Some(300), Some(300)),
        ];
        let o = compute(&lines, &free_terms(), 0.30);
        assert_eq!(single(&o, "A").total_cents, 1000);
        assert_eq!(o.split.as_ref().expect("split").total_cents, 700);
        assert_eq!(o.recommended, "split");
        assert_eq!(o.saving_cents, 300);
    }

    #[test]
    fn split_under_minimum_is_dissolved() {
        let o = compute(&xy(1000, 200, 200, 1000), &terms(5000, 0, 0, 0), 0.30);
        assert!(o.split.is_none());
        assert_ne!(o.recommended, "split");
        assert!(o.singles.iter().any(|c| c.key == o.recommended));
        assert_eq!(o.saving_cents, 0);
        // A cannot reach its minimum, so B is the only valid single basket.
        assert!(!single(&o, "A").valid);
        assert!(single(&o, "B").valid);
        assert_eq!(o.recommended, "B");
    }

    #[test]
    fn dissolved_group_moves_to_next_cheapest_store() {
        // Three stores. C holds one cheap item but its group stays under the
        // minimum: it is dissolved and the item goes to its next best store.
        let mut lines = vec![
            line(1, Some(100), Some(900)),
            line(2, Some(900), Some(100)),
            line(3, Some(900), Some(100)),
        ];
        for l in &mut lines {
            l.prices
                .insert("C".into(), Some(if l.item == 1 { 50 } else { 950 }));
        }
        let mut t = terms(0, 0, 0, 0);
        t.push(Terms {
            provider: "C".into(),
            min_order_cents: 500,
            fee_cents: 0,
        });
        let o = compute(&lines, &t, 0.0);
        let split = o.split.as_ref().expect("split");
        assert!(!split.groups.contains_key("C"));
        assert_eq!(split.groups["A"], vec![1]);
        assert_eq!(split.groups["B"], vec![2, 3]);
        assert_eq!(split.total_cents, 300);
        assert!(split.missing.is_empty());
    }

    #[test]
    fn only_groups_under_their_minimum_are_dissolved() {
        // Cheapest stores: item 1 in A (100), items 2 and 3 in B (300 each),
        // item 4 in C (2000). A is the smallest group but has no minimum; B
        // (600) is under its 1000 minimum. B goes, not A: its items move to A.
        let prices = |a, b, c| {
            BTreeMap::from([
                ("A".to_string(), Some(a)),
                ("B".to_string(), Some(b)),
                ("C".to_string(), Some(c)),
            ])
        };
        let mk = |item, a, b, c| Line {
            item,
            product: format!("p{item}"),
            qty: 1.0,
            fixed_provider: None,
            prices: prices(a, b, c),
        };
        let lines = vec![
            mk(1, 100, 500, 800),
            mk(2, 400, 300, 900),
            mk(3, 400, 300, 900),
            mk(4, 2500, 2500, 2000),
        ];
        let mut t = terms(0, 0, 1000, 0);
        t.push(Terms {
            provider: "C".into(),
            min_order_cents: 0,
            fee_cents: 0,
        });
        let o = compute(&lines, &t, 0.0);
        let split = o.split.as_ref().expect("split");
        assert!(!split.groups.contains_key("B"));
        assert_eq!(split.groups["A"], vec![1, 2, 3]);
        assert_eq!(split.groups["C"], vec![4]);
        assert_eq!(split.total_cents, 100 + 400 + 400 + 2000);
        assert!(split.missing.is_empty());
        assert!(split.valid);
    }

    #[test]
    fn fees_count() {
        let o = compute(&xy(1000, 200, 200, 1000), &terms(0, 900, 0, 900), 0.30);
        assert_eq!(single(&o, "A").total_cents, 2100);
        assert_eq!(single(&o, "B").total_cents, 2100);
        assert_eq!(o.split.as_ref().expect("split").total_cents, 2200);
        assert_ne!(o.recommended, "split");
        assert_eq!(o.saving_cents, -100);
    }

    #[test]
    fn missing_everywhere_is_set_aside() {
        let mut lines = xy(1000, 200, 200, 1000);
        lines.push(line(9, None, None));
        let o = compute(&lines, &free_terms(), 0.30);
        assert_eq!(o.nowhere, vec![9]);
        assert!(o.elsewhere.is_empty());
        assert_eq!(single(&o, "A").total_cents, 1200);
        assert!(single(&o, "A").missing.is_empty());
        assert_eq!(o.split.as_ref().expect("split").total_cents, 400);
        assert_eq!(o.recommended, "split");
    }

    #[test]
    fn fixed_provider_is_set_aside() {
        let mut lines = xy(1000, 200, 200, 1000);
        let mut capsules = line(7, Some(50), Some(50));
        capsules.fixed_provider = Some("nespresso".into());
        lines.push(capsules);
        let o = compute(&lines, &free_terms(), 0.30);
        assert_eq!(o.elsewhere, vec![7]);
        assert!(o.nowhere.is_empty());
        assert_eq!(single(&o, "A").total_cents, 1200);
    }

    #[test]
    fn fixed_provider_among_terms_limits_the_item_to_it() {
        let mut only_b = line(1, Some(100), Some(300));
        only_b.fixed_provider = Some("B".into());
        let o = compute(&[only_b], &free_terms(), 0.30);
        assert!(o.elsewhere.is_empty());
        assert_eq!(single(&o, "A").missing, vec![1]);
        assert_eq!(single(&o, "B").total_cents, 300);
        assert_eq!(o.recommended, "B");
    }

    #[test]
    fn fewer_missing_beats_cheaper() {
        let lines = vec![line(1, Some(100), Some(150)), line(2, None, Some(150))];
        let o = compute(&lines, &free_terms(), 0.30);
        assert_eq!(single(&o, "A").total_cents, 100);
        assert_eq!(single(&o, "A").missing, vec![2]);
        assert_eq!(single(&o, "B").total_cents, 300);
        assert_eq!(o.recommended, "B");
    }

    #[test]
    fn quantity_is_rounded_per_line() {
        let mut l = line(1, Some(333), Some(999));
        l.qty = 1.5;
        let o = compute(&[l], &free_terms(), 0.30);
        // 1.5 * 333 = 499.5 -> 500 (half away from zero).
        assert_eq!(single(&o, "A").total_cents, 500);
        assert_eq!(single(&o, "B").total_cents, 1499);
    }

    #[test]
    fn absent_store_counts_as_unknown_price() {
        let mut l = line(1, Some(100), None);
        l.prices.remove("B");
        let o = compute(&[l], &free_terms(), 0.30);
        assert_eq!(single(&o, "B").missing, vec![1]);
        assert_eq!(o.recommended, "A");
    }

    #[test]
    fn one_store_has_no_split() {
        let t = vec![Terms {
            provider: "A".into(),
            min_order_cents: 0,
            fee_cents: 0,
        }];
        let o = compute(&xy(1000, 200, 200, 1000), &t, 0.30);
        assert_eq!(o.singles.len(), 1);
        assert!(o.split.is_none());
        assert_eq!(o.recommended, "A");
    }

    #[test]
    fn no_store_gives_empty_options() {
        let o = compute(&xy(1, 2, 3, 4), &[], 0.30);
        assert!(o.singles.is_empty());
        assert!(o.split.is_none());
        assert_eq!(o.recommended, "");
        assert!(o.elsewhere.is_empty());
        assert_eq!(o.nowhere, vec![1, 2]);
    }

    #[test]
    fn options_round_trip_through_json() {
        let o = compute(&xy(1000, 200, 200, 1000), &free_terms(), 0.30);
        let json = serde_json::to_string(&o).expect("json");
        let back: BasketOptions = serde_json::from_str(&json).expect("back");
        assert_eq!(o, back);
    }
}

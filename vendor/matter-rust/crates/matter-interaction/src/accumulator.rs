//! Reassembles chunked `ReportData` messages — the controller-side analogue
//! of chip's `ClusterStateCache`. Merges [`AttributeReportItem`](crate::AttributeReportItem)s
//! across one or more chunks keyed by `(endpoint, cluster, attribute)`.

#![forbid(unsafe_code)]

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use matter_codec::Value;

use crate::error::ImError;
use crate::path::AttributePath;
use crate::read::{ReportData, ReportOp};

/// Default ceiling on the number of distinct accumulated attribute elements.
///
/// Sized far above any realistic single-device read (project history records a
/// 170-attribute dump; a busy multi-endpoint device is still only thousands of
/// attributes), so legitimate large reads never trip it, while a peer cannot
/// stream an unbounded count of distinct paths.
pub const DEFAULT_MAX_ELEMENTS: usize = 100_000;

/// Default ceiling on the estimated total in-memory byte size of accumulated
/// values.
///
/// The controller's pre-parse chunk gate caps raw chunked-read input at
/// 256 KiB (`MAX_READ_BYTES`). The parsed-`Value` tree this accumulator holds
/// can be somewhat larger than its wire encoding (per-value enum/heap
/// overhead), so this in-crate ceiling is set to 4 MiB — a generous multiple
/// of the wire cap that still bounds memory as defense-in-depth when the
/// accumulator is driven directly (e.g. without the controller's gate).
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Rough estimate of a [`Value`]'s in-memory byte footprint, used only to
/// bound accumulator growth (not a precise allocation count). Heap-bearing
/// variants are walked recursively; scalars count as a small fixed size.
fn estimate_value_bytes(v: &Value) -> usize {
    const SCALAR: usize = 8;
    match v {
        Value::Utf8(s) => s.len(),
        Value::Bytes(b) => b.len(),
        Value::Array(items) => items.iter().map(estimate_value_bytes).sum::<usize>() + SCALAR,
        Value::Structure(members) | Value::List(members) => {
            members
                .iter()
                .map(|(_, mv)| estimate_value_bytes(mv))
                .sum::<usize>()
                + SCALAR
        }
        // Scalars (`Bool`/`Uint`/`Int`/`Float`/`Double`/`Null`) and — since
        // `Value` is `#[non_exhaustive]` — any future scalar-ish variant charge
        // a small fixed size so the ceiling still bounds them.
        _ => SCALAR,
    }
}

/// One accumulated attribute: its current value and the `DataVersion` it
/// was stored under (`None` when the report carried no version).
struct Slot {
    value: Value,
    version: Option<u32>,
}

/// Accumulates attribute reports across chunked `ReportData` messages and
/// produces the final concrete `(path, value)` set.
///
/// - `Replace` items set the attribute's value; the newest `DataVersion`
///   wins when the same attribute is replaced more than once.
/// - `Append` items (`ListIndex` = null) push one element onto the
///   attribute's list, starting from an empty list if none was seen.
///
/// First-seen attribute order is preserved by [`finish`](Self::finish).
///
/// This accumulator enforces an in-crate **total-size ceiling** as
/// defense-in-depth: [`push`](Self::push) returns
/// [`ImError::AccumulatorOverflow`] once the number of distinct accumulated
/// elements or the estimated total byte size would exceed the configured caps
/// ([`DEFAULT_MAX_ELEMENTS`] / [`DEFAULT_MAX_BYTES`], or the values given to
/// [`with_limits`](Self::with_limits)). This bounds memory even when the
/// accumulator is driven directly from an untrusted peer streaming an
/// unbounded chunked read/report set; a caller may still layer its own
/// chunk-count / wire-byte cap on top (the read-transaction layer does).
///
/// # Examples
///
/// ```
/// use matter_interaction::{parse_report_data, ReportAccumulator};
///
/// # fn demo(chunk_bytes: &[Vec<u8>]) -> Result<(), matter_interaction::ImError> {
/// let mut acc = ReportAccumulator::new();
/// for chunk in chunk_bytes {
///     acc.push(parse_report_data(chunk)?)?; // errors if the ceiling is exceeded
/// }
/// let attributes = acc.finish(); // every attribute across all chunks
/// # let _ = attributes;
/// # Ok(())
/// # }
/// ```
pub struct ReportAccumulator {
    order: Vec<AttributePath>,
    slots: HashMap<(u16, u32, u32), Slot>,
    /// Estimated total byte size of every currently-stored value.
    bytes: usize,
    max_elements: usize,
    max_bytes: usize,
}

impl Default for ReportAccumulator {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_ELEMENTS, DEFAULT_MAX_BYTES)
    }
}

impl ReportAccumulator {
    /// Create an empty accumulator with the default total-size ceiling
    /// ([`DEFAULT_MAX_ELEMENTS`] / [`DEFAULT_MAX_BYTES`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty accumulator with explicit caps on the number of
    /// distinct accumulated elements and the estimated total byte size.
    ///
    /// Use this to tighten the ceiling for a constrained transport, or to
    /// loosen it for an unusually large device. Prefer [`new`](Self::new)
    /// unless you have a concrete reason to override the defaults.
    #[must_use]
    pub fn with_limits(max_elements: usize, max_bytes: usize) -> Self {
        Self {
            order: Vec::new(),
            slots: HashMap::new(),
            bytes: 0,
            max_elements,
            max_bytes,
        }
    }

    /// Build the [`ImError::AccumulatorOverflow`] describing the current state
    /// against the configured caps.
    fn overflow(&self) -> ImError {
        ImError::AccumulatorOverflow {
            elements: self.order.len(),
            bytes: self.bytes,
            max_elements: self.max_elements,
            max_bytes: self.max_bytes,
        }
    }

    /// Merge one parsed `ReportData` chunk's items into the accumulated state.
    ///
    /// # Errors
    ///
    /// Returns [`ImError::AccumulatorOverflow`] if merging would push the
    /// number of distinct accumulated elements above the configured element
    /// cap, or the estimated total accumulated byte size above the configured
    /// byte cap. On overflow the offending item is not merged and the
    /// accumulator is left holding only the items accepted before the cap was
    /// reached; the caller should treat the report set as truncated and
    /// discard the transaction.
    pub fn push(&mut self, report: ReportData) -> Result<(), ImError> {
        for item in report.items {
            let key = (item.path.endpoint, item.path.cluster, item.path.attribute);
            let item_bytes = estimate_value_bytes(&item.value);
            // Adding this value's bytes must not exceed the byte cap. (The
            // element-cap check moved to each op arm below, single-lookup —
            // see the Vacant/else-branch comments.)
            if self.bytes.saturating_add(item_bytes) > self.max_bytes {
                return Err(self.overflow());
            }
            match item.op {
                ReportOp::Replace => match self.slots.entry(key) {
                    Entry::Occupied(mut e) => {
                        let slot = e.get_mut();
                        let newer = match (slot.version, item.data_version) {
                            (Some(old), Some(new)) => new >= old,
                            _ => true, // unknown versions ⇒ last write wins
                        };
                        if newer {
                            // Replacing an existing value: drop its byte charge
                            // before adding the new one so the running total
                            // tracks what is actually held.
                            self.bytes = self
                                .bytes
                                .saturating_sub(estimate_value_bytes(&slot.value))
                                .saturating_add(item_bytes);
                            slot.value = item.value;
                            slot.version = item.data_version;
                        }
                    }
                    Entry::Vacant(e) => {
                        // A genuinely new key adds an element; reject before
                        // inserting so the count never exceeds the cap.
                        // (`overflow()` needs `&self`, which the Entry's
                        // `&mut self.slots` borrow forbids — build inline
                        // from the disjoint fields.)
                        if self.order.len() >= self.max_elements {
                            return Err(ImError::AccumulatorOverflow {
                                elements: self.order.len(),
                                bytes: self.bytes,
                                max_elements: self.max_elements,
                                max_bytes: self.max_bytes,
                            });
                        }
                        self.order.push(item.path);
                        self.bytes = self.bytes.saturating_add(item_bytes);
                        e.insert(Slot {
                            value: item.value,
                            version: item.data_version,
                        });
                    }
                },
                ReportOp::Append => {
                    if let Some(slot) = self.slots.get_mut(&key) {
                        // A strictly-older DataVersion append must not
                        // land on a newer list; same/unknown versions proceed.
                        if let (Some(old), Some(new)) = (slot.version, item.data_version) {
                            if new < old {
                                continue;
                            }
                        }
                        slot.version = item.data_version;
                        self.bytes = self.bytes.saturating_add(item_bytes);
                        match &mut slot.value {
                            Value::Array(list) => list.push(item.value),
                            // Malformed: an append targeting a non-list value.
                            // Coerce to a fresh single-element list rather than
                            // silently dropping the element.
                            other => *other = Value::Array(vec![item.value]),
                        }
                    } else {
                        // A genuinely new key adds an element; reject before
                        // inserting so the count never exceeds the cap. No
                        // entry borrow is held here, so `overflow()` is usable
                        // directly.
                        if self.order.len() >= self.max_elements {
                            return Err(self.overflow());
                        }
                        self.order.push(item.path);
                        self.bytes = self.bytes.saturating_add(item_bytes);
                        self.slots.insert(
                            key,
                            Slot {
                                value: Value::Array(vec![item.value]),
                                version: item.data_version,
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Consume the accumulator, yielding `(path, value)` in first-seen order.
    ///
    /// Each [`Value`] is **moved** out of the consumed accumulator rather than
    /// cloned: `self.order` records every accumulated path exactly once (a path
    /// is pushed only on the first insert for its key — see [`push`](Self::push)),
    /// so a single [`HashMap::remove`] per path drains the map without aliasing.
    /// This avoids a full deep copy of every attribute subtree on the
    /// chunked-read / subscription completion path.
    #[must_use]
    pub fn finish(mut self) -> Vec<(AttributePath, Value)> {
        let mut out = Vec::with_capacity(self.order.len());
        for path in std::mem::take(&mut self.order) {
            let key = (path.endpoint, path.cluster, path.attribute);
            if let Some(slot) = self.slots.remove(&key) {
                out.push((path, slot.value));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::read::AttributeReportItem;

    fn report(items: Vec<AttributeReportItem>) -> ReportData {
        ReportData {
            items,
            subscription_id: None,
            more_chunked_messages: false,
            suppress_response: false,
            events: Vec::new(),
            statuses: Vec::new(),
        }
    }

    fn ap(endpoint: u16, cluster: u32, attribute: u32) -> AttributePath {
        AttributePath {
            endpoint,
            cluster,
            attribute,
        }
    }

    fn replace(p: AttributePath, v: Value) -> AttributeReportItem {
        AttributeReportItem {
            path: p,
            op: ReportOp::Replace,
            value: v,
            data_version: None,
        }
    }

    fn append(p: AttributePath, v: Value) -> AttributeReportItem {
        AttributeReportItem {
            path: p,
            op: ReportOp::Append,
            value: v,
            data_version: None,
        }
    }

    fn append_v(p: AttributePath, v: Value, version: u32) -> AttributeReportItem {
        AttributeReportItem {
            path: p,
            op: ReportOp::Append,
            value: v,
            data_version: Some(version),
        }
    }

    #[test]
    fn stale_version_append_is_rejected() {
        // A strictly-older DataVersion append must not land on a newer
        // list. Two appends at version 5 build the list; a version-3 append is
        // stale and must be dropped (not appended).
        let mut acc = ReportAccumulator::new();
        let p = ap(0, 0x1d, 0x0003);
        acc.push(report(vec![append_v(p, Value::Uint(1), 5)]))
            .unwrap();
        acc.push(report(vec![append_v(p, Value::Uint(2), 5)]))
            .unwrap();
        acc.push(report(vec![append_v(p, Value::Uint(99), 3)]))
            .unwrap();
        let out = acc.finish();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].1,
            Value::Array(vec![Value::Uint(1), Value::Uint(2)]),
            "the stale-version append must not land on the newer list"
        );
    }

    #[test]
    fn message_level_merge_preserves_order() {
        let mut acc = ReportAccumulator::new();
        acc.push(report(vec![replace(
            ap(0, 0x28, 0x0002),
            Value::Uint(5010),
        )]))
        .unwrap();
        acc.push(report(vec![replace(
            ap(1, 0x06, 0x0000),
            Value::Bool(true),
        )]))
        .unwrap();
        let out = acc.finish();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, ap(0, 0x28, 0x0002));
        assert_eq!(out[0].1, Value::Uint(5010));
        assert_eq!(out[1].0, ap(1, 0x06, 0x0000));
        assert_eq!(out[1].1, Value::Bool(true));
    }

    #[test]
    fn list_append_after_empty_replace() {
        let mut acc = ReportAccumulator::new();
        let p = ap(0, 0x1d, 0x0003);
        acc.push(report(vec![replace(p, Value::Array(Vec::new()))]))
            .unwrap();
        acc.push(report(vec![append(p, Value::Uint(1))])).unwrap();
        acc.push(report(vec![append(p, Value::Uint(2))])).unwrap();
        let out = acc.finish();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Value::Array(vec![Value::Uint(1), Value::Uint(2)]));
    }

    #[test]
    fn append_without_base_starts_empty() {
        let mut acc = ReportAccumulator::new();
        let p = ap(0, 0x1d, 0x0003);
        acc.push(report(vec![append(p, Value::Uint(9))])).unwrap();
        assert_eq!(acc.finish()[0].1, Value::Array(vec![Value::Uint(9)]));
    }

    #[test]
    fn append_onto_non_array_coerces_instead_of_dropping() {
        // Malformed input: a scalar Replace then an Append on the same path.
        // The element must not vanish — the slot coerces to a fresh list.
        let mut acc = ReportAccumulator::new();
        let p = ap(0, 0x1d, 0x0003);
        acc.push(report(vec![replace(p, Value::Uint(1))])).unwrap();
        acc.push(report(vec![append(p, Value::Uint(2))])).unwrap();
        assert_eq!(acc.finish()[0].1, Value::Array(vec![Value::Uint(2)]));
    }

    #[test]
    fn newest_data_version_wins() {
        let mut acc = ReportAccumulator::new();
        let p = ap(0, 0x28, 0x0000);
        acc.push(report(vec![AttributeReportItem {
            path: p,
            op: ReportOp::Replace,
            value: Value::Uint(1),
            data_version: Some(5),
        }]))
        .unwrap();
        acc.push(report(vec![AttributeReportItem {
            path: p,
            op: ReportOp::Replace,
            value: Value::Uint(2),
            data_version: Some(3),
        }]))
        .unwrap();
        assert_eq!(
            acc.finish()[0].1,
            Value::Uint(1),
            "older DataVersion must not overwrite"
        );
    }

    /// `finish()` now MOVES values out of the consumed accumulator rather than
    /// cloning them. Drive it with heap-bearing values (strings, byte strings,
    /// nested lists) and assert the resulting `(path, value)` set is exactly
    /// what was inserted — proving the move preserves content and order and
    /// drops nothing.
    #[test]
    fn finish_moves_values_preserving_content_and_order() {
        let mut acc = ReportAccumulator::new();
        let p0 = ap(0, 0x28, 0x0001);
        let p1 = ap(1, 0x06, 0x0000);
        let p2 = ap(2, 0x1d, 0x0003);
        let v0 = Value::Utf8(String::from("VendorName"));
        let v1 = Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]);
        let v2 = Value::Array(vec![Value::Uint(1), Value::Utf8(String::from("x"))]);
        acc.push(report(vec![
            replace(p0, v0.clone()),
            replace(p1, v1.clone()),
            replace(p2, v2.clone()),
        ]))
        .unwrap();

        let out = acc.finish();
        assert_eq!(
            out,
            vec![(p0, v0), (p1, v1), (p2, v2)],
            "moved-out set must match inserted (path, value) pairs in first-seen order"
        );
    }

    use proptest::prelude::*;

    #[test]
    fn element_ceiling_is_enforced() {
        // A tiny element cap; feeding past it must error rather than grow.
        let mut acc = ReportAccumulator::with_limits(3, usize::MAX);
        // 3 distinct attributes fit.
        for i in 0..3u32 {
            acc.push(report(vec![replace(
                ap(0, 0x06, i),
                Value::Uint(u64::from(i)),
            )]))
            .expect("within element cap");
        }
        // The 4th distinct attribute crosses the ceiling.
        let err = acc
            .push(report(vec![replace(ap(0, 0x06, 99), Value::Uint(1))]))
            .expect_err("4th distinct element must exceed the cap");
        assert!(
            matches!(
                err,
                ImError::AccumulatorOverflow {
                    max_elements: 3,
                    ..
                }
            ),
            "expected AccumulatorOverflow, got {err:?}"
        );
    }

    #[test]
    fn element_ceiling_is_enforced_for_append_new_key() {
        // The Append "new key via insert" path had its own cap check inline
        // with the deleted top-of-loop `contains_key` check; this pins that
        // it still enforces the ceiling after the single-lookup refactor.
        let mut acc = ReportAccumulator::with_limits(1, usize::MAX);
        acc.push(report(vec![replace(ap(0, 0x06, 0), Value::Uint(1))]))
            .unwrap();
        let err = acc
            .push(report(vec![append(ap(0, 0x06, 1), Value::Uint(2))]))
            .unwrap_err();
        assert!(matches!(
            err,
            ImError::AccumulatorOverflow {
                max_elements: 1,
                ..
            }
        ));
    }

    #[test]
    fn byte_ceiling_is_enforced() {
        // Generous element cap, tiny byte cap. A large byte string trips it.
        let mut acc = ReportAccumulator::with_limits(usize::MAX, 16);
        let err = acc
            .push(report(vec![replace(
                ap(0, 0x28, 0x0001),
                Value::Bytes(vec![0u8; 1024]),
            )]))
            .expect_err("1 KiB value must exceed a 16-byte cap");
        assert!(
            matches!(err, ImError::AccumulatorOverflow { max_bytes: 16, .. }),
            "expected AccumulatorOverflow, got {err:?}"
        );
    }

    #[test]
    fn normal_sized_report_set_is_ok() {
        // The default cap must comfortably admit a realistic large dump
        // (project history: a 170-attribute device read). Simulate 200
        // attributes carrying small values; all must accumulate without error.
        let mut acc = ReportAccumulator::new();
        for i in 0..200u32 {
            acc.push(report(vec![replace(
                ap(0, 0x28, i),
                Value::Utf8(String::from("a-realistic-attribute-value")),
            )]))
            .expect("200 small attributes are well within the default ceiling");
        }
        assert_eq!(acc.finish().len(), 200);
    }

    proptest! {
        // Splitting a set of whole-attribute Replace items across N chunks
        // yields the same final set as one chunk (message-level chunking is
        // transparent to reassembly), with first-seen order preserved.
        #[test]
        fn message_chunking_is_order_preserving(
            attrs in proptest::collection::vec((0u16..4, 0u32..8, 0u32..8, 0u64..1000), 1..20),
        ) {
            // Dedup by key keeping first occurrence (matches accumulator semantics).
            let mut seen = std::collections::HashSet::new();
            let unique: Vec<_> = attrs.into_iter()
                .filter(|(e, c, a, _)| seen.insert((*e, *c, *a)))
                .collect();

            // All items in one chunk.
            let mut whole = ReportAccumulator::new();
            whole.push(report(
                unique.iter().map(|&(e, c, a, v)| replace(ap(e, c, a), Value::Uint(v))).collect(),
            )).unwrap();
            let whole_out = whole.finish();

            // Same items, one per chunk.
            let mut split = ReportAccumulator::new();
            for &(e, c, a, v) in &unique {
                split.push(report(vec![replace(ap(e, c, a), Value::Uint(v))])).unwrap();
            }
            let split_out = split.finish();

            prop_assert_eq!(&whole_out, &split_out);
            // Order matches first-seen.
            for (i, &(e, c, a, _)) in unique.iter().enumerate() {
                prop_assert_eq!(split_out[i].0, ap(e, c, a));
            }
        }

        // Appends accumulate into a list of exactly the pushed elements, in order.
        #[test]
        fn appends_build_list_in_order(elems in proptest::collection::vec(0u64..1000, 0..30)) {
            let p = ap(0, 0x1d, 0x0003);
            let mut acc = ReportAccumulator::new();
            acc.push(report(vec![replace(p, Value::Array(Vec::new()))])).unwrap();
            for &v in &elems {
                acc.push(report(vec![append(p, Value::Uint(v))])).unwrap();
            }
            let out = acc.finish();
            let want: Vec<Value> = elems.iter().map(|&v| Value::Uint(v)).collect();
            prop_assert_eq!(&out[0].1, &Value::Array(want));
        }
    }
}

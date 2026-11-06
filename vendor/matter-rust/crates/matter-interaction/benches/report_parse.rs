//! `parse_report_data` benchmark. Targets the per-report parse path where the
//! 2026-06-12 audit removed the eager deep-clone of every Replace
//! value into a redundant `attributes` Vec.
//!
//! The signal scales with attribute count × value size: pre-audit each Replace
//! `Value` (here a 64-byte octet string) was cloned once into `attributes`;
//! post-audit it is not. A 170-attribute report mirrors the project's observed
//! initial wildcard dump.
//!
//! Run: `cargo bench --bench report_parse`

// Bench code, not library code: the criterion macros emit undocumented items,
// and setup uses expect()/casts. Mirrors the repo's test-code lint carve-outs.
#![allow(
    missing_docs,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use criterion::{criterion_group, criterion_main, Criterion};
use matter_codec::{Tag, TlvWriter};
use matter_interaction::parse_report_data;
use std::hint::black_box;

/// Build a `ReportData` wire blob with `n_attrs` AttributeReportIBs, each
/// carrying a `val_len`-byte octet-string Data value. Layout mirrors the
/// matter-interaction read.rs tests (stable across both compared commits).
fn build_report(n_attrs: usize, val_len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).expect("msg");
    w.start_array(Tag::Context(1)).expect("AttributeReports"); // [1]
    let data = vec![0xCDu8; val_len];
    for i in 0..n_attrs {
        w.start_structure(Tag::Anonymous).expect("IB"); // AttributeReportIB
        w.start_structure(Tag::Context(1)).expect("AttributeData"); // [1]
        w.start_list(Tag::Context(1)).expect("Path"); // [1] AttributePathIB
        w.put_uint(Tag::Context(2), 1).expect("endpoint"); // [2]
        w.put_uint(Tag::Context(3), 0x0006).expect("cluster"); // [3]
        w.put_uint(Tag::Context(4), i as u64).expect("attribute"); // [4]
        w.end_container().expect("end path");
        w.put_bytes(Tag::Context(2), &data).expect("Data"); // [2] = octet string
        w.end_container().expect("end AttributeData");
        w.end_container().expect("end IB");
    }
    w.end_container().expect("end array");
    w.end_container().expect("end msg");
    buf
}

fn bench_parse(c: &mut Criterion) {
    let report = build_report(170, 64);
    c.bench_function("parse_report_data/170attr_64B", |b| {
        b.iter(|| black_box(parse_report_data(black_box(&report)).expect("parse")));
    });

    // Smaller, scalar-only report — control where the deep-clone savings are
    // minimal (small values), isolating the structural parse cost.
    let small = build_report(20, 4);
    c.bench_function("parse_report_data/20attr_4B", |b| {
        b.iter(|| black_box(parse_report_data(black_box(&small)).expect("parse")));
    });
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);

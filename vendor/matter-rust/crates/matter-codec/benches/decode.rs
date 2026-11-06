//! Decode benchmarks. Targets the TLV `read_value` tree-materialisation path
//! (`read_container_body`), where the 2026-06-12 audit changed array decoding
//! to a single allocation and added a per-element budget check.
//!
//! Run: `cargo bench --bench decode`
//! Cross-commit compare: see the perf-baseline harness.

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
use matter_codec::{Tag, TlvReader, TlvWriter};
use std::hint::black_box;

/// A TLV array of `n` byte-string elements, each `elem` bytes. Array children
/// carry anonymous tags (required by the spec; enforced post-audit).
fn build_byte_array(n: usize, elem: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_array(Tag::Anonymous).expect("start array");
    let data = vec![0xABu8; elem];
    for _ in 0..n {
        w.put_bytes(Tag::Anonymous, &data).expect("put bytes");
    }
    w.end_container().expect("end array");
    buf
}

/// A TLV array of `n` small uints (cheap elements — isolates the array
/// container handling itself from per-element payload cost).
fn build_uint_array(n: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_array(Tag::Anonymous).expect("start array");
    for i in 0..n {
        w.put_uint(Tag::Anonymous, i as u64).expect("put uint");
    }
    w.end_container().expect("end array");
    buf
}

/// A wide structure of `n` context-tagged uint fields (control: the struct
/// decode path was NOT changed by the audit, so this isolates the array win).
fn build_wide_struct(n: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).expect("start struct");
    for i in 0..n {
        w.put_uint(Tag::Context((i % 255) as u8), i as u64)
            .expect("put uint");
    }
    w.end_container().expect("end struct");
    buf
}

/// `depth` nested structures, each level carrying one uint field beside the
/// nested container. Exercises the recursive tree-builder path (bounded by
/// the codec's `MAX_DEPTH` of 32).
fn build_nested(depth: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    for i in 0..depth {
        w.start_structure(if i == 0 {
            Tag::Anonymous
        } else {
            Tag::Context(0)
        })
        .expect("start struct");
        w.put_uint(Tag::Context(1), i as u64).expect("put uint");
    }
    for _ in 0..depth {
        w.end_container().expect("end struct");
    }
    buf
}

/// The 170-attribute wildcard-read `ReportData` shape at the raw TLV layer
/// (the matter-interaction `report_parse` bench covers the IM layer on top).
/// Layout mirrors that bench's builder: per attribute an AttributeReportIB
/// structure holding an AttributeData structure with a path list and a
/// `val_len`-byte octet-string value.
fn build_report_shape(n_attrs: usize, val_len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).expect("msg");
    w.start_array(Tag::Context(1)).expect("AttributeReports");
    let data = vec![0xCDu8; val_len];
    for i in 0..n_attrs {
        w.start_structure(Tag::Anonymous).expect("IB");
        w.start_structure(Tag::Context(1)).expect("AttributeData");
        w.start_list(Tag::Context(1)).expect("Path");
        w.put_uint(Tag::Context(2), 1).expect("endpoint");
        w.put_uint(Tag::Context(3), 0x0006).expect("cluster");
        w.put_uint(Tag::Context(4), i as u64).expect("attribute");
        w.end_container().expect("end path");
        w.put_bytes(Tag::Context(2), &data).expect("Data");
        w.end_container().expect("end AttributeData");
        w.end_container().expect("end IB");
    }
    w.end_container().expect("end array");
    w.end_container().expect("end msg");
    buf
}

fn bench_decode(c: &mut Criterion) {
    let arr_bytes = build_byte_array(1000, 32);
    c.bench_function("decode/array_1000x32B", |b| {
        b.iter(|| {
            let mut r = TlvReader::new(black_box(&arr_bytes));
            black_box(r.read_value().expect("decode"))
        });
    });

    let arr_uints = build_uint_array(2000);
    c.bench_function("decode/array_2000_uint", |b| {
        b.iter(|| {
            let mut r = TlvReader::new(black_box(&arr_uints));
            black_box(r.read_value().expect("decode"))
        });
    });

    let wide = build_wide_struct(500);
    c.bench_function("decode/struct_500_uint", |b| {
        b.iter(|| {
            let mut r = TlvReader::new(black_box(&wide));
            black_box(r.read_value().expect("decode"))
        });
    });

    let small = build_small_struct();
    c.bench_function("decode/struct_5_uint", |b| {
        b.iter(|| {
            let mut r = TlvReader::new(black_box(&small));
            black_box(r.read_value().expect("decode"))
        });
    });

    let nested = build_nested(30);
    c.bench_function("decode/nested_30deep", |b| {
        b.iter(|| {
            let mut r = TlvReader::new(black_box(&nested));
            black_box(r.read_value().expect("decode"))
        });
    });

    let report = build_report_shape(170, 64);
    c.bench_function("decode/report_170attr_64B", |b| {
        b.iter(|| {
            let mut r = TlvReader::new(black_box(&report));
            black_box(r.read_value().expect("decode"))
        });
    });
}

fn build_small_struct() -> Vec<u8> {
    build_wide_struct(5)
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);

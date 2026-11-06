//! Encode benchmarks — the `TlvWriter` counterparts of the decode suite.
//! Each bench builds a full TLV blob into a fresh `Vec` per iteration, so the
//! numbers include the allocation cost a real encode pays.
//!
//! Run: `cargo bench --bench encode` (or `just bench`).

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
use std::hint::black_box;

fn encode_byte_array(n: usize, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_array(Tag::Anonymous).expect("start array");
    for _ in 0..n {
        w.put_bytes(Tag::Anonymous, data).expect("put bytes");
    }
    w.end_container().expect("end array");
    buf
}

fn encode_uint_array(n: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_array(Tag::Anonymous).expect("start array");
    for i in 0..n {
        w.put_uint(Tag::Anonymous, i as u64).expect("put uint");
    }
    w.end_container().expect("end array");
    buf
}

fn encode_wide_struct(n: usize) -> Vec<u8> {
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

fn encode_nested(depth: usize) -> Vec<u8> {
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

/// The 170-attribute wildcard-read `ReportData` shape (see the decode bench's
/// builder for the layout provenance).
fn encode_report_shape(n_attrs: usize, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).expect("msg");
    w.start_array(Tag::Context(1)).expect("AttributeReports");
    for i in 0..n_attrs {
        w.start_structure(Tag::Anonymous).expect("IB");
        w.start_structure(Tag::Context(1)).expect("AttributeData");
        w.start_list(Tag::Context(1)).expect("Path");
        w.put_uint(Tag::Context(2), 1).expect("endpoint");
        w.put_uint(Tag::Context(3), 0x0006).expect("cluster");
        w.put_uint(Tag::Context(4), i as u64).expect("attribute");
        w.end_container().expect("end path");
        w.put_bytes(Tag::Context(2), data).expect("Data");
        w.end_container().expect("end AttributeData");
        w.end_container().expect("end IB");
    }
    w.end_container().expect("end array");
    w.end_container().expect("end msg");
    buf
}

fn bench_encode(c: &mut Criterion) {
    let elem = vec![0xABu8; 32];
    c.bench_function("encode/array_1000x32B", |b| {
        b.iter(|| black_box(encode_byte_array(black_box(1000), &elem)));
    });

    c.bench_function("encode/array_2000_uint", |b| {
        b.iter(|| black_box(encode_uint_array(black_box(2000))));
    });

    c.bench_function("encode/struct_500_uint", |b| {
        b.iter(|| black_box(encode_wide_struct(black_box(500))));
    });

    c.bench_function("encode/struct_5_uint", |b| {
        b.iter(|| black_box(encode_wide_struct(black_box(5))));
    });

    c.bench_function("encode/nested_30deep", |b| {
        b.iter(|| black_box(encode_nested(black_box(30))));
    });

    let val = vec![0xCDu8; 64];
    c.bench_function("encode/report_170attr_64B", |b| {
        b.iter(|| black_box(encode_report_shape(black_box(170), &val)));
    });
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);

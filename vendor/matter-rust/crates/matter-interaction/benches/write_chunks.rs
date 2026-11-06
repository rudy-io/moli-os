//! `build_list_write_chunks` benchmark — the chunked list-write packer.
//! Pre-phase-3 the packer re-encoded the whole candidate chunk per element
//! (O(n²) in elements per chunk); phase 3 replaces that with incremental
//! size accounting. 100 elements mirrors a large ACL/binding list write.
//!
//! Run: `cargo bench --bench write_chunks`

// Bench code, not library code: mirrors the repo's test-code lint carve-outs.
#![allow(missing_docs, clippy::doc_markdown, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use matter_codec::{Tag, TlvWriter};
use matter_interaction::{build_list_write_chunks, AttributePath};
use std::hint::black_box;

/// One pre-encoded anonymous-tagged list element carrying a `len`-byte
/// octet string (the shape `write_list_chunked` callers pass).
fn element(len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.put_bytes(Tag::Anonymous, &vec![0xAB; len])
        .expect("vec writer");
    buf
}

fn bench_write_chunks(c: &mut Criterion) {
    let path = AttributePath {
        endpoint: 0,
        cluster: 0x001F,
        attribute: 0,
    };
    let elems: Vec<Vec<u8>> = (0..100).map(|_| element(64)).collect();
    // Budget forces ~10 elements per chunk → exercises the append path hard.
    c.bench_function("write_chunks/100x64B/budget900", |b| {
        b.iter(|| black_box(build_list_write_chunks(path, black_box(&elems), 900, false)));
    });
    // Single-chunk fast path: everything fits.
    c.bench_function("write_chunks/100x64B/single_chunk", |b| {
        b.iter(|| {
            black_box(build_list_write_chunks(
                path,
                black_box(&elems),
                1 << 16,
                false,
            ));
        });
    });
}

criterion_group!(benches, bench_write_chunks);
criterion_main!(benches);

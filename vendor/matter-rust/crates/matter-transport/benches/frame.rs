//! Secured-message framing benchmarks: `SessionManager::encode_outbound` and
//! `decode_inbound` over an established (PASE-keyed) session pair. This is
//! the per-message cost every operational exchange pays — AES-CCM seal/open
//! plus header/protocol-header codec and replay-window bookkeeping.
//!
//! Payload sizes: 32 B (a small IM command) and 960 B (the OTA/BDX block
//! size, the largest payload the project ships routinely).
//!
//! Decode uses `iter_batched`: each iteration decodes a FRESH packet (new
//! message counter), because the replay window rejects a re-decoded one.
//!
//! Run: `cargo bench --bench frame` (or `just bench`).

// Bench code, not library code: the criterion macros emit undocumented items,
// and setup uses expect(). Mirrors the repo's test-code lint carve-outs.
#![allow(missing_docs, clippy::doc_markdown, clippy::expect_used)]

use std::time::Instant;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use matter_crypto::pase::PaseSessionKeys;
use matter_transport::{MrpFlags, PeerHint, ProtocolId, SessionId, SessionManager, SessionRole};
use std::hint::black_box;

/// Two `SessionManager`s sharing one symmetric key set, cross-registered as
/// Initiator/Responder (mirrors the session.rs unit-test pairing).
fn paired_sessions() -> (SessionManager, SessionManager) {
    let keys = PaseSessionKeys {
        ke: [0u8; 16],
        i2r_key: [1u8; 16],
        r2i_key: [2u8; 16],
        attestation_key: [3u8; 16],
    };
    let mut a = SessionManager::new();
    let mut b = SessionManager::new();
    a.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
    b.register_pase(keys, SessionRole::Responder, 1, PeerHint::default());
    (a, b)
}

fn bench_frame(c: &mut Criterion) {
    // Both managers allocate local id 1 (the allocator starts at 1), so the
    // session handle on either side is SessionId(1) — same as the session.rs
    // unit tests.
    let session = SessionId(1);

    for (label, size) in [("32B", 32usize), ("960B", 960usize)] {
        let payload = vec![0x5Au8; size];

        // Encode: the sender's per-message cost. Unreliable flags so the MRP
        // retransmit registry stays empty across iterations.
        c.bench_function(&format!("frame/encode_{label}"), |b| {
            let (mut a, _b) = paired_sessions();
            b.iter(|| {
                let out = a
                    .encode_outbound(
                        session,
                        None,
                        0x08,
                        ProtocolId::INTERACTION_MODEL,
                        black_box(&payload),
                        MrpFlags { reliable: false },
                        Instant::now(),
                    )
                    .expect("encode");
                black_box(out.wire_bytes)
            });
        });

        // Decode: each iteration gets a fresh wire packet (unique counter) so
        // the receiver's replay window accepts it.
        c.bench_function(&format!("frame/decode_{label}"), |b| {
            let (mut a, mut r) = paired_sessions();
            b.iter_batched(
                || {
                    a.encode_outbound(
                        session,
                        None,
                        0x08,
                        ProtocolId::INTERACTION_MODEL,
                        &payload,
                        MrpFlags { reliable: false },
                        Instant::now(),
                    )
                    .expect("encode")
                    .wire_bytes
                },
                |wire| black_box(r.decode_inbound(&wire, Instant::now()).expect("decode")),
                BatchSize::SmallInput,
            );
        });
    }
}

criterion_group!(benches, bench_frame);
criterion_main!(benches);

//! CASE (SIGMA-I) handshake benchmarks — per-step costs plus the full
//! three-message exchange. This is the load-bearing perf comparison for the
//! project's "embedded-grade performance" positioning (see TODO-1.0
//! "Benchmark suite"): matter.js pays JS crypto + TLV costs on the same steps.
//!
//! Certificate/credential construction happens in the fixture (once) and in
//! `iter_batched` setup closures (excluded from measurement); the measured
//! routines are exactly the state-machine steps a controller pays per
//! handshake.
//!
//! Run: `cargo bench --bench case` (or `just bench`).

// Bench code, not library code: the criterion macros emit undocumented items,
// and setup uses expect(). Mirrors the repo's test-code lint carve-outs.
#![allow(missing_docs, clippy::doc_markdown, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use matter_cert::test_support::{build_unsigned, with_signature, TestCertFields};
use matter_cert::{
    BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyIdentifier, KeyUsage,
    MatterCertificate, MatterTime, PublicKey, Signature, TrustAnchor, TrustedRoots,
};
use matter_crypto::{CaseCredentials, CaseInitiator, CaseResponder, CaseSigner, RingSigner};
use std::hint::black_box;

const TEST_FABRIC_ID: u64 = 0x4242_4242_4242_4242;
const INITIATOR_NODE_ID: u64 = 0xDEAD_BEEF_CAFE_F00D;
const RESPONDER_NODE_ID: u64 = 0xBABE_FEED_1234_5678;
const IPK: [u8; 16] = [0x77; 16];
const TEST_SKI: [u8; 20] = [0x01; 20];
const NOC_SKI: [u8; 20] = [0x02; 20];
fn now() -> MatterTime {
    MatterTime::from_unix_secs(2_000_000_000)
}

/// One-time fixture: a self-signed RCAC plus one NOC per side. Per-iteration
/// credentials are rebuilt from the stored PKCS#8 blobs (signers are not
/// clonable) — that rebuild happens in setup closures, never in the timed
/// routine.
struct Fixture {
    roots: TrustedRoots,
    rcac_pub: [u8; 65],
    initiator_noc: MatterCertificate,
    initiator_pkcs8: Vec<u8>,
    responder_noc: MatterCertificate,
    responder_pkcs8: Vec<u8>,
}

/// Build a test certificate (RCAC when `ca` / self-signed, NOC otherwise),
/// mirroring the `case_roundtrip.rs` integration-test fixtures.
fn build_cert(
    subject: DistinguishedName,
    issuer: DistinguishedName,
    public_key: [u8; 65],
    ca: bool,
    signer: &RingSigner,
) -> MatterCertificate {
    let extensions = if ca {
        Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(true, Some(1))))
            .key_usage(Some(KeyUsage::KEY_CERT_SIGN))
            .subject_key_identifier(Some(KeyIdentifier(TEST_SKI)))
            .authority_key_identifier(Some(KeyIdentifier(TEST_SKI)))
            .build()
    } else {
        Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(false, None)))
            .key_usage(Some(KeyUsage::DIGITAL_SIGNATURE))
            .subject_key_identifier(Some(KeyIdentifier(NOC_SKI)))
            .authority_key_identifier(Some(KeyIdentifier(TEST_SKI)))
            .build()
    };
    let fields = TestCertFields {
        serial: vec![if ca { 0x01 } else { 0x02 }],
        issuer,
        not_before: MatterTime::from_unix_secs(1_700_000_000),
        not_after: MatterTime::from_unix_secs(2_500_000_000),
        subject,
        public_key: PublicKey::new(public_key).expect("pub key"),
        extensions,
        signature: Signature::new([0u8; 64]),
    };
    let unsigned = build_unsigned(fields);
    let tbs = unsigned.to_x509_tbs_der().expect("tbs");
    let sig = signer.sign_p256_sha256(&tbs).expect("sign");
    with_signature(&unsigned, Signature::new(sig))
}

fn build_fixture() -> Fixture {
    let (rcac_signer, _) = RingSigner::generate().expect("rcac signer");
    let rcac_pub = *rcac_signer.public_key().as_bytes();
    let rcac_dn = DistinguishedName::new(vec![DnAttribute::RcacId(1)]);
    let rcac = build_cert(
        rcac_dn.clone(),
        rcac_dn.clone(),
        rcac_pub,
        true,
        &rcac_signer,
    );
    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));

    let noc_for = |node_id: u64| {
        let (signer, pkcs8) = RingSigner::generate().expect("noc signer");
        let subject = DistinguishedName::new(vec![
            DnAttribute::FabricId(TEST_FABRIC_ID),
            DnAttribute::NodeId(node_id),
        ]);
        let noc = build_cert(
            subject,
            rcac_dn.clone(),
            *signer.public_key().as_bytes(),
            false,
            &rcac_signer,
        );
        (noc, pkcs8)
    };
    let (initiator_noc, initiator_pkcs8) = noc_for(INITIATOR_NODE_ID);
    let (responder_noc, responder_pkcs8) = noc_for(RESPONDER_NODE_ID);

    Fixture {
        roots,
        rcac_pub,
        initiator_noc,
        initiator_pkcs8,
        responder_noc,
        responder_pkcs8,
    }
}

impl Fixture {
    fn creds(&self, noc: &MatterCertificate, pkcs8: &[u8], node_id: u64) -> CaseCredentials {
        CaseCredentials {
            noc: noc.clone(),
            icac: None,
            signer: Box::new(RingSigner::from_pkcs8(pkcs8).expect("signer")),
            fabric_id: TEST_FABRIC_ID,
            node_id,
            ipk: IPK,
            rcac_public_key: self.rcac_pub,
        }
    }

    fn initiator(&self) -> CaseInitiator {
        CaseInitiator::new(
            self.creds(
                &self.initiator_noc,
                &self.initiator_pkcs8,
                INITIATOR_NODE_ID,
            ),
            self.roots.clone(),
            RESPONDER_NODE_ID,
            TEST_FABRIC_ID,
            0x0001,
            now(),
        )
        .expect("initiator")
    }

    fn responder(&self) -> CaseResponder {
        CaseResponder::new(
            self.creds(
                &self.responder_noc,
                &self.responder_pkcs8,
                RESPONDER_NODE_ID,
            ),
            self.roots.clone(),
            0x0002,
            now(),
        )
        .expect("responder")
    }
}

fn bench_case(c: &mut Criterion) {
    let fx = build_fixture();

    c.bench_function("case/sigma1_generate", |b| {
        b.iter_batched(
            || fx.initiator(),
            |mut i| black_box(i.start().expect("sigma1")),
            BatchSize::PerIteration,
        );
    });

    // One Sigma1 is enough for the responder-side benches: each iteration
    // gets a FRESH responder, so replaying the same Sigma1 is a valid
    // (and deterministic) input.
    let sigma1 = fx.initiator().start().expect("sigma1");

    c.bench_function("case/sigma1_handle", |b| {
        b.iter_batched(
            || fx.responder(),
            |mut r| black_box(r.handle_sigma1(&sigma1).expect("handle sigma1")),
            BatchSize::PerIteration,
        );
    });

    c.bench_function("case/sigma2_generate", |b| {
        b.iter_batched(
            || {
                let mut r = fx.responder();
                r.handle_sigma1(&sigma1).expect("handle sigma1");
                r
            },
            |mut r| black_box(r.next_message().expect("sigma2")),
            BatchSize::PerIteration,
        );
    });

    // Sigma2 handling verifies the responder's chain + signature — the
    // expensive initiator-side step. Each iteration needs a Sigma2 built for
    // that iteration's initiator (it binds the initiator's ephemeral key).
    c.bench_function("case/sigma2_handle", |b| {
        b.iter_batched(
            || {
                let mut i = fx.initiator();
                let s1 = i.start().expect("sigma1");
                let mut r = fx.responder();
                r.handle_sigma1(&s1).expect("handle sigma1");
                let s2 = r.next_message().expect("sigma2");
                (i, s2)
            },
            |(mut i, s2)| {
                i.handle_sigma2(&s2).expect("handle sigma2");
                black_box(i)
            },
            BatchSize::PerIteration,
        );
    });

    c.bench_function("case/sigma3_generate", |b| {
        b.iter_batched(
            || {
                let mut i = fx.initiator();
                let s1 = i.start().expect("sigma1");
                let mut r = fx.responder();
                r.handle_sigma1(&s1).expect("handle sigma1");
                let s2 = r.next_message().expect("sigma2");
                i.handle_sigma2(&s2).expect("handle sigma2");
                i
            },
            |mut i| black_box(i.next_message().expect("sigma3")),
            BatchSize::PerIteration,
        );
    });

    c.bench_function("case/sigma3_handle", |b| {
        b.iter_batched(
            || {
                let mut i = fx.initiator();
                let s1 = i.start().expect("sigma1");
                let mut r = fx.responder();
                r.handle_sigma1(&s1).expect("handle sigma1");
                let s2 = r.next_message().expect("sigma2");
                i.handle_sigma2(&s2).expect("handle sigma2");
                let s3 = i.next_message().expect("sigma3");
                (r, s3)
            },
            |(mut r, s3)| {
                r.handle_sigma3(&s3).expect("handle sigma3");
                black_box(r)
            },
            BatchSize::PerIteration,
        );
    });

    c.bench_function("case/full_handshake", |b| {
        b.iter_batched(
            || (fx.initiator(), fx.responder()),
            |(mut i, mut r)| {
                let s1 = i.start().expect("sigma1");
                r.handle_sigma1(&s1).expect("handle sigma1");
                let s2 = r.next_message().expect("sigma2");
                i.handle_sigma2(&s2).expect("handle sigma2");
                let s3 = i.next_message().expect("sigma3");
                r.handle_sigma3(&s3).expect("handle sigma3");
                let io = i.finish().expect("initiator finish");
                let ro = r.finish().expect("responder finish");
                black_box((io, ro))
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group!(benches, bench_case);
criterion_main!(benches);

//! Local CASE roundtrip tests — drive [`CaseInitiator`] and [`CaseResponder`]
//! against each other and confirm shared session output.
//!
//! This file is the M4.1 correctness gate. If it passes, the two
//! state machines agree on:
//!   - The full 3-message Sigma1/2/3 handshake.
//!   - Derived session keys (`i2r_key`, `r2i_key`, `attestation_challenge`).
//!   - Peer identity (`NodeId`, `FabricId`, NOC).
//!
//! Session resumption is M4.2; matter.js byte-parity is M4.3.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use matter_cert::operational::{
    icac, noc, rcac, sign_with_ring, IcacParams, NocParams, RcacParams,
};
use matter_cert::test_support::{build_unsigned, with_signature, TestCertFields};
use matter_cert::{
    BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyIdentifier, KeyUsage,
    MatterCertificate, MatterTime, PublicKey, Signature, TrustAnchor, TrustedRoots,
};
use matter_crypto::{
    CaseCredentials, CaseInitiator, CaseResponder, CaseSigner, RingSigner, Sigma1Outcome,
};

const TEST_FABRIC_ID: u64 = 0x4242_4242_4242_4242;
const INITIATOR_NODE_ID: u64 = 0xDEAD_BEEF_CAFE_F00D;
const RESPONDER_NODE_ID: u64 = 0xBABE_FEED_1234_5678;
const IPK: [u8; 16] = [0x77; 16];

// Shared SKI/AKI value used for both the RCAC and the NOC's AKI field.
// When TrustAnchor::from_root_cert extracts the RCAC's SKI and the NOC
// carries a matching AKI, the SKI gate in CertificateChain::validate passes.
const TEST_SKI: [u8; 20] = [0x01; 20];
const NOC_SKI: [u8; 20] = [0x02; 20];

/// Build a self-signed RCAC for the test fabric.
///
/// Returns the RCAC, its `RingSigner` (needed to sign NOCs), a `TrustedRoots`
/// set containing that RCAC, and the raw 65-byte RCAC public key (needed for
/// `DestinationId` computation in `CaseCredentials`).
fn build_test_rcac() -> (MatterCertificate, RingSigner, TrustedRoots, [u8; 65]) {
    // 1. Generate RCAC keypair via RingSigner::generate.
    let (rcac_signer, _pkcs8) = RingSigner::generate().expect("rcac signer");
    let rcac_pub = *rcac_signer.public_key().as_bytes();

    // 2. Build RCAC subject DN with RcacId attribute.
    //    The NOC issuer DN must equal this DN for chain validation to succeed.
    let rcac_dn = DistinguishedName::new(vec![DnAttribute::RcacId(1)]);

    let extensions = Extensions::builder()
        .basic_constraints(Some(BasicConstraints::new(true, Some(1))))
        .key_usage(Some(KeyUsage::KEY_CERT_SIGN))
        .subject_key_identifier(Some(KeyIdentifier(TEST_SKI)))
        // Self-signed: AKI == SKI.
        .authority_key_identifier(Some(KeyIdentifier(TEST_SKI)))
        .build();

    let fields = TestCertFields {
        serial: vec![0x01],
        issuer: rcac_dn.clone(),
        not_before: MatterTime::from_unix_secs(1_700_000_000),
        not_after: MatterTime::from_unix_secs(2_500_000_000),
        subject: rcac_dn,
        public_key: PublicKey::new(rcac_pub).expect("rcac pub key"),
        extensions,
        signature: Signature::new([0u8; 64]),
    };
    let unsigned = build_unsigned(fields);

    // 3. Sign the X.509 TBS with the RCAC's own key.
    let tbs = unsigned.to_x509_tbs_der().expect("rcac tbs");
    let sig_bytes = rcac_signer.sign_p256_sha256(&tbs).expect("rcac sign self");
    let rcac = with_signature(&unsigned, Signature::new(sig_bytes));

    // 4. TrustedRoots containing this RCAC as a trust anchor.
    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));

    (rcac, rcac_signer, roots, rcac_pub)
}

/// Build a NOC signed by the given RCAC signer.
///
/// The NOC subject carries `FabricId` and `NodeId` attributes per Matter
/// spec §6.5.6. The issuer DN matches the RCAC subject so that
/// `CertificateChain::validate` succeeds. The NOC's AKI matches the
/// RCAC's SKI so the asymmetric SKI gate in chain validation passes.
fn build_test_noc(
    rcac_signer: &RingSigner,
    fabric_id: u64,
    node_id: u64,
) -> (MatterCertificate, RingSigner) {
    let (noc_signer, _) = RingSigner::generate().expect("noc signer");
    let noc_pub = *noc_signer.public_key().as_bytes();

    let subject_dn = DistinguishedName::new(vec![
        DnAttribute::FabricId(fabric_id),
        DnAttribute::NodeId(node_id),
    ]);
    // Issuer DN must match the RCAC subject DN.
    let issuer_dn = DistinguishedName::new(vec![DnAttribute::RcacId(1)]);

    let extensions = Extensions::builder()
        .basic_constraints(Some(BasicConstraints::new(false, None)))
        .key_usage(Some(KeyUsage::DIGITAL_SIGNATURE))
        .subject_key_identifier(Some(KeyIdentifier(NOC_SKI)))
        // AKI must match the RCAC's SKI to pass the SKI gate.
        .authority_key_identifier(Some(KeyIdentifier(TEST_SKI)))
        .build();

    let fields = TestCertFields {
        serial: vec![0x02],
        issuer: issuer_dn,
        not_before: MatterTime::from_unix_secs(1_700_000_000),
        not_after: MatterTime::from_unix_secs(2_500_000_000),
        subject: subject_dn,
        public_key: PublicKey::new(noc_pub).expect("noc pub key"),
        extensions,
        signature: Signature::new([0u8; 64]),
    };
    let unsigned = build_unsigned(fields);

    let tbs = unsigned.to_x509_tbs_der().expect("noc tbs");
    let sig_bytes = rcac_signer.sign_p256_sha256(&tbs).expect("rcac sign noc");
    let noc = with_signature(&unsigned, Signature::new(sig_bytes));

    (noc, noc_signer)
}

/// Assemble a `CaseCredentials` from its components.
fn build_credentials(
    noc: MatterCertificate,
    signer: RingSigner,
    fabric_id: u64,
    node_id: u64,
    ipk: [u8; 16],
    rcac_public_key: [u8; 65],
) -> CaseCredentials {
    CaseCredentials {
        noc,
        icac: None,
        signer: Box::new(signer),
        fabric_id,
        node_id,
        ipk,
        rcac_public_key,
    }
}

// ---------------------------------------------------------------------------
// Test: full new-session roundtrip
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // long because it mirrors a multi-step protocol exchange
fn case_roundtrip_new_session() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();

    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    let initiator_creds = build_credentials(
        initiator_noc,
        initiator_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc,
        responder_signer,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator construction");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder construction");

    // --- Sigma1 ---
    let sigma1 = initiator.start().expect("sigma1");
    let outcome = responder.handle_sigma1(&sigma1).expect("handle sigma1");
    assert!(
        matches!(outcome, Sigma1Outcome::NewSession),
        "expected NewSession outcome from Sigma1"
    );

    // --- Sigma2 ---
    let sigma2 = responder.next_message().expect("sigma2");
    initiator.handle_sigma2(&sigma2).expect("handle sigma2");

    // --- Sigma3 ---
    let sigma3 = initiator.next_message().expect("sigma3");
    responder.handle_sigma3(&sigma3).expect("handle sigma3");

    // --- Collect session outputs ---
    let init_output = initiator.finish().expect("initiator finish");
    let resp_output = responder.finish().expect("responder finish");

    // --- Session key parity ---
    // Both sides derive the same 48-byte HKDF output from the same inputs.
    // Both assign i2r_key = keys[0..16] and r2i_key = keys[16..32] (see
    // NodeSession.ts comments in initiator.rs and responder.rs).
    // The semantic name (who "encrypts" vs who "decrypts") flips per role,
    // but the raw bytes in each field are identical on both sides.
    assert_eq!(
        init_output.keys.i2r_key, resp_output.keys.i2r_key,
        "i2r_key must match on both sides"
    );
    assert_eq!(
        init_output.keys.r2i_key, resp_output.keys.r2i_key,
        "r2i_key must match on both sides"
    );
    assert_eq!(
        init_output.keys.attestation_challenge, resp_output.keys.attestation_challenge,
        "attestation_challenge must match on both sides"
    );

    // --- Peer identity (initiator side) ---
    assert_eq!(
        init_output.peer.node_id, RESPONDER_NODE_ID,
        "initiator must see responder's node_id"
    );
    assert_eq!(
        init_output.peer.fabric_id, TEST_FABRIC_ID,
        "initiator must see correct fabric_id for peer"
    );

    // --- Peer identity (responder side) ---
    assert_eq!(
        resp_output.peer.node_id, INITIATOR_NODE_ID,
        "responder must see initiator's node_id"
    );
    assert_eq!(
        resp_output.peer.fabric_id, TEST_FABRIC_ID,
        "responder must see correct fabric_id for peer"
    );

    // --- Local identity ---
    assert_eq!(
        init_output.local.node_id, INITIATOR_NODE_ID,
        "initiator's local node_id must be correct"
    );
    assert_eq!(
        init_output.local.fabric_id, TEST_FABRIC_ID,
        "initiator's local fabric_id must be correct"
    );
    assert_eq!(
        resp_output.local.node_id, RESPONDER_NODE_ID,
        "responder's local node_id must be correct"
    );
    assert_eq!(
        resp_output.local.fabric_id, TEST_FABRIC_ID,
        "responder's local fabric_id must be correct"
    );

    // --- Resumption records ---
    // Both sides must emerge with the SAME (id, secret) pair: the id the
    // responder sent in TBEData2 and the session's ECDH SharedSecret. Either
    // peer can later present it in Sigma1 to resume against the other.
    let init_record = init_output
        .resumption_record
        .expect("initiator record after full handshake");
    let resp_record = resp_output
        .resumption_record
        .expect("responder record after full handshake");
    assert_eq!(
        init_record.id, resp_record.id,
        "both sides must store the same resumption id"
    );
    assert_eq!(
        init_record.shared_secret, resp_record.shared_secret,
        "both sides must store the same shared secret"
    );
}

// ---------------------------------------------------------------------------
// Test: full 3-tier RCAC -> ICAC -> NOC chain over CASE
// ---------------------------------------------------------------------------

/// Build a real 3-tier operational-PKI chain via `matter_cert::operational`
/// (real SHA-1 SKIs, so AKI/SKI linkage is automatic) — deliberately NOT
/// mixed with the fixed-SKI `build_test_rcac`/`build_test_noc` helpers above,
/// whose SKIs would not match an operational-module chain.
///
/// Returns `(rcac, icac, icac_pkcs8, trusted_roots, rcac_pub)` — the ICAC key
/// signs the per-side NOCs, and the RCAC seeds `trusted_roots`.
// `rcac_*`/`icac_*` bindings are intentionally parallel-named (same role,
// different tier), mirroring `matter-cert/tests/operational_certs.rs`.
#[allow(clippy::similar_names)]
#[allow(clippy::type_complexity)]
fn build_operational_rcac_icac_chain() -> (
    MatterCertificate,
    MatterCertificate,
    Vec<u8>,
    TrustedRoots,
    [u8; 65],
) {
    let not_before = MatterTime::from_unix_secs(1_700_000_000);
    let not_after = MatterTime::NO_EXPIRY;

    // --- RCAC: self-signed root ---
    let (rcac_signer, rcac_pkcs8) = RingSigner::generate().expect("rcac signer");
    let rcac_pub_key = rcac_signer.public_key().clone();
    let unsigned_rcac = rcac(RcacParams::new(
        1,
        rcac_pub_key,
        vec![0x01],
        not_before,
        not_after,
        Some(1),
    ))
    .expect("build rcac");
    let rcac_cert = sign_with_ring(unsigned_rcac, &rcac_pkcs8).expect("sign rcac");
    let rcac_pub: [u8; 65] = *rcac_cert.public_key().as_bytes();

    let rcac_subject = rcac_cert.subject().clone();
    let rcac_skid = rcac_cert
        .extensions()
        .subject_key_identifier
        .expect("rcac ski");

    // --- ICAC: signed by the RCAC ---
    let (icac_signer, icac_pkcs8) = RingSigner::generate().expect("icac signer");
    let icac_pub_key = icac_signer.public_key().clone();
    let unsigned_icac = icac(IcacParams::new(
        2,
        rcac_subject,
        rcac_skid,
        icac_pub_key,
        vec![0x02],
        not_before,
        not_after,
    ))
    .expect("build icac");
    let icac_cert = sign_with_ring(unsigned_icac, &rcac_pkcs8).expect("sign icac");

    let mut trusted_roots = TrustedRoots::new();
    trusted_roots.add(TrustAnchor::from_root_cert(&rcac_cert));

    (rcac_cert, icac_cert, icac_pkcs8, trusted_roots, rcac_pub)
}

/// Build a NOC signed under the given ICAC's key (3-tier: ICAC issues the
/// NOC, not the RCAC).
fn build_operational_noc_under_icac(
    icac_cert: &MatterCertificate,
    icac_pkcs8: &[u8],
    fabric_id: u64,
    node_id: u64,
    serial: Vec<u8>,
) -> (MatterCertificate, RingSigner) {
    let not_before = MatterTime::from_unix_secs(1_700_000_000);
    let not_after = MatterTime::NO_EXPIRY;

    let icac_subject = icac_cert.subject().clone();
    let icac_skid = icac_cert
        .extensions()
        .subject_key_identifier
        .expect("icac ski");

    let (noc_signer, _noc_pkcs8) = RingSigner::generate().expect("noc signer");
    let noc_pub_key = noc_signer.public_key().clone();
    let unsigned_noc = noc(NocParams::new(
        fabric_id,
        node_id,
        vec![],
        icac_subject,
        icac_skid,
        noc_pub_key,
        serial,
        not_before,
        not_after,
    ))
    .expect("build noc");
    let noc_cert = sign_with_ring(unsigned_noc, icac_pkcs8).expect("sign noc under icac");

    (noc_cert, noc_signer)
}

/// The headline ICAC proof: a full Sigma1 -> Sigma2 -> Sigma3 CASE handshake
/// where both peers present a NOC issued under a 3-tier RCAC -> ICAC -> NOC
/// chain (`CaseCredentials.icac = Some(icac)`), and the initiator validates
/// the whole chain against a `TrustedRoots` that only contains the RCAC.
#[test]
#[allow(clippy::too_many_lines)] // mirrors case_roundtrip_new_session's multi-step protocol exchange
fn case_establishes_over_three_tier_chain() {
    let (rcac_cert, icac_cert, icac_pkcs8, trusted_roots, rcac_pub) =
        build_operational_rcac_icac_chain();

    let (initiator_noc, initiator_signer) = build_operational_noc_under_icac(
        &icac_cert,
        &icac_pkcs8,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        vec![0x03],
    );
    let (responder_noc, responder_signer) = build_operational_noc_under_icac(
        &icac_cert,
        &icac_pkcs8,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        vec![0x04],
    );

    // Negative control: each NOC's issuer must be the ICAC's subject, not
    // the RCAC's — proving this really is a 3-tier chain, not a disguised
    // 2-tier one.
    assert_eq!(
        initiator_noc.issuer(),
        icac_cert.subject(),
        "NOC issuer must be the ICAC subject"
    );
    assert_ne!(
        initiator_noc.issuer(),
        rcac_cert.subject(),
        "NOC issuer must NOT be the RCAC subject in a 3-tier chain"
    );

    let initiator_creds = CaseCredentials {
        noc: initiator_noc,
        icac: Some(icac_cert.clone()),
        signer: Box::new(initiator_signer),
        fabric_id: TEST_FABRIC_ID,
        node_id: INITIATOR_NODE_ID,
        ipk: IPK,
        rcac_public_key: rcac_pub,
    };
    let responder_creds = CaseCredentials {
        noc: responder_noc,
        icac: Some(icac_cert),
        signer: Box::new(responder_signer),
        fabric_id: TEST_FABRIC_ID,
        node_id: RESPONDER_NODE_ID,
        ipk: IPK,
        rcac_public_key: rcac_pub,
    };

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator construction");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder construction");

    // --- Sigma1 ---
    let sigma1 = initiator.start().expect("sigma1");
    let outcome = responder.handle_sigma1(&sigma1).expect("handle sigma1");
    assert!(
        matches!(outcome, Sigma1Outcome::NewSession),
        "expected NewSession outcome from Sigma1"
    );

    // --- Sigma2 ---
    let sigma2 = responder.next_message().expect("sigma2");
    initiator.handle_sigma2(&sigma2).expect("handle sigma2");

    // --- Sigma3 ---
    let sigma3 = initiator.next_message().expect("sigma3");
    responder.handle_sigma3(&sigma3).expect("handle sigma3");

    // --- Collect session outputs ---
    let init_output = initiator.finish().expect("initiator finish");
    let resp_output = responder.finish().expect("responder finish");

    // --- Session key parity ---
    assert_eq!(
        init_output.keys.i2r_key, resp_output.keys.i2r_key,
        "i2r_key must match on both sides"
    );
    assert_eq!(
        init_output.keys.r2i_key, resp_output.keys.r2i_key,
        "r2i_key must match on both sides"
    );
    assert_eq!(
        init_output.keys.attestation_challenge, resp_output.keys.attestation_challenge,
        "attestation_challenge must match on both sides"
    );

    // --- Peer identity ---
    assert_eq!(
        init_output.peer.node_id, RESPONDER_NODE_ID,
        "initiator must see responder's node_id"
    );
    assert_eq!(
        init_output.peer.fabric_id, TEST_FABRIC_ID,
        "initiator must see correct fabric_id for peer"
    );
    assert_eq!(
        resp_output.peer.node_id, INITIATOR_NODE_ID,
        "responder must see initiator's node_id"
    );
    assert_eq!(
        resp_output.peer.fabric_id, TEST_FABRIC_ID,
        "responder must see correct fabric_id for peer"
    );
}

// ---------------------------------------------------------------------------
// Test: records produced by a FULL handshake drive a role-flipped resumption
// ---------------------------------------------------------------------------

/// The OTA provider-server scenario: session 1 has A (controller) initiating a
/// full handshake to B (device); session 2 has B initiating BACK to A with the
/// resumption record it stored from session 1. A (now the responder) must be
/// able to `accept_resumption` with ITS stored record and both sides must
/// derive the same resumed session keys.
#[test]
#[allow(clippy::too_many_lines)] // long because it mirrors two full protocol exchanges
fn full_handshake_records_flip_roles_for_resumption() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();

    // Two credential sets per identity: one consumed per handshake.
    let (a_noc, a_signer) = build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (b_noc, b_signer) = build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);
    let (a_noc2, a_signer2) = build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (b_noc2, b_signer2) = build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);
    let now = MatterTime::from_unix_secs(2_000_000_000);

    // --- Session 1: A → B, full handshake ---
    let a_creds = build_credentials(
        a_noc,
        a_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let b_creds = build_credentials(
        b_noc,
        b_signer,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );
    let mut initiator1 = CaseInitiator::new(
        a_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        now,
    )
    .expect("initiator1");
    let mut responder1 =
        CaseResponder::new(b_creds, trusted_roots.clone(), 0x0002, now).expect("responder1");

    let sigma1 = initiator1.start().expect("sigma1");
    assert!(matches!(
        responder1.handle_sigma1(&sigma1).expect("handle sigma1"),
        Sigma1Outcome::NewSession
    ));
    let sigma2 = responder1.next_message().expect("sigma2");
    initiator1.handle_sigma2(&sigma2).expect("handle sigma2");
    let sigma3 = initiator1.next_message().expect("sigma3");
    responder1.handle_sigma3(&sigma3).expect("handle sigma3");

    let a_record = initiator1
        .finish()
        .expect("finish1 initiator")
        .resumption_record
        .expect("A record");
    let b_record = responder1
        .finish()
        .expect("finish1 responder")
        .resumption_record
        .expect("B record");

    // --- Session 2: B → A, resumption (roles flipped) ---
    let a_creds2 = build_credentials(
        a_noc2,
        a_signer2,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let b_creds2 = build_credentials(
        b_noc2,
        b_signer2,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );
    let mut initiator2 = CaseInitiator::new_with_resumption(
        b_creds2,
        trusted_roots.clone(),
        INITIATOR_NODE_ID,
        TEST_FABRIC_ID,
        b_record,
        0x0001,
        now,
    )
    .expect("initiator2 (B resuming)");
    let mut responder2 =
        CaseResponder::new(a_creds2, trusted_roots, 0x0004, now).expect("responder2 (A)");

    let sigma1r = initiator2.start().expect("sigma1 (resume)");
    let outcome = responder2.handle_sigma1(&sigma1r).expect("handle sigma1r");
    let presented = match outcome {
        Sigma1Outcome::ResumptionRequested { id } => id,
        Sigma1Outcome::NewSession => panic!("expected ResumptionRequested"),
    };
    assert_eq!(
        presented, a_record.id,
        "B must present the id A stored from session 1"
    );
    responder2
        .accept_resumption(a_record)
        .expect("A accepts resumption with its stored record");
    let sigma2_resume = responder2.next_message().expect("sigma2_resume");
    initiator2
        .handle_sigma2_resume(&sigma2_resume)
        .expect("B handles sigma2_resume");

    let b_out = initiator2.finish().expect("finish2 initiator");
    let a_out = responder2.finish().expect("finish2 responder");
    assert_eq!(
        b_out.keys.i2r_key, a_out.keys.i2r_key,
        "resumed i2r keys must agree"
    );
    assert_eq!(
        b_out.keys.r2i_key, a_out.keys.r2i_key,
        "resumed r2i keys must agree"
    );

    // The rotated records must again agree on both sides for the NEXT session.
    let b_next = b_out.resumption_record.expect("B rotated record");
    let a_next = a_out.resumption_record.expect("A rotated record");
    assert_eq!(b_next.id, a_next.id, "rotated ids must agree");
    assert_eq!(
        b_next.shared_secret, a_next.shared_secret,
        "secret carries over unchanged"
    );
}

// ---------------------------------------------------------------------------
// Test: wrong-fabric handshake must fail
// ---------------------------------------------------------------------------

// Fabric ID that deliberately mismatches TEST_FABRIC_ID; used by
// `case_roundtrip_wrong_fabric_returns_fabric_mismatch`.
const WRONG_FABRIC: u64 = 0x9999_9999_9999_9999;

#[test]
fn case_roundtrip_wrong_fabric_returns_fabric_mismatch() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();

    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, WRONG_FABRIC, RESPONDER_NODE_ID);

    let initiator_creds = build_credentials(
        initiator_noc,
        initiator_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc,
        responder_signer,
        WRONG_FABRIC,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();

    // Sigma1: computed dest_id is HMAC(IPK, random || rcacPub || initiatorFabricId || responderNodeId).
    // The responder recomputes using WRONG_FABRIC — so dest_id will mismatch immediately.
    // In that case handle_sigma1 returns Error::InvalidParameter and we never get to Sigma2.
    // If handle_sigma1 somehow passes (dest_id happened to match), we expect an error before
    // or during Sigma2 processing (FabricIdMismatch). Either way: not Ok.
    let result = responder
        .handle_sigma1(&initiator.start().unwrap())
        .and_then(|_| {
            let sigma2 = responder.next_message()?;
            initiator.handle_sigma2(&sigma2)?;
            Ok(())
        });
    assert!(
        result.is_err(),
        "handshake must fail when fabric IDs mismatch"
    );
}

// ---------------------------------------------------------------------------
// Resumption helpers
// ---------------------------------------------------------------------------

/// Drive a fresh new-session handshake to completion and return the outputs
/// from both sides.
///
/// Re-builds credentials from scratch (all inputs generated fresh inside this
/// function), so each call is independent.  Also returns the RCAC signer and
/// the raw RCAC public key so the caller can build fresh credentials for the
/// resumption round without regenerating the root.
fn drive_new_session_handshake() -> (
    matter_crypto::CaseSessionOutput,
    matter_crypto::CaseSessionOutput,
) {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);
    let initiator_creds = build_credentials(
        initiator_noc,
        initiator_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc,
        responder_signer,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder");

    let sigma1 = initiator.start().expect("sigma1");
    let outcome = responder.handle_sigma1(&sigma1).expect("handle sigma1");
    assert!(
        matches!(outcome, Sigma1Outcome::NewSession),
        "expected NewSession from fresh handshake"
    );
    let sigma2 = responder.next_message().expect("sigma2");
    initiator.handle_sigma2(&sigma2).expect("handle sigma2");
    let sigma3 = initiator.next_message().expect("sigma3");
    responder.handle_sigma3(&sigma3).expect("handle sigma3");

    let init_out = initiator.finish().expect("initiator finish");
    let resp_out = responder.finish().expect("responder finish");
    (init_out, resp_out)
}

// ---------------------------------------------------------------------------
// Peer-signature verification (the line that authenticates CASE).
// ---------------------------------------------------------------------------
//
// The ECDSA verify over TBSData2 / TBSData3 is what proves the peer holds the
// private key for the NOC it presented. A byte-flip anywhere in the *outer*
// Sigma2 dies at the AEAD tag and never reaches this verify
// (`case_random_byte_flip_in_sigma2_never_panics` only asserts "no panic").
// Here we keep the AEAD valid but make the inner signature wrong, by signing
// the handshake transcript with a key that does NOT match the presented NOC.
// The NOC still chains to the trusted RCAC and the AEAD still authenticates,
// so ONLY the peer-signature verify can catch the mismatch — it must return
// `PeerSignatureInvalid`. Mirrors chip `TestCASESession.cpp:650`
// (`BadSignatureFailsCASE`, which fault-injects a corrupt signature inside
// TBEData). Without these tests the highest-consequence line in the crate had
// zero coverage.

/// A responder whose signing key does not match its NOC public key must be
/// rejected by the initiator at Sigma2 with `PeerSignatureInvalid`.
#[test]
fn sigma2_peer_signature_mismatch_is_rejected() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, _responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);
    // Fresh key whose public half is NOT the one bound into `responder_noc`.
    let (wrong_signer, _) = RingSigner::generate().expect("wrong signer");

    let initiator_creds = build_credentials(
        initiator_noc,
        initiator_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc,
        wrong_signer, // <-- signs TBSData2 with a key the NOC does not authorize
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder");

    let sigma1 = initiator.start().expect("sigma1");
    responder.handle_sigma1(&sigma1).expect("handle sigma1");
    let sigma2 = responder.next_message().expect("sigma2"); // AEAD valid, inner sig wrong

    let err = initiator
        .handle_sigma2(&sigma2)
        .expect_err("a Sigma2 whose inner signature does not match the NOC must be rejected");
    assert!(
        matches!(err, matter_crypto::Error::PeerSignatureInvalid),
        "expected PeerSignatureInvalid (AEAD passed; only ECDSA-verify catches this), got {err:?}"
    );
}

/// The Sigma3 equivalent: an initiator whose signing key does not match its
/// NOC must be rejected by the responder at Sigma3 with `PeerSignatureInvalid`.
#[test]
fn sigma3_peer_signature_mismatch_is_rejected() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, _initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);
    let (wrong_signer, _) = RingSigner::generate().expect("wrong signer");

    let initiator_creds = build_credentials(
        initiator_noc,
        wrong_signer, // <-- signs TBSData3 with a key the NOC does not authorize
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc,
        responder_signer,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder");

    let sigma1 = initiator.start().expect("sigma1");
    responder.handle_sigma1(&sigma1).expect("handle sigma1");
    let sigma2 = responder.next_message().expect("sigma2");
    // Responder is honest, so Sigma2 verifies fine.
    initiator.handle_sigma2(&sigma2).expect("handle sigma2");
    let sigma3 = initiator.next_message().expect("sigma3"); // AEAD valid, inner sig wrong

    let err = responder
        .handle_sigma3(&sigma3)
        .expect_err("a Sigma3 whose inner signature does not match the NOC must be rejected");
    assert!(
        matches!(err, matter_crypto::Error::PeerSignatureInvalid),
        "expected PeerSignatureInvalid (AEAD passed; only ECDSA-verify catches this), got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test: resumption accepted — 2-message fast path
// ---------------------------------------------------------------------------

/// Drive a fresh handshake; build synthetic resumption records; drive a
/// second handshake via [`CaseInitiator::new_with_resumption`] +
/// [`CaseResponder::accept_resumption`].  Both sides must agree on the
/// resumed session keys and produce a new [`ResumptionRecord`].
// The test necessarily covers many sequential steps: fresh handshake,
// record construction, resumption round, key-agreement assertions, and
// record refresh.  Splitting it would make it harder to follow the protocol
// flow, so the line-count lint is suppressed here.
#[allow(clippy::too_many_lines)]
#[test]
fn case_resumption_roundtrip_accepted() {
    // Step 1: Drive an initial new-session handshake.  We only need the
    // peer NOCs captured by the session outputs (stored in peer.noc) so we
    // can build valid PeerInfo structs for the synthetic records.
    let (initiator_out_1, _responder_out_1) = drive_new_session_handshake();

    // The new-session path does NOT populate resumption_record (per M4.2
    // design; M6 commissioning fills it).  We synthesise records manually
    // using a known shared_secret and id to drive the resumption path.
    let resumption_id = matter_crypto::ResumptionId([0x42; 16]);
    let shared_secret = [0x77u8; 32];

    // Step 2: Build fresh credentials for the resumption round (the first
    // round's credentials were consumed by drive_new_session_handshake).
    let (_rcac2, rcac_signer2, trusted_roots2, rcac_pub2) = build_test_rcac();
    let (initiator_noc2, initiator_signer2) =
        build_test_noc(&rcac_signer2, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc2, responder_signer2) =
        build_test_noc(&rcac_signer2, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    // From the initiator's perspective its peer is the RESPONDER, so
    // peer.noc must be a valid responder NOC.  We use the fresh responder_noc2
    // (which is validly signed by rcac_signer2, matching trusted_roots2).
    let initiator_peer_info = matter_crypto::PeerInfo {
        node_id: RESPONDER_NODE_ID,
        fabric_id: TEST_FABRIC_ID,
        noc: responder_noc2.clone(),
        session_id: 0,
    };
    // The initiator's record: id = the OLD id it will present in Sigma1.
    let record_for_initiator = matter_crypto::ResumptionRecord {
        id: resumption_id,
        shared_secret,
        peer: initiator_peer_info,
        expires_at: None,
    };

    // From the responder's perspective its peer is the INITIATOR.
    let responder_peer_info = matter_crypto::PeerInfo {
        node_id: INITIATOR_NODE_ID,
        fabric_id: TEST_FABRIC_ID,
        noc: initiator_noc2.clone(),
        session_id: 0,
    };
    // The responder's record: id must match what the initiator presents (same OLD id).
    let record_for_responder = matter_crypto::ResumptionRecord {
        id: resumption_id, // same id — accept_resumption checks equality
        shared_secret,
        peer: responder_peer_info,
        expires_at: None,
    };

    let initiator_creds2 = build_credentials(
        initiator_noc2,
        initiator_signer2,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub2,
    );
    let responder_creds2 = build_credentials(
        responder_noc2,
        responder_signer2,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub2,
    );

    // Step 3: Initiator with resumption record.
    let mut initiator2 = CaseInitiator::new_with_resumption(
        initiator_creds2,
        trusted_roots2.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        record_for_initiator,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator2 with resumption");
    let mut responder2 = CaseResponder::new(
        responder_creds2,
        trusted_roots2,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder2");

    // Step 4: Drive the 2-message resumption handshake.
    let sigma1_r = initiator2.start().expect("sigma1 (resumption)");

    let outcome2 = responder2
        .handle_sigma1(&sigma1_r)
        .expect("handle sigma1 (resumption)");
    let presented_id = match outcome2 {
        Sigma1Outcome::ResumptionRequested { id } => id,
        Sigma1Outcome::NewSession => panic!("expected ResumptionRequested, got NewSession"),
    };
    assert_eq!(
        presented_id, resumption_id,
        "responder must see the resumption_id the initiator embedded in Sigma1"
    );

    // Responder accepts with the matching record.
    responder2
        .accept_resumption(record_for_responder)
        .expect("accept_resumption");

    // Responder emits Sigma2_Resume (the 2nd and final protocol message).
    let sigma2_resume = responder2.next_message().expect("sigma2_resume");

    // Initiator verifies the MIC and derives resumed session keys.
    initiator2
        .handle_sigma2_resume(&sigma2_resume)
        .expect("handle_sigma2_resume");

    // Step 5: Both sides agree on resumed session keys.
    let init_out2 = initiator2.finish().expect("initiator2 finish");
    let resp_out2 = responder2.finish().expect("responder2 finish");

    assert_eq!(
        init_out2.keys.i2r_key, resp_out2.keys.i2r_key,
        "both sides must derive matching i2r_key via resumption"
    );
    assert_eq!(
        init_out2.keys.r2i_key, resp_out2.keys.r2i_key,
        "both sides must derive matching r2i_key via resumption"
    );
    assert_eq!(
        init_out2.keys.attestation_challenge, resp_out2.keys.attestation_challenge,
        "both sides must agree on attestation_challenge via resumption"
    );

    // Resumed session keys MUST differ from the original new-session keys
    // (different HKDF inputs: shared_secret, salt=initiatorRandom||OLD_id).
    assert_ne!(
        init_out2.keys.i2r_key, initiator_out_1.keys.i2r_key,
        "resumed session keys must differ from original new-session keys"
    );

    // Peer identity is carried from the resumption record (no fresh NOC exchange).
    assert_eq!(
        init_out2.peer.node_id, RESPONDER_NODE_ID,
        "initiator sees responder's node_id after resumption"
    );
    assert_eq!(
        resp_out2.peer.node_id, INITIATOR_NODE_ID,
        "responder sees initiator's node_id after resumption"
    );

    // Both sides receive a fresh ResumptionRecord for the next session.
    let init_next = init_out2
        .resumption_record
        .expect("initiator gets a fresh resumption record");
    let resp_next = resp_out2
        .resumption_record
        .expect("responder gets a fresh resumption record");
    assert_eq!(
        init_next.id, resp_next.id,
        "both sides agree on the new resumption_id for the next session"
    );
    assert_ne!(
        init_next.id, resumption_id,
        "new resumption_id differs from the one we just used (responder generates fresh)"
    );
}

// ---------------------------------------------------------------------------
// Test: resumption declined → falls back to full Sigma1/2/3
// ---------------------------------------------------------------------------

/// Initiator presents a resumption record; responder calls
/// [`CaseResponder::reject_resumption`]; handshake falls back to the normal
/// new-session 3-message path and completes successfully.
#[test]
fn case_resumption_declined_falls_back_to_new_session() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    let initiator_creds = build_credentials(
        initiator_noc,
        initiator_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc.clone(),
        responder_signer,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    // Initiator presents a (bogus) resumption_id; responder has no matching record
    // and will call reject_resumption.
    let bogus_peer_info = matter_crypto::PeerInfo {
        node_id: RESPONDER_NODE_ID,
        fabric_id: TEST_FABRIC_ID,
        noc: responder_noc,
        session_id: 0,
    };
    let bogus_record = matter_crypto::ResumptionRecord {
        id: matter_crypto::ResumptionId([0x99; 16]),
        shared_secret: [0xCC; 32],
        peer: bogus_peer_info,
        expires_at: None,
    };

    let mut initiator = CaseInitiator::new_with_resumption(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        bogus_record,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator with resumption");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder");

    let sigma1 = initiator.start().expect("sigma1");
    let outcome = responder.handle_sigma1(&sigma1).expect("handle sigma1");
    assert!(
        matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }),
        "responder must surface ResumptionRequested when Sigma1 has resumption fields"
    );

    // Responder has no matching record — declines and falls back.
    responder.reject_resumption().expect("reject_resumption");

    // next_message() now returns a regular Sigma2, not Sigma2_Resume.
    let sigma2 = responder.next_message().expect("sigma2 (fallback)");

    // Initiator's handle_sigma2 takes the fallback new-session path.
    initiator
        .handle_sigma2(&sigma2)
        .expect("handle sigma2 (fallback)");
    let sigma3 = initiator.next_message().expect("sigma3 (fallback)");
    responder
        .handle_sigma3(&sigma3)
        .expect("handle sigma3 (fallback)");

    // Both sides must agree on session keys via the full new-session path.
    let init_out = initiator.finish().expect("initiator finish (fallback)");
    let resp_out = responder.finish().expect("responder finish (fallback)");
    assert_eq!(
        init_out.keys.i2r_key, resp_out.keys.i2r_key,
        "i2r_key must match after fallback to new-session"
    );
    assert_eq!(
        init_out.keys.r2i_key, resp_out.keys.r2i_key,
        "r2i_key must match after fallback to new-session"
    );
    assert_eq!(
        init_out.keys.attestation_challenge, resp_out.keys.attestation_challenge,
        "attestation_challenge must match after fallback to new-session"
    );
}

// ---------------------------------------------------------------------------
// Test: accept_resumption with mismatching id → InvalidParameter
// ---------------------------------------------------------------------------

/// If the caller supplies a [`ResumptionRecord`] whose `id` does not match
/// the `resumption_id` the initiator presented in Sigma1,
/// [`CaseResponder::accept_resumption`] must return
/// [`matter_crypto::Error::InvalidParameter`].
#[test]
fn case_resumption_wrong_id_returns_invalid_parameter() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    let initiator_creds = build_credentials(
        initiator_noc.clone(),
        initiator_signer,
        TEST_FABRIC_ID,
        INITIATOR_NODE_ID,
        IPK,
        rcac_pub,
    );
    let responder_creds = build_credentials(
        responder_noc.clone(),
        responder_signer,
        TEST_FABRIC_ID,
        RESPONDER_NODE_ID,
        IPK,
        rcac_pub,
    );

    // Initiator presents id = [0x42; 16].
    let presented_id = matter_crypto::ResumptionId([0x42; 16]);
    let initiator_record = matter_crypto::ResumptionRecord {
        id: presented_id,
        shared_secret: [0x77; 32],
        peer: matter_crypto::PeerInfo {
            node_id: RESPONDER_NODE_ID,
            fabric_id: TEST_FABRIC_ID,
            noc: responder_noc,
            session_id: 0,
        },
        expires_at: None,
    };

    // Responder's store has a record with a DIFFERENT id ([0x99; 16]).
    // accept_resumption must detect the mismatch and return InvalidParameter.
    let responder_record_wrong_id = matter_crypto::ResumptionRecord {
        id: matter_crypto::ResumptionId([0x99; 16]), // DIFFERENT from presented_id
        shared_secret: [0x77; 32],
        peer: matter_crypto::PeerInfo {
            node_id: INITIATOR_NODE_ID,
            fabric_id: TEST_FABRIC_ID,
            noc: initiator_noc,
            session_id: 0,
        },
        expires_at: None,
    };

    let mut initiator = CaseInitiator::new_with_resumption(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        initiator_record,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator with resumption");
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder");

    let sigma1 = initiator.start().expect("sigma1");
    let outcome = responder.handle_sigma1(&sigma1).expect("handle sigma1");
    assert!(
        matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }),
        "expected ResumptionRequested"
    );

    // accept_resumption with the WRONG id must return InvalidParameter.
    let err = responder
        .accept_resumption(responder_record_wrong_id)
        .expect_err("accept_resumption with wrong id must fail");
    assert!(
        matches!(err, matter_crypto::Error::InvalidParameter),
        "expected InvalidParameter, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test: session IDs thread through both sides
// ---------------------------------------------------------------------------

/// Verify that `CaseInitiator::new` and `CaseResponder::new` accept a session
/// ID and that `CaseSessionOutput.local.session_id` and `.peer.session_id`
/// reflect the values threaded through the wire messages.
#[test]
fn case_roundtrip_threads_session_ids() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    let initiator_creds = CaseCredentials {
        noc: initiator_noc,
        icac: None,
        signer: Box::new(initiator_signer),
        fabric_id: TEST_FABRIC_ID,
        node_id: INITIATOR_NODE_ID,
        ipk: IPK,
        rcac_public_key: rcac_pub,
    };
    let responder_creds = CaseCredentials {
        noc: responder_noc,
        icac: None,
        signer: Box::new(responder_signer),
        fabric_id: TEST_FABRIC_ID,
        node_id: RESPONDER_NODE_ID,
        ipk: IPK,
        rcac_public_key: rcac_pub,
    };

    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x00C1,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        0x00D2,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();

    let sigma1 = initiator.start().unwrap();
    let outcome = responder.handle_sigma1(&sigma1).unwrap();
    assert!(
        matches!(outcome, Sigma1Outcome::NewSession),
        "expected NewSession"
    );
    let sigma2 = responder.next_message().unwrap();
    initiator.handle_sigma2(&sigma2).unwrap();
    let sigma3 = initiator.next_message().unwrap();
    responder.handle_sigma3(&sigma3).unwrap();

    let init_out = initiator.finish().unwrap();
    let resp_out = responder.finish().unwrap();

    assert_eq!(
        init_out.local.session_id, 0x00C1,
        "initiator local session_id"
    );
    assert_eq!(
        init_out.peer.session_id, 0x00D2,
        "initiator peer session_id"
    );
    assert_eq!(
        resp_out.local.session_id, 0x00D2,
        "responder local session_id"
    );
    assert_eq!(
        resp_out.peer.session_id, 0x00C1,
        "responder peer session_id"
    );
}

// ---------------------------------------------------------------------------
// Proptest properties
// ---------------------------------------------------------------------------

use proptest::prelude::*;

proptest! {
    // Each case does ECDH key-gen + ECDSA sign × 2 + chain validation.
    // 8 cases keeps the suite fast while still exercising the distribution.
    #![proptest_config(ProptestConfig {
        cases: 8,
        ..ProptestConfig::default()
    })]

    /// Any two distinct NodeIDs on the same fabric must produce a clean
    /// new-session CASE handshake with matching session keys.
    ///
    /// This exercises the full Sigma1 → Sigma2 → Sigma3 path with
    /// arbitrary caller-supplied NodeIDs, verifying that the state
    /// machines are not accidentally coupled to the fixed test constants.
    #[test]
    fn case_random_node_ids_roundtrip(
        initiator_node_id in 1u64..=0x0000_FFFF_FFFF_FFFFu64,
        responder_node_id in 1u64..=0x0000_FFFF_FFFF_FFFFu64,
    ) {
        prop_assume!(initiator_node_id != responder_node_id);

        let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
        let (initiator_noc, initiator_signer) =
            build_test_noc(&rcac_signer, TEST_FABRIC_ID, initiator_node_id);
        let (responder_noc, responder_signer) =
            build_test_noc(&rcac_signer, TEST_FABRIC_ID, responder_node_id);

        let initiator_creds = build_credentials(
            initiator_noc,
            initiator_signer,
            TEST_FABRIC_ID,
            initiator_node_id,
            IPK,
            rcac_pub,
        );
        let responder_creds = build_credentials(
            responder_noc,
            responder_signer,
            TEST_FABRIC_ID,
            responder_node_id,
            IPK,
            rcac_pub,
        );

        let mut initiator = CaseInitiator::new(
            initiator_creds,
            trusted_roots.clone(),
            responder_node_id,
            TEST_FABRIC_ID,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let mut responder = CaseResponder::new(responder_creds, trusted_roots, 0x0002, MatterTime::from_unix_secs(2_000_000_000)).unwrap();

        let sigma1 = initiator.start().unwrap();
        responder.handle_sigma1(&sigma1).unwrap();
        let sigma2 = responder.next_message().unwrap();
        initiator.handle_sigma2(&sigma2).unwrap();
        let sigma3 = initiator.next_message().unwrap();
        responder.handle_sigma3(&sigma3).unwrap();

        let init_out = initiator.finish().unwrap();
        let resp_out = responder.finish().unwrap();

        prop_assert_eq!(init_out.keys.i2r_key, resp_out.keys.i2r_key);
        prop_assert_eq!(init_out.keys.r2i_key, resp_out.keys.r2i_key);
        prop_assert_eq!(init_out.peer.node_id, responder_node_id);
        prop_assert_eq!(resp_out.peer.node_id, initiator_node_id);
    }

    /// Flipping a single bit anywhere in a Sigma2 message must produce an
    /// `Err` from `handle_sigma2` — never a panic.
    ///
    /// This is an anti-panic property, not a correctness property.  It verifies
    /// that all parse and authentication checks in `handle_sigma2` return
    /// structured errors rather than indexing out-of-bounds or unwrapping.
    #[test]
    fn case_random_byte_flip_in_sigma2_never_panics(
        flip_offset in any::<usize>(),
        flip_bit in 0u8..8u8,
    ) {
        let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
        let (initiator_noc, initiator_signer) =
            build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
        let (responder_noc, responder_signer) =
            build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);
        let initiator_creds = build_credentials(
            initiator_noc,
            initiator_signer,
            TEST_FABRIC_ID,
            INITIATOR_NODE_ID,
            IPK,
            rcac_pub,
        );
        let responder_creds = build_credentials(
            responder_noc,
            responder_signer,
            TEST_FABRIC_ID,
            RESPONDER_NODE_ID,
            IPK,
            rcac_pub,
        );

        let mut initiator = CaseInitiator::new(
            initiator_creds,
            trusted_roots.clone(),
            RESPONDER_NODE_ID,
            TEST_FABRIC_ID,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let mut responder = CaseResponder::new(responder_creds, trusted_roots, 0x0002, MatterTime::from_unix_secs(2_000_000_000)).unwrap();

        let sigma1 = initiator.start().unwrap();
        responder.handle_sigma1(&sigma1).unwrap();
        let mut sigma2 = responder.next_message().unwrap();

        if !sigma2.is_empty() {
            let off = flip_offset % sigma2.len();
            sigma2[off] ^= 1 << flip_bit;
        }

        // The only requirement: no panic.  An Err is the expected outcome.
        let _ = initiator.handle_sigma2(&sigma2);
    }
}

// ---------------------------------------------------------------------------
// SessionParameters (Matter Core Spec §4.12.8) advertised across the handshake
// ---------------------------------------------------------------------------

/// The initiator's advertisement in Sigma1 (context tag 5) must arrive intact
/// at the responder, and the responder's in Sigma2 (also context tag 5) must
/// arrive intact at the initiator.
///
/// This is the whole point of the element: as the responder we have no mDNS
/// record for the peer, so Sigma1 is the only channel through which a sleepy
/// initiator can tell us its retransmit timings.
#[test]
fn session_parameters_cross_the_wire_in_both_directions() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    // Deliberately asymmetric, and deliberately not the spec defaults, so a
    // dropped field cannot pass by coinciding with a default.
    let initiator_params = matter_crypto::SessionParameters::new()
        .with_session_idle_interval_ms(3300)
        .with_session_active_interval_ms(1100)
        .with_session_active_threshold_ms(4000)
        .with_data_model_revision(19)
        .with_interaction_model_revision(11)
        .with_specification_version(0x0104_0000)
        .with_max_paths_per_invoke(1);
    let responder_params = matter_crypto::SessionParameters::new()
        .with_session_idle_interval_ms(1800)
        .with_session_active_interval_ms(1000)
        // The eufy E31 advertises an SAT *below* its own SAI. Honour it
        // verbatim rather than "correcting" it — the shape occurs in the wild.
        .with_session_active_threshold_ms(300);

    let mut initiator = CaseInitiator::new(
        build_credentials(
            initiator_noc,
            initiator_signer,
            TEST_FABRIC_ID,
            INITIATOR_NODE_ID,
            IPK,
            rcac_pub,
        ),
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator")
    .with_session_params(initiator_params);

    let mut responder = CaseResponder::new(
        build_credentials(
            responder_noc,
            responder_signer,
            TEST_FABRIC_ID,
            RESPONDER_NODE_ID,
            IPK,
            rcac_pub,
        ),
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder")
    .with_session_params(responder_params);

    assert_eq!(
        responder.peer_session_params(),
        None,
        "nothing is known before Sigma1 is processed"
    );

    let sigma1 = initiator.start().expect("sigma1");
    responder.handle_sigma1(&sigma1).expect("handle sigma1");

    assert_eq!(
        responder.peer_session_params(),
        Some(initiator_params),
        "Sigma1 tag 5 must round-trip every field"
    );

    let sigma2 = responder.next_message().expect("sigma2");
    initiator.handle_sigma2(&sigma2).expect("handle sigma2");

    assert_eq!(
        initiator.peer_session_params(),
        Some(responder_params),
        "Sigma2 tag 5 must round-trip every field"
    );
    // The responder advertised only three of seven fields; the rest must stay
    // absent rather than being filled in with defaults. "Not sent" and "sent as
    // the default value" are different facts, and the MRP merge depends on it.
    let got = initiator.peer_session_params().expect("params");
    assert_eq!(got.data_model_revision, None);
    assert_eq!(got.max_paths_per_invoke, None);
}

/// `Sigma2_Resume` carries the element at context tag **4**, not 5. A crossed
/// tag number would encode an element the peer silently ignores, so this
/// asserts the resumption path specifically.
#[test]
fn session_parameters_cross_the_wire_on_the_resumption_path() {
    let (_rcac, rcac_signer, trusted_roots, rcac_pub) = build_test_rcac();
    let (initiator_noc, initiator_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, INITIATOR_NODE_ID);
    let (responder_noc, responder_signer) =
        build_test_noc(&rcac_signer, TEST_FABRIC_ID, RESPONDER_NODE_ID);

    let responder_params = matter_crypto::SessionParameters::new()
        .with_session_idle_interval_ms(3300)
        .with_session_active_interval_ms(1100)
        .with_session_active_threshold_ms(4000);

    // A matched pair of records: same id, same secret, as a real prior session
    // would have left on both sides.
    let shared_secret = [0x77u8; 32];
    let id = matter_crypto::ResumptionId([0x42u8; 16]);
    let initiator_record = matter_crypto::ResumptionRecord {
        id,
        shared_secret,
        peer: matter_crypto::PeerInfo {
            node_id: RESPONDER_NODE_ID,
            fabric_id: TEST_FABRIC_ID,
            noc: responder_noc.clone(),
            session_id: 0x0002,
        },
        expires_at: None,
    };
    let responder_record = matter_crypto::ResumptionRecord {
        id,
        shared_secret,
        peer: matter_crypto::PeerInfo {
            node_id: INITIATOR_NODE_ID,
            fabric_id: TEST_FABRIC_ID,
            noc: initiator_noc.clone(),
            session_id: 0x0001,
        },
        expires_at: None,
    };

    let mut initiator = CaseInitiator::new_with_resumption(
        build_credentials(
            initiator_noc,
            initiator_signer,
            TEST_FABRIC_ID,
            INITIATOR_NODE_ID,
            IPK,
            rcac_pub,
        ),
        trusted_roots.clone(),
        RESPONDER_NODE_ID,
        TEST_FABRIC_ID,
        initiator_record,
        0x0001,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("initiator");

    let mut responder = CaseResponder::new(
        build_credentials(
            responder_noc,
            responder_signer,
            TEST_FABRIC_ID,
            RESPONDER_NODE_ID,
            IPK,
            rcac_pub,
        ),
        trusted_roots,
        0x0002,
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .expect("responder")
    .with_session_params(responder_params);

    let sigma1 = initiator.start().expect("sigma1");
    let outcome = responder.handle_sigma1(&sigma1).expect("handle sigma1");
    assert!(matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }));
    responder
        .accept_resumption(responder_record)
        .expect("accept resumption");
    let sigma2_resume = responder.next_message().expect("sigma2_resume");
    initiator
        .handle_sigma2_resume(&sigma2_resume)
        .expect("handle sigma2_resume");

    assert_eq!(
        initiator.peer_session_params(),
        Some(responder_params),
        "Sigma2_Resume carries the element at context tag 4, not 5"
    );
}

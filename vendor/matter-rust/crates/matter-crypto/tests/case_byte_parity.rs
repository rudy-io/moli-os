//! matter.js byte-parity tests for CASE.
//!
//! Each test loads a captured handshake JSON fixture from
//! `test-vectors/case/` and replays it through our state machines,
//! asserting byte-identical message output at every step.
//!
//! Fixtures produced by `cargo xtask capture-case`; a test fails with
//! "fixture not found" if its fixture is missing.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::doc_markdown)] // test module docs need not link every identifier
#![allow(clippy::struct_field_names)] // fixture field names must match JSON keys verbatim

use std::fs;
use std::path::PathBuf;

use matter_cert::{MatterCertificate, MatterTime, TrustAnchor, TrustedRoots};
use matter_crypto::{
    test_support::{
        case_initiator_with_eph_key, case_initiator_with_resumption_eph_key,
        case_responder_with_eph_key_and_resumption_id,
    },
    CaseCredentials, PeerInfo, ResumptionId, ResumptionRecord, RingSigner, Sigma1Outcome,
};
use serde::Deserialize;

// =============================================================================
// Fixture types — field names match the JSON produced by the capture script
// =============================================================================

#[derive(Debug, Deserialize)]
struct Fixture {
    inputs: FixtureInputs,
    messages: FixtureMessages,
}

#[derive(Debug, Deserialize)]
struct FixtureInputs {
    fabric_id: u64,
    initiator_node_id: u64,
    responder_node_id: u64,
    /// Hex-encoded 16-byte Identity Protection Key.
    ipk: String,
    /// Hex-encoded RCAC certificate TLV bytes.
    rcac_noc: String,
    /// Hex-encoded 65-byte SEC1-uncompressed RCAC public key.
    rcac_public_key: String,
    /// Hex-encoded initiator NOC TLV bytes.
    initiator_noc: String,
    /// Hex-encoded PKCS#8 DER for the initiator's NOC private key.
    initiator_pkcs8: String,
    /// Hex-encoded responder NOC TLV bytes.
    responder_noc: String,
    /// Hex-encoded PKCS#8 DER for the responder's NOC private key.
    responder_pkcs8: String,
    /// Hex-encoded ICAC certificate TLV bytes (optional; present when CA uses 3-tier PKI).
    #[serde(default)]
    icac_noc: Option<String>,
    /// Hex-encoded 32-byte initiator ephemeral private key scalar.
    initiator_eph_priv: String,
    /// Hex-encoded 32-byte initiator random.
    initiator_random: String,
    /// Hex-encoded 32-byte responder ephemeral private key scalar.
    responder_eph_priv: String,
    /// Hex-encoded 32-byte responder random.
    responder_random: String,
    // Resumption-only fields (optional in new-session fixtures):
    #[serde(default)]
    resumption_id: Option<String>,
    #[serde(default)]
    resumption_shared_secret: Option<String>,
    /// The fresh resumption id the capture script's responder embeds in
    /// TBEData2 (new-session / declined paths) or `Sigma2_Resume` (accepted
    /// path). Injected into our responder via
    /// `case_responder_with_eph_key_and_resumption_id` so the encrypted
    /// output is byte-comparable.
    #[serde(default)]
    new_resumption_id: Option<String>,
    /// The `SessionParameters` the capture script advertised in Sigma1's
    /// `initiatorSessionParams` (context tag 5). Present only in the
    /// session-params scenario.
    #[serde(default)]
    session_params: Option<FixtureSessionParams>,
}

/// Mirrors matter.js's `TlvSessionParameters` field names so the fixture is
/// readable next to `CaseMessages.ts`.
#[derive(Debug, Deserialize)]
struct FixtureSessionParams {
    #[serde(rename = "idleInterval")]
    idle_interval: u32,
    #[serde(rename = "activeInterval")]
    active_interval: u32,
    #[serde(rename = "activeThreshold")]
    active_threshold: u16,
    #[serde(rename = "dataModelRevision")]
    data_model_revision: u16,
    #[serde(rename = "interactionModelRevision")]
    interaction_model_revision: u16,
    #[serde(rename = "specificationVersion")]
    specification_version: u32,
    #[serde(rename = "maxPathsPerInvoke")]
    max_paths_per_invoke: u16,
}

impl FixtureSessionParams {
    fn to_params(&self) -> matter_crypto::SessionParameters {
        matter_crypto::SessionParameters::new()
            .with_session_idle_interval_ms(self.idle_interval)
            .with_session_active_interval_ms(self.active_interval)
            .with_session_active_threshold_ms(self.active_threshold)
            .with_data_model_revision(self.data_model_revision)
            .with_interaction_model_revision(self.interaction_model_revision)
            .with_specification_version(self.specification_version)
            .with_max_paths_per_invoke(self.max_paths_per_invoke)
    }
}

#[derive(Debug, Deserialize)]
struct FixtureMessages {
    /// Hex-encoded Sigma1 bytes.
    sigma1: String,
    /// Hex-encoded Sigma2 bytes (new-session path and resumption-declined path).
    #[serde(default)]
    sigma2: Option<String>,
    /// Hex-encoded Sigma2_Resume bytes (resumption-accepted path).
    #[serde(default)]
    sigma2_resume: Option<String>,
    /// Hex-encoded Sigma3 bytes (new-session path and resumption-declined path).
    #[serde(default)]
    sigma3: Option<String>,
}

// =============================================================================
// Helpers
// =============================================================================

fn load_fixture(scenario: &str) -> Fixture {
    let path = PathBuf::from("../../test-vectors/case").join(format!("{scenario}.json"));
    let bytes = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "fixture {} not found — run `cargo xtask capture-case` to regenerate",
            path.display()
        )
    });
    serde_json::from_str(&bytes)
        .unwrap_or_else(|e| panic!("malformed fixture {}: {}", path.display(), e))
}

fn hex_to_array<const N: usize>(s: &str) -> [u8; N] {
    let bytes = hex::decode(s).expect("valid hex");
    assert_eq!(bytes.len(), N, "expected {N} bytes, got {}", bytes.len());
    bytes.try_into().unwrap()
}

fn build_credentials_from_fixture(
    noc_hex: &str,
    pkcs8_hex: &str,
    icac_hex: Option<&str>,
    fabric_id: u64,
    node_id: u64,
    ipk: [u8; 16],
    rcac_public_key: [u8; 65],
) -> CaseCredentials {
    let noc = MatterCertificate::from_tlv(&hex::decode(noc_hex).unwrap()).expect("parse NOC");
    let signer = RingSigner::from_pkcs8(&hex::decode(pkcs8_hex).unwrap()).expect("load signer");
    let icac = icac_hex
        .map(|h| MatterCertificate::from_tlv(&hex::decode(h).unwrap()).expect("parse ICAC"));
    CaseCredentials {
        noc,
        icac,
        signer: Box::new(signer),
        fabric_id,
        node_id,
        ipk,
        rcac_public_key,
    }
}

fn build_trusted_roots(rcac_noc_hex: &str) -> TrustedRoots {
    let rcac =
        MatterCertificate::from_tlv(&hex::decode(rcac_noc_hex).unwrap()).expect("parse RCAC");
    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));
    roots
}

// =============================================================================
// Diagnostic: verify fixture NOC roundtrips byte-for-byte through matter-cert.
// If this fails, MatterCertificate::to_tlv changes the encoding and we need
// to fix the Rust encoder to be byte-identical with matter.js output.
// =============================================================================

/// Verify the ECDSA signature that Rust produces over the known TBSData2 bytes.
///
/// This test computes TBSData2 bytes by hand (replicating `encode_tbs_data`
/// using the public `TlvWriter`), signs with `RingSigner`, and compares the
/// signature hex to the known-good value produced by the JS capture script.
///
/// This diagnostic isolates whether the signature mismatch is in:
///   (a) the TBSData2 bytes being different, or
///   (b) the signature algorithm / key format
#[test]
fn debug_tbs_data2_and_signature() {
    use matter_codec::{Tag, TlvWriter};
    use matter_crypto::CaseSigner;
    use p256::elliptic_curve::sec1::ToSec1Point;
    use p256::{NonZeroScalar, SecretKey};

    let fx = load_fixture("handshake-new-session");

    // Re-encode NOC (same as what the responder does)
    let noc_bytes = hex::decode(&fx.inputs.responder_noc).unwrap();
    let cert = MatterCertificate::from_tlv(&noc_bytes).expect("parse responder NOC");
    let re_encoded_noc = cert.to_tlv().expect("re-encode NOC");

    // Derive ephemeral public keys from the fixed scalars (same as the test)
    let resp_eph_priv_bytes = hex_to_array::<32>(&fx.inputs.responder_eph_priv);
    let init_eph_priv_bytes = hex_to_array::<32>(&fx.inputs.initiator_eph_priv);

    let resp_eph_scalar = NonZeroScalar::from_repr(resp_eph_priv_bytes.into()).unwrap();
    let resp_eph_sk = SecretKey::from(resp_eph_scalar);
    let resp_eph_pub_encoded = resp_eph_sk.public_key().to_sec1_point(false);
    let mut resp_eph_pub = [0u8; 65];
    resp_eph_pub.copy_from_slice(resp_eph_pub_encoded.as_bytes());

    let init_eph_scalar = NonZeroScalar::from_repr(init_eph_priv_bytes.into()).unwrap();
    let init_eph_sk = SecretKey::from(init_eph_scalar);
    let init_eph_pub_encoded = init_eph_sk.public_key().to_sec1_point(false);
    let mut init_eph_pub = [0u8; 65];
    init_eph_pub.copy_from_slice(init_eph_pub_encoded.as_bytes());

    // Re-encode ICAC when present (mirrors encode_tbs_data)
    let icac_re_encoded: Option<Vec<u8>> = fx.inputs.icac_noc.as_deref().map(|h| {
        let icac_bytes = hex::decode(h).unwrap();
        let icac_cert = MatterCertificate::from_tlv(&icac_bytes).expect("parse ICAC");
        icac_cert.to_tlv().expect("re-encode ICAC")
    });

    // Build TBSData2 manually using TlvWriter (mirrors encode_tbs_data)
    let mut tbs_data = Vec::new();
    {
        let mut w = TlvWriter::new(&mut tbs_data);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &re_encoded_noc).unwrap();
        if let Some(icac) = &icac_re_encoded {
            w.put_bytes(Tag::Context(2), icac).unwrap();
        }
        w.put_bytes(Tag::Context(3), &resp_eph_pub).unwrap();
        w.put_bytes(Tag::Context(4), &init_eph_pub).unwrap();
        w.end_container().unwrap();
    }

    eprintln!("TBSData2 (Rust): {}", hex::encode(&tbs_data));
    eprintln!("TBSData2 len (Rust): {}", tbs_data.len());

    // Sign with RingSigner
    let signer =
        matter_crypto::RingSigner::from_pkcs8(&hex::decode(&fx.inputs.responder_pkcs8).unwrap())
            .expect("load signer");
    let sig = signer.sign_p256_sha256(&tbs_data).expect("sign");
    eprintln!("Signature (Rust/ring): {}", hex::encode(sig));

    eprintln!(
        "signer public_key: {}",
        hex::encode(signer.public_key().as_bytes())
    );

    // The TBSData2 bytes and signature printed above can be compared against the
    // JS debug_tbs script output for diagnostic purposes. No hardcoded assertion
    // here — the matter_js_byte_parity_new_session test verifies end-to-end.
}

/// Verify the ECDSA signature that Rust produces over the known TBSData3 bytes.
///
/// Sigma3 TBSData3 is signed by the **initiator's** NOC key (the "responder" role in
/// TlvSignedData field names — see comment in initiator.rs). This test prints the
/// TBSData3 hex so it can be compared against JS output for diagnosis.
#[test]
fn debug_tbs_data3_and_signature() {
    use matter_codec::{Tag, TlvWriter};
    use matter_crypto::CaseSigner;
    use p256::elliptic_curve::sec1::ToSec1Point;
    use p256::{NonZeroScalar, SecretKey};

    let fx = load_fixture("handshake-new-session");

    // Re-encode initiator NOC (same as what the initiator does in process_sigma2)
    let noc_bytes = hex::decode(&fx.inputs.initiator_noc).unwrap();
    let cert = MatterCertificate::from_tlv(&noc_bytes).expect("parse initiator NOC");
    let re_encoded_noc = cert.to_tlv().expect("re-encode initiator NOC");
    eprintln!("initiator_noc raw:        {}", hex::encode(&noc_bytes));
    eprintln!("initiator_noc re-encoded: {}", hex::encode(&re_encoded_noc));
    eprintln!("roundtrip matches: {}", noc_bytes == re_encoded_noc);

    // Derive ephemeral public keys from the fixed scalars
    let resp_eph_priv_bytes = hex_to_array::<32>(&fx.inputs.responder_eph_priv);
    let init_eph_priv_bytes = hex_to_array::<32>(&fx.inputs.initiator_eph_priv);

    let resp_eph_scalar = NonZeroScalar::from_repr(resp_eph_priv_bytes.into()).unwrap();
    let resp_eph_sk = SecretKey::from(resp_eph_scalar);
    let resp_eph_pub_encoded = resp_eph_sk.public_key().to_sec1_point(false);
    let mut resp_eph_pub = [0u8; 65];
    resp_eph_pub.copy_from_slice(resp_eph_pub_encoded.as_bytes());

    let init_eph_scalar = NonZeroScalar::from_repr(init_eph_priv_bytes.into()).unwrap();
    let init_eph_sk = SecretKey::from(init_eph_scalar);
    let init_eph_pub_encoded = init_eph_sk.public_key().to_sec1_point(false);
    let mut init_eph_pub = [0u8; 65];
    init_eph_pub.copy_from_slice(init_eph_pub_encoded.as_bytes());

    // Re-encode ICAC when present
    let icac_re_encoded: Option<Vec<u8>> = fx.inputs.icac_noc.as_deref().map(|h| {
        let icac_bytes = hex::decode(h).unwrap();
        let icac_cert = MatterCertificate::from_tlv(&icac_bytes).expect("parse ICAC");
        icac_cert.to_tlv().expect("re-encode ICAC")
    });

    // Build TBSData3:
    // In Sigma3, initiator plays "responder" role in TlvSignedData:
    //   tag1 (responderNoc) = initiator's NOC
    //   tag2 (responderIcac) = initiator's ICAC (optional)
    //   tag3 (responderPublicKey) = initiator's eph pub
    //   tag4 (initiatorPublicKey) = responder's eph pub
    let mut tbs_data = Vec::new();
    {
        let mut w = TlvWriter::new(&mut tbs_data);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &re_encoded_noc).unwrap();
        if let Some(icac) = &icac_re_encoded {
            w.put_bytes(Tag::Context(2), icac).unwrap();
        }
        w.put_bytes(Tag::Context(3), &init_eph_pub).unwrap(); // initiator's eph pub
        w.put_bytes(Tag::Context(4), &resp_eph_pub).unwrap(); // responder's eph pub
        w.end_container().unwrap();
    }

    eprintln!("TBSData3 (Rust): {}", hex::encode(&tbs_data));
    eprintln!("TBSData3 len (Rust): {}", tbs_data.len());

    // Sign with initiator's NOC key
    let signer =
        matter_crypto::RingSigner::from_pkcs8(&hex::decode(&fx.inputs.initiator_pkcs8).unwrap())
            .expect("load initiator signer");
    let sig = signer.sign_p256_sha256(&tbs_data).expect("sign");
    eprintln!("Sigma3 Signature (Rust): {}", hex::encode(sig));
    eprintln!(
        "initiator signer public_key: {}",
        hex::encode(signer.public_key().as_bytes())
    );
}

/// Verify that the NOC bytes from the fixture survive a from_tlv + to_tlv roundtrip.
///
/// If this test fails, `MatterCertificate::to_tlv` is not byte-identical with
/// the matter.js-produced TLV and the Sigma2/Sigma3 signature bytes will differ
/// between JS and Rust (both sign the re-encoded NOC, but the re-encoded form differs).
#[test]
fn fixture_noc_roundtrips_through_matter_cert() {
    let fx = load_fixture("handshake-new-session");
    // Check both NOCs.
    for (label, noc_hex) in [
        ("responder_noc", &fx.inputs.responder_noc),
        ("initiator_noc", &fx.inputs.initiator_noc),
    ] {
        let noc_bytes = hex::decode(noc_hex).unwrap();
        let cert = MatterCertificate::from_tlv(&noc_bytes).expect("parse NOC");
        let re_encoded = cert.to_tlv().expect("re-encode NOC");
        assert_eq!(
            noc_bytes, re_encoded,
            "{label}: MatterCertificate::to_tlv must produce byte-identical output"
        );
    }
}

// =============================================================================
// Test scenarios
// =============================================================================

/// New-session CASE handshake: Sigma1 → Sigma2 → Sigma3.
///
/// Loads `test-vectors/case/handshake-new-session.json`, replays the full
/// three-message exchange through our state machines, and asserts byte-identical
/// output at every step. Also verifies that both sides derive identical session
/// keys.
///
/// Fixtures produced by `cargo xtask capture-case`.
#[test]
fn matter_js_byte_parity_new_session() {
    let fx = load_fixture("handshake-new-session");
    let ipk = hex_to_array::<16>(&fx.inputs.ipk);
    let rcac_pub = hex_to_array::<65>(&fx.inputs.rcac_public_key);
    let roots = build_trusted_roots(&fx.inputs.rcac_noc);

    let icac_hex = fx.inputs.icac_noc.as_deref();
    let initiator_creds = build_credentials_from_fixture(
        &fx.inputs.initiator_noc,
        &fx.inputs.initiator_pkcs8,
        icac_hex,
        fx.inputs.fabric_id,
        fx.inputs.initiator_node_id,
        ipk,
        rcac_pub,
    );
    let responder_creds = build_credentials_from_fixture(
        &fx.inputs.responder_noc,
        &fx.inputs.responder_pkcs8,
        icac_hex,
        fx.inputs.fabric_id,
        fx.inputs.responder_node_id,
        ipk,
        rcac_pub,
    );

    let mut initiator = case_initiator_with_eph_key(
        initiator_creds,
        roots.clone(),
        fx.inputs.responder_node_id,
        fx.inputs.fabric_id,
        hex_to_array::<32>(&fx.inputs.initiator_eph_priv),
        hex_to_array::<32>(&fx.inputs.initiator_random),
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();
    // Inject the capture script's fixed fresh resumption id so the encrypted
    // TBEData2 in our Sigma2 is byte-comparable with the fixture.
    let mut responder = case_responder_with_eph_key_and_resumption_id(
        responder_creds,
        roots,
        hex_to_array::<32>(&fx.inputs.responder_eph_priv),
        hex_to_array::<32>(&fx.inputs.responder_random),
        hex_to_array::<16>(
            fx.inputs
                .new_resumption_id
                .as_deref()
                .expect("fixture has new_resumption_id"),
        ),
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();

    // ── Sigma1 ────────────────────────────────────────────────────────────
    let our_sigma1 = initiator.start().unwrap();
    assert_eq!(
        hex::encode(&our_sigma1),
        fx.messages.sigma1,
        "Sigma1 byte parity"
    );

    // ── Sigma2 ────────────────────────────────────────────────────────────
    let outcome = responder.handle_sigma1(&our_sigma1).unwrap();
    assert!(
        matches!(outcome, Sigma1Outcome::NewSession),
        "expected NewSession outcome"
    );
    let our_sigma2 = responder.next_message().unwrap();
    assert_eq!(
        hex::encode(&our_sigma2),
        fx.messages.sigma2.as_deref().expect("fixture has sigma2"),
        "Sigma2 byte parity"
    );

    // ── Sigma3 ────────────────────────────────────────────────────────────
    initiator.handle_sigma2(&our_sigma2).unwrap();
    let our_sigma3 = initiator.next_message().unwrap();
    assert_eq!(
        hex::encode(&our_sigma3),
        fx.messages.sigma3.as_deref().expect("fixture has sigma3"),
        "Sigma3 byte parity"
    );

    responder.handle_sigma3(&our_sigma3).unwrap();

    // ── Session key agreement ─────────────────────────────────────────────
    let init_out = initiator.finish().unwrap();
    let resp_out = responder.finish().unwrap();
    assert_eq!(
        init_out.keys.i2r_key, resp_out.keys.i2r_key,
        "i2r session keys must agree"
    );
    assert_eq!(
        init_out.keys.r2i_key, resp_out.keys.r2i_key,
        "r2i session keys must agree"
    );
    assert_eq!(init_out.peer.node_id, fx.inputs.responder_node_id);
    assert_eq!(resp_out.peer.node_id, fx.inputs.initiator_node_id);
}

/// Resumption-accepted path: Sigma1 (with resumption fields) → Sigma2_Resume.
///
/// Loads `test-vectors/case/handshake-resumption-accepted.json`, replays the
/// two-message exchange, and asserts byte-identical output. The shared_secret
/// and resumption_id in the fixture are used to seed the `ResumptionRecord`.
///
/// Requires fixtures from `cargo xtask capture-case`.
///
/// The responder is built via
/// `case_responder_with_eph_key_and_resumption_id` so the fresh
/// `resumption_id` it puts in `Sigma2_Resume` matches the capture script's
/// patched-RNG value (all-zero bytes) instead of a live `SystemRandom` draw.
#[test]
#[allow(clippy::too_many_lines)] // long because it mirrors a multi-step protocol exchange
fn matter_js_byte_parity_resumption_accepted() {
    let fx = load_fixture("handshake-resumption-accepted");
    let ipk = hex_to_array::<16>(&fx.inputs.ipk);
    let rcac_pub = hex_to_array::<65>(&fx.inputs.rcac_public_key);
    let roots = build_trusted_roots(&fx.inputs.rcac_noc);

    let resumption_id = ResumptionId(hex_to_array::<16>(
        fx.inputs
            .resumption_id
            .as_deref()
            .expect("fixture has resumption_id"),
    ));
    let shared_secret = hex_to_array::<32>(
        fx.inputs
            .resumption_shared_secret
            .as_deref()
            .expect("fixture has resumption_shared_secret"),
    );

    let icac_hex = fx.inputs.icac_noc.as_deref();
    let initiator_creds = build_credentials_from_fixture(
        &fx.inputs.initiator_noc,
        &fx.inputs.initiator_pkcs8,
        icac_hex,
        fx.inputs.fabric_id,
        fx.inputs.initiator_node_id,
        ipk,
        rcac_pub,
    );
    let responder_creds = build_credentials_from_fixture(
        &fx.inputs.responder_noc,
        &fx.inputs.responder_pkcs8,
        icac_hex,
        fx.inputs.fabric_id,
        fx.inputs.responder_node_id,
        ipk,
        rcac_pub,
    );

    // Build a ResumptionRecord. The peer identity uses the responder's NOC
    // verbatim — on the resumption path this is what the record would have
    // stored from the original session.
    let responder_noc_for_peer =
        MatterCertificate::from_tlv(&hex::decode(&fx.inputs.responder_noc).unwrap()).unwrap();
    let record = ResumptionRecord {
        id: resumption_id,
        shared_secret,
        peer: PeerInfo {
            node_id: fx.inputs.responder_node_id,
            fabric_id: fx.inputs.fabric_id,
            noc: responder_noc_for_peer,
            session_id: 0,
        },
        expires_at: None,
    };

    let mut initiator = case_initiator_with_resumption_eph_key(
        initiator_creds,
        roots.clone(),
        fx.inputs.responder_node_id,
        fx.inputs.fabric_id,
        record.clone(),
        hex_to_array::<32>(&fx.inputs.initiator_eph_priv),
        hex_to_array::<32>(&fx.inputs.initiator_random),
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();
    // The capture script's patched RNG produced an all-zero fresh resumption
    // id (visible verbatim in the pinned sigma2_resume bytes) — inject it so
    // our responder's Sigma2_Resume is byte-comparable.
    let mut responder = case_responder_with_eph_key_and_resumption_id(
        responder_creds,
        roots,
        hex_to_array::<32>(&fx.inputs.responder_eph_priv),
        hex_to_array::<32>(&fx.inputs.responder_random),
        [0u8; 16],
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();

    // ── Sigma1 (with resumption fields) ──────────────────────────────────
    let our_sigma1 = initiator.start().unwrap();
    assert_eq!(
        hex::encode(&our_sigma1),
        fx.messages.sigma1,
        "Sigma1 (resumption) byte parity"
    );

    // ── Responder decides to accept resumption ────────────────────────────
    let outcome = responder.handle_sigma1(&our_sigma1).unwrap();
    let presented_id = match outcome {
        Sigma1Outcome::ResumptionRequested { id } => id,
        Sigma1Outcome::NewSession => panic!("expected ResumptionRequested"),
    };
    assert_eq!(
        presented_id, resumption_id,
        "presented resumption_id must match fixture"
    );

    responder.accept_resumption(record).unwrap();

    // ── Sigma2_Resume ─────────────────────────────────────────────────────
    let our_sigma2_resume = responder.next_message().unwrap();
    assert_eq!(
        hex::encode(&our_sigma2_resume),
        fx.messages
            .sigma2_resume
            .as_deref()
            .expect("fixture has sigma2_resume"),
        "Sigma2_Resume byte parity"
    );

    // ── Initiator processes Sigma2_Resume — no Sigma3 on resumption ───────
    initiator.handle_sigma2_resume(&our_sigma2_resume).unwrap();
    let init_out = initiator.finish().unwrap();
    let resp_out = responder.finish().unwrap();
    assert_eq!(
        init_out.keys.i2r_key, resp_out.keys.i2r_key,
        "resumed i2r keys must agree"
    );
    assert_eq!(
        init_out.keys.r2i_key, resp_out.keys.r2i_key,
        "resumed r2i keys must agree"
    );
}

/// Resumption-declined path: initiator presents a resumption ID the responder
/// doesn't recognise; responder falls back to the full new-session path.
///
/// Loads `test-vectors/case/handshake-resumption-declined.json` and replays
/// the full Sigma1 → Sigma2 → Sigma3 exchange that results when the responder
/// calls `reject_resumption()`.
///
/// Requires fixtures from `cargo xtask capture-case`.
///
/// (The old `#[ignore]` on this test blamed a `compute_sigma1_resume_mic`
/// composition mismatch — that was a misdiagnosis: the test fed a hardcoded
/// `[0xCC; 16]` record secret while the fixture MIC was computed from the
/// capture scenario's `resumption_shared_secret`. Feeding the fixture's
/// secret makes the Sigma1 — including its resume MIC — match byte-for-byte,
/// so the composition was correct all along.)
#[test]
#[allow(clippy::too_many_lines)] // long because it mirrors a multi-step protocol exchange
fn matter_js_byte_parity_resumption_declined() {
    let fx = load_fixture("handshake-resumption-declined");
    let ipk = hex_to_array::<16>(&fx.inputs.ipk);
    let rcac_pub = hex_to_array::<65>(&fx.inputs.rcac_public_key);
    let roots = build_trusted_roots(&fx.inputs.rcac_noc);

    let icac_hex = fx.inputs.icac_noc.as_deref();
    let initiator_creds = build_credentials_from_fixture(
        &fx.inputs.initiator_noc,
        &fx.inputs.initiator_pkcs8,
        icac_hex,
        fx.inputs.fabric_id,
        fx.inputs.initiator_node_id,
        ipk,
        rcac_pub,
    );
    let responder_creds = build_credentials_from_fixture(
        &fx.inputs.responder_noc,
        &fx.inputs.responder_pkcs8,
        icac_hex,
        fx.inputs.fabric_id,
        fx.inputs.responder_node_id,
        ipk,
        rcac_pub,
    );

    // Build the ResumptionRecord the initiator presents. The responder will
    // decline (the test drives `reject_resumption()`), but the record's
    // contents must match the fixture inputs exactly: the capture script
    // computed Sigma1's `initiator_resume_mic` from this id + secret, so the
    // Sigma1 byte-parity assertion depends on them.
    let bogus_id = ResumptionId(hex_to_array::<16>(
        fx.inputs
            .resumption_id
            .as_deref()
            .expect("bogus resumption_id in fixture"),
    ));
    let responder_noc_for_peer =
        MatterCertificate::from_tlv(&hex::decode(&fx.inputs.responder_noc).unwrap()).unwrap();
    let bogus_record = ResumptionRecord {
        id: bogus_id,
        shared_secret: hex_to_array::<32>(
            fx.inputs
                .resumption_shared_secret
                .as_deref()
                .expect("fixture has resumption_shared_secret"),
        ),
        peer: PeerInfo {
            node_id: fx.inputs.responder_node_id,
            fabric_id: fx.inputs.fabric_id,
            noc: responder_noc_for_peer,
            session_id: 0,
        },
        expires_at: None,
    };

    let mut initiator = case_initiator_with_resumption_eph_key(
        initiator_creds,
        roots.clone(),
        fx.inputs.responder_node_id,
        fx.inputs.fabric_id,
        bogus_record,
        hex_to_array::<32>(&fx.inputs.initiator_eph_priv),
        hex_to_array::<32>(&fx.inputs.initiator_random),
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();
    // Inject the capture script's fixed fresh resumption id so the encrypted
    // TBEData2 in our Sigma2 is byte-comparable with the fixture.
    let mut responder = case_responder_with_eph_key_and_resumption_id(
        responder_creds,
        roots,
        hex_to_array::<32>(&fx.inputs.responder_eph_priv),
        hex_to_array::<32>(&fx.inputs.responder_random),
        hex_to_array::<16>(
            fx.inputs
                .new_resumption_id
                .as_deref()
                .expect("fixture has new_resumption_id"),
        ),
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap();

    // ── Sigma1 (with resumption fields) ──────────────────────────────────
    let our_sigma1 = initiator.start().unwrap();
    assert_eq!(
        hex::encode(&our_sigma1),
        fx.messages.sigma1,
        "Sigma1 (declined resumption) byte parity"
    );

    // ── Responder declines — falls back to new-session Sigma2 ─────────────
    responder.handle_sigma1(&our_sigma1).unwrap();
    responder.reject_resumption().unwrap();

    // ── Sigma2 (new-session fallback) ─────────────────────────────────────
    let our_sigma2 = responder.next_message().unwrap();
    assert_eq!(
        hex::encode(&our_sigma2),
        fx.messages
            .sigma2
            .as_deref()
            .expect("fixture has sigma2 (fallback path)"),
        "Sigma2 (fallback) byte parity"
    );

    // ── Sigma3 ────────────────────────────────────────────────────────────
    initiator.handle_sigma2(&our_sigma2).unwrap();
    let our_sigma3 = initiator.next_message().unwrap();
    assert_eq!(
        hex::encode(&our_sigma3),
        fx.messages
            .sigma3
            .as_deref()
            .expect("fixture has sigma3 (fallback path)"),
        "Sigma3 (fallback) byte parity"
    );

    responder.handle_sigma3(&our_sigma3).unwrap();

    // ── Session key agreement ─────────────────────────────────────────────
    let init_out = initiator.finish().unwrap();
    let resp_out = responder.finish().unwrap();
    assert_eq!(
        init_out.keys.i2r_key, resp_out.keys.i2r_key,
        "i2r session keys must agree on declined-resumption path"
    );
    assert_eq!(
        init_out.keys.r2i_key, resp_out.keys.r2i_key,
        "r2i session keys must agree on declined-resumption path"
    );
}

/// Byte parity for the `SessionParameters` element we advertise in Sigma1
/// (context tag 5), against matter.js's own `TlvSessionParameters` encoder.
///
/// This is the check that matters for the wire change: the element is encoded
/// by matter.js in the fixture and by us in the assertion, from the same seven
/// values, so a wrong tag number, a wrong integer width, or a wrong field order
/// shows up as a byte difference rather than as a silently-ignored element on a
/// real device.
///
/// It also covers the whole Sigma1 around it — a correct sub-element spliced in
/// at the wrong place in the parent structure would still fail here.
#[test]
fn matter_js_byte_parity_sigma1_session_params() {
    let fx = load_fixture("handshake-with-session-params");
    let ipk = hex_to_array::<16>(&fx.inputs.ipk);
    let rcac_pub = hex_to_array::<65>(&fx.inputs.rcac_public_key);
    let roots = build_trusted_roots(&fx.inputs.rcac_noc);
    let params = fx
        .inputs
        .session_params
        .as_ref()
        .expect("fixture advertises session params")
        .to_params();

    let initiator_creds = build_credentials_from_fixture(
        &fx.inputs.initiator_noc,
        &fx.inputs.initiator_pkcs8,
        fx.inputs.icac_noc.as_deref(),
        fx.inputs.fabric_id,
        fx.inputs.initiator_node_id,
        ipk,
        rcac_pub,
    );

    let mut initiator = case_initiator_with_eph_key(
        initiator_creds,
        roots,
        fx.inputs.responder_node_id,
        fx.inputs.fabric_id,
        hex_to_array::<32>(&fx.inputs.initiator_eph_priv),
        hex_to_array::<32>(&fx.inputs.initiator_random),
        MatterTime::from_unix_secs(2_000_000_000),
    )
    .unwrap()
    .with_session_params(params);

    let our_sigma1 = initiator.start().unwrap();
    assert_eq!(
        hex::encode(&our_sigma1),
        fx.messages.sigma1,
        "Sigma1 with initiatorSessionParams must match matter.js byte for byte"
    );

    // And the standalone element encoder produces exactly the sub-element
    // matter.js embedded, so callers that build the struct themselves get the
    // same bytes.
    let element = params.encode(5).unwrap();
    assert!(
        our_sigma1
            .windows(element.len())
            .any(|w| w == element.as_slice()),
        "the encoded element must appear verbatim inside Sigma1"
    );
}

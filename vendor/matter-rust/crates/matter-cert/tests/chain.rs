//! Chain validation integration tests.
//!
//! Tier 1: positive end-to-end against the captured chain.
//! Tier 2: hand-written negative tests, one per failure mode (this file).
//! Tier 3: proptest properties.

#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

mod common;

use matter_cert::{
    BasicConstraints, CertificateChain, DistinguishedName, DnAttribute, Error, KeyIdentifier,
    KeyUsage, MatterCertificate, MatterTime, Signature, TrustAnchor, TrustedRoots,
};

// ------- helpers ---------------------------------------------------------

fn parse_cert(id: &str) -> MatterCertificate {
    let path = format!("../../test-vectors/certs/{id}.bin");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    MatterCertificate::from_tlv(&bytes).unwrap_or_else(|e| panic!("parse {id}: {e}"))
}

fn at_inside_window(cert: &MatterCertificate) -> MatterTime {
    let lo = cert.not_before().to_unix_secs();
    let hi = if cert.not_after() == MatterTime::NO_EXPIRY {
        lo + 365 * 86_400
    } else {
        cert.not_after().to_unix_secs()
    };
    MatterTime::from_unix_secs(u64::midpoint(lo, hi))
}

// ------- smoke test (gates the negative suite) ---------------------------

#[test]
fn synthesis_produces_a_self_consistent_chain() {
    let (chain, anchor) = common::synthesise_two_cert_chain(0xCAFE);
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].issuer(), chain[1].subject());
    assert_eq!(chain[1].issuer(), anchor.subject());
    chain[0].verify_signed_by(chain[1].public_key()).unwrap();
    chain[1].verify_signed_by(anchor.public_key()).unwrap();
}

// ------- Tier 2: one negative test per failure mode ----------------------

#[test]
fn empty_chain_returns_untrusted_root() {
    let roots = TrustedRoots::new();
    let chain = CertificateChain::new(&[]);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_700_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::UntrustedRoot));
}

#[test]
fn expired_leaf_returns_expired_at_index_0() {
    let rcac = parse_cert("rcac");
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));

    assert_ne!(
        noc.not_after(),
        MatterTime::NO_EXPIRY,
        "captured NOC must be bounded"
    );
    let past = MatterTime::from_unix_secs(noc.not_after().to_unix_secs() + 1);

    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain.validate(&roots, past).unwrap_err();
    assert!(matches!(err, Error::Expired { cert_index: 0, .. }));
}

#[test]
fn not_yet_valid_leaf_returns_at_index_0() {
    let rcac = parse_cert("rcac");
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));

    let before = MatterTime::from_unix_secs(noc.not_before().to_unix_secs().saturating_sub(1));

    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain.validate(&roots, before).unwrap_err();
    assert!(matches!(err, Error::NotYetValid { cert_index: 0, .. }));
}

#[test]
fn unknown_root_returns_untrusted_root() {
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");
    let roots = TrustedRoots::new();

    let leaf_for_window = noc.clone();
    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, at_inside_window(&leaf_for_window))
        .unwrap_err();
    assert!(matches!(err, Error::UntrustedRoot));
}

#[test]
fn wrong_root_key_returns_untrusted_root() {
    let rcac = parse_cert("rcac");
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");

    let rng = ring::rand::SystemRandom::new();
    let wrong_key = common::TestKey::generate(&rng).public_key;
    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(
        rcac.subject().clone(),
        wrong_key,
        rcac.extensions().subject_key_identifier,
    ));

    let leaf_for_window = noc.clone();
    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, at_inside_window(&leaf_for_window))
        .unwrap_err();
    assert!(matches!(err, Error::UntrustedRoot));
}

#[test]
fn anchor_without_ski_falls_back_to_dn_match() {
    let rcac = parse_cert("rcac");
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(
        rcac.subject().clone(),
        rcac.public_key().clone(),
        None,
    ));

    let leaf_for_window = noc.clone();
    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    chain
        .validate(&roots, at_inside_window(&leaf_for_window))
        .expect("DN-only match must accept the chain");
}

#[test]
fn anchor_with_mismatching_ski_does_not_match() {
    let rcac = parse_cert("rcac");
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");

    let wrong_ski = KeyIdentifier([0x00; 20]);
    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(
        rcac.subject().clone(),
        rcac.public_key().clone(),
        Some(wrong_ski),
    ));

    let leaf_for_window = noc.clone();
    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, at_inside_window(&leaf_for_window))
        .unwrap_err();
    assert!(matches!(err, Error::UntrustedRoot));
}

#[test]
fn non_ca_intermediate_returns_not_a_ca_at_index_1() {
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    // ICA with is_ca = false (this is what we're testing).
    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    ica_fields.extensions.basic_constraints = Some(BasicConstraints::new(false, None));
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::NotACa { cert_index: 1 }));
}

#[test]
fn ca_without_key_cert_sign_returns_missing_key_cert_sign_at_index_1() {
    // ICA with is_ca = true but NO keyCertSign bit (key_usage = None).
    // RFC 5280 §4.2.1.3 / Matter §6.5.5: a signing CA must carry keyCertSign.
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    // is_ca stays true (from ca_template); strip the keyCertSign bit.
    ica_fields.extensions.key_usage = None;
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::MissingKeyCertSign { cert_index: 1 }));
}

#[test]
fn ca_with_key_usage_but_no_key_cert_sign_is_rejected() {
    // ICA with is_ca = true and a KeyUsage extension present, but the
    // keyCertSign bit is absent (only CRL_SIGN set). Still rejected.
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    ica_fields.extensions.key_usage = Some(KeyUsage::CRL_SIGN);
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::MissingKeyCertSign { cert_index: 1 }));
}

#[test]
fn ca_with_key_cert_sign_is_accepted() {
    // Normal valid path: ICA with is_ca = true AND keyCertSign present.
    // ca_template already sets KeyUsage::KEY_CERT_SIGN.
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    assert!(ica_fields
        .extensions
        .key_usage
        .is_some_and(|ku| ku.contains(KeyUsage::KEY_CERT_SIGN)));
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .expect("CA with keyCertSign must validate");
}

#[test]
fn leaf_asserting_is_ca_true_is_rejected() {
    // End-entity leaf at index 0 with basic_constraints.is_ca = true.
    // RFC 5280 forbids this; must be rejected with Error::LeafIsCa.
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    // Force the leaf to (illegally) assert the CA bit.
    leaf_fields.extensions.basic_constraints = Some(BasicConstraints::new(true, None));
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::LeafIsCa));
}

#[test]
fn leaf_without_basic_constraints_is_accepted() {
    // A leaf with NO basic_constraints extension is permitted (absent is
    // not a violation; only an explicit is_ca=true is).
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    leaf_fields.extensions.basic_constraints = None;
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .expect("leaf without basic_constraints must validate");
}

#[test]
fn wrong_issuer_dn_returns_mismatch_at_index_0() {
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let ica_dn = common::ca_dn("test-ica");

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    let ica = common::build_signed_cert(ica_fields, &root_key);

    // Leaf with WRONG issuer (something other than ica_dn).
    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = DistinguishedName::new(vec![DnAttribute::CommonName(
        "definitely-not-the-ica".into(),
    )]);
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(
        err,
        Error::IssuerSubjectMismatch { cert_index: 0 }
    ));
}

#[test]
fn tampered_intermediate_signature_returns_untrusted_root() {
    let (mut chain_certs, anchor) = common::synthesise_two_cert_chain(0xCAFE);

    // Replace the ICA's signature with zeros — the anchor signature
    // verification will fail, causing the anchor loop to exhaust without
    // finding a valid match, which returns UntrustedRoot.
    let bad_ica =
        matter_cert::test_support::with_signature(&chain_certs[1], Signature::new([0u8; 64]));
    chain_certs[1] = bad_ica;

    let mut roots = TrustedRoots::new();
    roots.add(anchor);

    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::UntrustedRoot));
}

#[test]
fn path_len_zero_at_index_2_rejects_two_intermediates_below() {
    // 3-cert chain [leaf, ica, root_intermediate] where root_intermediate
    // has path_len = 0. validate should reject because one intermediate
    // (ica at i=1) follows root_intermediate.
    let rng = ring::rand::SystemRandom::new();
    let root_key = common::TestKey::generate(&rng);
    let root_intermediate_key = common::TestKey::generate(&rng);
    let ica_key = common::TestKey::generate(&rng);
    let leaf_key = common::TestKey::generate(&rng);

    let root_dn = common::ca_dn("test-root");
    let root_intermediate_dn = common::ca_dn("test-root-intermediate");
    let ica_dn = common::ca_dn("test-ica");

    let mut ri_fields = common::ca_template(root_intermediate_key.public_key.clone(), Some(0));
    ri_fields.subject = root_intermediate_dn.clone();
    ri_fields.issuer = root_dn.clone();
    let root_intermediate = common::build_signed_cert(ri_fields, &root_key);

    let mut ica_fields = common::ca_template(ica_key.public_key.clone(), None);
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_intermediate_dn;
    let ica = common::build_signed_cert(ica_fields, &root_intermediate_key);

    let mut leaf_fields = common::leaf_template(leaf_key.public_key.clone());
    leaf_fields.issuer = ica_dn;
    let leaf = common::build_signed_cert(leaf_fields, &ica_key);

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_raw(root_dn, root_key.public_key, None));

    let chain_certs = vec![leaf, ica, root_intermediate];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain
        .validate(&roots, MatterTime::from_unix_secs(1_750_000_000))
        .unwrap_err();
    assert!(matches!(err, Error::PathLengthExceeded { cert_index: 2 }));
}

// ------- Tier 1: captured chain end-to-end -------------------------------

#[test]
fn captured_chain_validates_against_rcac_as_trust_anchor() {
    let rcac = parse_cert("rcac");
    let icac = parse_cert("icac");
    let noc = parse_cert("noc");

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));

    let leaf_for_window = noc.clone();
    let chain_certs = vec![noc, icac];
    let chain = CertificateChain::new(&chain_certs);
    let at = at_inside_window(&leaf_for_window);
    chain
        .validate(&roots, at)
        .expect("captured chain must validate against rcac as the trust anchor");
}

// ------- Tier 3: proptest properties -------------------------------------

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64, // sign-heavy; lower than default 256 for CI duration
        ..ProptestConfig::default()
    })]

    #[test]
    fn synthetic_valid_chain_always_validates(seed in any::<u64>()) {
        let (chain_certs, anchor) = common::synthesise_two_cert_chain(seed);
        let at = at_inside_window(&chain_certs[0]);

        let mut roots = TrustedRoots::new();
        roots.add(anchor);

        prop_assert!(CertificateChain::new(&chain_certs).validate(&roots, at).is_ok());
    }

    #[test]
    fn random_byte_flip_never_panics(
        seed in any::<u64>(),
        flip_offset in any::<usize>(),
        flip_bit in 0u8..8,
    ) {
        let (chain_certs, anchor) = common::synthesise_two_cert_chain(seed);
        let leaf_bytes = chain_certs[0].to_tlv().unwrap();
        let mut mutated = leaf_bytes.clone();
        if !mutated.is_empty() {
            let off = flip_offset % mutated.len();
            mutated[off] ^= 1 << flip_bit;
        }

        // Parse may fail (codec error); if it succeeds, validate must
        // not panic — only return Ok or Err.
        if let Ok(leaf) = MatterCertificate::from_tlv(&mutated) {
            let mut roots = TrustedRoots::new();
            roots.add(anchor);
            let new_chain = vec![leaf, chain_certs[1].clone()];
            let _ = CertificateChain::new(&new_chain)
                .validate(&roots, MatterTime::from_unix_secs(1_750_000_000));
        }
    }
}

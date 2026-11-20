//! Build → sign → parse round-trip for `matter_cert::operational` +
//! [`matter_cert::operational::sign_with_ring`].
//!
//! Generates a fresh P-256 keypair with `ring`, builds a self-signed RCAC
//! via [`matter_cert::operational::rcac`], signs it with
//! `sign_with_ring`, round-trips the assembled certificate through TLV,
//! and confirms the self-signature verifies against a `TrustAnchor` built
//! from the same RCAC.
//!
//! Signature verification here uses `MatterCertificate::verify_signed_by`
//! directly rather than `CertificateChain::validate` on a 1-element chain:
//! `validate` treats chain index 0 as the end-entity leaf and correctly
//! rejects any cert asserting `basic_constraints.is_ca = true` there (RFC
//! 5280 §4.2.1.9 forbids an end-entity leaf from asserting the CA bit —
//! see `Error::LeafIsCa` in `matter_cert::chain`). Since an RCAC always
//! asserts `is_ca = true` (spec §6.5.5), a bare self-signed root can never
//! validate as chain index 0 — that check is intentional, not a bug, and
//! is exactly why a lone root cert is verified directly instead.
//! `verify_signed_by` is the same primitive `CertificateChain::validate`
//! calls internally for the anchor-signature check, so this still exercises
//! the real signature-verification path; `TrustAnchor` is still built here
//! because this test demonstrates the anchor-construction step.

#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use matter_cert::operational::{
    icac, noc, rcac, sign_with_ring, IcacParams, NocParams, RcacParams,
};
use matter_cert::{
    CertificateChain, Error, MatterCertificate, MatterTime, PublicKey, TrustAnchor, TrustedRoots,
};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

/// Generate a fresh P-256 keypair, returning (PKCS#8 DER bytes, the
/// matching `matter_cert::PublicKey`).
fn generate_keypair() -> (Vec<u8>, PublicKey) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let key_pair =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let mut pk_bytes = [0u8; 65];
    pk_bytes.copy_from_slice(key_pair.public_key().as_ref());
    let public_key = PublicKey::new(pk_bytes).unwrap();
    (pkcs8.as_ref().to_vec(), public_key)
}

#[test]
fn rcac_build_sign_parse_round_trips_and_self_signature_verifies() {
    let (pkcs8, public_key) = generate_keypair();

    let unsigned = rcac(RcacParams::new(
        1,
        public_key,
        vec![0x01],
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        Some(1),
    ))
    .unwrap();

    let signed = sign_with_ring(unsigned, &pkcs8).unwrap();

    // Round-trip through TLV.
    let tlv = signed.to_tlv().unwrap();
    let parsed = MatterCertificate::from_tlv(&tlv).unwrap();
    assert_eq!(parsed.subject().rcac_id(), Some(1));
    assert_eq!(parsed, signed);

    // Confirm the RCAC's not_before/not_after window covers our chosen
    // validation instant, matching the "at a time inside the validity
    // window" requirement.
    let at = MatterTime::from_unix_secs(1_700_000_000);
    assert!(parsed.not_before() <= at);

    // Build a TrustAnchor from the RCAC itself, then verify the
    // self-signature against it.
    let anchor = TrustAnchor::from_root_cert(&parsed);
    parsed.verify_signed_by(anchor.public_key()).unwrap();
}

/// Build a full 3-tier operational-PKI chain: a self-signed RCAC, an ICAC
/// signed by the RCAC, and a NOC signed by the ICAC. Each tier gets its own
/// fresh P-256 keypair.
///
/// Returns `(rcac, icac, noc)`, all signed and TLV-round-trip-safe
/// [`MatterCertificate`]s.
// `rcac_*`/`icac_*` bindings are intentionally parallel-named (same role,
// different tier) — clippy's `similar_names` pedantic lint flags that
// on-purpose symmetry as a typo risk; it isn't one here.
#[allow(clippy::similar_names)]
fn build_rcac_icac_noc_chain() -> (MatterCertificate, MatterCertificate, MatterCertificate) {
    let not_before = MatterTime::from_unix_secs(1_700_000_000);
    let not_after = MatterTime::NO_EXPIRY;

    // --- RCAC: self-signed root ---
    let (rcac_pkcs8, rcac_pub) = generate_keypair();
    let unsigned_rcac = rcac(RcacParams::new(
        1,
        rcac_pub,
        vec![0x01],
        not_before,
        not_after,
        Some(1),
    ))
    .unwrap();
    let rcac_cert = sign_with_ring(unsigned_rcac, &rcac_pkcs8).unwrap();

    // Read the RCAC's own subject DN / SKID back off the signed cert rather
    // than reconstructing them: simpler and less error-prone, and it can
    // never drift from what `rcac()` actually produced.
    let rcac_subject = rcac_cert.subject().clone();
    let rcac_skid = rcac_cert.extensions().subject_key_identifier.unwrap();

    // --- ICAC: signed by the RCAC ---
    let (icac_pkcs8, icac_pub) = generate_keypair();
    let unsigned_icac = icac(IcacParams::new(
        2,
        rcac_subject,
        rcac_skid,
        icac_pub,
        vec![0x02],
        not_before,
        not_after,
    ))
    .unwrap();
    let icac_cert = sign_with_ring(unsigned_icac, &rcac_pkcs8).unwrap();

    let icac_subject = icac_cert.subject().clone();
    let icac_skid = icac_cert.extensions().subject_key_identifier.unwrap();

    // --- NOC: signed by the ICAC ---
    let (_noc_pkcs8, noc_pub) = generate_keypair();
    let unsigned_noc = noc(NocParams::new(
        7,
        42,
        vec![],
        icac_subject,
        icac_skid,
        noc_pub,
        vec![0x03],
        not_before,
        not_after,
    ))
    .unwrap();
    let noc_cert = sign_with_ring(unsigned_noc, &icac_pkcs8).unwrap();

    (rcac_cert, icac_cert, noc_cert)
}

#[test]
fn rcac_icac_noc_chain_validates() {
    let (rcac_cert, icac_cert, noc_cert) = build_rcac_icac_noc_chain();

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac_cert));

    let at = MatterTime::from_unix_secs(1_700_000_000);

    // Leaf-to-topmost-intermediate order: [NOC, ICAC]. The RCAC itself is
    // supplied separately via `TrustedRoots`, not as a chain element.
    let chain_certs = vec![noc_cert, icac_cert];
    let chain = CertificateChain::new(&chain_certs);
    chain
        .validate(&roots, at)
        .expect("RCAC->ICAC->NOC chain must validate against the RCAC trust anchor");
}

#[test]
fn noc_without_icac_fails() {
    let (rcac_cert, _icac_cert, noc_cert) = build_rcac_icac_noc_chain();

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac_cert));

    let at = MatterTime::from_unix_secs(1_700_000_000);

    // The NOC is signed by (and issued by) the ICAC, not the RCAC — so
    // presenting it as a direct leaf under the RCAC, with no intermediate,
    // must fail: its issuer DN doesn't match the RCAC's subject DN, so the
    // anchor loop never finds a match and the chain is rejected.
    let chain_certs = vec![noc_cert];
    let chain = CertificateChain::new(&chain_certs);
    let err = chain.validate(&roots, at).unwrap_err();
    assert!(
        matches!(err, Error::UntrustedRoot),
        "expected UntrustedRoot, got: {err:?}"
    );
}

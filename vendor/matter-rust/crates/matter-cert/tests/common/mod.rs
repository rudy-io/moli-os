//! Chain synthesis helpers for the chain-validation test suite.
//!
//! Each helper builds a Matter certificate (or a chain of them) using
//! `ring` for key generation + signing and matter-cert's `test_support`
//! module for cert construction. The result is a parsed, signed
//! `MatterCertificate` whose X.509 signature actually verifies against
//! its issuer's public key — exactly what the chain-validation tests
//! need to exercise the real `validate` path.

#![allow(dead_code)] // Not all helpers are consumed by every test file.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use matter_cert::test_support::{build_unsigned, with_signature, TestCertFields};
use matter_cert::{
    BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyIdentifier, KeyUsage,
    MatterCertificate, MatterTime, PublicKey, Signature, TrustAnchor,
};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

/// A keypair plus the corresponding `matter_cert::PublicKey`.
pub(crate) struct TestKey {
    pub(crate) keypair: EcdsaKeyPair,
    pub(crate) public_key: PublicKey,
}

impl TestKey {
    /// Generate a fresh P-256 keypair using ring's system RNG.
    pub(crate) fn generate(rng: &SystemRandom) -> Self {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, rng).unwrap();
        let keypair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), rng)
                .unwrap();
        let mut pk_bytes = [0u8; 65];
        pk_bytes.copy_from_slice(keypair.public_key().as_ref());
        let public_key = PublicKey::new(pk_bytes).unwrap();
        Self {
            keypair,
            public_key,
        }
    }
}

/// Build a cert with the given fields, signed by `signer`.
///
/// Implementation:
/// 1. Build the cert with the placeholder signature supplied in `fields`
///    (the caller should set `fields.signature = Signature::new([0u8; 64])`).
/// 2. Compute its X.509 TBS via `to_x509_tbs_der`.
/// 3. Sign the TBS with `signer` using ECDSA-P256-SHA256-FIXED.
/// 4. Replace the placeholder signature on the cert via `with_signature`.
///
/// The returned cert's `verify_signed_by(&signer.public_key)` returns `Ok`.
pub(crate) fn build_signed_cert(mut fields: TestCertFields, signer: &TestKey) -> MatterCertificate {
    // Ensure a known placeholder signature so the TBS is deterministic.
    fields.signature = Signature::new([0u8; 64]);
    let placeholder = build_unsigned(fields);

    let tbs = placeholder
        .to_x509_tbs_der()
        .expect("synthesised cert must produce a valid X.509 TBS");

    let rng = SystemRandom::new();
    let sig = signer
        .keypair
        .sign(&rng, &tbs)
        .expect("ring sign must succeed");

    // ring's FIXED (r || s) encoding is exactly 64 bytes for P-256.
    assert_eq!(
        sig.as_ref().len(),
        64,
        "ECDSA-P256-FIXED signature must be 64 bytes"
    );
    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(sig.as_ref());

    with_signature(&placeholder, Signature::new(sig_bytes))
}

/// Default DN with a single `CommonName` attribute.
pub(crate) fn ca_dn(common_name: &str) -> DistinguishedName {
    DistinguishedName::new(vec![DnAttribute::CommonName(common_name.into())])
}

/// Build a leaf-cert template (NOC-shaped).
///
/// The caller must overwrite `issuer` and `subject` before signing.
pub(crate) fn leaf_template(public_key: PublicKey) -> TestCertFields {
    TestCertFields {
        serial: vec![0x01],
        issuer: DistinguishedName::new(vec![]),
        subject: ca_dn("test-leaf"),
        not_before: MatterTime::from_unix_secs(1_700_000_000),
        not_after: MatterTime::from_unix_secs(1_800_000_000),
        public_key,
        extensions: Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(false, None)))
            .key_usage(Some(KeyUsage::DIGITAL_SIGNATURE))
            .subject_key_identifier(Some(KeyIdentifier([0x01; 20])))
            .authority_key_identifier(Some(KeyIdentifier([0x02; 20])))
            .build(),
        signature: Signature::new([0u8; 64]),
    }
}

/// Build a CA-cert template (ICA/RCA-shaped).
///
/// The caller must overwrite `issuer` and `subject` before signing.
pub(crate) fn ca_template(public_key: PublicKey, path_len: Option<u8>) -> TestCertFields {
    TestCertFields {
        extensions: Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(true, path_len)))
            .key_usage(Some(KeyUsage::KEY_CERT_SIGN))
            .subject_key_identifier(Some(KeyIdentifier([0x02; 20])))
            .authority_key_identifier(Some(KeyIdentifier([0x03; 20])))
            .build(),
        subject: ca_dn("test-ca"),
        ..leaf_template(public_key)
    }
}

/// Build a 2-cert chain `[leaf, ica]` signed by a freshly-generated root.
///
/// Returns `(chain, root_anchor)`.  The chain validates against a
/// `TrustedRoots` containing only `root_anchor` at any `at` in
/// `[1_700_000_000, 1_800_000_000]`.
///
/// The `_seed` parameter is reserved for future deterministic synthesis if
/// proptest shrinking needs it; ring's CSPRNG is non-deterministic by design.
pub(crate) fn synthesise_two_cert_chain(_seed: u64) -> (Vec<MatterCertificate>, TrustAnchor) {
    let rng = SystemRandom::new();

    let root_key = TestKey::generate(&rng);
    let ica_key = TestKey::generate(&rng);
    let leaf_key = TestKey::generate(&rng);

    let root_dn = ca_dn("test-root");
    let ica_dn = ca_dn("test-ica");
    let leaf_dn = ca_dn("test-leaf");

    // ICA: signed by root.
    let mut ica_fields = ca_template(ica_key.public_key.clone(), Some(1));
    ica_fields.subject = ica_dn.clone();
    ica_fields.issuer = root_dn.clone();
    ica_fields.extensions.subject_key_identifier = Some(KeyIdentifier([0x02; 20]));
    ica_fields.extensions.authority_key_identifier = Some(KeyIdentifier([0x03; 20]));
    let ica = build_signed_cert(ica_fields, &root_key);

    // Leaf: signed by ICA.
    let mut leaf_fields = leaf_template(leaf_key.public_key.clone());
    leaf_fields.subject = leaf_dn;
    leaf_fields.issuer = ica_dn;
    leaf_fields.extensions.subject_key_identifier = Some(KeyIdentifier([0x01; 20]));
    leaf_fields.extensions.authority_key_identifier = Some(KeyIdentifier([0x02; 20]));
    let leaf = build_signed_cert(leaf_fields, &ica_key);

    // Root anchor: same DN and public key as the root that signed the ICA.
    let anchor = TrustAnchor::from_raw(
        root_dn,
        root_key.public_key.clone(),
        Some(KeyIdentifier([0x03; 20])),
    );

    (vec![leaf, ica], anchor)
}

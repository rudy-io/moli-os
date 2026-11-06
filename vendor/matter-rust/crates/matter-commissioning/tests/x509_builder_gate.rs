//! Authoritative webpki gate for `matter_cert::test_support::build_x509_der`.
//!
//! This is the riskiest correctness gate of M6.6.4: the X.509 DER
//! certificates emitted by `build_x509_der` MUST satisfy strict
//! `rustls-webpki` path validation, exactly as `attestation::chain::
//! verify_chain` runs it during real commissioning. We build a synthetic
//! PAA -> PAI -> DAC chain entirely in Rust, parse each cert back with
//! `x509-parser` to confirm the VID/PID RDNs round-trip, then run the REAL
//! `verify_chain` and assert `Ok`.
//!
//! If this test fails, the fix belongs in the cert BUILDER
//! (DER/extensions/EKU/validity), never in `verify_chain` — weakening the
//! verifier would defeat the entire point of attestation.
//!
//! Extension recipe (mirrors `scripts/gen-negative-fixtures.py`, the
//! known-good C++/Python reference):
//! - PAA: self-signed; BasicConstraints CA:true, pathLen:1 (critical);
//!   KeyUsage keyCertSign+cRLSign (critical).
//! - PAI: issued by PAA; BasicConstraints CA:true, pathLen:0 (critical);
//!   KeyUsage keyCertSign+cRLSign (critical); subject VID 0xFFF1.
//! - DAC: issued by PAI; BasicConstraints CA:false (critical); KeyUsage
//!   digitalSignature (critical); ExtendedKeyUsage id-kp-clientAuth
//!   (non-critical); subject VID 0xFFF1 + PID 0x8001.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
// `paa_pkcs8`/`paa_pk` (and pai_/dac_ peers) are intentionally paired
// names: the PKCS#8 private key and its public-key half. The similarity
// is meaningful, not accidental.
#![allow(clippy::similar_names)]
// Doc identifiers like PAA/PAI/DAC/VID/PID/EKU are domain acronyms, not
// code items; backticking every occurrence in prose hurts readability.
#![allow(clippy::doc_markdown)]

use matter_cert::test_support::{build_x509_der, TestCertFields};
use matter_cert::{
    BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyUsage, MatterTime, PublicKey,
    Signature,
};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

use matter_commissioning::attestation::{
    verify_chain, Dac, Paa, PaaTrustStore, Pai, ProductId, VendorId,
};

const VID: u16 = 0xFFF1;
const PID: u16 = 0x8001;

/// The anchor time the chain's validity windows straddle (matches the
/// Python fixture script's `AT_UNIX`). 2027-01-15T08:00:00Z.
const AT_UNIX: u64 = 1_800_000_000;

/// EKU TLV compact integer for id-kp-clientAuth (see matter-cert's x509
/// encoder: 2 -> 1.3.6.1.5.5.7.3.2).
const EKU_CLIENT_AUTH: u32 = 2;

/// Generate a P-256 keypair as (PKCS#8 DER, our `PublicKey`).
fn gen_key() -> (Vec<u8>, PublicKey) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let pkcs8 = pkcs8.as_ref().to_vec();
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng).unwrap();
    let pk = PublicKey::from_slice(kp.public_key().as_ref()).unwrap();
    (pkcs8, pk)
}

fn at_offset(days: i64) -> MatterTime {
    // AT_UNIX is mid-2027 and offsets are at most a few thousand days, so
    // the arithmetic never goes negative or overflows; the casts are safe
    // for these fixed test inputs.
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    let secs = (AT_UNIX as i64 + days * 86_400) as u64;
    MatterTime::from_unix_secs(secs)
}

fn placeholder_sig() -> Signature {
    Signature::new([0u8; 64])
}

fn main_chain(common_name_fallback: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // --- PAA: self-signed root ---
    let (paa_pkcs8, paa_pk) = gen_key();
    let paa_dn = DistinguishedName::new(if common_name_fallback {
        vec![DnAttribute::CommonName("Synthetic PAA Mvid:FFF1".into())]
    } else {
        vec![
            DnAttribute::CommonName("Matter Test PAA (synthetic)".into()),
            DnAttribute::VendorId(VID),
        ]
    });
    let paa_der = build_x509_der(
        TestCertFields {
            serial: vec![0x01],
            issuer: paa_dn.clone(),
            not_before: at_offset(-365),
            not_after: at_offset(365 * 10),
            subject: paa_dn.clone(),
            public_key: paa_pk,
            extensions: Extensions::builder()
                .basic_constraints(Some(BasicConstraints::new(true, Some(1))))
                .key_usage(Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN))
                .build(),
            signature: placeholder_sig(),
        },
        &paa_pkcs8, // self-signed
    )
    .expect("PAA builds");

    // --- PAI: issued by PAA, VID-scoped ---
    let (pai_pkcs8, pai_pk) = gen_key();
    let pai_dn = DistinguishedName::new(if common_name_fallback {
        vec![DnAttribute::CommonName("Synthetic PAI Mvid:FFF1".into())]
    } else {
        vec![
            DnAttribute::CommonName("Matter Test PAI (synthetic)".into()),
            DnAttribute::VendorId(VID),
        ]
    });
    let pai_der = build_x509_der(
        TestCertFields {
            serial: vec![0x02],
            issuer: paa_dn, // == PAA subject, byte-for-byte
            not_before: at_offset(-180),
            not_after: at_offset(365 * 5),
            subject: pai_dn.clone(),
            public_key: pai_pk,
            extensions: Extensions::builder()
                .basic_constraints(Some(BasicConstraints::new(true, Some(0))))
                .key_usage(Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN))
                .build(),
            signature: placeholder_sig(),
        },
        &paa_pkcs8, // signed by PAA
    )
    .expect("PAI builds");

    // --- DAC: leaf, issued by PAI, VID+PID, clientAuth EKU ---
    let (_dac_pkcs8, dac_pk) = gen_key();
    let dac_dn = DistinguishedName::new(if common_name_fallback {
        vec![DnAttribute::CommonName(
            "Synthetic DAC Mvid:FFF1 Mpid:8001".into(),
        )]
    } else {
        vec![
            DnAttribute::CommonName("Matter Test DAC (synthetic)".into()),
            DnAttribute::VendorId(VID),
            DnAttribute::ProductId(PID),
        ]
    });
    let dac_der = build_x509_der(
        TestCertFields {
            serial: vec![0x03],
            issuer: pai_dn, // == PAI subject, byte-for-byte
            not_before: at_offset(-30),
            not_after: at_offset(365),
            subject: dac_dn,
            public_key: dac_pk,
            extensions: Extensions::builder()
                .basic_constraints(Some(BasicConstraints::new(false, None)))
                .key_usage(Some(KeyUsage::DIGITAL_SIGNATURE))
                .extended_key_usage(Some(vec![EKU_CLIENT_AUTH]))
                .build(),
            signature: placeholder_sig(),
        },
        &pai_pkcs8, // signed by PAI
    )
    .expect("DAC builds");

    (paa_der, pai_der, dac_der)
}

#[test]
fn built_chain_round_trips_vid_pid_via_x509_parser() {
    let (paa_der, pai_der, dac_der) = main_chain(false);

    // Parsing through the production Dac/Pai/Paa wrappers exercises
    // x509-parser's extract_vid/extract_pid, which require exactly 4
    // UPPERCASE hex chars under the Matter OIDs. If our PrintableString
    // encoding were wrong, these would error.
    let paa = Paa::from_der(&paa_der).expect("PAA parses (and VID/PID well-formed)");
    let pai = Pai::from_der(&pai_der).expect("PAI parses");
    let dac = Dac::from_der(&dac_der).expect("DAC parses");

    assert_eq!(paa.subject_vid(), Some(VendorId::new(VID)));
    assert_eq!(pai.subject_vid(), VendorId::new(VID));
    assert_eq!(dac.subject_vid(), VendorId::new(VID));
    assert_eq!(dac.subject_pid(), ProductId::new(PID));
}

#[test]
fn built_chain_passes_real_verify_chain() {
    let (paa_der, pai_der, dac_der) = main_chain(false);

    let dac = Dac::from_der(&dac_der).unwrap();
    let pai = Pai::from_der(&pai_der).unwrap();

    let mut store = PaaTrustStore::empty();
    store.add(Paa::from_der(&paa_der).unwrap());

    let at = MatterTime::from_unix_secs(AT_UNIX);

    // THE GATE: the real verifier, unmodified, must accept our chain.
    let result = verify_chain(&dac, &pai, &store, at)
        .expect("webpki MUST accept the synthetic chain — fix the builder, not the verifier");

    assert_eq!(result.vendor_id, VendorId::new(VID));
    assert_eq!(result.product_id, ProductId::new(PID));
    assert_eq!(result.dac_public_key.len(), 65);
    assert_ne!(result.paa_subject, b"");
}

#[test]
fn common_name_fallback_chain_passes_real_verifier_and_still_requires_trust_and_signature() {
    let (paa_der, pai_der, mut dac_der) = main_chain(true);
    let paa = Paa::from_der(&paa_der).expect("fallback PAA");
    let pai = Pai::from_der(&pai_der).expect("fallback PAI");
    let dac = Dac::from_der(&dac_der).expect("fallback DAC");
    assert_eq!(paa.subject_vid(), Some(VendorId::new(VID)));
    assert_eq!(pai.subject_vid(), VendorId::new(VID));
    assert_eq!(pai.subject_pid(), None);
    assert_eq!(dac.subject_vid(), VendorId::new(VID));
    assert_eq!(dac.subject_pid(), ProductId::new(PID));

    let at = MatterTime::from_unix_secs(AT_UNIX);
    assert!(verify_chain(&dac, &pai, &PaaTrustStore::empty(), at).is_err());
    let mut store = PaaTrustStore::empty();
    store.add(paa);
    let result = verify_chain(&dac, &pai, &store, at).expect("trusted fallback chain");
    assert_eq!(result.vendor_id, VendorId::new(VID));
    assert_eq!(result.product_id, ProductId::new(PID));

    // Change the signature only: the subject remains parseable but untrusted.
    *dac_der.last_mut().expect("nonempty certificate") ^= 1;
    let tampered = Dac::from_der(&dac_der).expect("subject still parses");
    assert!(verify_chain(&tampered, &pai, &store, at).is_err());
}

#[test]
fn common_name_fallback_preserves_required_and_forbidden_fields() {
    use der::{Decode as _, Encode as _};
    use std::str::FromStr as _;
    use x509_cert::{name::Name, Certificate};

    // These altered certificates exercise structural parsing only; changing
    // a subject intentionally invalidates its signature.
    fn with_subject(der: &[u8], subject: &str) -> Vec<u8> {
        let mut cert = Certificate::from_der(der).expect("fixture certificate");
        cert.tbs_certificate.subject = Name::from_str(subject).expect("fixture subject");
        cert.to_der().expect("fixture DER")
    }

    let (paa_der, pai_der, dac_der) = main_chain(true);
    assert!(Dac::from_der(&with_subject(&dac_der, "CN=Mvid:FFF1")).is_err());
    assert!(Dac::from_der(&with_subject(&dac_der, "CN=Mpid:8001")).is_err());
    assert!(Pai::from_der(&with_subject(&pai_der, "CN=Mpid:8001")).is_err());
    assert!(Pai::from_der(&with_subject(&pai_der, "CN=Mvid:FFF1 Mpid:bad")).is_err());
    assert!(Paa::from_der(&with_subject(&paa_der, "CN=Mvid:FFF1 Mpid:8001")).is_err());
}

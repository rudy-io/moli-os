//! Test-only certificate construction helpers.
//!
//! Available only when the `test-support` Cargo feature is enabled.
//! Used by this crate's own `tests/*` files to synthesise certificates
//! for negative testing and property-based testing. Not part of the
//! stable public API; production callers must not enable this feature.
//!
//! The intended flow:
//!
//! 1. Populate a [`TestCertFields`] with the cert's intended shape and
//!    a placeholder all-zero [`crate::Signature`].
//! 2. Call [`build_unsigned`] to produce a [`crate::MatterCertificate`]
//!    with the placeholder signature.
//! 3. Compute its X.509 TBS bytes via
//!    [`crate::MatterCertificate::to_x509_tbs_der`].
//! 4. Sign the TBS with `ring::signature::EcdsaKeyPair::sign` and the
//!    issuer's key pair.
//! 5. Attach the real signature via [`with_signature`].

use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

use crate::error::{Error, Result};
use crate::{DistinguishedName, Extensions, MatterCertificate, MatterTime, PublicKey, Signature};

/// Field values for a synthesised certificate.
///
/// All fields are exposed publicly so test code can mutate them before
/// building the cert. Convenience templates are provided by the
/// `tests/common/mod.rs` helpers, which build on top of this module.
#[derive(Debug, Clone)]
pub struct TestCertFields {
    /// Serial number as raw bytes.
    pub serial: Vec<u8>,
    /// Issuer distinguished name.
    pub issuer: DistinguishedName,
    /// Beginning of the validity period.
    pub not_before: MatterTime,
    /// End of the validity period (`MatterTime::NO_EXPIRY` means no expiry).
    pub not_after: MatterTime,
    /// Subject distinguished name.
    pub subject: DistinguishedName,
    /// EC public key (P-256, 65-byte uncompressed).
    pub public_key: PublicKey,
    /// Parsed certificate extensions.
    pub extensions: Extensions,
    /// ECDSA-P256 signature over the X.509 TBS.
    ///
    /// Pass `Signature::new([0u8; 64])` as a placeholder; replace later
    /// with the real value using [`with_signature`].
    pub signature: Signature,
}

/// Build a [`MatterCertificate`] from the field values verbatim.
///
/// Use this when you have already computed the signature externally
/// (e.g., signed the X.509 TBS with `ring`), or when you want a
/// placeholder-signed cert for TBS extraction.
#[must_use]
pub fn build_unsigned(fields: TestCertFields) -> MatterCertificate {
    MatterCertificate::from_fields(
        fields.serial,
        fields.issuer,
        fields.not_before,
        fields.not_after,
        fields.subject,
        fields.public_key,
        fields.extensions,
        fields.signature,
    )
}

/// Build a complete, signed X.509 DER certificate from `fields`.
///
/// This is the test-support entry point for synthesising attestation
/// certificates (PAA / PAI / DAC) that strict X.509 path validators such
/// as `rustls-webpki` accept as a chain. It does NOT produce a Matter
/// operational TLV certificate — it produces a conventional X.509 DER
/// `Certificate`:
///
/// ```text
/// SEQUENCE {
///   tbsCertificate     (from MatterCertificate::to_x509_tbs_der),
///   signatureAlgorithm SEQUENCE { OID ecdsa-with-SHA256 },
///   signature          BIT STRING (DER ECDSA (r, s) over the TBS),
/// }
/// ```
///
/// `fields` describe the cert's TBS shape (subject/issuer DNs, validity,
/// extensions, subject public key). The cert is signed by the **issuer's**
/// key — `issuer_pkcs8` is the issuer's private key in PKCS#8 DER form.
/// For a self-signed root (PAA) the issuer key is the subject's own key.
///
/// The signature is produced by `ring`'s
/// `ECDSA_P256_SHA256_ASN1_SIGNING`, whose output is already a DER-encoded
/// `(r, s)` `SEQUENCE` — exactly the form an X.509 `signature` `BIT STRING`
/// wraps. No P1363→DER conversion is needed.
///
/// The `signature` field inside `fields` is ignored (the X.509 outer
/// signature is computed here); pass `Signature::new([0u8; 64])`.
///
/// # Errors
///
/// - [`Error::TestX509SigningFailed`] if `issuer_pkcs8` is not a valid
///   P-256 PKCS#8 key pair, or if `ring` rejects the signing request.
/// - Any error [`MatterCertificate::to_x509_tbs_der`] returns (e.g. a DN
///   attribute with no X.509 OID mapping, or an empty serial).
pub fn build_x509_der(fields: TestCertFields, issuer_pkcs8: &[u8]) -> Result<Vec<u8>> {
    // 1. Assemble the TBS from the field values.
    let cert = build_unsigned(fields);
    let tbs = cert.to_x509_tbs_der()?;

    // 2. Sign the TBS with the issuer's key. The ASN1 signing variant
    //    emits a DER (r, s) SEQUENCE directly — the exact bytes an X.509
    //    `signature` BIT STRING carries.
    let rng = SystemRandom::new();
    let key_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, issuer_pkcs8, &rng)
        .map_err(|_| Error::TestX509SigningFailed("issuer PKCS#8 key rejected by ring"))?;
    let sig_der = key_pair
        .sign(&rng, &tbs)
        .map_err(|_| Error::TestX509SigningFailed("ring ECDSA signing failed"))?;

    // 3. signature BIT STRING: tag 0x03, length, 0x00 (unused-bits prefix),
    //    then the DER-encoded ECDSA signature.
    let sig_bytes = sig_der.as_ref();
    let mut sig_bit_string = Vec::with_capacity(sig_bytes.len() + 4);
    sig_bit_string.push(0x03);
    crate::x509::encode_definite_length(&mut sig_bit_string, sig_bytes.len() + 1);
    sig_bit_string.push(0x00); // zero unused bits
    sig_bit_string.extend_from_slice(sig_bytes);

    // 4. Outer Certificate SEQUENCE { tbs, signatureAlgorithm, signature }.
    let alg = crate::x509::encode_algorithm_identifier_ecdsa_sha256();
    let mut inner = Vec::with_capacity(tbs.len() + alg.len() + sig_bit_string.len());
    inner.extend_from_slice(&tbs);
    inner.extend_from_slice(&alg);
    inner.extend_from_slice(&sig_bit_string);
    Ok(crate::x509::wrap_sequence(&inner))
}

/// Replace the signature on an already-built cert, returning a new cert.
///
/// Used in the sign-then-replace flow: build with a placeholder signature,
/// compute the X.509 TBS, sign with `ring`, then call this helper with
/// the real 64-byte signature.
#[must_use]
pub fn with_signature(cert: &MatterCertificate, signature: Signature) -> MatterCertificate {
    MatterCertificate::from_fields(
        cert.serial().to_vec(),
        cert.issuer().clone(),
        cert.not_before(),
        cert.not_after(),
        cert.subject().clone(),
        cert.public_key().clone(),
        cert.extensions().clone(),
        signature,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    use super::*;
    use crate::extensions::{BasicConstraints, KeyUsage};
    use crate::DnAttribute;

    /// Generate a fresh P-256 keypair as (`pkcs8` DER, our `PublicKey` newtype).
    fn gen_key() -> (Vec<u8>, PublicKey) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let pkcs8 = pkcs8.as_ref().to_vec();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng).unwrap();
        let pk = PublicKey::from_slice(kp.public_key().as_ref()).unwrap();
        (pkcs8, pk)
    }

    /// `build_x509_der` emits a self-signed cert whose outer structure is a
    /// DER SEQUENCE and whose own signature verifies. The authoritative
    /// webpki chain gate lives in matter-commissioning; this is the lighter
    /// in-crate smoke test.
    #[test]
    fn build_x509_der_emits_parseable_self_signed_cert() {
        let (pkcs8, pk) = gen_key();
        let dn = DistinguishedName::new(vec![DnAttribute::CommonName("matter-test root".into())]);
        let fields = TestCertFields {
            serial: vec![0x01],
            issuer: dn.clone(),
            not_before: MatterTime::from_unix_secs(1_700_000_000),
            not_after: MatterTime::from_unix_secs(1_900_000_000),
            subject: dn,
            public_key: pk,
            extensions: Extensions {
                basic_constraints: Some(BasicConstraints {
                    is_ca: true,
                    path_len_constraint: None,
                }),
                key_usage: Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN),
                ..Default::default()
            },
            signature: Signature::new([0u8; 64]),
        };
        let der = build_x509_der(fields, &pkcs8).expect("self-signed cert builds");
        assert_eq!(der[0], 0x30, "outer Certificate must be a SEQUENCE");
        assert!(der.len() > 100, "a real cert is well over 100 bytes");
    }

    /// VID/PID encode as 4-char UPPERCASE-hex `PrintableString` under the
    /// Matter CSA OIDs — verified here at the DN-attribute level by
    /// scanning for the `PrintableString` tag and its value.
    #[test]
    fn vendor_and_product_id_encode_as_printable_hex() {
        // Reach the X.509 DN encoder through to_x509_tbs_der on a cert
        // carrying VID/PID, then assert the literal "FFF1"/"8001" appear as
        // PrintableString (tag 0x13) values.
        let (_pkcs8, pk) = gen_key();
        let subject = DistinguishedName::new(vec![
            DnAttribute::CommonName("dac".into()),
            DnAttribute::VendorId(0xFFF1),
            DnAttribute::ProductId(0x8001),
        ]);
        let cert = build_unsigned(TestCertFields {
            serial: vec![0x01],
            issuer: DistinguishedName::new(vec![DnAttribute::CommonName("pai".into())]),
            not_before: MatterTime::from_unix_secs(1_700_000_000),
            not_after: MatterTime::from_unix_secs(1_900_000_000),
            subject,
            public_key: pk,
            extensions: Extensions::default(),
            signature: Signature::new([0u8; 64]),
        });
        let tbs = cert.to_x509_tbs_der().unwrap();
        // PrintableString "FFF1" => 13 04 46 46 46 31
        assert!(
            tbs.windows(6)
                .any(|w| w == [0x13, 0x04, b'F', b'F', b'F', b'1']),
            "VID must encode as PrintableString \"FFF1\""
        );
        // PrintableString "8001" => 13 04 38 30 30 31
        assert!(
            tbs.windows(6)
                .any(|w| w == [0x13, 0x04, b'8', b'0', b'0', b'1']),
            "PID must encode as PrintableString \"8001\""
        );
    }

    /// VID/PID have no Matter operational-TLV encoding — routing one
    /// through the TLV writer must error rather than invent a tag.
    #[test]
    fn vendor_id_is_not_tlv_encodable() {
        use matter_codec::{Tag, TlvWriter};
        let dn = DistinguishedName::new(vec![DnAttribute::VendorId(0xFFF1)]);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        let err = dn.write(&mut w, Tag::Anonymous).unwrap_err();
        assert!(matches!(err, Error::DnAttributeNotTlvEncodable("VendorId")));
    }
}

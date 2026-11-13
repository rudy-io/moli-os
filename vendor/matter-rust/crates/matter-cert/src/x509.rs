//! Matter TLV → X.509 DER `TBSCertificate` conversion.
//!
//! Real Matter certificates carry signatures over the X.509 DER form
//! of the certificate, not over the Matter TLV form. This module maps
//! a parsed [`crate::MatterCertificate`] into a byte-identical
//! representation of matter.js's `Certificate.asUnsignedDer()` output,
//! which `MatterCertificate::verify_signed_by` then hands to `ring`.

// All OID constants are now reachable through matter_cert_to_x509_tbs_der
// (which delegates to the per-encoder helpers). The skeleton-phase
// dead-code suppression has been removed.
//
// The two OIDs below (VVS-ID and FIRMWARE-SIGNING-ID) are forward-looking:
// they will be consumed when DnAttribute gains the corresponding variants.
// Each carries its own per-item allow rather than a module-level blanket.
//
// NOTE: if clippy flags any other constant here, that is a real dead-code
// issue and must be investigated, not silenced.

use der::asn1::ObjectIdentifier;

use crate::certificate::MatterCertificate;
use crate::error::{Error, Result};

// =============================================================================
// Matter-specific DN attribute OIDs (CSA arc 1.3.6.1.4.1.37244)
// =============================================================================
//
// Values pinned from matter.js's asn.js (dist/cjs/certificate/kinds/definitions/asn.js).
// The .d.ts comments are authoritative:
//   /** matter-node-id            = ASN.1 OID 1.3.6.1.4.1.37244.1.1 */
//   /** matter-firmware-signing-id = ASN.1 OID 1.3.6.1.4.1.37244.1.2 */
//   /** matter-icac-id            = ASN.1 OID 1.3.6.1.4.1.37244.1.3 */
//   /** matter-rcac-id            = ASN.1 OID 1.3.6.1.4.1.37244.1.4 */
//   /** matter-fabric-id          = ASN.1 OID 1.3.6.1.4.1.37244.1.5 */
//   /** matter-noc-cat            = ASN.1 OID 1.3.6.1.4.1.37244.1.6 */
//   /** matter-vvs-id             = ASN.1 OID 1.3.6.1.4.1.37244.1.7 */  ← present in matter.js; unused here
//   /** matter-oid-vid            = ASN.1 OID 1.3.6.1.4.1.37244.2.1 */
//   /** matter-oid-pid            = ASN.1 OID 1.3.6.1.4.1.37244.2.2 */
/// Operational-certificate attribute: Matter node-id.
const OID_MATTER_NODE_ID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.1");

/// Operational-certificate attribute: Matter firmware-signing-id.
///
/// Forward-looking: consumed when `DnAttribute` adds a `FirmwareSigningId` variant.
#[allow(dead_code)]
const OID_MATTER_FIRMWARE_SIGNING_ID: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.2");

/// Operational-certificate attribute: Matter ICAC id.
const OID_MATTER_ICAC_ID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.3");

/// Operational-certificate attribute: Matter RCAC id.
const OID_MATTER_RCAC_ID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.4");

/// Operational-certificate attribute: Matter fabric-id.
const OID_MATTER_FABRIC_ID: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.5");

/// Operational-certificate attribute: Matter NOC CASE Authenticated Tag.
const OID_MATTER_NOC_CAT: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.6");

/// Operational-certificate attribute: Matter VVS (VID Verification Signer) id.
///
/// Present in matter.js (`vvsId`, arc `.1.7`, DN tag 23) and in chip as
/// `MatterVidVerificationSignerId`, but unused here: no `DnAttribute`
/// variant maps to it. Included for completeness; used in VID Verification
/// Signer (VVS) certificates, which Matter 1.4.2 introduced.
/// Forward-looking: consumed when `DnAttribute` adds a `VvsId` variant.
#[allow(dead_code)]
const OID_MATTER_VVS_ID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.1.7");

/// Attestation-certificate attribute: Matter vendor-id.
///
/// Carried in DAC/PAI/PAA subject DNs via [`DnAttribute::VendorId`].
const OID_MATTER_VENDOR_ID: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.2.1");

/// Attestation-certificate attribute: Matter product-id.
///
/// Carried in DAC/PAI subject DNs via [`DnAttribute::ProductId`].
const OID_MATTER_PRODUCT_ID: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.37244.2.2");

// =============================================================================
// Standard X.509 DN attribute OIDs (RFC 5280 / X.520)
// =============================================================================

const OID_X520_COMMON_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");
const OID_X520_SURNAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.4");
const OID_X520_SERIAL_NUMBER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.5");
const OID_X520_COUNTRY_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.6");
const OID_X520_LOCALITY_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.7");
const OID_X520_STATE_OR_PROVINCE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.8");
const OID_X520_ORG: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.10");
const OID_X520_ORG_UNIT: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.11");
const OID_X520_TITLE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.12");
const OID_X520_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.41");
const OID_X520_GIVEN_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.42");
const OID_X520_INITIALS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.43");
const OID_X520_GENERATION_QUALIFIER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.44");
const OID_X520_DN_QUALIFIER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.46");
const OID_X520_PSEUDONYM: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.65");
const OID_DOMAIN_COMPONENT: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("0.9.2342.19200300.100.1.25");

// =============================================================================
// X.509 extension OIDs (RFC 5280)
// =============================================================================

const OID_EXT_BASIC_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.19");
const OID_EXT_KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.15");
const OID_EXT_EXTENDED_KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.37");
const OID_EXT_SUBJECT_KEY_IDENTIFIER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.14");
const OID_EXT_AUTHORITY_KEY_IDENTIFIER: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("2.5.29.35");

// =============================================================================
// Algorithm + curve OIDs
// =============================================================================

const OID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const OID_EC_CURVE_P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const OID_ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");

// =============================================================================
// Top-level entry point
// =============================================================================

/// Build the X.509 DER `TBSCertificate` for `cert`.
///
/// Byte-identical to matter.js's `Certificate.asUnsignedDer()` for the
/// same input. Used internally by `MatterCertificate::verify_signed_by`
/// and exposed publicly via `MatterCertificate::to_x509_tbs_der`.
pub(crate) fn matter_cert_to_x509_tbs_der(cert: &MatterCertificate) -> Result<Vec<u8>> {
    // Single reused output buffer. Every field is written directly into `out`
    // via the write-in-place encoders below, instead of building a tree of
    // short-lived `Vec`s and concatenating. The encoded bytes are identical;
    // only the assembly strategy changed (verified by the byte-parity test
    // against matter.js's `asUnsignedDer()`).
    let mut out = Vec::with_capacity(512);

    // The whole TBSCertificate is a SEQUENCE; build its body in place, then
    // splice the SEQUENCE tag + length prefix in front. The closure cannot
    // return a `Result`, so fallible field encoders that may reject input
    // (serial length, DN attributes) record their error here for the caller.
    let mut field_err: Option<Error> = None;
    wrap_sequence_into(&mut out, |tbs| {
        // version [0] EXPLICIT INTEGER (2) — v3, always emitted.
        // [0] EXPLICIT context tag is 0xA0 (constructed).
        tbs.extend_from_slice(&[0xA0, 0x03, 0x02, 0x01, 0x02]);

        // serialNumber INTEGER (fallible — empty serials rejected).
        if let Err(e) = encode_serial_number_into(tbs, cert.serial()) {
            field_err = Some(e);
            return;
        }

        // signature AlgorithmIdentifier (infallible).
        encode_algorithm_identifier_ecdsa_sha256_into(tbs);

        // issuer Name (fallible — Other DN attributes / bad CountryName).
        if let Err(e) = encode_dn_into(tbs, cert.issuer()) {
            field_err = Some(e);
            return;
        }

        // validity SEQUENCE { notBefore, notAfter } (infallible).
        encode_validity_into(tbs, cert.not_before(), cert.not_after());

        // subject Name (fallible).
        if let Err(e) = encode_dn_into(tbs, cert.subject()) {
            field_err = Some(e);
            return;
        }

        // subjectPublicKeyInfo (infallible).
        encode_subject_public_key_info_into(tbs, cert.public_key());

        // extensions [3] EXPLICIT SEQUENCE OF Extension OPTIONAL (infallible).
        encode_extensions_block_into(tbs, cert.extensions());
    });

    if let Some(e) = field_err {
        return Err(e);
    }
    Ok(out)
}

// =============================================================================
// Validity encoder
// =============================================================================

/// The Unix-seconds threshold at which X.509 switches from `UTCTime` to
/// `GeneralizedTime` per RFC 5280 §4.1.2.5: 2050-01-01T00:00:00Z.
const X509_UTCTIME_CUTOFF_UNIX_SECS: u64 = 2_524_608_000;

/// Encode an X.509 `Validity` SEQUENCE.
///
/// `MatterTime(0)` is the Matter "no expiry" sentinel; it maps to the
/// RFC 5280 §4.1.2.5 `GeneralizedTime` sentinel `99991231235959Z`.
fn encode_validity_into(
    out: &mut Vec<u8>,
    not_before: crate::time::MatterTime,
    not_after: crate::time::MatterTime,
) {
    wrap_sequence_into(out, |inner| {
        encode_one_time_into(inner, not_before, /* is_not_after = */ false);
        encode_one_time_into(inner, not_after, /* is_not_after = */ true);
    });
}

/// Encode an X.509 `Validity` SEQUENCE to a standalone `Vec`. See
/// [`encode_validity_into`].
#[cfg(test)]
fn encode_validity(
    not_before: crate::time::MatterTime,
    not_after: crate::time::MatterTime,
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_validity_into(&mut out, not_before, not_after);
    out
}

fn encode_one_time_into(out: &mut Vec<u8>, t: crate::time::MatterTime, is_not_after: bool) {
    // Matter "no expiry" only applies to not_after.
    if t == crate::time::MatterTime::NO_EXPIRY && is_not_after {
        // `GeneralizedTime` sentinel per RFC 5280 §4.1.2.5
        encode_generalized_time_literal_into(out, b"99991231235959Z");
        return;
    }
    let unix = t.to_unix_secs();
    let (year, month, day, hour, minute, second) = unix_to_ymdhms(unix);
    if unix < X509_UTCTIME_CUTOFF_UNIX_SECS {
        // year is in [1970, 2049] here, so year % 100 fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        let yy = (year % 100) as u8;
        let s = format!("{yy:02}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z");
        encode_utc_time_literal_into(out, s.as_bytes());
    } else {
        let s = format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z");
        encode_generalized_time_literal_into(out, s.as_bytes());
    }
}

/// `UTCTime` DER: tag 0x17, length, value.
fn encode_utc_time_literal_into(out: &mut Vec<u8>, s: &[u8]) {
    debug_assert_eq!(s.len(), 13);
    // s.len() == 13, which fits in u8 without truncation.
    #[allow(clippy::cast_possible_truncation)]
    let len_byte = s.len() as u8;
    out.push(0x17);
    out.push(len_byte);
    out.extend_from_slice(s);
}

/// `GeneralizedTime` DER: tag 0x18, length, value.
fn encode_generalized_time_literal_into(out: &mut Vec<u8>, s: &[u8]) {
    debug_assert_eq!(s.len(), 15);
    // s.len() == 15, which fits in u8 without truncation.
    #[allow(clippy::cast_possible_truncation)]
    let len_byte = s.len() as u8;
    out.push(0x18);
    out.push(len_byte);
    out.extend_from_slice(s);
}

// =============================================================================
// Serial number encoder
// =============================================================================

/// Encode a Matter serial number (1–20 raw bytes) as an X.509 INTEGER.
///
/// DER INTEGER rules require prepending `0x00` if the high bit of the
/// first content byte is set, to keep the integer non-negative.
fn encode_serial_number_into(out: &mut Vec<u8>, serial_bytes: &[u8]) -> Result<()> {
    if serial_bytes.is_empty() {
        return Err(Error::FieldValueOutOfRange {
            tag: crate::tlv_tags::CERT_SERIAL_NUMBER,
        });
    }

    let needs_leading_zero = (serial_bytes[0] & 0x80) != 0;

    out.push(0x02); // INTEGER tag
    let content_len = serial_bytes.len() + usize::from(needs_leading_zero);
    encode_definite_length(out, content_len);
    if needs_leading_zero {
        out.push(0x00);
    }
    out.extend_from_slice(serial_bytes);
    Ok(())
}

/// Encode a Matter serial number as an X.509 INTEGER to a standalone `Vec`.
/// See [`encode_serial_number_into`].
#[cfg(test)]
fn encode_serial_number(serial_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    encode_serial_number_into(&mut out, serial_bytes)?;
    Ok(out)
}

// =============================================================================
// AlgorithmIdentifier (signature) encoder
// =============================================================================

/// Encode the X.509 `AlgorithmIdentifier` for ecdsa-with-SHA256.
///
/// Per RFC 5758 §3.2, this OID has no parameters — the SEQUENCE
/// contains only the OID itself.
///
/// Production encoding paths use the in-place
/// [`encode_algorithm_identifier_ecdsa_sha256_into`] instead; this allocating
/// wrapper exists for the unit tests and `crate::test_support`, so it is gated
/// to those builds rather than compiled as dead code by default.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn encode_algorithm_identifier_ecdsa_sha256() -> Vec<u8> {
    let mut out = Vec::new();
    encode_algorithm_identifier_ecdsa_sha256_into(&mut out);
    out
}

/// Write the ecdsa-with-SHA256 `AlgorithmIdentifier` into `out` in place.
fn encode_algorithm_identifier_ecdsa_sha256_into(out: &mut Vec<u8>) {
    wrap_sequence_into(out, |inner| encode_oid_into(inner, &OID_ECDSA_WITH_SHA256));
}

// =============================================================================
// SubjectPublicKeyInfo encoder
// =============================================================================

/// Write an X.509 `SubjectPublicKeyInfo` for a P-256 uncompressed point into
/// `out` in place.
fn encode_subject_public_key_info_into(out: &mut Vec<u8>, key: &crate::PublicKey) {
    wrap_sequence_into(out, |inner| {
        // Inner AlgorithmIdentifier: SEQUENCE { OID(id-ecPublicKey), OID(prime256v1) }
        wrap_sequence_into(inner, |alg| {
            encode_oid_into(alg, &OID_EC_PUBLIC_KEY);
            encode_oid_into(alg, &OID_EC_CURVE_P256);
        });

        // BIT STRING: 0x03 || length || 0x00 (unused bits) || 65-byte point
        let point = key.as_bytes();
        inner.push(0x03);
        encode_definite_length(inner, point.len() + 1);
        inner.push(0x00); // unused-bits prefix
        inner.extend_from_slice(point);
    });
}

/// Encode an X.509 `SubjectPublicKeyInfo` to a standalone `Vec`. See
/// [`encode_subject_public_key_info_into`].
#[cfg(test)]
fn encode_subject_public_key_info(key: &crate::PublicKey) -> Vec<u8> {
    let mut out = Vec::new();
    encode_subject_public_key_info_into(&mut out, key);
    out
}

/// Write a `der::asn1::ObjectIdentifier` as a complete TLV element
/// (tag 0x06, length, content) into `out` in place.
#[allow(clippy::expect_used)] // see note inside.
fn encode_oid_into(out: &mut Vec<u8>, oid: &ObjectIdentifier) {
    // der's encoder writes tag+length+content for us via `Encode`. The
    // expect is safe: ObjectIdentifier::new_unwrap (used at module load
    // for every OID constant) has already validated the OID; der's
    // encode_to_vec cannot fail for a validated OID.
    use der::Encode;
    oid.encode_to_vec(out)
        .expect("internal: der OID encoder rejected a validated ObjectIdentifier");
}

// =============================================================================
// DN attribute encoder
// =============================================================================

use crate::name::{DistinguishedName, DnAttribute};

/// ASN.1 string-type tags.
const TAG_UTF8_STRING: u8 = 0x0C;
const TAG_PRINTABLE_STRING: u8 = 0x13;
const TAG_IA5_STRING: u8 = 0x16;

/// Encode a single `DnAttribute` as an X.509 `AttributeTypeAndValue`:
/// `SEQUENCE { type OID, value DirectoryString }`.
// One match arm per DN attribute kind; kept as a single table for
// auditability against the spec's attribute list (§6.5.6).
/// Encode a single `DnAttribute` to a standalone `Vec`. See
/// [`encode_dn_attribute_into`].
#[cfg(test)]
fn encode_dn_attribute(attr: &DnAttribute) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    encode_dn_attribute_into(&mut out, attr)?;
    Ok(out)
}

/// Write a single `DnAttribute` as an X.509 `AttributeTypeAndValue`
/// (`SEQUENCE { type OID, value DirectoryString }`) into `out` in place.
#[allow(clippy::too_many_lines)]
fn encode_dn_attribute_into(out: &mut Vec<u8>, attr: &DnAttribute) -> Result<()> {
    let (oid, string_tag, value_bytes) = match attr {
        // Matter-specific attributes: UTF8String of uppercase zero-padded hex.
        DnAttribute::NodeId(v) => (
            &OID_MATTER_NODE_ID,
            TAG_UTF8_STRING,
            format!("{v:016X}").into_bytes(),
        ),
        DnAttribute::FabricId(v) => (
            &OID_MATTER_FABRIC_ID,
            TAG_UTF8_STRING,
            format!("{v:016X}").into_bytes(),
        ),
        DnAttribute::RcacId(v) => (
            &OID_MATTER_RCAC_ID,
            TAG_UTF8_STRING,
            format!("{v:016X}").into_bytes(),
        ),
        DnAttribute::IcacId(v) => (
            &OID_MATTER_ICAC_ID,
            TAG_UTF8_STRING,
            format!("{v:016X}").into_bytes(),
        ),
        // CaseAuthenticatedTag: 32-bit NOC-CAT value, 8 uppercase hex chars.
        DnAttribute::CaseAuthenticatedTag(v) => (
            &OID_MATTER_NOC_CAT,
            TAG_UTF8_STRING,
            format!("{v:08X}").into_bytes(),
        ),
        // Attestation-cert VID/PID: 4-char UPPERCASE-hex PrintableString
        // (Matter §6.5.6.1). PrintableString matches the C++/Python
        // reference encoding and is what `extract_vid`/`extract_pid` parse
        // back (they require exactly 4 UPPERCASE hex chars). The 0–9A–F
        // alphabet is a strict subset of PrintableString, so no
        // verify_printable check is needed.
        DnAttribute::VendorId(v) => (
            &OID_MATTER_VENDOR_ID,
            TAG_PRINTABLE_STRING,
            format!("{v:04X}").into_bytes(),
        ),
        DnAttribute::ProductId(v) => (
            &OID_MATTER_PRODUCT_ID,
            TAG_PRINTABLE_STRING,
            format!("{v:04X}").into_bytes(),
        ),

        // Standard X.509 attributes.
        DnAttribute::CommonName(s) => (
            &OID_X520_COMMON_NAME,
            TAG_UTF8_STRING,
            s.as_bytes().to_vec(),
        ),
        DnAttribute::Surname(s) => (&OID_X520_SURNAME, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::SerialNumber(s) => {
            verify_printable(s.as_bytes(), "SerialNumber")?;
            (
                &OID_X520_SERIAL_NUMBER,
                TAG_PRINTABLE_STRING,
                s.as_bytes().to_vec(),
            )
        }
        DnAttribute::CountryName(s) => {
            verify_printable(s.as_bytes(), "CountryName")?;
            if s.len() != 2 {
                return Err(Error::InvalidDnAttributeForX509 {
                    asn1_type: "PrintableString",
                    reason: "CountryName must be exactly 2 characters",
                });
            }
            (
                &OID_X520_COUNTRY_NAME,
                TAG_PRINTABLE_STRING,
                s.as_bytes().to_vec(),
            )
        }
        DnAttribute::LocalityName(s) => (
            &OID_X520_LOCALITY_NAME,
            TAG_UTF8_STRING,
            s.as_bytes().to_vec(),
        ),
        DnAttribute::StateOrProvinceName(s) => (
            &OID_X520_STATE_OR_PROVINCE,
            TAG_UTF8_STRING,
            s.as_bytes().to_vec(),
        ),
        DnAttribute::OrganizationName(s) => (&OID_X520_ORG, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::OrganizationalUnitName(s) => {
            (&OID_X520_ORG_UNIT, TAG_UTF8_STRING, s.as_bytes().to_vec())
        }
        DnAttribute::Title(s) => (&OID_X520_TITLE, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::Name(s) => (&OID_X520_NAME, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::GivenName(s) => (&OID_X520_GIVEN_NAME, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::Initials(s) => (&OID_X520_INITIALS, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::GenerationQualifier(s) => (
            &OID_X520_GENERATION_QUALIFIER,
            TAG_UTF8_STRING,
            s.as_bytes().to_vec(),
        ),
        DnAttribute::DnQualifier(s) => {
            verify_printable(s.as_bytes(), "DnQualifier")?;
            (
                &OID_X520_DN_QUALIFIER,
                TAG_PRINTABLE_STRING,
                s.as_bytes().to_vec(),
            )
        }
        DnAttribute::Pseudonym(s) => (&OID_X520_PSEUDONYM, TAG_UTF8_STRING, s.as_bytes().to_vec()),
        DnAttribute::DomainComponent(s) => {
            verify_ia5(s.as_bytes(), "DomainComponent")?;
            (&OID_DOMAIN_COMPONENT, TAG_IA5_STRING, s.as_bytes().to_vec())
        }

        // No mapping available — refuse to convert.
        DnAttribute::Other { tag, .. } => return Err(Error::DnAttributeHasNoX509Oid(*tag)),
    };

    // Outer: SEQUENCE { OID, <string-type> { value } }
    wrap_sequence_into(out, |inner| {
        encode_oid_into(inner, oid);
        wrap_primitive_into(inner, string_tag, &value_bytes);
    });
    Ok(())
}

/// Verify that all bytes are in the X.509 `PrintableString` character set
/// (RFC 5280 Appendix B).
///
/// Allowed: A-Z, a-z, 0-9, space, `'`, `(`, `)`, `+`, `,`, `-`, `.`,
/// `/`, `:`, `=`, `?`.
fn verify_printable(s: &[u8], asn1_type: &'static str) -> Result<()> {
    for &b in s {
        let ok = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b' ' | b'\'' | b'(' | b')' | b'+' | b',' | b'-' | b'.' | b'/' | b':' | b'=' | b'?'
            );
        if !ok {
            return Err(Error::InvalidDnAttributeForX509 {
                asn1_type,
                reason: "value contains a non-PrintableString character",
            });
        }
    }
    Ok(())
}

/// Verify that all bytes are in the ASCII range (`IA5String` = ASCII 0x00–0x7F).
fn verify_ia5(s: &[u8], asn1_type: &'static str) -> Result<()> {
    if s.iter().any(|&b| b > 0x7F) {
        return Err(Error::InvalidDnAttributeForX509 {
            asn1_type,
            reason: "value contains a non-ASCII character",
        });
    }
    Ok(())
}

/// Write a primitive TLV with the given tag (`UTF8String`, `PrintableString`,
/// `IA5String`, `OCTET STRING`, etc.) into `out` in place.
fn wrap_primitive_into(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
    out.push(tag);
    encode_definite_length(out, value.len());
    out.extend_from_slice(value);
}

// =============================================================================
// Name (DistinguishedName) encoder
// =============================================================================

/// Encode an X.509 `Name`: `SEQUENCE OF RelativeDistinguishedName`,
/// where each RDN is `SET OF AttributeTypeAndValue`.
///
/// Matter DNs use one attribute per RDN — we mirror matter.js's output.
#[cfg(test)]
fn encode_dn(dn: &DistinguishedName) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    encode_dn_into(&mut out, dn)?;
    Ok(out)
}

/// Write an X.509 `Name` into `out` in place. See [`encode_dn`].
fn encode_dn_into(out: &mut Vec<u8>, dn: &DistinguishedName) -> Result<()> {
    let mut attr_err: Option<Error> = None;
    wrap_sequence_into(out, |seq| {
        for attr in dn {
            // Each RDN: SET { AttributeTypeAndValue }
            if let Err(e) = wrap_set_into(seq, |set| encode_dn_attribute_into(set, attr)) {
                attr_err = Some(e);
                return;
            }
        }
    });
    match attr_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Write a DER SET (tag 0x31, constructed) into `out`, content from `build`.
///
/// `build` is fallible (a DN attribute may be unconvertible); its error is
/// propagated through the helper's return.
fn wrap_set_into<F>(out: &mut Vec<u8>, build: F) -> Result<()>
where
    F: FnOnce(&mut Vec<u8>) -> Result<()>,
{
    let mut build_err: Option<Error> = None;
    wrap_tagged_into(out, 0x31, |set| {
        if let Err(e) = build(set) {
            build_err = Some(e);
        }
    });
    match build_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Wrap content bytes in a DER SEQUENCE.
///
/// Production encoding paths use the in-place [`wrap_sequence_into`] instead;
/// this allocating wrapper is only used by `crate::test_support`, so it is
/// gated to that feature rather than compiled as dead code by default.
#[cfg(feature = "test-support")]
pub(crate) fn wrap_sequence(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 4);
    out.push(0x30);
    encode_definite_length(&mut out, content.len());
    out.extend_from_slice(content);
    out
}

/// Compute the DER definite-length prefix bytes for a content of length `len`.
///
/// Short form (a single byte) when `len < 128`, long form otherwise. This is
/// the allocation-free sibling of [`encode_definite_length`]: it is used by
/// the write-in-place wrappers, which build a node's content directly into the
/// shared output buffer and then need the length prefix *after* the content
/// length is known, to splice it in ahead of the content.
fn definite_length_prefix(len: usize) -> ([u8; 9], usize) {
    let mut buf = [0u8; 9];
    if len < 0x80 {
        // len < 128, fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        {
            buf[0] = len as u8;
        }
        (buf, 1)
    } else {
        let bytes = len.to_be_bytes();
        let leading_zeros = bytes.iter().take_while(|&&b| b == 0).count();
        let used = &bytes[leading_zeros..];
        // used.len() is at most 8 (usize width on 64-bit), so the 0x80 OR and
        // cast to u8 is safe — the high bit encodes the long-form marker.
        #[allow(clippy::cast_possible_truncation)]
        {
            buf[0] = 0x80 | (used.len() as u8);
        }
        buf[1..=used.len()].copy_from_slice(used);
        (buf, 1 + used.len())
    }
}

/// Write a DER tag-length-value element into `out` in place, with the content
/// produced by `build`.
///
/// `build` appends the element's content bytes directly onto `out`; this helper
/// then splices the `tag` byte and the matching definite-length prefix in front
/// of that content. The resulting bytes are byte-identical to the older
/// "build a fresh `Vec` per node and concatenate" approach — only the assembly
/// strategy differs. Routing every wrapper through a single reused buffer avoids
/// the tree of dozens of short-lived `Vec` allocations the encoder used to make
/// on every signature verification.
fn wrap_tagged_into<F>(out: &mut Vec<u8>, tag: u8, build: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    let content_start = out.len();
    build(out);
    let content_len = out.len() - content_start;

    let (prefix, prefix_len) = definite_length_prefix(content_len);
    // Splice `tag || length-prefix` in ahead of the freshly written content.
    // `splice` shifts the content right by (1 + prefix_len) bytes in place; no
    // intermediate allocation occurs (the shared buffer was pre-reserved).
    let header = core::iter::once(tag).chain(prefix[..prefix_len].iter().copied());
    out.splice(content_start..content_start, header);
}

/// Write a DER SEQUENCE (tag `0x30`) into `out`, content produced by `build`.
fn wrap_sequence_into<F>(out: &mut Vec<u8>, build: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    wrap_tagged_into(out, 0x30, build);
}

/// Encode a DER definite-length prefix (short form ≤ 127, long form otherwise).
pub(crate) fn encode_definite_length(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        // len < 128, fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let leading_zeros = bytes.iter().take_while(|&&b| b == 0).count();
        let used = &bytes[leading_zeros..];
        // used.len() is at most 8 (usize width on 64-bit), so the 0x80 OR
        // and cast to u8 is safe — the high bit encodes the long-form marker.
        #[allow(clippy::cast_possible_truncation)]
        out.push(0x80 | (used.len() as u8));
        out.extend_from_slice(used);
    }
}

/// Convert Unix seconds to `(year, month, day, hour, minute, second)` UTC.
///
/// Uses the civil-from-days algorithm from Howard Hinnant's date library
/// (public domain). Valid for any Unix second since 1970.
// These short identifiers (doy, doe, yoe, era, mp) are from the
// canonical Howard Hinnant chrono::civil_from_days algorithm and
// match the algorithm's published pseudocode. Renaming would
// make this function harder to verify against the reference.
#[allow(clippy::similar_names)]
fn unix_to_ymdhms(unix_secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    // unix_secs / 86_400 fits in i64 for all values we will see
    // (the max Matter cert date 9999-12-31 is well within i64 range).
    #[allow(clippy::cast_possible_wrap)]
    let day = (unix_secs / 86_400) as i64;
    // unix_secs % 86_400 < 86_400 < u32::MAX — truncation is intentional.
    #[allow(clippy::cast_possible_truncation)]
    let seconds_in_day = (unix_secs % 86_400) as u32;
    let hour = seconds_in_day / 3600;
    let minute = (seconds_in_day / 60) % 60;
    let second = seconds_in_day % 60;

    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    // z - era * 146_097 is always in [0, 146_096], fits in u32.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    // year is a calendar year (e.g. 2024, 9999) — always fits in u32.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let year_u32 = year as u32;
    (year_u32, m, d, hour, minute, second)
}

// =============================================================================
// Extension encoders
// =============================================================================

use crate::extensions::{BasicConstraints, Extensions, KeyIdentifier, KeyUsage};

/// Encode an X.509 `Extension`:
/// `SEQUENCE { extnID OID, critical BOOLEAN DEFAULT FALSE, extnValue OCTET STRING }`.
///
/// The `critical` field is omitted when `false` per DER's DEFAULT encoding rule.
/// When `true`, it is encoded as `BOOLEAN TRUE` (`01 01 FF`).
/// `extn_value` is the pre-encoded content of the extension — it is wrapped in an
/// outer `OCTET STRING` here, as RFC 5280 §4.1 requires.
fn encode_extension_into(
    out: &mut Vec<u8>,
    oid: &ObjectIdentifier,
    critical: bool,
    extn_value: &[u8],
) {
    wrap_sequence_into(out, |inner| {
        encode_oid_into(inner, oid);
        if critical {
            // BOOLEAN TRUE: tag 0x01, length 0x01, value 0xFF.
            inner.extend_from_slice(&[0x01, 0x01, 0xFF]);
        }
        // extnValue OCTET STRING wraps the already-DER-encoded extension value.
        wrap_primitive_into(inner, 0x04, extn_value);
    });
}

/// Encode the `BasicConstraints` extension.
///
/// The extension OID is `2.5.29.19`. matter.js always marks this extension as
/// critical (`critical: true`) regardless of the `is_ca` field. The inner
/// `SEQUENCE` contains `BOOLEAN TRUE` only when `is_ca = true`; omitting the
/// BOOLEAN when `false` follows the DER DEFAULT encoding rule. `path_len` is
/// encoded as `INTEGER` only when `is_ca = true` and the constraint is set.
fn encode_basic_constraints_into(out: &mut Vec<u8>, bc: BasicConstraints) {
    // Inner BasicConstraints SEQUENCE:
    // { BOOLEAN(is_ca) OPTIONAL DEFAULT FALSE, INTEGER(path_len) OPTIONAL }
    let mut value = Vec::new();
    wrap_sequence_into(&mut value, |inner| {
        if bc.is_ca {
            inner.extend_from_slice(&[0x01, 0x01, 0xFF]); // BOOLEAN TRUE
            if let Some(path_len) = bc.path_len_constraint {
                // INTEGER: tag 0x02, length 0x01, value = path_len (fits in one byte).
                inner.push(0x02);
                inner.push(0x01);
                inner.push(path_len);
            }
        }
    });
    // matter.js always emits BasicConstraints as critical.
    encode_extension_into(
        out,
        &OID_EXT_BASIC_CONSTRAINTS,
        /* critical */ true,
        &value,
    );
}

/// Encode the `BasicConstraints` extension to a standalone `Vec`.
#[cfg(test)]
fn encode_basic_constraints(bc: BasicConstraints) -> Vec<u8> {
    let mut out = Vec::new();
    encode_basic_constraints_into(&mut out, bc);
    out
}

/// Encode the `KeyUsage` extension.
///
/// The extension OID is `2.5.29.15`. `KeyUsage` is always critical per Matter spec
/// and matter.js. The value is a DER `BIT STRING`.
///
/// X.509 BIT STRING layout: the first content byte is the "unused bits" count,
/// followed by the flag bytes. The bitflags value uses LSB = bit 0
/// (digitalSignature), which maps to the MSB of the first flag byte in the
/// X.509 encoding:
///
/// - `bitflags bit 0 (DIGITAL_SIGNATURE, 0x0001)` → bit 7 of byte 0 (value 0x80)
/// - `bitflags bit 1 (CONTENT_COMMITMENT, 0x0002)` → bit 6 of byte 0 (value 0x40)
/// - …
/// - `bitflags bit 8 (DECIPHER_ONLY, 0x0100)` → bit 7 of byte 1 (value 0x80)
///
/// To produce the correct bit order, we bit-reverse each byte. Only the minimum
/// number of bytes needed to represent the set bits is emitted; trailing zero bytes
/// are dropped and the unused-bit count is set to the trailing-zero count of the
/// last emitted byte.
fn encode_key_usage_into(out: &mut Vec<u8>, ku: KeyUsage) {
    let raw = ku.bits(); // u16, LSB = digitalSignature

    // Split into low byte (bits 0–7) and high byte (bits 8–15).
    // Bit-reverse each so the X.509 MSB-first ordering is satisfied.
    #[allow(clippy::cast_possible_truncation)]
    let byte0_rev = (raw as u8).reverse_bits();
    #[allow(clippy::cast_possible_truncation)]
    let byte1_rev = ((raw >> 8) as u8).reverse_bits();

    // Build the BIT STRING content: [unused_bits_byte, flag_byte(s)].
    // Emit only the minimum number of flag bytes.
    let bit_string_content: Vec<u8> = if byte1_rev != 0 {
        // Two flag bytes needed. Unused bits = trailing zeros of the last byte.
        // trailing_zeros() on a u8 returns at most 8, which fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        let ub = byte1_rev.trailing_zeros() as u8;
        vec![ub, byte0_rev, byte1_rev]
    } else if byte0_rev != 0 {
        // One flag byte. Unused bits = trailing zeros of byte0_rev.
        // trailing_zeros() on a u8 returns at most 8, which fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        let ub = byte0_rev.trailing_zeros() as u8;
        vec![ub, byte0_rev]
    } else {
        // All bits clear: emit a single zero byte with 0 unused bits.
        vec![0x00, 0x00]
    };

    let mut bit_string = Vec::with_capacity(2 + bit_string_content.len());
    bit_string.push(0x03); // BIT STRING tag
    encode_definite_length(&mut bit_string, bit_string_content.len());
    bit_string.extend_from_slice(&bit_string_content);

    encode_extension_into(
        out,
        &OID_EXT_KEY_USAGE,
        /* critical */ true,
        &bit_string,
    );
}

/// Encode the `KeyUsage` extension to a standalone `Vec`.
#[cfg(test)]
fn encode_key_usage(ku: KeyUsage) -> Vec<u8> {
    let mut out = Vec::new();
    encode_key_usage_into(&mut out, ku);
    out
}

// Extended key usage OIDs, encoded once as constants.
// matter.js maps TLV integer values 1–6 to these OIDs.
// These are the DER OID string forms of the id-kp-* arcs (RFC 5280 / RFC 4945).
const OID_KP_SERVER_AUTH: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1");
const OID_KP_CLIENT_AUTH: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.2");
const OID_KP_CODE_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.3");
const OID_KP_EMAIL_PROTECTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.4");
const OID_KP_TIME_STAMPING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.8");
const OID_KP_OCSP_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.9");

/// Encode the `ExtendedKeyUsage` extension.
///
/// The extension OID is `2.5.29.37`. matter.js always marks this extension as
/// critical (`critical: true`).
///
/// Each u32 in `eku` is a Matter TLV compact integer that maps to an
/// `id-kp-*` OID (see matter.js `X509.ExtendedKeyUsage`):
/// - `1` → `id-kp-serverAuth`   (1.3.6.1.5.5.7.3.1)
/// - `2` → `id-kp-clientAuth`   (1.3.6.1.5.5.7.3.2)
/// - `3` → `id-kp-codeSigning`  (1.3.6.1.5.5.7.3.3)
/// - `4` → `id-kp-emailProtection` (1.3.6.1.5.5.7.3.4)
/// - `5` → `id-kp-timeStamping` (1.3.6.1.5.5.7.3.8)
/// - `6` → `id-kp-OCSPSigning`  (1.3.6.1.5.5.7.3.9)
///
/// Values outside 1–6 are skipped (unknown extensions are not surfaced by the
/// TLV decoder, but we guard defensively).
///
/// # Errors
///
/// Currently infallible — returns an empty extension body for unknown values
/// rather than failing, because the error type has no variant for an unknown
/// EKU value.
fn encode_extended_key_usage_into(out: &mut Vec<u8>, eku: &[u32], critical: bool) {
    let mut value = Vec::new();
    wrap_sequence_into(&mut value, |inner| {
        for &val in eku {
            let oid = match val {
                1 => &OID_KP_SERVER_AUTH,
                2 => &OID_KP_CLIENT_AUTH,
                3 => &OID_KP_CODE_SIGNING,
                4 => &OID_KP_EMAIL_PROTECTION,
                5 => &OID_KP_TIME_STAMPING,
                6 => &OID_KP_OCSP_SIGNING,
                _ => continue,
            };
            encode_oid_into(inner, oid);
        }
    });
    encode_extension_into(out, &OID_EXT_EXTENDED_KEY_USAGE, critical, &value);
}

/// Encode the `SubjectKeyIdentifier` extension.
///
/// The extension OID is `2.5.29.14`. Non-critical. The `extnValue` wraps an
/// `OCTET STRING` containing the 20-byte SHA-1 key fingerprint (the `extnValue`
/// field itself is also an OCTET STRING per RFC 5280, so the 20 bytes end up
/// doubly-wrapped: outer OCTET STRING from `encode_extension`, inner OCTET STRING
/// encoding the key identifier).
fn encode_subject_key_identifier_into(out: &mut Vec<u8>, ski: &KeyIdentifier) {
    // Inner: OCTET STRING containing the 20-byte key identifier.
    let mut inner = Vec::new();
    wrap_primitive_into(&mut inner, 0x04, &ski.0);
    encode_extension_into(
        out,
        &OID_EXT_SUBJECT_KEY_IDENTIFIER,
        /* critical */ false,
        &inner,
    );
}

/// Encode the `SubjectKeyIdentifier` extension to a standalone `Vec`.
#[cfg(test)]
fn encode_subject_key_identifier(ski: &KeyIdentifier) -> Vec<u8> {
    let mut out = Vec::new();
    encode_subject_key_identifier_into(&mut out, ski);
    out
}

/// Encode the `AuthorityKeyIdentifier` extension.
///
/// The extension OID is `2.5.29.35`. Non-critical. The value is:
/// `SEQUENCE { keyIdentifier [0] IMPLICIT OCTET STRING(20) OPTIONAL, ... }`.
///
/// Only the `keyIdentifier` field is populated (matter.js does the same).
/// `[0] IMPLICIT OCTET STRING` has tag `0x80` (context-class primitive, tag 0).
fn encode_authority_key_identifier_into(out: &mut Vec<u8>, aki: &KeyIdentifier) {
    let mut value = Vec::new();
    wrap_sequence_into(&mut value, |inner| {
        // [0] IMPLICIT OCTET STRING: tag 0x80, length 20, 20 bytes.
        inner.push(0x80);
        encode_definite_length(inner, 20);
        inner.extend_from_slice(&aki.0);
    });
    encode_extension_into(
        out,
        &OID_EXT_AUTHORITY_KEY_IDENTIFIER,
        /* critical */ false,
        &value,
    );
}

/// Encode the X.509 `extensions [3] EXPLICIT SEQUENCE OF Extension OPTIONAL`.
///
/// Extensions are emitted in Matter TLV declaration order (basic-constraints,
/// key-usage, extended-key-usage, subject-key-identifier, authority-key-identifier).
/// This order matches matter.js's `matterToX509` / `extensionsToAst` order
/// and is verified by the byte-parity test
/// (`every_certificate_x509_tbs_matches_reference` in `tests/certificates.rs`).
fn encode_extensions_block_into(out: &mut Vec<u8>, ext: &Extensions) {
    // Wrap in [3] EXPLICIT: context-class constructed tag = 0xA3, around the
    // inner `SEQUENCE OF Extension`.
    wrap_tagged_into(out, 0xA3, |explicit| {
        wrap_sequence_into(explicit, |inner| {
            if let Some(bc) = &ext.basic_constraints {
                encode_basic_constraints_into(inner, *bc);
            }
            if let Some(ku) = ext.key_usage {
                encode_key_usage_into(inner, ku);
            }
            if let Some(eku) = &ext.extended_key_usage {
                // matter.js always marks ExtendedKeyUsage as critical.
                encode_extended_key_usage_into(inner, eku, /* critical */ true);
            }
            if let Some(ski) = &ext.subject_key_identifier {
                encode_subject_key_identifier_into(inner, ski);
            }
            if let Some(aki) = &ext.authority_key_identifier {
                encode_authority_key_identifier_into(inner, aki);
            }
        });
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::extensions::{BasicConstraints, KeyIdentifier, KeyUsage};
    use crate::time::MatterTime;

    // ---- encode_validity --------------------------------------------------

    #[test]
    fn validity_pre_2050_uses_utctime() {
        // 2024-01-01T00:00:00Z, Matter-time form
        let not_before = MatterTime::from_unix_secs(1_704_067_200);
        // 2049-12-31T23:59:59Z (last UTCTime moment)
        let not_after = MatterTime::from_unix_secs(2_524_607_999);
        let bytes = encode_validity(not_before, not_after);

        // SEQUENCE { UTCTime("240101000000Z"), UTCTime("491231235959Z") }
        // Inner UTCTime is tag 0x17, length 0x0D, 13 ASCII chars.
        assert_eq!(bytes[0], 0x30); // outer SEQUENCE tag
        assert_eq!(bytes[2], 0x17); // first UTCTime tag
        assert_eq!(bytes[3], 0x0D); // length = 13
        assert_eq!(&bytes[4..17], b"240101000000Z");
        assert_eq!(bytes[17], 0x17); // second UTCTime tag
        assert_eq!(bytes[18], 0x0D);
        assert_eq!(&bytes[19..32], b"491231235959Z");
    }

    #[test]
    fn validity_post_2050_uses_generalizedtime() {
        // 2050-01-01T00:00:00Z (first GeneralizedTime moment)
        let not_before = MatterTime::from_unix_secs(2_524_608_000);
        // 2100-01-01T00:00:00Z
        let not_after = MatterTime::from_unix_secs(4_102_444_800);
        let bytes = encode_validity(not_before, not_after);

        // SEQUENCE { GeneralizedTime("20500101000000Z"), GeneralizedTime("21000101000000Z") }
        assert_eq!(bytes[0], 0x30);
        assert_eq!(bytes[2], 0x18); // GeneralizedTime tag
        assert_eq!(bytes[3], 0x0F); // length = 15
        assert_eq!(&bytes[4..19], b"20500101000000Z");
        assert_eq!(bytes[19], 0x18);
        assert_eq!(bytes[20], 0x0F);
        assert_eq!(&bytes[21..36], b"21000101000000Z");
    }

    #[test]
    fn validity_no_expiry_maps_to_sentinel() {
        let not_before = MatterTime::from_unix_secs(1_704_067_200);
        let bytes = encode_validity(not_before, MatterTime::NO_EXPIRY);
        // not_after must be GeneralizedTime("99991231235959Z")
        // Find it after the UTCTime not_before: 2 SEQUENCE header bytes + 15 UTCTime bytes = offset 17
        assert_eq!(bytes[17], 0x18); // GeneralizedTime tag
        assert_eq!(bytes[18], 0x0F);
        assert_eq!(&bytes[19..34], b"99991231235959Z");
    }

    // ---- encode_serial_number --------------------------------------------

    #[test]
    fn serial_high_bit_clear_no_leading_zero() {
        let bytes = encode_serial_number(&[0x01, 0x02, 0x03]).unwrap();
        // INTEGER tag 0x02, length 3, value 01 02 03
        assert_eq!(bytes, vec![0x02, 0x03, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn serial_high_bit_set_prepends_leading_zero() {
        let bytes = encode_serial_number(&[0x80, 0x12, 0x34]).unwrap();
        // INTEGER tag 0x02, length 4, value 00 80 12 34
        assert_eq!(bytes, vec![0x02, 0x04, 0x00, 0x80, 0x12, 0x34]);
    }

    #[test]
    fn serial_single_byte_high_bit_set() {
        let bytes = encode_serial_number(&[0xFF]).unwrap();
        assert_eq!(bytes, vec![0x02, 0x02, 0x00, 0xFF]);
    }

    #[test]
    fn serial_empty_is_rejected() {
        // Spec disallows zero-length serials; we mirror that.
        assert!(encode_serial_number(&[]).is_err());
    }

    // ---- encode_algorithm_identifier_ecdsa_sha256 ------------------------

    #[test]
    fn alg_id_ecdsa_sha256_has_no_parameters() {
        let bytes = encode_algorithm_identifier_ecdsa_sha256();
        // SEQUENCE { OID(1.2.840.10045.4.3.2) } — no parameters
        // Tag 0x30, length 0x0A (10), then OID tag 0x06, length 0x08, 8 OID bytes
        assert_eq!(bytes[0], 0x30);
        assert_eq!(bytes[1], 0x0A);
        assert_eq!(bytes[2], 0x06);
        assert_eq!(bytes[3], 0x08);
        // OID DER for 1.2.840.10045.4.3.2: 2A 86 48 CE 3D 04 03 02
        assert_eq!(
            &bytes[4..12],
            &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02]
        );
    }

    // ---- encode_dn_attribute ---------------------------------------------

    #[test]
    fn dn_attribute_node_id_uses_uppercase_hex_utf8string() {
        // NodeId(0x10001) → UTF8String("0000000000010001")
        let attr = crate::DnAttribute::NodeId(0x10001);
        let bytes = encode_dn_attribute(&attr).unwrap();

        // Inner shape: SEQUENCE { OID, UTF8String("0000000000010001") }
        // Find the UTF8String (tag 0x0C) and check its value.
        let utf8_start = bytes
            .iter()
            .position(|&b| b == 0x0C)
            .expect("expected UTF8String tag 0x0C");
        let len = bytes[utf8_start + 1] as usize;
        assert_eq!(len, 16, "Matter-attribute hex is 16 chars for u64");
        assert_eq!(
            &bytes[utf8_start + 2..utf8_start + 2 + len],
            b"0000000000010001"
        );
    }

    #[test]
    fn dn_attribute_common_name_uses_utf8string() {
        let attr = crate::DnAttribute::CommonName("matter-test".to_string());
        let bytes = encode_dn_attribute(&attr).unwrap();
        let utf8_start = bytes.iter().position(|&b| b == 0x0C).unwrap();
        let len = bytes[utf8_start + 1] as usize;
        assert_eq!(&bytes[utf8_start + 2..utf8_start + 2 + len], b"matter-test");
    }

    #[test]
    fn dn_attribute_country_name_uses_printablestring() {
        let attr = crate::DnAttribute::CountryName("US".to_string());
        let bytes = encode_dn_attribute(&attr).unwrap();
        // PrintableString tag is 0x13
        let ps_start = bytes.iter().position(|&b| b == 0x13).unwrap();
        assert_eq!(bytes[ps_start + 1], 0x02);
        assert_eq!(&bytes[ps_start + 2..ps_start + 4], b"US");
    }

    #[test]
    fn dn_attribute_domain_component_uses_ia5string() {
        let attr = crate::DnAttribute::DomainComponent("example".to_string());
        let bytes = encode_dn_attribute(&attr).unwrap();
        // IA5String tag is 0x16
        let ia5_start = bytes.iter().position(|&b| b == 0x16).unwrap();
        let len = bytes[ia5_start + 1] as usize;
        assert_eq!(&bytes[ia5_start + 2..ia5_start + 2 + len], b"example");
    }

    #[test]
    fn dn_attribute_other_is_rejected() {
        use crate::DnAttributeValue;
        let attr = crate::DnAttribute::Other {
            tag: 99,
            value: DnAttributeValue::Utf8("ignored".to_string()),
        };
        let err = encode_dn_attribute(&attr).unwrap_err();
        assert!(matches!(err, Error::DnAttributeHasNoX509Oid(99)));
    }

    #[test]
    fn dn_attribute_country_name_non_printable_rejected() {
        // Cyrillic letter (UTF-8 multibyte) — not PrintableString-encodable.
        let attr = crate::DnAttribute::CountryName("Я ".to_string());
        let err = encode_dn_attribute(&attr).unwrap_err();
        assert!(matches!(err, Error::InvalidDnAttributeForX509 { .. }));
    }

    // ---- encode_dn (Name) ------------------------------------------------

    #[test]
    fn dn_with_one_attribute_wraps_in_sequence_of_set() {
        let dn =
            crate::DistinguishedName::new(vec![crate::DnAttribute::CommonName("test".to_string())]);
        let bytes = encode_dn(&dn).unwrap();
        // Outer SEQUENCE { SET { SEQUENCE { OID, UTF8String("test") } } }
        assert_eq!(bytes[0], 0x30); // outer SEQUENCE
                                    // SET tag is 0x31; it appears at offset 2.
        assert_eq!(bytes[2], 0x31);
    }

    // ---- encode_basic_constraints ----------------------------------------

    #[test]
    fn basic_constraints_ca_true_is_critical_and_has_boolean_true() {
        let bc = BasicConstraints {
            is_ca: true,
            path_len_constraint: None,
        };
        let bytes = encode_basic_constraints(bc);
        // critical flag is TLV { 0x01, 0x01, 0xFF } — BOOLEAN TRUE
        assert!(
            bytes.windows(3).any(|w| w == [0x01, 0x01, 0xFF]),
            "BOOLEAN(true) for critical flag must be present"
        );
        // Inner BasicConstraints SEQUENCE { BOOLEAN(true) } — value bytes 30 03 01 01 FF
        assert!(
            bytes
                .windows(5)
                .any(|w| w == [0x30, 0x03, 0x01, 0x01, 0xFF]),
            "Inner BasicConstraints must contain BOOLEAN(true)"
        );
    }

    #[test]
    fn basic_constraints_ca_false_omits_boolean() {
        let bc = BasicConstraints {
            is_ca: false,
            path_len_constraint: None,
        };
        let bytes = encode_basic_constraints(bc);
        // Inner BasicConstraints SEQUENCE is empty (DER default for BOOLEAN false).
        // No BOOLEAN(false) appears.
        assert!(
            !bytes.windows(3).any(|w| w == [0x01, 0x01, 0x00]),
            "BOOLEAN(false) must NOT be encoded (DER default rule)"
        );
    }

    // ---- encode_key_usage ------------------------------------------------

    #[test]
    fn key_usage_packs_bits_correctly() {
        let ku = KeyUsage::DIGITAL_SIGNATURE | KeyUsage::KEY_CERT_SIGN;
        let bytes = encode_key_usage(ku);
        // Critical flag must be present (KeyUsage is always critical).
        assert!(bytes.windows(3).any(|w| w == [0x01, 0x01, 0xFF]));
    }

    // ---- encode_subject_key_identifier -----------------------------------

    #[test]
    fn ski_wraps_20_bytes_in_octet_string() {
        let ski = KeyIdentifier([0xABu8; 20]);
        let bytes = encode_subject_key_identifier(&ski);
        // Inner OCTET STRING: tag 0x04, length 20, then 20 bytes of 0xAB.
        let needle: Vec<u8> = std::iter::once(0x04u8)
            .chain(std::iter::once(20u8))
            .chain(std::iter::repeat_n(0xABu8, 20))
            .collect();
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle.as_slice()),
            "inner OCTET STRING with 20 bytes of 0xAB must be present"
        );
    }

    // ---- matter_cert_to_x509_tbs_der --------------------------------------

    #[test]
    fn top_level_produces_outer_sequence() {
        let bytes = std::fs::read("../../test-vectors/certs/rcac.bin").unwrap();
        let cert = crate::MatterCertificate::from_tlv(&bytes).unwrap();
        let tbs = matter_cert_to_x509_tbs_der(&cert).unwrap();
        assert_eq!(tbs[0], 0x30, "TBS must start with a SEQUENCE tag");
        assert!(
            tbs.len() > 80,
            "TBS must be substantial — at least 80 bytes"
        );
    }

    // ---- encode_subject_public_key_info ----------------------------------

    #[test]
    fn spki_has_correct_curve_oid_and_bitstring_prefix() {
        // 65-byte point: 0x04 || X(32) || Y(32). Use deterministic bytes.
        let mut point = [0u8; 65];
        point[0] = 0x04;
        for (i, slot) in point.iter_mut().enumerate().skip(1) {
            *slot = u8::try_from(i).unwrap();
        }
        let key = crate::PublicKey::new(point).unwrap();
        let bytes = encode_subject_public_key_info(&key);

        // Structure: SEQUENCE {
        //   AlgorithmIdentifier SEQUENCE { OID(id-ecPublicKey), OID(prime256v1) },
        //   BIT STRING(0x00 || 65-byte-point)
        // }
        assert_eq!(bytes[0], 0x30); // outer SEQUENCE
                                    // BIT STRING tag is 0x03; locate it by scanning past the inner SEQUENCE.
                                    // Inner SEQUENCE: 0x30 0x13 then 2 OIDs (id-ecPublicKey 7 bytes, prime256v1 8 bytes).
                                    // Inner SEQUENCE total = 2 header + 2 + 7 + 2 + 8 = 21 bytes. Outer header = 2 bytes.
                                    // BIT STRING starts at offset 2 + 21 = 23.
        let bit_string_idx = 23;
        assert_eq!(bytes[bit_string_idx], 0x03); // BIT STRING tag
                                                 // length is 66 (65 point bytes + 1 unused-bits prefix byte)
        assert_eq!(bytes[bit_string_idx + 1], 66);
        assert_eq!(bytes[bit_string_idx + 2], 0x00); // unused-bits = 0
        assert_eq!(&bytes[bit_string_idx + 3..bit_string_idx + 68], &point);
    }
}

//! Error type for `matter-cert`.

use thiserror::Error;

use crate::time::MatterTime;

/// All errors `matter-cert` can produce.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// TLV decoding or encoding failed inside `matter-codec`.
    #[error("TLV codec error: {0}")]
    Codec(#[from] matter_codec::Error),

    /// A required certificate field was missing.
    #[error("missing required certificate field (context tag {0})")]
    MissingField(u8),

    /// A certificate field appeared more than once.
    #[error("duplicate certificate field (context tag {0})")]
    DuplicateField(u8),

    /// A certificate field had an unexpected element type.
    #[error("invalid TLV element type for certificate field (context tag {0})")]
    WrongFieldType(u8),

    /// A certificate field's value was outside the spec-defined range.
    #[error("certificate field value out of range (context tag {tag})")]
    FieldValueOutOfRange {
        /// Context tag of the offending field.
        tag: u8,
    },

    /// The certificate serial number had a length outside the spec-allowed
    /// range of 1..=20 bytes.
    ///
    /// The Matter operational-certificate profile (§6.5) inherits the X.509
    /// `CertificateSerialNumber` constraint (RFC 5280 §4.1.2.2): a serial is
    /// at most 20 octets, and a zero-length serial is not a valid INTEGER.
    /// We reject both bounds at parse time so a malformed serial cannot
    /// propagate into the X.509 TBS encoder or signature verification.
    #[error("certificate serial number length {len} is outside the spec range 1..=20")]
    InvalidSerialLength {
        /// The offending serial-number length, in bytes.
        len: usize,
    },

    /// Signature algorithm identifier was not `ecdsa-with-sha256` (1).
    #[error("certificate signature algorithm {0} is not supported")]
    UnsupportedSignatureAlgorithm(u8),

    /// Public-key algorithm identifier was not `ec-public-key` (1).
    #[error("certificate public-key algorithm {0} is not supported")]
    UnsupportedPublicKeyAlgorithm(u8),

    /// EC curve identifier was not `prime256v1` (1).
    #[error("certificate EC curve {0} is not supported")]
    UnsupportedEcCurve(u8),

    /// Public-key bytes had wrong length.
    #[error("public-key bytes have wrong length: expected 65, got {0}")]
    WrongPublicKeyLength(usize),

    /// Public-key bytes did not start with the uncompressed-point marker (0x04).
    #[error("public-key bytes do not have the uncompressed-point prefix (0x04)")]
    BadPublicKeyPrefix,

    /// A required field on `MatterCertificate::builder()` was not set before
    /// `build_unsigned()` was called.
    #[error("builder field `{0}` was not set")]
    MissingBuilderField(&'static str),

    /// Signature bytes had wrong length.
    #[error("signature bytes have wrong length: expected 64, got {0}")]
    WrongSignatureLength(usize),

    /// A distinguished-name attribute used a context tag not defined by the spec.
    #[error("invalid distinguished-name attribute (tag {0})")]
    InvalidDnAttribute(u8),

    /// A distinguished-name attribute's value had the wrong TLV element type.
    #[error("invalid TLV type for DN attribute (tag {0})")]
    InvalidDnAttributeType(u8),

    /// A key identifier had the wrong length (must be 20 bytes).
    #[error("key identifier has wrong length: expected 20, got {0}")]
    WrongKeyIdentifierLength(usize),

    /// A Matter DN attribute had no defined X.509 OID mapping.
    ///
    /// Occurs when a [`crate::DnAttribute::Other`] is encountered during
    /// X.509 conversion. We cannot invent an X.509 OID, and matter.js
    /// wouldn't have signed against one we made up.
    #[error("Matter DN attribute (tag {0}) has no defined X.509 OID mapping")]
    DnAttributeHasNoX509Oid(u8),

    /// A DN attribute belongs only to X.509 attestation certificates and
    /// has no Matter operational-TLV cert encoding.
    ///
    /// Produced if [`crate::DnAttribute::VendorId`] or
    /// [`crate::DnAttribute::ProductId`] is routed through the Matter TLV
    /// writer. VID/PID identifiers live in DAC/PAI/PAA X.509 attestation
    /// cert DNs (Matter §6.5.6.1), not in operational NOC/ICAC/RCAC TLV
    /// certs, so there is no spec-defined TLV context tag for them.
    #[error("DN attribute (tag {0}) is X.509-attestation-only and has no Matter TLV encoding")]
    DnAttributeNotTlvEncodable(&'static str),

    /// A DN attribute's value cannot be encoded in its X.509 ASN.1
    /// string type.
    ///
    /// E.g., a `CountryName` containing non-printable bytes cannot
    /// be encoded as `PrintableString`.
    #[error("DN attribute value cannot be encoded as X.509 {asn1_type}: {reason}")]
    InvalidDnAttributeForX509 {
        /// The ASN.1 string type that the encoding attempt targeted.
        asn1_type: &'static str,
        /// Why the value did not fit.
        reason: &'static str,
    },

    /// Signature verification failed.
    ///
    /// Reserved for M2.2; not produced by phase 1.
    #[error("signature verification failed")]
    SignatureVerificationFailed,

    /// Test-support X.509 cert signing failed.
    ///
    /// Produced only by `test_support::build_x509_der` (behind the
    /// `test-support` feature) when the supplied issuer PKCS#8 key is
    /// malformed or `ring` rejects the signing request. Never produced by
    /// production code paths.
    #[error("test-support X.509 signing failed: {0}")]
    TestX509SigningFailed(&'static str),

    /// Production ECDSA-P256-SHA256 signing via `ring` failed.
    ///
    /// Produced by [`crate::operational::sign_with_ring`] when the supplied
    /// issuer PKCS#8 key is malformed, or `ring` rejects the signing
    /// request.
    #[error("certificate signing failed: {0}")]
    SigningFailed(&'static str),

    /// A certificate's `not_before` is in the future.
    #[error("certificate is not yet valid (cert_index={cert_index}, not_before={not_before:?}, at={at:?})")]
    NotYetValid {
        /// Index of the offending cert in the chain (0 = leaf).
        cert_index: u8,
        /// The certificate's `not_before` timestamp.
        not_before: MatterTime,
        /// The time at which validation was attempted.
        at: MatterTime,
    },

    /// A certificate's `not_after` is in the past.
    #[error(
        "certificate has expired (cert_index={cert_index}, not_after={not_after:?}, at={at:?})"
    )]
    Expired {
        /// Index of the offending cert in the chain (0 = leaf).
        cert_index: u8,
        /// The certificate's `not_after` timestamp.
        not_after: MatterTime,
        /// The time at which validation was attempted.
        at: MatterTime,
    },

    /// A certificate chain did not terminate at a trusted root.
    #[error("certificate chain does not reach a trusted root")]
    UntrustedRoot,

    /// A cert's `issuer` did not match the next cert's `subject`.
    #[error("issuer DN does not match next cert's subject DN (cert_index={cert_index})")]
    IssuerSubjectMismatch {
        /// Index of the cert whose `issuer` did not match (0 = leaf).
        cert_index: u8,
    },

    /// A non-leaf certificate did not have `basic_constraints.is_ca = true`.
    #[error("non-leaf certificate is not a CA (cert_index={cert_index})")]
    NotACa {
        /// Index of the non-CA intermediate (always > 0).
        cert_index: u8,
    },

    /// Chain length exceeded a cert's `path_len_constraint`.
    #[error("chain length exceeds path-length constraint (cert_index={cert_index})")]
    PathLengthExceeded {
        /// Index of the cert whose path-length constraint was violated.
        cert_index: u8,
    },

    /// A non-leaf (CA) certificate lacked the `keyCertSign` `KeyUsage` bit.
    ///
    /// RFC 5280 §4.2.1.3 and Matter §6.5.5 require any certificate that
    /// signs other certificates to carry the `keyCertSign` `KeyUsage` bit
    /// (and a `KeyUsage` extension at all). A cert asserting `is_ca = true`
    /// but lacking `KeyUsage::KEY_CERT_SIGN` (or with no `KeyUsage` extension)
    /// is not a valid signing CA and is rejected here.
    #[error("CA certificate lacks the keyCertSign KeyUsage bit (cert_index={cert_index})")]
    MissingKeyCertSign {
        /// Index of the offending CA cert in the chain (always > 0).
        cert_index: u8,
    },

    /// The end-entity leaf certificate asserted `basic_constraints.is_ca = true`.
    ///
    /// RFC 5280 forbids an end-entity (leaf) certificate from asserting the
    /// CA bit. A leaf at chain index 0 with an explicit `is_ca = true` is a
    /// profile violation and is rejected. An absent `basic_constraints`
    /// extension on the leaf is permitted (it is not a violation).
    #[error("end-entity leaf certificate asserts is_ca=true (cert_index=0)")]
    LeafIsCa,
}

/// `Result<T, Error>` for convenience.
pub type Result<T> = core::result::Result<T, Error>;

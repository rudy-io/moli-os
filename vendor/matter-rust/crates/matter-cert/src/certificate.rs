//! Matter certificate parser and serialiser.
//!
//! Implements parsing and serialisation for the Matter TLV certificate
//! format defined in spec §6.5. A [`MatterCertificate`] is parsed from
//! raw TLV bytes by [`MatterCertificate::from_tlv`] and re-emitted by
//! [`MatterCertificate::to_tlv`]; byte-for-byte round-trip is enforced
//! by the unit tests.

use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

use crate::error::{Error, Result};
use crate::extensions::Extensions;
use crate::name::DistinguishedName;
use crate::public_key::PublicKey;
use crate::signature::Signature;
use crate::time::MatterTime;
use crate::tlv_tags as tags;

/// Maximum certificate serial-number length, in bytes.
///
/// X.509 (RFC 5280 §4.1.2.2), inherited by the Matter operational-certificate
/// profile (§6.5), caps the `CertificateSerialNumber` INTEGER at 20 octets.
const MAX_SERIAL_LEN: usize = 20;

/// Reject a serial-number length outside the spec-allowed range `1..=20`.
///
/// A zero-length serial is not a valid DER INTEGER, and a serial longer than
/// [`MAX_SERIAL_LEN`] violates RFC 5280 §4.1.2.2. Both bounds are enforced at
/// parse time so a malformed serial cannot reach the X.509 TBS encoder.
///
/// # Errors
///
/// Returns [`Error::InvalidSerialLength`] if `len` is `0` or greater than
/// [`MAX_SERIAL_LEN`].
fn validate_serial_len(len: usize) -> Result<()> {
    if len == 0 || len > MAX_SERIAL_LEN {
        return Err(Error::InvalidSerialLength { len });
    }
    Ok(())
}

/// A parsed Matter certificate.
///
/// Holds every spec §6.5 field. The algorithm identifiers (signature
/// algorithm, public-key algorithm, EC curve) are validated against the
/// only spec-allowed values during `from_tlv` but not stored — they are
/// re-emitted at fixed values by `to_tlv`. The same approach is used in
/// both directions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatterCertificate {
    serial: Vec<u8>,
    issuer: DistinguishedName,
    not_before: MatterTime,
    not_after: MatterTime,
    subject: DistinguishedName,
    public_key: PublicKey,
    extensions: Extensions,
    signature: Signature,
}

impl MatterCertificate {
    /// Parse a certificate from its TLV byte representation.
    ///
    /// Validates the anonymous outer structure and all 11 spec §6.5 fields.
    /// Algorithm identifiers are validated against the only spec-allowed
    /// values (`ecdsa-with-sha256`, `ec-public-key`, `prime256v1`) but not
    /// stored; they are re-emitted at fixed values by [`to_tlv`][Self::to_tlv].
    ///
    /// # Errors
    ///
    /// - [`Error::Codec`] — underlying TLV stream is malformed.
    /// - [`Error::MissingField`] — a required field is absent.
    /// - [`Error::DuplicateField`] — a field appears more than once.
    /// - [`Error::WrongFieldType`] — a field has an unexpected TLV type.
    /// - [`Error::FieldValueOutOfRange`] — a numeric field overflows its spec range.
    /// - [`Error::UnsupportedSignatureAlgorithm`] / [`Error::UnsupportedPublicKeyAlgorithm`]
    ///   / [`Error::UnsupportedEcCurve`] — non-spec algorithm or curve identifier.
    // The single-pass exhaustive match over 11 context tags is intentionally
    // kept as one function for readability and auditability.
    #[allow(clippy::too_many_lines)]
    pub fn from_tlv(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);

        // The outer envelope is an anonymous Structure.
        match reader.next()? {
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure,
            }) => {}
            _ => return Err(Error::WrongFieldType(0)),
        }

        let mut serial: Option<Vec<u8>> = None;
        let mut sig_alg_seen = false;
        let mut issuer: Option<DistinguishedName> = None;
        let mut not_before: Option<MatterTime> = None;
        let mut not_after: Option<MatterTime> = None;
        let mut subject: Option<DistinguishedName> = None;
        let mut pubkey_alg_seen = false;
        let mut ec_curve_seen = false;
        let mut public_key: Option<PublicKey> = None;
        let mut extensions: Option<Extensions> = None;
        let mut signature: Option<Signature> = None;

        loop {
            match reader.next()? {
                None => return Err(matter_codec::Error::UnclosedContainer.into()),
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar { tag, value }) => {
                    let t = context_tag(&tag)?;
                    match (t, value) {
                        (tags::CERT_SERIAL_NUMBER, Value::Bytes(b)) => {
                            ensure_first_seen(t, serial.as_ref())?;
                            validate_serial_len(b.len())?;
                            serial = Some(b);
                        }
                        (tags::CERT_SIG_ALGORITHM, Value::Uint(v)) => {
                            if sig_alg_seen {
                                return Err(Error::DuplicateField(t));
                            }
                            let v8 = u8::try_from(v)
                                .map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                            if v8 != tags::SIG_ALGORITHM_ECDSA_SHA256 {
                                return Err(Error::UnsupportedSignatureAlgorithm(v8));
                            }
                            sig_alg_seen = true;
                        }
                        (tags::CERT_NOT_BEFORE, Value::Uint(v)) => {
                            ensure_first_seen(t, not_before.as_ref())?;
                            let v32 = u32::try_from(v)
                                .map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                            not_before = Some(MatterTime(v32));
                        }
                        (tags::CERT_NOT_AFTER, Value::Uint(v)) => {
                            ensure_first_seen(t, not_after.as_ref())?;
                            let v32 = u32::try_from(v)
                                .map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                            not_after = Some(MatterTime(v32));
                        }
                        (tags::CERT_PUBKEY_ALGORITHM, Value::Uint(v)) => {
                            if pubkey_alg_seen {
                                return Err(Error::DuplicateField(t));
                            }
                            let v8 = u8::try_from(v)
                                .map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                            if v8 != tags::PUBKEY_ALGORITHM_EC_PUBLIC_KEY {
                                return Err(Error::UnsupportedPublicKeyAlgorithm(v8));
                            }
                            pubkey_alg_seen = true;
                        }
                        (tags::CERT_EC_CURVE, Value::Uint(v)) => {
                            if ec_curve_seen {
                                return Err(Error::DuplicateField(t));
                            }
                            let v8 = u8::try_from(v)
                                .map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                            if v8 != tags::EC_CURVE_PRIME256V1 {
                                return Err(Error::UnsupportedEcCurve(v8));
                            }
                            ec_curve_seen = true;
                        }
                        (tags::CERT_EC_PUBLIC_KEY, Value::Bytes(b)) => {
                            ensure_first_seen(t, public_key.as_ref())?;
                            public_key = Some(PublicKey::from_slice(&b)?);
                        }
                        (tags::CERT_SIGNATURE, Value::Bytes(b)) => {
                            ensure_first_seen(t, signature.as_ref())?;
                            signature = Some(Signature::from_slice(&b)?);
                        }
                        (t, _) => return Err(Error::WrongFieldType(t)),
                    }
                }

                Some(Element::ContainerStart { tag, kind }) => {
                    let t = context_tag(&tag)?;
                    match t {
                        tags::CERT_ISSUER => {
                            if !matches!(kind, ContainerKind::List) {
                                return Err(Error::WrongFieldType(t));
                            }
                            ensure_first_seen(t, issuer.as_ref())?;
                            issuer = Some(DistinguishedName::read_from_open_list(&mut reader)?);
                        }
                        tags::CERT_SUBJECT => {
                            if !matches!(kind, ContainerKind::List) {
                                return Err(Error::WrongFieldType(t));
                            }
                            ensure_first_seen(t, subject.as_ref())?;
                            subject = Some(DistinguishedName::read_from_open_list(&mut reader)?);
                        }
                        tags::CERT_EXTENSIONS => {
                            if !matches!(kind, ContainerKind::List) {
                                return Err(Error::WrongFieldType(t));
                            }
                            ensure_first_seen(t, extensions.as_ref())?;
                            extensions = Some(Extensions::read_from_open_list(&mut reader)?);
                        }
                        other => return Err(Error::WrongFieldType(other)),
                    }
                }

                // Element is #[non_exhaustive]; future element kinds are
                // wire-format violations at the certificate top level.
                Some(_) => return Err(Error::WrongFieldType(0)),
            }
        }

        // Validate that all mandatory algorithm-identifier fields were present.
        if !sig_alg_seen {
            return Err(Error::MissingField(tags::CERT_SIG_ALGORITHM));
        }
        if !pubkey_alg_seen {
            return Err(Error::MissingField(tags::CERT_PUBKEY_ALGORITHM));
        }
        if !ec_curve_seen {
            return Err(Error::MissingField(tags::CERT_EC_CURVE));
        }

        Ok(Self {
            serial: serial.ok_or(Error::MissingField(tags::CERT_SERIAL_NUMBER))?,
            issuer: issuer.ok_or(Error::MissingField(tags::CERT_ISSUER))?,
            not_before: not_before.ok_or(Error::MissingField(tags::CERT_NOT_BEFORE))?,
            not_after: not_after.ok_or(Error::MissingField(tags::CERT_NOT_AFTER))?,
            subject: subject.ok_or(Error::MissingField(tags::CERT_SUBJECT))?,
            public_key: public_key.ok_or(Error::MissingField(tags::CERT_EC_PUBLIC_KEY))?,
            extensions: extensions.ok_or(Error::MissingField(tags::CERT_EXTENSIONS))?,
            signature: signature.ok_or(Error::MissingField(tags::CERT_SIGNATURE))?,
        })
    }

    /// Serialise to TLV bytes.
    ///
    /// Fields are emitted in the spec §6.5-defined order (context tags 1–11).
    /// Algorithm identifiers are always emitted at their fixed spec-allowed
    /// values (`ecdsa-with-sha256 = 1`, `ec-public-key = 1`, `prime256v1 = 1`).
    ///
    /// A certificate parsed from bytes `B` and re-serialised produces
    /// exactly `B` byte-for-byte (this is the basis for the integration
    /// test's round-trip assertion).
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] from the underlying writer.
    pub fn to_tlv(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(512);
        let mut w = TlvWriter::new(&mut buf);

        w.start_structure(Tag::Anonymous)?;
        w.put_bytes(Tag::Context(tags::CERT_SERIAL_NUMBER), &self.serial)?;
        w.put_uint(
            Tag::Context(tags::CERT_SIG_ALGORITHM),
            u64::from(tags::SIG_ALGORITHM_ECDSA_SHA256),
        )?;
        self.issuer.write(&mut w, Tag::Context(tags::CERT_ISSUER))?;
        w.put_uint(
            Tag::Context(tags::CERT_NOT_BEFORE),
            u64::from(self.not_before.0),
        )?;
        w.put_uint(
            Tag::Context(tags::CERT_NOT_AFTER),
            u64::from(self.not_after.0),
        )?;
        self.subject
            .write(&mut w, Tag::Context(tags::CERT_SUBJECT))?;
        w.put_uint(
            Tag::Context(tags::CERT_PUBKEY_ALGORITHM),
            u64::from(tags::PUBKEY_ALGORITHM_EC_PUBLIC_KEY),
        )?;
        w.put_uint(
            Tag::Context(tags::CERT_EC_CURVE),
            u64::from(tags::EC_CURVE_PRIME256V1),
        )?;
        w.put_bytes(
            Tag::Context(tags::CERT_EC_PUBLIC_KEY),
            self.public_key.as_bytes(),
        )?;
        self.extensions
            .write(&mut w, Tag::Context(tags::CERT_EXTENSIONS))?;
        w.put_bytes(
            Tag::Context(tags::CERT_SIGNATURE),
            self.signature.as_bytes(),
        )?;
        w.end_container()?;

        Ok(buf)
    }

    /// Serial number as raw bytes.
    #[must_use]
    pub fn serial(&self) -> &[u8] {
        &self.serial
    }

    /// Issuer distinguished name.
    #[must_use]
    pub fn issuer(&self) -> &DistinguishedName {
        &self.issuer
    }

    /// Subject distinguished name.
    #[must_use]
    pub fn subject(&self) -> &DistinguishedName {
        &self.subject
    }

    /// Beginning of the validity period.
    #[must_use]
    pub fn not_before(&self) -> MatterTime {
        self.not_before
    }

    /// End of the validity period (`MatterTime::NO_EXPIRY` means no expiry).
    #[must_use]
    pub fn not_after(&self) -> MatterTime {
        self.not_after
    }

    /// EC public key (P-256, 65-byte uncompressed).
    #[must_use]
    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    /// Parsed extensions.
    #[must_use]
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Raw 64-byte ECDSA signature.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// Compute the X.509 DER `TBSCertificate` bytes derived from this cert.
    ///
    /// These are the bytes whose signature is stored in [`Self::signature`].
    /// Byte-identical to matter.js's `Certificate.asUnsignedDer()` for the
    /// same input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DnAttributeHasNoX509Oid`] if a parsed
    /// [`crate::DnAttribute::Other`] has no defined X.509 OID mapping.
    /// Returns [`Error::InvalidDnAttributeForX509`] if a DN attribute's
    /// value cannot be encoded in its X.509 ASN.1 string type.
    pub fn to_x509_tbs_der(&self) -> Result<Vec<u8>> {
        crate::x509::matter_cert_to_x509_tbs_der(self)
    }

    /// Construct a [`MatterCertificate`] directly from its field values.
    ///
    /// `pub(crate)` — production callers route through
    /// [`MatterCertificate::builder()`] (M6.3) or [`Self::from_tlv`].
    /// The `test_support` module uses this directly for synthetic-cert
    /// scaffolding; the `builder` module uses it as the final assembly
    /// step.
    ///
    /// Algorithm identifiers (`ecdsa-with-sha256`, `ec-public-key`,
    /// `prime256v1`) are not stored; [`Self::to_tlv`] always emits them
    /// at their single spec-allowed values.
    #[allow(clippy::too_many_arguments)] // Eight fields mirror the eight spec §6.5 cert fields.
    pub(crate) fn from_fields(
        serial: Vec<u8>,
        issuer: DistinguishedName,
        not_before: MatterTime,
        not_after: MatterTime,
        subject: DistinguishedName,
        public_key: PublicKey,
        extensions: Extensions,
        signature: Signature,
    ) -> Self {
        Self {
            serial,
            issuer,
            not_before,
            not_after,
            subject,
            public_key,
            extensions,
            signature,
        }
    }

    /// Verify this certificate's signature against the given issuer's
    /// public key.
    ///
    /// Computes the X.509 DER `TBSCertificate` from this cert's fields,
    /// then verifies `self.signature()` against the TBS using
    /// ECDSA-P256-SHA256 via the `ring` library.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SignatureVerificationFailed`] if the signature
    /// does not verify. Returns any error [`Self::to_x509_tbs_der`]
    /// would return on conversion failure.
    pub fn verify_signed_by(&self, issuer_key: &PublicKey) -> Result<()> {
        let tbs = self.to_x509_tbs_der()?;
        issuer_key.verify(&tbs, &self.signature)
    }
}

/// Extract the context-tag number from a [`Tag`], or return
/// [`Error::WrongFieldType(0)`] for non-context tags.
fn context_tag(tag: &Tag) -> Result<u8> {
    match tag {
        Tag::Context(n) => Ok(*n),
        _ => Err(Error::WrongFieldType(0)),
    }
}

/// Return [`Error::DuplicateField`] if `slot` is already `Some`.
fn ensure_first_seen<T>(tag: u8, slot: Option<&T>) -> Result<()> {
    if slot.is_some() {
        Err(Error::DuplicateField(tag))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::extensions::{BasicConstraints, Extensions};
    use crate::name::DnAttribute;

    /// Construct a minimal but structurally valid synthetic certificate.
    fn sample_cert() -> MatterCertificate {
        let mut key_bytes = [0u8; 65];
        key_bytes[0] = 0x04;
        MatterCertificate {
            serial: vec![1, 2, 3],
            issuer: DistinguishedName::new(vec![DnAttribute::RcacId(1)]),
            not_before: MatterTime(1000),
            not_after: MatterTime::NO_EXPIRY,
            subject: DistinguishedName::new(vec![DnAttribute::NodeId(42)]),
            public_key: PublicKey::new(key_bytes).unwrap(),
            extensions: Extensions {
                basic_constraints: Some(BasicConstraints {
                    is_ca: false,
                    path_len_constraint: None,
                }),
                ..Default::default()
            },
            signature: Signature::new([0u8; 64]),
        }
    }

    #[test]
    fn round_trip_synthetic_cert() {
        let cert = sample_cert();
        let bytes = cert.to_tlv().unwrap();
        let parsed = MatterCertificate::from_tlv(&bytes).unwrap();
        assert_eq!(parsed, cert);
    }

    /// Build the TLV bytes for `sample_cert()` with its serial replaced by a
    /// serial of the given length (filled with `0x01`). `to_tlv` itself has no
    /// length guard, so this is the way to manufacture an out-of-range serial
    /// on the wire and exercise the `from_tlv` parse-time check.
    fn cert_tlv_with_serial_len(len: usize) -> Vec<u8> {
        let mut cert = sample_cert();
        cert.serial = vec![0x01; len];
        cert.to_tlv().unwrap()
    }

    #[test]
    fn from_tlv_accepts_one_byte_serial() {
        let bytes = cert_tlv_with_serial_len(1);
        let parsed = MatterCertificate::from_tlv(&bytes).unwrap();
        assert_eq!(parsed.serial(), &[0x01]);
    }

    #[test]
    fn from_tlv_accepts_twenty_byte_serial() {
        let bytes = cert_tlv_with_serial_len(20);
        let parsed = MatterCertificate::from_tlv(&bytes).unwrap();
        assert_eq!(parsed.serial().len(), 20);
    }

    #[test]
    fn from_tlv_rejects_zero_byte_serial() {
        let bytes = cert_tlv_with_serial_len(0);
        let err = MatterCertificate::from_tlv(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidSerialLength { len: 0 }));
    }

    #[test]
    fn from_tlv_rejects_twenty_one_byte_serial() {
        let bytes = cert_tlv_with_serial_len(21);
        let err = MatterCertificate::from_tlv(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidSerialLength { len: 21 }));
    }

    #[test]
    fn from_tlv_rejects_truncated_input() {
        let cert = sample_cert();
        let mut bytes = cert.to_tlv().unwrap();
        // Drop the final end-of-container byte so the stream is unclosed.
        bytes.pop();
        let err = MatterCertificate::from_tlv(&bytes).unwrap_err();
        // matter-codec's UnclosedContainer propagates as Error::Codec.
        assert!(matches!(err, Error::Codec(_)));
    }
}

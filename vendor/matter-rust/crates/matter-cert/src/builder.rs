//! Public builder API for [`MatterCertificate`].
//!
//! Introduced in M6.3. Splits certificate construction into two stages so
//! callers can sign with any backend (sync, HSM, OS keychain, offline
//! ceremony) without the builder depending on a signer trait.
//!
//! ```text
//! MatterCertificate::builder()
//!     .serial(...).issuer(...).subject(...).validity(...)
//!     .public_key(...).extensions(...)
//!     .build_unsigned()?            // Result<UnsignedCertificate, Error>
//!     .tbs_der()?                   // Result<Vec<u8>, Error>  — bytes to sign
//!     // ... caller invokes its own ECDSA-P256-SHA256 signer ...
//!     unsigned.assemble(sig);       // MatterCertificate, infallible
//! ```

#![forbid(unsafe_code)]

use crate::certificate::MatterCertificate;
use crate::error::{Error, Result};
use crate::extensions::Extensions;
use crate::name::DistinguishedName;
use crate::public_key::PublicKey;
use crate::signature::Signature;
use crate::time::MatterTime;

/// Builder for [`MatterCertificate`]. Construct via
/// [`MatterCertificate::builder()`].
#[derive(Debug, Default)]
pub struct Builder {
    serial: Option<Vec<u8>>,
    issuer: Option<DistinguishedName>,
    not_before: Option<MatterTime>,
    not_after: Option<MatterTime>,
    subject: Option<DistinguishedName>,
    public_key: Option<PublicKey>,
    extensions: Option<Extensions>,
}

/// A certificate whose fields are set but whose signature has not yet
/// been computed. Convert to a signed [`MatterCertificate`] via
/// [`Self::assemble`] once an external signer produces the 64-byte raw
/// ECDSA signature over [`Self::tbs_der`].
#[derive(Debug, Clone)]
pub struct UnsignedCertificate {
    serial: Vec<u8>,
    issuer: DistinguishedName,
    not_before: MatterTime,
    not_after: MatterTime,
    subject: DistinguishedName,
    public_key: PublicKey,
    extensions: Extensions,
}

impl Builder {
    /// Set the certificate serial number (1..=20 raw bytes per spec §6.5.1).
    #[must_use]
    pub fn serial(mut self, serial: Vec<u8>) -> Self {
        self.serial = Some(serial);
        self
    }

    /// Set the issuer DN.
    #[must_use]
    pub fn issuer(mut self, dn: DistinguishedName) -> Self {
        self.issuer = Some(dn);
        self
    }

    /// Set the subject DN.
    #[must_use]
    pub fn subject(mut self, dn: DistinguishedName) -> Self {
        self.subject = Some(dn);
        self
    }

    /// Set the validity window.
    #[must_use]
    pub fn validity(mut self, not_before: MatterTime, not_after: MatterTime) -> Self {
        self.not_before = Some(not_before);
        self.not_after = Some(not_after);
        self
    }

    /// Set the subject's EC P-256 public key.
    #[must_use]
    pub fn public_key(mut self, pk: PublicKey) -> Self {
        self.public_key = Some(pk);
        self
    }

    /// Set the extensions.
    #[must_use]
    pub fn extensions(mut self, ext: Extensions) -> Self {
        self.extensions = Some(ext);
        self
    }

    /// Validate that every required field is set and return an
    /// [`UnsignedCertificate`] ready to be hashed/signed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MissingBuilderField`] naming the first missing field, or
    /// [`Error::FieldValueOutOfRange`] if the serial number is empty or longer
    /// than the 20-byte maximum required by Matter spec §6.5.1.
    pub fn build_unsigned(self) -> Result<UnsignedCertificate> {
        let serial = self.serial.ok_or(Error::MissingBuilderField("serial"))?;
        if serial.is_empty() || serial.len() > 20 {
            return Err(Error::FieldValueOutOfRange {
                tag: crate::tlv_tags::CERT_SERIAL_NUMBER,
            });
        }
        Ok(UnsignedCertificate {
            serial,
            issuer: self.issuer.ok_or(Error::MissingBuilderField("issuer"))?,
            not_before: self
                .not_before
                .ok_or(Error::MissingBuilderField("not_before"))?,
            not_after: self
                .not_after
                .ok_or(Error::MissingBuilderField("not_after"))?,
            subject: self.subject.ok_or(Error::MissingBuilderField("subject"))?,
            public_key: self
                .public_key
                .ok_or(Error::MissingBuilderField("public_key"))?,
            extensions: self
                .extensions
                .ok_or(Error::MissingBuilderField("extensions"))?,
        })
    }
}

impl UnsignedCertificate {
    /// The certificate's extensions, as set on the builder.
    #[must_use]
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// The certificate's subject distinguished name, as set on the builder.
    #[must_use]
    pub fn subject(&self) -> &DistinguishedName {
        &self.subject
    }

    /// The certificate's issuer distinguished name, as set on the builder.
    #[must_use]
    pub fn issuer(&self) -> &DistinguishedName {
        &self.issuer
    }

    /// Return the X.509 `TBSCertificate` DER bytes that an external signer
    /// must sign. Byte-identical to matter.js's `Certificate.asUnsignedDer()`.
    ///
    /// # Errors
    ///
    /// Returns any [`Error`] [`crate::MatterCertificate::to_x509_tbs_der`] would
    /// return on conversion failure (DN attribute with no defined X.509 OID
    /// mapping, etc.).
    pub fn tbs_der(&self) -> Result<Vec<u8>> {
        // The signature field is not part of the TBS by definition; use a
        // zero placeholder so we can reuse the existing certificate -> X.509
        // conversion path without refactoring it for two field shapes.
        let placeholder = MatterCertificate::from_fields(
            self.serial.clone(),
            self.issuer.clone(),
            self.not_before,
            self.not_after,
            self.subject.clone(),
            self.public_key.clone(),
            self.extensions.clone(),
            Signature::new([0u8; 64]),
        );
        placeholder.to_x509_tbs_der()
    }

    /// Combine the unsigned fields with a 64-byte raw ECDSA signature
    /// (the bytes the signer produced for `self.tbs_der()`).
    /// Infallible: all fields were validated at `build_unsigned()`.
    #[must_use]
    pub fn assemble(self, signature: [u8; 64]) -> MatterCertificate {
        MatterCertificate::from_fields(
            self.serial,
            self.issuer,
            self.not_before,
            self.not_after,
            self.subject,
            self.public_key,
            self.extensions,
            Signature::new(signature),
        )
    }
}

impl MatterCertificate {
    /// Begin constructing a new certificate.
    #[must_use]
    pub fn builder() -> Builder {
        Builder::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_possible_truncation)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::extensions::{BasicConstraints, Extensions};
    use crate::name::DnAttribute;

    fn sample_public_key() -> PublicKey {
        let mut key_bytes = [0u8; 65];
        key_bytes[0] = 0x04;
        // Body bytes don't matter for builder roundtrips (no signature math here).
        for (i, b) in key_bytes.iter_mut().enumerate().skip(1) {
            *b = i as u8;
        }
        PublicKey::new(key_bytes).unwrap()
    }

    #[test]
    fn build_unsigned_then_assemble_roundtrips() {
        let pk = sample_public_key();
        let unsigned = MatterCertificate::builder()
            .serial(vec![1, 2, 3])
            .issuer(DistinguishedName::new(vec![DnAttribute::RcacId(1)]))
            .subject(DistinguishedName::new(vec![
                DnAttribute::FabricId(7),
                DnAttribute::NodeId(42),
            ]))
            .validity(MatterTime(1_000), MatterTime::NO_EXPIRY)
            .public_key(pk.clone())
            .extensions(Extensions {
                basic_constraints: Some(BasicConstraints {
                    is_ca: false,
                    path_len_constraint: None,
                }),
                ..Default::default()
            })
            .build_unsigned()
            .unwrap();

        // tbs_der must succeed (DN attrs are all in the typed range).
        let tbs = unsigned.tbs_der().unwrap();
        assert!(!tbs.is_empty(), "TBS DER must be non-empty");

        let cert = unsigned.assemble([0xAB; 64]);
        // The assembled cert round-trips through TLV.
        let tlv = cert.to_tlv().unwrap();
        let parsed = MatterCertificate::from_tlv(&tlv).unwrap();
        assert_eq!(parsed, cert);
        // TBS produced by the unsigned helper must match what the assembled
        // cert produces — catches a future regression where the two paths diverge.
        assert_eq!(
            tbs,
            cert.to_x509_tbs_der().unwrap(),
            "TBS from unsigned must match TBS from assembled cert"
        );
    }

    #[test]
    fn build_unsigned_fails_on_missing_serial() {
        let err = MatterCertificate::builder()
            .issuer(DistinguishedName::new(vec![DnAttribute::RcacId(1)]))
            .subject(DistinguishedName::new(vec![DnAttribute::NodeId(42)]))
            .validity(MatterTime(1_000), MatterTime::NO_EXPIRY)
            .public_key(sample_public_key())
            .extensions(Extensions::default())
            .build_unsigned()
            .unwrap_err();
        assert!(
            matches!(err, Error::MissingBuilderField("serial")),
            "got: {err:?}"
        );
    }

    #[test]
    fn build_unsigned_fails_on_missing_subject() {
        let err = MatterCertificate::builder()
            .serial(vec![1])
            .issuer(DistinguishedName::new(vec![DnAttribute::RcacId(1)]))
            .validity(MatterTime(1_000), MatterTime::NO_EXPIRY)
            .public_key(sample_public_key())
            .extensions(Extensions::default())
            .build_unsigned()
            .unwrap_err();
        assert!(
            matches!(err, Error::MissingBuilderField("subject")),
            "got: {err:?}"
        );
    }

    #[test]
    fn build_unsigned_rejects_oversized_serial() {
        let err = MatterCertificate::builder()
            .serial(vec![0u8; 21])
            .issuer(DistinguishedName::new(vec![DnAttribute::RcacId(1)]))
            .subject(DistinguishedName::new(vec![DnAttribute::NodeId(42)]))
            .validity(MatterTime(1_000), MatterTime::NO_EXPIRY)
            .public_key(sample_public_key())
            .extensions(Extensions::default())
            .build_unsigned()
            .unwrap_err();
        assert!(
            matches!(err, Error::FieldValueOutOfRange { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn build_unsigned_rejects_empty_serial() {
        let err = MatterCertificate::builder()
            .serial(vec![])
            .issuer(DistinguishedName::new(vec![DnAttribute::RcacId(1)]))
            .subject(DistinguishedName::new(vec![DnAttribute::NodeId(42)]))
            .validity(MatterTime(1_000), MatterTime::NO_EXPIRY)
            .public_key(sample_public_key())
            .extensions(Extensions::default())
            .build_unsigned()
            .unwrap_err();
        assert!(
            matches!(err, Error::FieldValueOutOfRange { .. }),
            "got: {err:?}"
        );
    }
}

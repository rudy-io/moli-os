//! Error type for the attestation module.
//!
//! M6.2.1 shipped only the [`AttestationError::Parse`] variant.
//! M6.2.2 added six chain-validation outcomes. M6.2.3 adds the single
//! signature-verification outcome
//! ([`AttestationError::BadResponseSignature`]).

use thiserror::Error;

use crate::attestation::extensions::VendorId;

/// Errors produced by device attestation verification.
///
/// `#[non_exhaustive]` so future phases can add variants without a
/// breaking change.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AttestationError {
    /// The DER bytes passed to one of [`crate::attestation::x509::Dac`],
    /// [`crate::attestation::x509::Pai`], or
    /// [`crate::attestation::x509::Paa`]'s `from_der` constructor failed
    /// to parse, or failed a Matter-specific subject-DN structural check
    /// (missing required VID/PID attribute, or — for
    /// [`crate::attestation::x509::Paa`] — a forbidden PID attribute).
    #[error("X.509 parse failure")]
    Parse(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Path validation rejected the chain for a reason not captured by
    /// a more specific variant. Sources a boxed `webpki::Error`
    /// (downcastable via `Error::downcast_ref` on the trait object
    /// returned by `source()`) so callers who care about the
    /// underlying webpki kind can still inspect it without our
    /// public API mentioning webpki by type.
    #[error("certificate chain validation failed")]
    InvalidChain(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// One of the certs in the chain was outside its validity window
    /// at the supplied [`matter_cert::time::MatterTime`].
    #[error("certificate expired or not yet valid")]
    TimeBoundsViolation,

    /// A non-CA cert was marked `BasicConstraints.cA = true`, or the
    /// path-length-constraint was violated.
    #[error("BasicConstraints violation")]
    BasicConstraintsViolation,

    /// The certificate breaches the Matter attestation certificate
    /// *profile* (Matter Core Spec §6.2.2) in a way other than
    /// `BasicConstraints`: a wrong version or signature algorithm, or a
    /// `KeyUsage` / `SubjectKeyIdentifier` / `AuthorityKeyIdentifier`
    /// extension that is absent, mis-flagged (wrong criticality or
    /// bits), duplicated, or malformed. Mirrors connectedhomeip's
    /// `VerifyAttestationCertificateFormat`; `rustls-webpki` enforces
    /// none of these (it ignores `KeyUsage` and never requires
    /// SKID/AKID), so this check runs in our own code as a peer of
    /// [`crate::attestation::verify_chain`]. (`BasicConstraints`
    /// breaches surface as [`AttestationError::BasicConstraintsViolation`].)
    #[error("attestation certificate format violation: {reason}")]
    CertFormatViolation {
        /// Human-readable description of which profile rule failed.
        reason: &'static str,
    },

    /// No PAA in the supplied [`crate::attestation::PaaTrustStore`]
    /// matched the PAI's issuer.
    #[error("PAA not in trust store")]
    UntrustedRoot,

    /// DAC subject [`VendorId`] did not equal PAI subject [`VendorId`]
    /// (Matter §6.2.3 requires equality).
    #[error("VID mismatch: DAC={dac:?} PAI={pai:?}")]
    VidMismatch {
        /// [`VendorId`] observed on the DAC subject.
        dac: VendorId,
        /// [`VendorId`] observed on the PAI subject.
        pai: VendorId,
    },

    /// PAI is product-scoped (`subject_pid` is `Some`) and its
    /// [`crate::attestation::ProductId`] differs from the DAC's.
    /// Matter §6.2.3: a scoped PAI authorises only the matching
    /// product.
    #[error("PAI is not authorized for DAC's product")]
    PaiVidNotAuthorized,

    /// The PAA that anchored the chain is VID-scoped (its subject DN
    /// carries a [`VendorId`]) but that VID does not equal the DAC/PAI
    /// subject VID.
    ///
    /// Matter Core Spec §6.2.2.1 requires a commissioner to verify that
    /// a VID-scoped PAA only anchors attestation chains whose DAC and
    /// PAI subject VID equal the PAA's scoped VID. `rustls-webpki`
    /// performs only RFC 5280 DN-chaining — it treats the Matter VID
    /// OID as an opaque DN attribute, not as a `NameConstraint` — so
    /// without this overlay a VID-scoped PAA could anchor a chain for a
    /// different vendor. (chip's `DeviceAttestationVerifier` enforces
    /// the same rule.)
    #[error("VID-scoped PAA scope mismatch: PAA={paa_vid:?} DAC/PAI={dac_vid:?}")]
    PaaVidScopeMismatch {
        /// [`VendorId`] the anchoring PAA is scoped to (its subject DN).
        paa_vid: VendorId,
        /// [`VendorId`] observed on the DAC/PAI subject (these two are
        /// already known equal by the time this check runs).
        dac_vid: VendorId,
    },

    /// `attestation_elements` TLV failed to decode or is missing
    /// required fields (CD bytes, nonce, timestamp).
    ///
    /// Returned by
    /// [`crate::attestation::extract_attestation_elements_fields`] when
    /// the outer shape is not an anonymous structure, the structure is
    /// truncated, a required context-tagged field (1 = CD bytes,
    /// 2 = nonce, 3 = timestamp) is missing or has the wrong wire type,
    /// the nonce is not exactly 32 bytes, or a required field appears
    /// more than once.
    #[error("attestation_elements malformed or missing required fields")]
    ResponseElementsMalformed,

    /// Certification Declaration (CD) has invalid CMS structure: it
    /// failed `ContentInfo` / `SignedData` DER parse, declared
    /// multiple signers, lacked an attached eContent, used an
    /// unexpected `contentType` / `signatureAlgorithm`, or otherwise
    /// did not match the Matter Core Spec §6.3.1 shape expected by
    /// [`crate::attestation::verify_certification_declaration`].
    #[error("certification declaration has invalid CMS structure")]
    CertificationDeclarationMalformed,

    /// Certification Declaration signature did not verify against any
    /// trusted root in the supplied
    /// [`crate::attestation::CdSigningRoots`] store.
    #[error("certification declaration signature does not verify against any trusted root")]
    CertificationDeclarationSignatureInvalid,

    /// Certification Declaration inner TLV (the signed eContent
    /// payload) is malformed, truncated, or missing a required
    /// context-tagged field per Matter Core Spec §6.3.1.
    #[error("certification declaration inner TLV malformed")]
    CertificationDeclarationTlvMalformed,

    /// Vendor ID declared inside the verified Certification
    /// Declaration does not equal the VID the caller expected (sourced
    /// from the verified DAC subject in M6.4.x).
    #[error(
        "certification declaration VID mismatch: declared {declared:?}, expected {expected:?}"
    )]
    CertificationDeclarationVidMismatch {
        /// Vendor ID declared inside the CD's inner TLV (tag 1).
        declared: crate::attestation::VendorId,
        /// Vendor ID the caller required (typically the DAC subject's VID).
        expected: crate::attestation::VendorId,
    },

    /// Product ID list inside the verified Certification Declaration
    /// does not contain the PID the caller expected.
    #[error("certification declaration PID list does not contain expected {0:?}")]
    CertificationDeclarationPidMismatch(crate::attestation::ProductId),

    /// The Certification Declaration carries an `authorized_paa_list`
    /// (Matter Core Spec §6.3.1 tag 11) that does not include the
    /// `SubjectKeyIdentifier` of the PAA that anchored the device's DAC
    /// chain. Per Matter §6.2.3 the device may only be attested under a
    /// PAA the CD authorizes, so this is a counterfeit-detection reject
    /// (chip `DefaultDeviceAttestationVerifier.cpp:738`). Also raised when
    /// the CD scopes its PAAs but the anchoring PAA carries no SKID to
    /// match.
    #[error("certification declaration authorized_paa_list does not include the device's PAA")]
    CertificationDeclarationPaaNotAuthorized,

    /// ECDSA verification of the device's attestation-response signature
    /// over `attestation_elements || attestation_challenge` did not
    /// succeed against the DAC public key.
    ///
    /// **Deliberately coarse**, so that the error does not leak
    /// information: this variant does NOT distinguish
    /// between
    ///
    /// - signature bytes corrupted in transit,
    /// - the device signed with a key other than the DAC's,
    /// - the wrong `attestation_challenge` was supplied (e.g. a
    ///   replay or session-state mismatch), or
    /// - `attestation_elements` was tampered.
    ///
    /// A more granular surface here would let an attacker probe which
    /// of these failed, narrowing their guess for the actual session
    /// challenge.
    #[error("AttestationResponse signature verification failed")]
    BadResponseSignature,
}

/// Map a [`webpki::Error`] kind to our typed [`AttestationError`].
///
/// The mapping of the well-known [`webpki::Error`] kinds, adapted to
/// `webpki 0.103`'s actual variant set:
///
/// | [`webpki::Error`] kind                                                              | [`AttestationError`]          |
/// |-------------------------------------------------------------------------------------|-------------------------------|
/// | `CertExpired{..}`, `CertNotValidYet{..}`, `InvalidCertValidity`                     | `TimeBoundsViolation`         |
/// | `PathLenConstraintViolated`, `EndEntityUsedAsCa`, `CaUsedAsEndEntity`               | `BasicConstraintsViolation`   |
/// | `UnknownIssuer`                                                                     | `UntrustedRoot`               |
/// | (any other kind)                                                                    | `InvalidChain(boxed)`         |
///
/// [`AttestationError::BasicConstraintsViolation`] covers three
/// distinct `BasicConstraints`-extension errors webpki distinguishes:
/// - `EndEntityUsedAsCa` — a CA-marked cert was relied on as a leaf
///   (this is what fires when a DAC has `cA = true`, since webpki then
///   refuses to use it as the end-entity).
/// - `CaUsedAsEndEntity` — same family of bug from the other direction.
/// - `PathLenConstraintViolated` — chain too long for the intermediate's
///   declared `pathLenConstraint`.
///
/// All three are `BasicConstraints` extension semantics per RFC 5280
/// §4.2.1.9, so they fold into our single typed variant.
//
// pub(crate) because the only legitimate caller is chain.rs::verify_chain.
// Directed tests in chain.rs / this file cover every row of the table.
pub(crate) fn map_webpki_error(err: webpki::Error) -> AttestationError {
    use webpki::Error as W;
    match err {
        W::CertExpired { .. } | W::CertNotValidYet { .. } | W::InvalidCertValidity => {
            AttestationError::TimeBoundsViolation
        }
        W::PathLenConstraintViolated | W::EndEntityUsedAsCa | W::CaUsedAsEndEntity => {
            AttestationError::BasicConstraintsViolation
        }
        W::UnknownIssuer => AttestationError::UntrustedRoot,
        other => AttestationError::InvalidChain(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use rustls_pki_types::UnixTime;

    fn epoch() -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_secs(0))
    }

    #[test]
    fn maps_cert_expired_to_time_bounds_violation() {
        let err = map_webpki_error(webpki::Error::CertExpired {
            time: epoch(),
            not_after: epoch(),
        });
        assert!(matches!(err, AttestationError::TimeBoundsViolation));
    }

    #[test]
    fn maps_cert_not_valid_yet_to_time_bounds_violation() {
        let err = map_webpki_error(webpki::Error::CertNotValidYet {
            time: epoch(),
            not_before: epoch(),
        });
        assert!(matches!(err, AttestationError::TimeBoundsViolation));
    }

    #[test]
    fn maps_invalid_cert_validity_to_time_bounds_violation() {
        let err = map_webpki_error(webpki::Error::InvalidCertValidity);
        assert!(matches!(err, AttestationError::TimeBoundsViolation));
    }

    #[test]
    fn maps_path_len_constraint_violated_to_basic_constraints_violation() {
        let err = map_webpki_error(webpki::Error::PathLenConstraintViolated);
        assert!(matches!(err, AttestationError::BasicConstraintsViolation));
    }

    #[test]
    fn maps_end_entity_used_as_ca_to_basic_constraints_violation() {
        let err = map_webpki_error(webpki::Error::EndEntityUsedAsCa);
        assert!(matches!(err, AttestationError::BasicConstraintsViolation));
    }

    #[test]
    fn maps_ca_used_as_end_entity_to_basic_constraints_violation() {
        let err = map_webpki_error(webpki::Error::CaUsedAsEndEntity);
        assert!(matches!(err, AttestationError::BasicConstraintsViolation));
    }

    #[test]
    fn maps_unknown_issuer_to_untrusted_root() {
        let err = map_webpki_error(webpki::Error::UnknownIssuer);
        assert!(matches!(err, AttestationError::UntrustedRoot));
    }

    #[test]
    fn maps_long_tail_to_invalid_chain() {
        // Pick a kind that's NOT in the mapping table — signature
        // failure is a representative member of the "everything else"
        // bucket.
        let err = map_webpki_error(webpki::Error::InvalidSignatureForPublicKey);
        assert!(matches!(err, AttestationError::InvalidChain(_)));
    }

    #[test]
    fn bad_response_signature_variant_exists() {
        // Construction smoke test: this variant must be a unit variant so
        // it carries no information beyond "verification failed": the
        // single coarse variant prevents the error channel from leaking
        // which secret (key, challenge, elements, or signature) was off.
        let err = AttestationError::BadResponseSignature;
        assert!(matches!(err, AttestationError::BadResponseSignature));
        // Display string covers what the operator will see; assert on its
        // exact text so a future rename breaks a test, not a log.
        assert_eq!(
            format!("{err}"),
            "AttestationResponse signature verification failed"
        );
    }
}

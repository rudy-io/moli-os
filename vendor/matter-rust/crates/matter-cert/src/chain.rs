//! Matter certificate chain validation.
//!
//! Walks an ordered slice of [`MatterCertificate`]s (leaf to topmost
//! intermediate) and verifies that the chain anchors against a known
//! trusted root. Per-cert checks: time bounds, CA bit (above the leaf),
//! issuer/subject linkage (structural DN equality), path-length
//! constraint, and signature verification (via M2.3's
//! [`crate::MatterCertificate::verify_signed_by`]).

use crate::certificate::MatterCertificate;
use crate::error::{Error, Result};
use crate::extensions::KeyIdentifier;
use crate::name::DistinguishedName;
use crate::public_key::PublicKey;
use crate::time::MatterTime;

/// A trust anchor — a known-good public key paired with the DN under
/// which it was certified and, optionally, its subject-key-identifier
/// for the X.509-style AKI/SKI link check.
#[derive(Debug, Clone)]
pub struct TrustAnchor {
    subject: DistinguishedName,
    public_key: PublicKey,
    subject_key_identifier: Option<KeyIdentifier>,
}

impl TrustAnchor {
    /// Build an anchor from a known-good root certificate.
    ///
    /// Extracts subject, public key, and (if present) SKI from the cert.
    /// When the cert lacks a `SubjectKeyIdentifier` extension, the
    /// anchor matches by DN only.
    #[must_use]
    pub fn from_root_cert(root: &MatterCertificate) -> Self {
        Self {
            subject: root.subject().clone(),
            public_key: root.public_key().clone(),
            subject_key_identifier: root.extensions().subject_key_identifier,
        }
    }

    /// Build an anchor from raw fields.
    ///
    /// `subject_key_identifier` is optional — when `None`, this anchor
    /// matches by DN only (the SKI gate is skipped for this anchor).
    #[must_use]
    pub fn from_raw(
        subject: DistinguishedName,
        public_key: PublicKey,
        subject_key_identifier: Option<KeyIdentifier>,
    ) -> Self {
        Self {
            subject,
            public_key,
            subject_key_identifier,
        }
    }

    /// Returns the subject DN of this trust anchor.
    #[must_use]
    pub fn subject(&self) -> &DistinguishedName {
        &self.subject
    }

    /// Returns the public key of this trust anchor.
    #[must_use]
    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    /// Returns the subject key identifier of this trust anchor, if present.
    #[must_use]
    pub fn subject_key_identifier(&self) -> Option<&KeyIdentifier> {
        self.subject_key_identifier.as_ref()
    }
}

/// A collection of trusted roots.
///
/// Validation succeeds only if the chain anchors against at least
/// one entry here.
#[derive(Debug, Clone, Default)]
pub struct TrustedRoots {
    anchors: Vec<TrustAnchor>,
}

impl TrustedRoots {
    /// Create an empty set of trusted roots.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a trust anchor to this set.
    pub fn add(&mut self, anchor: TrustAnchor) {
        self.anchors.push(anchor);
    }

    /// Iterate over all trust anchors in this set.
    pub fn iter(&self) -> impl Iterator<Item = &TrustAnchor> {
        self.anchors.iter()
    }

    /// Returns the number of trust anchors in this set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Returns `true` if this set contains no trust anchors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// A chain of Matter certificates, ordered from leaf to topmost
/// intermediate. The root itself is supplied separately via
/// [`TrustedRoots`].
#[derive(Debug, Clone, Copy)]
pub struct CertificateChain<'a> {
    certs: &'a [MatterCertificate],
}

impl<'a> CertificateChain<'a> {
    /// Wrap a slice of certs as a chain.
    ///
    /// Empty slices are accepted here — [`Self::validate`] is what
    /// rejects them (with [`Error::UntrustedRoot`]).
    #[must_use]
    pub fn new(certs: &'a [MatterCertificate]) -> Self {
        Self { certs }
    }

    /// Returns the number of certificates in this chain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.certs.len()
    }

    /// Returns `true` if this chain contains no certificates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.certs.is_empty()
    }

    /// Validate the chain against `roots` at the moment `at`.
    ///
    /// Returns `Ok(())` iff every per-cert check passes AND the topmost
    /// cert anchors against at least one entry in `roots`.
    ///
    /// # Errors
    ///
    /// Returns the most-specific `Error` variant identifying which check
    /// failed; for per-cert failures the variant carries `cert_index`
    /// (0 = leaf). [`Error::UntrustedRoot`] is returned for empty chains,
    /// no matching anchor, or anchor signature failure.
    /// [`Error::MissingKeyCertSign`] is returned when a non-leaf CA cert
    /// lacks the `keyCertSign` `KeyUsage` bit, and [`Error::LeafIsCa`] when
    /// the end-entity leaf asserts `basic_constraints.is_ca = true`.
    /// Returns any error [`MatterCertificate::to_x509_tbs_der`] returns for
    /// the top certificate.
    pub fn validate(&self, roots: &TrustedRoots, at: MatterTime) -> Result<()> {
        if self.certs.is_empty() {
            return Err(Error::UntrustedRoot);
        }

        let len = self.certs.len();
        for i in 0..len {
            let cert = &self.certs[i];
            let i_u8 = u8::try_from(i).unwrap_or(u8::MAX);

            // ---- Time bounds (cheap; fail fast) ----
            let nb = cert.not_before();
            let na = cert.not_after();
            if nb > at {
                return Err(Error::NotYetValid {
                    cert_index: i_u8,
                    not_before: nb,
                    at,
                });
            }
            if na != MatterTime::NO_EXPIRY && na < at {
                return Err(Error::Expired {
                    cert_index: i_u8,
                    not_after: na,
                    at,
                });
            }

            // ---- CA bit + keyCertSign (above the leaf) ----
            if i > 0 {
                let is_ca = cert
                    .extensions()
                    .basic_constraints
                    .as_ref()
                    .is_some_and(|bc| bc.is_ca);
                if !is_ca {
                    return Err(Error::NotACa { cert_index: i_u8 });
                }
                // RFC 5280 §4.2.1.3 / Matter §6.5.5: a cert that signs other
                // certs MUST carry a KeyUsage extension asserting keyCertSign.
                // An absent KeyUsage, or one without the bit, is not a valid
                // signing CA.
                let has_key_cert_sign = cert
                    .extensions()
                    .key_usage
                    .is_some_and(|ku| ku.contains(crate::extensions::KeyUsage::KEY_CERT_SIGN));
                if !has_key_cert_sign {
                    return Err(Error::MissingKeyCertSign { cert_index: i_u8 });
                }
            } else {
                // ---- Leaf (index 0): must NOT assert the CA bit ----
                // RFC 5280 forbids an end-entity cert from asserting is_ca.
                // An absent basic_constraints extension is permitted; only an
                // explicit is_ca = true is a violation.
                let leaf_is_ca = cert
                    .extensions()
                    .basic_constraints
                    .as_ref()
                    .is_some_and(|bc| bc.is_ca);
                if leaf_is_ca {
                    return Err(Error::LeafIsCa);
                }
            }

            // ---- Path-length constraint ----
            if i > 0 {
                if let Some(plc) = cert
                    .extensions()
                    .basic_constraints
                    .as_ref()
                    .and_then(|bc| bc.path_len_constraint)
                {
                    // Intermediates strictly between this cert and the leaf
                    // (exclude the leaf at index 0).
                    let intermediates_below = u8::try_from(i.saturating_sub(1)).unwrap_or(u8::MAX);
                    if intermediates_below > plc {
                        return Err(Error::PathLengthExceeded { cert_index: i_u8 });
                    }
                }
            }

            // ---- Issuer / subject linkage + signature (intra-chain) ----
            if i + 1 < len {
                let next = &self.certs[i + 1];
                if cert.issuer() != next.subject() {
                    return Err(Error::IssuerSubjectMismatch { cert_index: i_u8 });
                }
                cert.verify_signed_by(next.public_key())?;
            }
        }

        // ---- Anchor the top cert against TrustedRoots ----
        let top = &self.certs[len - 1];
        // TBS depends only on `top` — compute once, not per anchor.
        let top_tbs = top.to_x509_tbs_der()?;
        for anchor in roots.iter() {
            if top.issuer() != anchor.subject() {
                continue;
            }
            // Asymmetric SKI gate: only the anchor's SKI controls strictness.
            // When anchor.SKI is Some(X), the cert MUST present a matching AKI.
            // When anchor.SKI is None, the gate is skipped (DN-only match).
            if let Some(anchor_ski) = anchor.subject_key_identifier() {
                let top_aki = top.extensions().authority_key_identifier;
                if top_aki != Some(*anchor_ski) {
                    continue;
                }
            }
            if anchor
                .public_key()
                .verify(&top_tbs, top.signature())
                .is_ok()
            {
                return Ok(());
            }
        }

        Err(Error::UntrustedRoot)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn trusted_roots_default_is_empty() {
        let roots = TrustedRoots::default();
        assert!(roots.is_empty());
        assert_eq!(roots.len(), 0);
        assert_eq!(roots.iter().count(), 0);
    }

    #[test]
    fn certificate_chain_empty_reports_zero_length() {
        let chain = CertificateChain::new(&[]);
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn validate_returns_untrusted_root_for_empty_chain() {
        let roots = TrustedRoots::new();
        let chain = CertificateChain::new(&[]);
        let err = chain
            .validate(&roots, MatterTime::from_unix_secs(1_700_000_000))
            .unwrap_err();
        assert!(matches!(err, Error::UntrustedRoot));
    }
}

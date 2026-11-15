//! Matter attestation certificate *format* enforcement.
//!
//! Mirrors connectedhomeip's `VerifyAttestationCertificateFormat`
//! (`src/crypto/CHIPCryptoPALOpenSSL.cpp:1227`). Matter Core Spec
//! §6.2.2 fixes a strict X.509 profile on DAC / PAI / PAA
//! certificates: the version and signature algorithm, and the
//! presence, criticality, and contents of the `BasicConstraints`,
//! `KeyUsage`, `SubjectKeyIdentifier`, and `AuthorityKeyIdentifier`
//! extensions.
//!
//! `rustls-webpki` does **not** enforce this profile. It ignores the
//! `KeyUsage` extension entirely (`rustls-webpki` `verify_cert.rs`:
//! *"For cert validation, we ignore the `KeyUsage` extension"*) and it
//! never requires SKID/AKID. So a counterfeit DAC bearing
//! `keyUsage = keyCertSign` — a CA signing key masquerading as a
//! device leaf — would sail through path validation. We close that gap
//! here, calling this check as a peer of
//! [`crate::attestation::verify_chain`] in the commissioner's
//! attestation step, exactly where chip's device attestation verifier
//! runs it (PAI first, then DAC).

use x509_parser::extensions::ParsedExtension;
use x509_parser::prelude::{FromDer, X509Certificate};
use x509_parser::x509::X509Version;

use crate::attestation::error::AttestationError;

/// `ecdsa-with-SHA256` (RFC 5758) — the only signature algorithm a
/// Matter attestation certificate may carry.
#[rustfmt::skip]
const OID_ECDSA_WITH_SHA256: x509_parser::der_parser::oid::Oid<'static> =
    x509_parser::der_parser::oid!(1.2.840.10045.4.3.2);

/// keyIdentifier length (bytes) Matter requires for both SKID and
/// AKID — the 20-byte SHA-1 of the SPKI (chip
/// `kSubjectKeyIdentifierLength` / `kAuthorityKeyIdentifierLength`).
const KEY_ID_LEN: usize = 20;

// KeyUsage bit masks. x509-parser decodes the RFC 5280 §4.2.1.3 bit
// string into `KeyUsage::flags` with digitalSignature at bit 0,
// keyCertSign at bit 5, and cRLSign at bit 6.
const KU_DIGITAL_SIGNATURE: u16 = 1 << 0;
const KU_KEY_CERT_SIGN: u16 = 1 << 5;
const KU_CRL_SIGN: u16 = 1 << 6;

/// Which attestation role's profile to enforce. Selects the
/// role-specific `BasicConstraints`, `KeyUsage`, and AKID rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CertRole {
    /// Device Attestation Certificate — the leaf. Not a CA;
    /// `KeyUsage` is exactly `digitalSignature`.
    Dac,
    /// Product Attestation Intermediate — a CA with `pathLen == 0`;
    /// `KeyUsage` is `keyCertSign + cRLSign`.
    Pai,
    /// Product Attestation Authority — the self-signed root; a CA with
    /// `pathLen` absent or `1`; `KeyUsage` is `keyCertSign + cRLSign`.
    ///
    /// The rule is defined here for completeness (chip's format check
    /// covers the PAA too), but the commissioner does not yet route PAA
    /// roots through it — they are operator-supplied trust anchors, and
    /// several synthetic test roots predate SKID. Wiring the PAA check
    /// into trust-store loading is a follow-up (2026-07-18).
    #[allow(dead_code)]
    Paa,
}

/// Enforce the Matter attestation certificate profile on one
/// DER-encoded certificate, per its [`CertRole`].
///
/// # Errors
///
/// Returns [`AttestationError::BasicConstraintsViolation`] if the
/// `BasicConstraints` extension is absent, non-critical, duplicated,
/// or carries a cA/pathLen wrong for the role; or
/// [`AttestationError::CertFormatViolation`] for any other profile
/// breach (wrong version or signature algorithm; a `KeyUsage` that is
/// absent, non-critical, duplicated, or carries the wrong bits; a SKID
/// or AKID that is absent, critical, duplicated, or not 20 bytes; or a
/// DAC/PAI missing its mandatory AKID). Returns
/// [`AttestationError::Parse`] only if the DER fails to re-parse (it
/// will already have parsed once in the `from_der` constructor).
pub(crate) fn verify_attestation_cert_format(
    der: &[u8],
    role: CertRole,
) -> Result<(), AttestationError> {
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| AttestationError::Parse(Box::new(e.clone())))?;

    if cert.version() != X509Version::V3 {
        return Err(fmt("certificate is not X.509 v3"));
    }
    if cert.signature_algorithm.algorithm != OID_ECDSA_WITH_SHA256 {
        return Err(fmt("signature algorithm is not ecdsa-with-SHA256"));
    }

    let mut basic = false;
    let mut key_usage = false;
    let mut skid = false;
    let mut akid = false;

    for ext in cert.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::BasicConstraints(bc) => {
                // Critical + appears exactly once; role-correct cA/pathLen.
                if basic || !ext.critical {
                    return Err(AttestationError::BasicConstraintsViolation);
                }
                basic = true;
                let ok = match role {
                    CertRole::Dac => !bc.ca && bc.path_len_constraint.is_none(),
                    CertRole::Pai => bc.ca && bc.path_len_constraint == Some(0),
                    CertRole::Paa => bc.ca && matches!(bc.path_len_constraint, None | Some(1)),
                };
                if !ok {
                    return Err(AttestationError::BasicConstraintsViolation);
                }
            }
            ParsedExtension::KeyUsage(ku) => {
                if key_usage || !ext.critical {
                    return Err(fmt("KeyUsage absent, non-critical, or duplicated"));
                }
                key_usage = true;
                let ok = match role {
                    // DAC SHALL have ONLY the digitalSignature bit.
                    CertRole::Dac => ku.flags == KU_DIGITAL_SIGNATURE,
                    // PAI/PAA SHALL have keyCertSign + cRLSign and no
                    // bit outside {digitalSignature, keyCertSign, cRLSign}.
                    CertRole::Pai | CertRole::Paa => {
                        ku.flags & KU_KEY_CERT_SIGN != 0
                            && ku.flags & KU_CRL_SIGN != 0
                            && ku.flags & !(KU_DIGITAL_SIGNATURE | KU_KEY_CERT_SIGN | KU_CRL_SIGN)
                                == 0
                    }
                };
                if !ok {
                    return Err(fmt("KeyUsage bits are wrong for the certificate role"));
                }
            }
            ParsedExtension::SubjectKeyIdentifier(kid) => {
                if skid || ext.critical {
                    return Err(fmt("SubjectKeyIdentifier duplicated or critical"));
                }
                skid = true;
                if kid.0.len() != KEY_ID_LEN {
                    return Err(fmt("SubjectKeyIdentifier is not 20 bytes"));
                }
            }
            ParsedExtension::AuthorityKeyIdentifier(a) => {
                if akid || ext.critical {
                    return Err(fmt("AuthorityKeyIdentifier duplicated or critical"));
                }
                akid = true;
                match a.key_identifier.as_ref() {
                    Some(k) if k.0.len() == KEY_ID_LEN => {}
                    _ => {
                        return Err(fmt(
                            "AuthorityKeyIdentifier keyIdentifier absent or not 20 bytes",
                        ))
                    }
                }
            }
            _ => {}
        }
    }

    // Mandatory on every attestation cert.
    if !(basic && key_usage && skid) {
        return Err(fmt(
            "missing a mandatory extension (BasicConstraints / KeyUsage / SubjectKeyIdentifier)",
        ));
    }
    // AKID is mandatory on the DAC and PAI (the PAA is self-signed).
    if matches!(role, CertRole::Dac | CertRole::Pai) && !akid {
        return Err(fmt("DAC/PAI missing the mandatory AuthorityKeyIdentifier"));
    }
    Ok(())
}

#[inline]
fn fmt(reason: &'static str) -> AttestationError {
    AttestationError::CertFormatViolation { reason }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    const HAPPY_DAC: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-DAC-FFF1-8000-0004-Cert.der"
    );
    const HAPPY_PAI: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-PAI-FFF1-8000-Cert.der"
    );

    macro_rules! fx {
        ($n:literal) => {
            include_bytes!(concat!(
                "../../../../test-vectors/certs/attestation/format/",
                $n,
                ".der"
            ))
        };
    }

    #[test]
    fn real_happy_path_certs_pass() {
        // The chip-issued test DAC/PAI (real silicon uses these) must
        // NOT be rejected by the profile check.
        verify_attestation_cert_format(HAPPY_DAC, CertRole::Dac).unwrap();
        verify_attestation_cert_format(HAPPY_PAI, CertRole::Pai).unwrap();
    }

    #[test]
    fn synthetic_well_formed_certs_pass() {
        verify_attestation_cert_format(fx!("dac-valid"), CertRole::Dac).unwrap();
        verify_attestation_cert_format(fx!("pai-valid"), CertRole::Pai).unwrap();
    }

    #[test]
    fn dac_with_keycertsign_bit_is_rejected() {
        // A signing key masquerading as a device leaf — the exact case
        // webpki lets through. This is the headline case the format check closes.
        assert!(matches!(
            verify_attestation_cert_format(fx!("dac-keycertsign"), CertRole::Dac),
            Err(AttestationError::CertFormatViolation { .. })
        ));
    }

    #[test]
    fn dac_missing_skid_is_rejected() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("dac-missing-skid"), CertRole::Dac),
            Err(AttestationError::CertFormatViolation { .. })
        ));
    }

    #[test]
    fn dac_missing_akid_is_rejected() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("dac-missing-akid"), CertRole::Dac),
            Err(AttestationError::CertFormatViolation { .. })
        ));
    }

    #[test]
    fn dac_keyusage_not_critical_is_rejected() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("dac-ku-not-critical"), CertRole::Dac),
            Err(AttestationError::CertFormatViolation { .. })
        ));
    }

    #[test]
    fn dac_marked_as_ca_is_basic_constraints_violation() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("dac-is-ca"), CertRole::Dac),
            Err(AttestationError::BasicConstraintsViolation)
        ));
    }

    #[test]
    fn pai_pathlen_nonzero_is_basic_constraints_violation() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("pai-pathlen-nonzero"), CertRole::Pai),
            Err(AttestationError::BasicConstraintsViolation)
        ));
    }

    #[test]
    fn pai_not_ca_is_basic_constraints_violation() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("pai-not-ca"), CertRole::Pai),
            Err(AttestationError::BasicConstraintsViolation)
        ));
    }

    #[test]
    fn pai_without_crlsign_is_rejected() {
        assert!(matches!(
            verify_attestation_cert_format(fx!("pai-missing-crlsign"), CertRole::Pai),
            Err(AttestationError::CertFormatViolation { .. })
        ));
    }

    #[test]
    fn real_pai_checked_under_dac_role_is_rejected() {
        // Role confusion: a valid PAI (CA cert) presented where a DAC
        // is expected must fail — its cA bit is illegal for a leaf.
        assert!(verify_attestation_cert_format(HAPPY_PAI, CertRole::Dac).is_err());
    }
}

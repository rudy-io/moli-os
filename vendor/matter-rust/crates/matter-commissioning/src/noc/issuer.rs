//! NOC certificate issuance.

#![forbid(unsafe_code)]

use matter_cert::{MatterCertificate, MatterTime};

use crate::noc::csr::VerifiedCsr;
use crate::noc::error::{NocError, NocRng};
use crate::noc::fabric::FabricRecord;

/// Construct + sign a NOC for the device whose CSR was verified.
///
/// Delegates the NOC extension/DN profile (`BasicConstraints{cA:false}`,
/// `KeyUsage{digitalSignature}`, EKU `[clientAuth, serverAuth]`, SKID/AKID)
/// to [`matter_cert::operational::noc`] — see that constructor for the
/// pinned spec profile. This function's job is just to supply the
/// fabric-derived issuer DN/SKID, generate a serial, and sign.
///
/// # Errors
///
/// See [`NocError`] variants `DnAttributeOverflow`, `CertBuild`,
/// `SigningFailed`, and `Rng`.
pub fn issue_noc(
    fabric: &FabricRecord,
    verified_csr: &VerifiedCsr,
    node_id: u64,
    case_authenticated_tags: &[u32],
    validity: (MatterTime, MatterTime),
    rng: &dyn NocRng,
) -> Result<MatterCertificate, NocError> {
    // A fabric with an ICAC tier signs NOCs under the ICAC, not the RCAC
    // directly (spec §6.5.5). Both `icac_signer` and `icac_cert` must be
    // `Some` to take this path — a fabric can't be half-migrated to an
    // ICAC tier, so a lone `Some` falls back to the flat RCAC path below.
    let (issuer_dn, issuer_ski, signer) =
        if let (Some(icac_signer), Some(icac_cert)) = (&fabric.icac_signer, &fabric.icac_cert) {
            let icac_ski = icac_cert
                .extensions()
                .subject_key_identifier
                .ok_or_else(|| NocError::CertBuild(matter_cert::Error::MissingField(4)))?;
            (icac_cert.subject().clone(), icac_ski, icac_signer.as_ref())
        } else {
            let root_ski = fabric
                .root_cert
                .extensions()
                .subject_key_identifier
                .ok_or_else(|| {
                    // Tag value chosen to point at "subject-key-identifier" in
                    // the existing extensions error space; the precise
                    // constant lives in matter-cert's
                    // tlv_tags::EXT_SUBJECT_KEY_IDENTIFIER if refactoring
                    // later — using a literal here keeps this crate
                    // independent of matter-cert's internal tag constants.
                    NocError::CertBuild(matter_cert::Error::MissingField(4))
                })?;
            (
                fabric.root_cert.subject().clone(),
                root_ski,
                fabric.root_signer.as_ref(),
            )
        };

    // Serial: 19 random bytes (matter.js convention; pinned by M6.3.3
    // byte-parity).
    let mut serial = vec![0u8; 19];
    rng.fill(&mut serial)?;
    // Top bit cleared — see fabric.rs `new_root_only` for the chip-vs-
    // matter.js INTEGER-normalization divergence this avoids.
    serial[0] &= 0x7F;

    let unsigned = matter_cert::operational::noc(matter_cert::operational::NocParams::new(
        fabric.fabric_id,
        node_id,
        case_authenticated_tags.to_vec(),
        issuer_dn,
        issuer_ski,
        verified_csr.public_key.clone(),
        serial,
        validity.0,
        validity.1,
    ))
    .map_err(NocError::CertBuild)?;
    let tbs = unsigned.tbs_der().map_err(NocError::CertBuild)?;
    let sig = signer
        .sign_p256_sha256(&tbs)
        .map_err(NocError::SigningFailed)?;
    Ok(unsigned.assemble(sig))
}

/// Construct + sign an Intermediate CA Certificate (ICAC, spec §6.5.5)
/// for this fabric, signed by the fabric's RCAC key.
///
/// Delegates the ICAC extension/DN profile (`BasicConstraints{cA:true,
/// pathLen:Some(0)}`, `KeyUsage{keyCertSign,cRLSign}`, SKID/AKID) to
/// [`matter_cert::operational::icac`] — see that constructor for the
/// pinned spec profile. This function's job is just to supply the
/// fabric-derived issuer DN/SKID, generate a serial, and sign.
///
/// # Errors
///
/// Returns [`NocError::CertBuild`] if the fabric's root certificate is
/// missing its `SubjectKeyIdentifier` extension (should not happen for a
/// `FabricRecord` built via [`FabricRecord::new_root_only`]) or if
/// certificate construction otherwise fails, [`NocError::SigningFailed`]
/// if the fabric's root signer rejects, and [`NocError::Rng`] on RNG
/// failure.
pub fn issue_icac(
    fabric: &FabricRecord,
    icac_id: u64,
    icac_public_key: &matter_cert::PublicKey,
    validity: (MatterTime, MatterTime),
    rng: &dyn NocRng,
) -> Result<MatterCertificate, NocError> {
    let root_ski = fabric
        .root_cert
        .extensions()
        .subject_key_identifier
        .ok_or_else(|| {
            // Same rationale as issue_noc's identical check above: a
            // literal tag value keeps this crate independent of
            // matter-cert's internal tag constants.
            NocError::CertBuild(matter_cert::Error::MissingField(4))
        })?;

    // Serial: 19 random bytes (matter.js convention; same construction
    // as issue_noc, above).
    let mut serial = vec![0u8; 19];
    rng.fill(&mut serial)?;
    // Top bit cleared — see fabric.rs `new_root_only` for the chip-vs-
    // matter.js INTEGER-normalization divergence this avoids.
    serial[0] &= 0x7F;

    let unsigned = matter_cert::operational::icac(matter_cert::operational::IcacParams::new(
        icac_id,
        fabric.root_cert.subject().clone(),
        root_ski,
        icac_public_key.clone(),
        serial,
        validity.0,
        validity.1,
    ))
    .map_err(NocError::CertBuild)?;
    let tbs = unsigned.tbs_der().map_err(NocError::CertBuild)?;
    let sig = fabric
        .root_signer
        .sign_p256_sha256(&tbs)
        .map_err(NocError::SigningFailed)?;
    Ok(unsigned.assemble(sig))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_possible_truncation)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::noc::error::SystemNocRng;
    use matter_cert::{BasicConstraints, KeyUsage, PublicKey};
    use matter_crypto::{RingSigner, Signer};
    use std::sync::Arc;

    fn sample_verified_csr() -> VerifiedCsr {
        let (signer, _) = RingSigner::generate().unwrap();
        VerifiedCsr {
            public_key: PublicKey::from_slice(signer.public_key().as_bytes()).unwrap(),
        }
    }

    fn sample_fabric() -> FabricRecord {
        let (signer, _) = RingSigner::generate().unwrap();
        let signer: Arc<dyn Signer> = Arc::new(signer);
        FabricRecord::new_root_only(
            0x0000_0000_0000_0001,
            signer,
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
            7,
            &SystemNocRng,
        )
        .unwrap()
    }

    /// RNG stub returning all-0xFF — forces the serial's would-be MSB high.
    #[derive(Debug)]
    struct AllOnesRng;
    impl crate::noc::NocRng for AllOnesRng {
        fn fill(&self, dest: &mut [u8]) -> Result<(), crate::noc::NocError> {
            dest.fill(0xFF);
            Ok(())
        }
    }

    #[test]
    fn noc_serial_top_bit_is_clear() {
        // Same rule as the RCAC (see fabric.rs::rcac_serial_top_bit_is_clear):
        // an MSB-set serial is reconstructed differently by chip (verbatim)
        // vs matter.js/us (0x00-prepended), breaking the signature on-device.
        let fabric = sample_fabric();
        let verified = sample_verified_csr();
        let noc = issue_noc(
            &fabric,
            &verified,
            0xDEAD_BEEF_CAFE_BABE,
            &[],
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            &AllOnesRng,
        )
        .unwrap();
        assert_eq!(
            noc.serial()[0] & 0x80,
            0,
            "generated NOC serial must have the top bit clear"
        );
    }

    #[test]
    fn issue_icac_produces_self_consistent_certificate() {
        let fabric = sample_fabric();
        let (icac_signer, _) = RingSigner::generate().unwrap();
        let icac_public_key = icac_signer.public_key().clone();

        let icac = issue_icac(
            &fabric,
            0x0000_0000_0000_0042,
            &icac_public_key,
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            &SystemNocRng,
        )
        .unwrap();

        // Issuer DN == fabric.root_cert subject.
        assert_eq!(icac.issuer(), fabric.root_cert.subject());

        // Subject DN carries the ICAC id.
        assert_eq!(icac.subject().icac_id(), Some(0x0000_0000_0000_0042));

        // Extensions match the ICAC profile: cA=true, pathLen=0,
        // KEY_CERT_SIGN|CRL_SIGN, AKID == RCAC's SKID.
        let extensions = icac.extensions();
        assert_eq!(
            extensions.basic_constraints,
            Some(BasicConstraints::new(true, Some(0)))
        );
        assert_eq!(
            extensions.key_usage,
            Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN)
        );
        assert_eq!(
            extensions.authority_key_identifier,
            fabric.root_cert.extensions().subject_key_identifier
        );

        // ICAC verifies under the fabric's root public key.
        icac.verify_signed_by(&fabric.root_public_key).unwrap();
    }

    #[test]
    fn issue_noc_signs_under_icac_when_present() {
        // When the fabric carries an ICAC (both `icac_signer` and
        // `icac_cert` set), issue_noc must sign the NOC under the ICAC key
        // and set the issuer DN/SKID from the ICAC — not the RCAC.
        let mut fabric = sample_fabric();
        let (icac_signer, _) = RingSigner::generate().unwrap();
        let icac_public_key = icac_signer.public_key().clone();
        let icac = issue_icac(
            &fabric,
            0x0000_0000_0000_0042,
            &icac_public_key,
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            &SystemNocRng,
        )
        .unwrap();
        let icac_signer: Arc<dyn Signer> = Arc::new(icac_signer);
        fabric.icac_signer = Some(icac_signer);
        fabric.icac_cert = Some(icac.clone());

        let verified = sample_verified_csr();
        let noc = issue_noc(
            &fabric,
            &verified,
            0xDEAD_BEEF_CAFE_BABE,
            &[],
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            &SystemNocRng,
        )
        .unwrap();

        // Issuer DN must be the ICAC's subject, not the RCAC's.
        assert_eq!(noc.issuer(), icac.subject());
        assert_ne!(noc.issuer(), fabric.root_cert.subject());

        // The NOC must verify under the ICAC's public key (i.e. it was
        // signed by the ICAC signer), not the RCAC's.
        noc.verify_signed_by(icac.public_key()).unwrap();
    }

    #[test]
    fn issue_noc_produces_self_consistent_certificate() {
        let fabric = sample_fabric();
        let verified = sample_verified_csr();
        let noc = issue_noc(
            &fabric,
            &verified,
            0xDEAD_BEEF_CAFE_BABE,
            &[0x0001_0002],
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            &SystemNocRng,
        )
        .unwrap();

        // Subject must contain FabricId and NodeId.
        assert_eq!(noc.subject().fabric_id(), Some(0x0000_0000_0000_0001));
        assert_eq!(noc.subject().node_id(), Some(0xDEAD_BEEF_CAFE_BABE));

        // Issuer DN == fabric.root_cert subject.
        assert_eq!(noc.issuer(), fabric.root_cert.subject());

        // NOC verifies under the fabric's root public key.
        noc.verify_signed_by(&fabric.root_public_key).unwrap();
    }

    /// Deterministic RNG stub: `fill` writes `dest[i] = i as u8` for every
    /// call, regardless of buffer length. Used only by
    /// [`issue_noc_tbs_bytes_are_stable`] so every random draw in the fixed
    /// fabric + NOC construction (RCAC serial, IPK, NOC serial) is
    /// reproducible run-to-run — required for pinning a golden TBS.
    #[derive(Debug)]
    struct GoldenRng;
    impl crate::noc::NocRng for GoldenRng {
        fn fill(&self, dest: &mut [u8]) -> Result<(), crate::noc::NocError> {
            for (i, b) in dest.iter_mut().enumerate() {
                *b = (i & 0xff) as u8;
            }
            Ok(())
        }
    }

    /// Fixed PKCS#8-encoded P-256 private key for the fabric's root signer.
    /// Captured once via `RingSigner::generate()`; hardcoded here purely so
    /// [`issue_noc_tbs_bytes_are_stable`] is fully reproducible — this key
    /// has no other significance and is not used anywhere outside this test.
    const RCAC_PKCS8: &[u8] = &[
        48, 129, 135, 2, 1, 0, 48, 19, 6, 7, 42, 134, 72, 206, 61, 2, 1, 6, 8, 42, 134, 72, 206,
        61, 3, 1, 7, 4, 109, 48, 107, 2, 1, 1, 4, 32, 73, 46, 194, 199, 69, 214, 149, 228, 175,
        236, 72, 195, 39, 129, 47, 13, 159, 182, 164, 240, 253, 177, 186, 3, 217, 51, 160, 169, 76,
        112, 219, 153, 161, 68, 3, 66, 0, 4, 55, 17, 222, 33, 152, 214, 179, 35, 51, 115, 190, 136,
        95, 63, 26, 125, 10, 97, 131, 176, 151, 11, 205, 27, 2, 130, 136, 147, 12, 67, 70, 185,
        116, 34, 216, 29, 211, 22, 10, 56, 163, 118, 47, 16, 245, 223, 113, 84, 161, 23, 1, 250,
        180, 235, 163, 5, 248, 45, 42, 219, 54, 128, 168, 76,
    ];

    /// Fixed SEC1-uncompressed P-256 public key standing in for a device's
    /// CSR public key. Captured once via `RingSigner::generate()`;
    /// hardcoded purely for reproducibility (see [`RCAC_PKCS8`]).
    const CSR_PUBKEY: [u8; 65] = [
        4, 1, 191, 75, 190, 104, 72, 238, 159, 14, 161, 72, 5, 185, 59, 232, 240, 115, 159, 47, 38,
        106, 154, 159, 127, 252, 168, 119, 154, 113, 252, 247, 82, 130, 96, 49, 184, 226, 191, 242,
        124, 85, 198, 137, 250, 174, 16, 111, 75, 86, 25, 114, 191, 224, 210, 217, 57, 57, 214, 5,
        132, 79, 59, 146, 58,
    ];

    /// Golden TBS-DER bytes (spec §6.5.4's X.509 `TBSCertificate` encoding,
    /// per [`MatterCertificate::to_x509_tbs_der`]) for a NOC built from the
    /// fixed fabric/CSR/node/CAT/validity/serial inputs below via the
    /// *current* `issue_noc` implementation. The TBS excludes the signature,
    /// so — unlike the full signed cert bytes, which vary run-to-run because
    /// `ring`'s ECDSA nonce is random — this is fully deterministic and
    /// therefore safe to pin byte-for-byte.
    ///
    /// This pinned `issue_noc`'s output across its move onto
    /// `matter_cert::operational::noc` and still guards it: it must produce
    /// this exact byte sequence. Do NOT edit these bytes to make a failing
    /// test pass — a mismatch means a change altered the wire format, which
    /// must be fixed at the source, not papered over here.
    #[rustfmt::skip]
    const GOLDEN_NOC_TBS: &[u8] = &[
        48, 130, 1, 173, 160, 3, 2, 1, 2, 2, 19, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14,
        15, 16, 17, 18, 48, 10, 6, 8, 42, 134, 72, 206, 61, 4, 3, 2, 48, 34, 49, 32, 48, 30, 6,
        10, 43, 6, 1, 4, 1, 130, 162, 124, 1, 4, 12, 16, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48,
        48, 48, 48, 48, 48, 55, 48, 32, 23, 13, 50, 51, 49, 49, 49, 52, 50, 50, 49, 51, 50, 48,
        90, 24, 15, 57, 57, 57, 57, 49, 50, 51, 49, 50, 51, 53, 57, 53, 57, 90, 48, 94, 49, 32,
        48, 30, 6, 10, 43, 6, 1, 4, 1, 130, 162, 124, 1, 5, 12, 16, 48, 48, 48, 48, 48, 48, 48,
        48, 48, 48, 48, 48, 48, 48, 48, 49, 49, 32, 48, 30, 6, 10, 43, 6, 1, 4, 1, 130, 162, 124,
        1, 1, 12, 16, 68, 69, 65, 68, 66, 69, 69, 70, 67, 65, 70, 69, 66, 65, 66, 69, 49, 24, 48,
        22, 6, 10, 43, 6, 1, 4, 1, 130, 162, 124, 1, 6, 12, 8, 48, 48, 48, 49, 48, 48, 48, 50, 48,
        89, 48, 19, 6, 7, 42, 134, 72, 206, 61, 2, 1, 6, 8, 42, 134, 72, 206, 61, 3, 1, 7, 3, 66,
        0, 4, 1, 191, 75, 190, 104, 72, 238, 159, 14, 161, 72, 5, 185, 59, 232, 240, 115, 159, 47,
        38, 106, 154, 159, 127, 252, 168, 119, 154, 113, 252, 247, 82, 130, 96, 49, 184, 226, 191,
        242, 124, 85, 198, 137, 250, 174, 16, 111, 75, 86, 25, 114, 191, 224, 210, 217, 57, 57,
        214, 5, 132, 79, 59, 146, 58, 163, 129, 131, 48, 129, 128, 48, 12, 6, 3, 85, 29, 19, 1, 1,
        255, 4, 2, 48, 0, 48, 14, 6, 3, 85, 29, 15, 1, 1, 255, 4, 4, 3, 2, 7, 128, 48, 32, 6, 3,
        85, 29, 37, 1, 1, 255, 4, 22, 48, 20, 6, 8, 43, 6, 1, 5, 5, 7, 3, 2, 6, 8, 43, 6, 1, 5, 5,
        7, 3, 1, 48, 29, 6, 3, 85, 29, 14, 4, 22, 4, 20, 45, 82, 3, 45, 20, 93, 191, 91, 118, 90,
        178, 217, 147, 62, 26, 206, 48, 232, 36, 24, 48, 31, 6, 3, 85, 29, 35, 4, 24, 48, 22, 128,
        20, 119, 26, 238, 51, 116, 65, 105, 44, 22, 8, 87, 137, 161, 158, 217, 247, 199, 49, 16,
        135,
    ];

    /// Fixed inputs shared by [`issue_noc_tbs_bytes_are_stable`]: node id,
    /// CAT list, and validity window. Arbitrary but fixed — only their
    /// stability matters, not their values.
    const GOLDEN_NODE_ID: u64 = 0xDEAD_BEEF_CAFE_BABE;
    const GOLDEN_CATS: [u32; 1] = [0x0001_0002];

    /// Wire-format guard: `issue_noc`'s TBS-DER output for a fixed
    /// fabric/CSR/node/CAT/validity/serial must stay byte-identical to
    /// [`GOLDEN_NOC_TBS`] (pinned before `issue_noc` moved onto
    /// `matter_cert::operational::noc`). See [`GOLDEN_NOC_TBS`] for the full
    /// rationale — do not weaken this test or edit the golden bytes to
    /// force a pass; a mismatch means a change altered the wire format.
    #[test]
    fn issue_noc_tbs_bytes_are_stable() {
        let signer = RingSigner::from_pkcs8(RCAC_PKCS8).unwrap();
        let signer: Arc<dyn Signer> = Arc::new(signer);
        let fabric = FabricRecord::new_root_only(
            0x0000_0000_0000_0001,
            signer,
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
            7,
            &GoldenRng,
        )
        .unwrap();

        let verified = VerifiedCsr {
            public_key: PublicKey::new(CSR_PUBKEY).unwrap(),
        };

        let noc = issue_noc(
            &fabric,
            &verified,
            GOLDEN_NODE_ID,
            &GOLDEN_CATS,
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            &GoldenRng,
        )
        .unwrap();

        let tbs = noc.to_x509_tbs_der().unwrap();
        assert_eq!(
            tbs, GOLDEN_NOC_TBS,
            "issue_noc's TBS-DER bytes changed — this is a wire-format regression"
        );
    }
}

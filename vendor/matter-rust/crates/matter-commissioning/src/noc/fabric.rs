//! `FabricRecord` — the per-fabric trust roots, signing keys, and IPK
//! that an M6.3 NOC issuer needs.

#![forbid(unsafe_code)]

use std::sync::Arc;

use matter_cert::{
    BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyIdentifier, KeyUsage,
    MatterCertificate, MatterTime, PublicKey,
};
use matter_crypto::Signer;
use ring::digest;

use crate::noc::error::{NocError, NocRng};

/// In-memory fabric record. Persistence is M8's concern; this struct is
/// what an M6.3 caller threads through `issuer::issue_noc` and onto the
/// `AddNOC` cluster command payload.
#[derive(Clone)]
pub struct FabricRecord {
    /// Matter fabric identifier (spec §6.2.1).
    pub fabric_id: u64,
    /// Public key of the fabric root signer (SEC1 uncompressed P-256).
    pub root_public_key: PublicKey,
    /// Signer for the fabric root key. Signs NOCs on a flat RCAC -> NOC
    /// fabric, and signs the ICAC on a 3-tier fabric.
    pub root_signer: Arc<dyn Signer>,
    /// Self-signed root (RCAC) certificate.
    pub root_cert: MatterCertificate,
    /// Intermediate-CA signer. `None` on a flat RCAC -> NOC fabric; `Some`
    /// when the fabric issues a 3-tier RCAC -> ICAC -> NOC chain, in which
    /// case NOCs are signed under this key rather than the root.
    pub icac_signer: Option<Arc<dyn Signer>>,
    /// Intermediate-CA certificate. `None` on a flat fabric; `Some` (paired
    /// with `icac_signer`) on a 3-tier fabric.
    pub icac_cert: Option<MatterCertificate>,
    /// 16-byte Identity Protection Key. Forms part of the `AddNOC` payload
    /// and seeds operational group-key derivation (spec §11.18.5.13).
    pub identity_protection_key: [u8; 16],
}

impl std::fmt::Debug for FabricRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FabricRecord")
            .field("fabric_id", &self.fabric_id)
            .field("root_public_key", &self.root_public_key)
            .field("root_signer", &"<dyn Signer>")
            .field("root_cert", &"<MatterCertificate>")
            .field(
                "icac_signer",
                &self.icac_signer.as_ref().map(|_| "<dyn Signer>"),
            )
            .field(
                "icac_cert",
                &self.icac_cert.as_ref().map(|_| "<MatterCertificate>"),
            )
            .field("identity_protection_key", &"<redacted; 16 bytes>")
            .finish()
    }
}

impl FabricRecord {
    /// Construct a fabric whose operational trust chain is RCAC -> NOC
    /// (no intermediate). Generates a fresh IPK via `rng`, builds the
    /// self-signed RCAC certificate via the matter-cert builder + the
    /// caller-supplied root signer.
    ///
    /// # Errors
    ///
    /// Returns [`NocError::CertBuild`] if certificate construction
    /// fails, [`NocError::SigningFailed`] if the root signer rejects,
    /// or [`NocError::Rng`] on RNG failure.
    pub fn new_root_only(
        fabric_id: u64,
        root_signer: Arc<dyn Signer>,
        not_before: MatterTime,
        not_after: MatterTime,
        rcac_id: u64,
        rng: &dyn NocRng,
    ) -> Result<Self, NocError> {
        let root_public_key = root_signer.public_key().clone();

        // Spec §6.5.4: SubjectKeyIdentifier is SHA-1 over the SEC1
        // uncompressed public-key bytes excluding the 0x04 prefix.
        // matter.js's `Crypto.hash` over the bare X||Y matches this;
        // the M6.3.3 byte-parity gate pins the convention.
        let ski_bytes = digest::digest(
            &digest::SHA1_FOR_LEGACY_USE_ONLY,
            &root_public_key.as_bytes()[1..],
        );
        let mut ski = [0u8; 20];
        ski.copy_from_slice(ski_bytes.as_ref());
        let ski_id = KeyIdentifier(ski);

        // Serial number: 19 random bytes is the convention matter.js
        // emits. The M6.3.3 byte-parity gate pins this length; if it
        // diverges, change the constant here in one place.
        let mut serial = vec![0u8; 19];
        rng.fill(&mut serial)?;
        // Clear the top bit: chip's TLV→X.509 conversion copies the serial
        // VERBATIM as the INTEGER value octets, while matter.js/our encoder
        // prepend 0x00 for MSB-set values — an MSB-set serial therefore
        // yields two different TBS encodings and real devices reject the
        // signature (observed: Tapo P110M, M6.6.5, IM status 0x85 on
        // AddTrustedRootCertificate). chip masks generated serials the
        // same way.
        serial[0] &= 0x7F;

        let rcac_subject = DistinguishedName::new(vec![DnAttribute::RcacId(rcac_id)]);
        let rcac_extensions = Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(true, Some(1))))
            .key_usage(Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN))
            .subject_key_identifier(Some(ski_id))
            // Self-signed root: AKI = SKI. RFC 5280 §4.2.1.1 would allow
            // omission, but Matter (spec §6.5.11.3) makes the extension
            // MANDATORY on every Matter cert — matter.js's
            // TlvRootCertificate rejects an RCAC without it, and real
            // devices answer AddTrustedRootCertificate with IM status 0x85
            // INVALID_COMMAND (observed: Tapo P110M, M6.6.5 validation).
            .authority_key_identifier(Some(ski_id))
            .build();

        let unsigned = MatterCertificate::builder()
            .serial(serial)
            .issuer(rcac_subject.clone())
            .subject(rcac_subject)
            .validity(not_before, not_after)
            .public_key(root_public_key.clone())
            .extensions(rcac_extensions)
            .build_unsigned()
            .map_err(NocError::CertBuild)?;
        let tbs = unsigned.tbs_der().map_err(NocError::CertBuild)?;
        let sig = root_signer
            .sign_p256_sha256(&tbs)
            .map_err(NocError::SigningFailed)?;
        let root_cert = unsigned.assemble(sig);

        // Sanity check: the cert we just built must verify under its own
        // public key. Catches any TBS-DER / signature shape regression
        // early — before this fabric gets used to issue real NOCs.
        root_cert
            .verify_signed_by(&root_public_key)
            .map_err(NocError::CertBuild)?;

        let mut ipk = [0u8; 16];
        rng.fill(&mut ipk)?;

        Ok(Self {
            fabric_id,
            root_public_key,
            root_signer,
            root_cert,
            icac_signer: None,
            icac_cert: None,
            identity_protection_key: ipk,
        })
    }

    /// Issue a NOC under this fabric. Forwards to
    /// [`crate::noc::issue_noc`].
    ///
    /// # Errors
    ///
    /// See [`NocError`] variants `DnAttributeOverflow`, `CertBuild`,
    /// `SigningFailed`, and `Rng`.
    pub fn issue_noc(
        &self,
        verified_csr: &crate::noc::csr::VerifiedCsr,
        node_id: u64,
        case_authenticated_tags: &[u32],
        validity: (MatterTime, MatterTime),
        rng: &dyn NocRng,
    ) -> Result<MatterCertificate, NocError> {
        crate::noc::issuer::issue_noc(
            self,
            verified_csr,
            node_id,
            case_authenticated_tags,
            validity,
            rng,
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::noc::error::SystemNocRng;
    use matter_crypto::RingSigner;

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
    fn rcac_serial_top_bit_is_clear() {
        // chip's TLV→X.509 conversion copies the serial VERBATIM as the
        // INTEGER value octets, while matter.js (and our encoder) prepend
        // 0x00 when the first byte's MSB is set. An MSB-set serial therefore
        // produces two different TBS encodings and the device rejects the
        // self-signature (observed: Tapo P110M, M6.6.5 — IM status 0x85 on
        // AddTrustedRootCertificate). chip sidesteps this by never generating
        // MSB-set serials; we must do the same.
        let (signer, _pkcs8) = RingSigner::generate().unwrap();
        let signer: Arc<dyn Signer> = Arc::new(signer);
        let fabric = FabricRecord::new_root_only(
            0xFEDC_BA98_7654_3210,
            signer,
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
            42,
            &AllOnesRng,
        )
        .unwrap();
        let serial = fabric.root_cert.serial();
        assert_eq!(
            serial[0] & 0x80,
            0,
            "generated RCAC serial must have the top bit clear"
        );
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn rcac_carries_mandatory_authority_key_identifier() {
        // Matter (spec §6.5.11.3) makes authority-key-id MANDATORY on every
        // Matter cert, including the self-signed RCAC (AKI = SKI) — stricter
        // than RFC 5280's omission allowance. matter.js's TlvRootCertificate
        // rejects an RCAC without it, and real devices reject
        // AddTrustedRootCertificate with IM status 0x85 INVALID_COMMAND
        // (observed: Tapo P110M, M6.6.5 validation).
        let (signer, _pkcs8) = RingSigner::generate().unwrap();
        let signer: Arc<dyn Signer> = Arc::new(signer);
        let fabric = FabricRecord::new_root_only(
            0xFEDC_BA98_7654_3210,
            signer,
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
            42,
            &SystemNocRng,
        )
        .unwrap();
        let exts = fabric.root_cert.extensions();
        let aki = exts
            .authority_key_identifier
            .expect("RCAC must carry authority-key-id");
        assert_eq!(
            Some(aki),
            exts.subject_key_identifier,
            "self-signed root: AKI must equal SKI"
        );
    }

    #[test]
    fn new_root_only_produces_self_verifying_rcac() {
        let (signer, _pkcs8) = RingSigner::generate().unwrap();
        let signer: Arc<dyn Signer> = Arc::new(signer);
        let fabric = FabricRecord::new_root_only(
            0xFEDC_BA98_7654_3210,
            signer,
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
            42,
            &SystemNocRng,
        )
        .unwrap();
        // RCAC verifies under its own key.
        fabric
            .root_cert
            .verify_signed_by(&fabric.root_public_key)
            .unwrap();
        // No ICAC slots populated.
        assert!(fabric.icac_signer.is_none());
        assert!(fabric.icac_cert.is_none());
        // IPK is not all-zero.
        assert_ne!(fabric.identity_protection_key, [0u8; 16]);
    }
}

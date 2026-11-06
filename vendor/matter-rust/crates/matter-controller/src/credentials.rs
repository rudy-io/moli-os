//! Assemble CASE operational credentials from a persisted [`FabricEntry`].
//!
//! The controller authenticates to devices as its stable commissioner
//! identity (minted once, at fabric creation). The IPK handed to CASE is the *derived*
//! operational IPK — `HKDF(epoch_key, compressed_fabric_id)` — NOT the raw
//! epoch key stored in the fabric (real devices reject the raw key).

use matter_cert::{TrustAnchor, TrustedRoots};
use matter_crypto::{derive_compressed_fabric_id, derive_operational_ipk, CaseCredentials};

use crate::error::Error;
use crate::state::FabricEntry;

/// Build the CASE credentials, trusted roots, and compressed fabric id the
/// actor needs to open an operational session to a device on this fabric.
///
/// # Errors
///
/// Returns [`Error::Signer`] if the commissioner key cannot be
/// reconstructed, or [`Error::Operational`] if IPK / compressed-fabric-id
/// derivation fails.
pub(crate) fn operational_credentials(
    fabric: &FabricEntry,
) -> Result<(CaseCredentials, TrustedRoots, [u8; 8]), Error> {
    let signer = fabric.commissioner_signer()?; // RingSigner: impl CaseSigner
    let rcac_public_key = *fabric.rcac_cert.public_key().as_bytes();

    let compressed = derive_compressed_fabric_id(&rcac_public_key, fabric.fabric_id)
        .map_err(|e| Error::Operational(e.to_string()))?;
    let operational_ipk = derive_operational_ipk(&fabric.ipk, &compressed)
        .map_err(|e| Error::Operational(e.to_string()))?;

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&fabric.rcac_cert));

    let credentials = CaseCredentials {
        noc: fabric.commissioner.noc.clone(),
        // On a 3-tier fabric the commissioner NOC is signed under the ICAC
        // (its issuer DN is the ICAC's subject), so the device — which trusts
        // only the RCAC — needs the ICAC to assemble NOC -> ICAC -> RCAC and
        // accept the operational CASE handshake. Present it whenever the
        // fabric carries one; a flat RCAC -> NOC fabric sends `None`,
        // unchanged.
        icac: fabric.icac.as_ref().map(|i| i.cert.clone()),
        signer: Box::new(signer),
        fabric_id: fabric.fabric_id,
        node_id: fabric.commissioner.node_id,
        ipk: operational_ipk,
        rcac_public_key,
    };

    Ok((credentials, roots, compressed))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::fabric::{create_fabric, FabricConfig};
    use matter_cert::MatterTime;
    use matter_commissioning::SystemNocRng;

    fn sample_fabric() -> FabricEntry {
        let cfg = FabricConfig {
            fabric_id: 0x1122_3344_5566_7788,
            rcac_id: 1,
            commissioner_node_id: 0x0000_0000_0000_0001,
            validity: (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            issue_icac: false,
        };
        create_fabric(&cfg, &SystemNocRng).expect("create_fabric")
    }

    fn sample_fabric_with_icac() -> FabricEntry {
        let cfg = FabricConfig {
            fabric_id: 0x1122_3344_5566_7788,
            rcac_id: 1,
            commissioner_node_id: 0x0000_0000_0000_0001,
            validity: (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            issue_icac: true,
        };
        create_fabric(&cfg, &SystemNocRng).expect("create_fabric")
    }

    #[test]
    fn credentials_carry_the_icac_for_a_three_tier_fabric() {
        let fabric = sample_fabric_with_icac();
        let icac = fabric.icac.as_ref().expect("3-tier fabric has an ICAC");
        let (creds, _roots, _compressed) = operational_credentials(&fabric).expect("creds");
        // Without the ICAC the device (which trusts only the RCAC) cannot
        // validate the ICAC-signed commissioner NOC and rejects the CASE
        // handshake, so operational sessions must present it.
        let sent = creds
            .icac
            .as_ref()
            .expect("operational creds must carry the ICAC");
        assert_eq!(
            sent.to_tlv().unwrap(),
            icac.cert.to_tlv().unwrap(),
            "the credentials must carry the fabric's ICAC certificate",
        );
    }

    #[test]
    fn credentials_omit_the_icac_for_a_flat_fabric() {
        let fabric = sample_fabric();
        let (creds, _roots, _compressed) = operational_credentials(&fabric).expect("creds");
        assert!(
            creds.icac.is_none(),
            "a flat RCAC->NOC fabric must not attach an ICAC",
        );
    }

    #[test]
    fn builds_credentials_with_derived_ipk_not_raw() {
        let fabric = sample_fabric();
        let (creds, roots, compressed) = operational_credentials(&fabric).expect("creds");

        // IPK passed to CASE must be the DERIVED operational IPK, not the raw epoch key.
        let expected = derive_operational_ipk(&fabric.ipk, &compressed).unwrap();
        assert_eq!(creds.ipk, expected);
        assert_ne!(
            creds.ipk, fabric.ipk,
            "must not hand CASE the raw epoch key"
        );

        assert_eq!(creds.node_id, fabric.commissioner.node_id);
        assert_eq!(creds.fabric_id, fabric.fabric_id);
        assert_eq!(
            creds.rcac_public_key,
            *fabric.rcac_cert.public_key().as_bytes()
        );
        assert_eq!(roots.iter().count(), 1);
    }

    #[test]
    fn credential_signer_matches_commissioner_noc() {
        let fabric = sample_fabric();
        let (creds, _roots, _c) = operational_credentials(&fabric).expect("creds");
        // The signer's public key must equal the NOC subject key — i.e. CASE
        // will sign Sigma3 with the key the NOC actually certifies.
        assert_eq!(
            creds.signer.public_key().as_bytes(),
            creds.noc.public_key().as_bytes()
        );
    }
}

//! M6.3.2: issued NOC round-trips through `MatterCertificate::from_tlv`
//! and validates against the issuing RCAC via `CertificateChain`.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::sync::Arc;

use matter_cert::{
    CertificateChain, MatterCertificate, MatterTime, PublicKey, TrustAnchor, TrustedRoots,
};
use matter_commissioning::{issue_noc, FabricRecord, SystemNocRng, VerifiedCsr};
use matter_crypto::{RingSigner, Signer};

#[test]
fn issued_noc_validates_against_issuing_rcac() {
    let (root_signer, _) = RingSigner::generate().unwrap();
    let root_signer: Arc<dyn Signer> = Arc::new(root_signer);
    let fabric = FabricRecord::new_root_only(
        1,
        root_signer,
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        7,
        &SystemNocRng,
    )
    .unwrap();

    // Use a fresh ring keypair as the device pubkey.
    let (device_signer, _) = RingSigner::generate().unwrap();
    let verified = VerifiedCsr {
        public_key: PublicKey::from_slice(device_signer.public_key().as_bytes()).unwrap(),
    };

    let noc = issue_noc(
        &fabric,
        &verified,
        0xCAFE_F00D,
        &[],
        (
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
        ),
        &SystemNocRng,
    )
    .unwrap();

    // Round-trip the NOC through TLV.
    let bytes = noc.to_tlv().unwrap();
    let parsed = MatterCertificate::from_tlv(&bytes).unwrap();
    assert_eq!(parsed, noc);

    // Validate the parsed NOC against the fabric's RCAC.
    let chain = [parsed];
    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&fabric.root_cert));
    CertificateChain::new(&chain)
        .validate(&roots, MatterTime::from_unix_secs(1_710_000_000))
        .unwrap();
}

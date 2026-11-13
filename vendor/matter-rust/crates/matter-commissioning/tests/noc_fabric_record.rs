//! Integration test for the M6.3.1 `FabricRecord` surface.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::sync::Arc;

use matter_cert::{MatterCertificate, MatterTime};
use matter_commissioning::{FabricRecord, SystemNocRng};
use matter_crypto::{RingSigner, Signer};

#[test]
fn rcac_round_trips_through_matter_cert_tlv() {
    let (signer, _pkcs8) = RingSigner::generate().unwrap();
    let signer: Arc<dyn Signer> = Arc::new(signer);
    let fabric = FabricRecord::new_root_only(
        1,
        signer,
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        7,
        &SystemNocRng,
    )
    .unwrap();

    // Serialise the RCAC and parse it back — every byte must survive.
    let tlv = fabric.root_cert.to_tlv().unwrap();
    let parsed = MatterCertificate::from_tlv(&tlv).unwrap();
    assert_eq!(parsed, fabric.root_cert);

    // The parsed RCAC verifies under the fabric's root public key.
    parsed.verify_signed_by(&fabric.root_public_key).unwrap();
}

#[test]
fn each_new_root_only_call_mints_a_distinct_ipk() {
    // Two fabrics with the same signer + RNG must still produce
    // independent IPKs (the RNG draws are independent calls).
    let (signer, _pkcs8) = RingSigner::generate().unwrap();
    let signer: Arc<dyn Signer> = Arc::new(signer);
    let f1 = FabricRecord::new_root_only(
        1,
        Arc::clone(&signer),
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        7,
        &SystemNocRng,
    )
    .unwrap();
    let f2 = FabricRecord::new_root_only(
        1,
        signer,
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        7,
        &SystemNocRng,
    )
    .unwrap();
    assert_ne!(f1.identity_protection_key, f2.identity_protection_key);
}

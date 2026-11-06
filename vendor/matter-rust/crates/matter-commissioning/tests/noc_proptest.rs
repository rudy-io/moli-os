//! M6.3.2 property tests.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::sync::Arc;

use matter_cert::{MatterCertificate, MatterTime, PublicKey};
use matter_commissioning::{issue_noc, FabricRecord, SystemNocRng, VerifiedCsr};
use matter_crypto::{RingSigner, Signer};
use proptest::prelude::*;

fn sample_fabric() -> FabricRecord {
    let (signer, _) = RingSigner::generate().unwrap();
    let signer: Arc<dyn Signer> = Arc::new(signer);
    FabricRecord::new_root_only(
        1,
        signer,
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        7,
        &SystemNocRng,
    )
    .unwrap()
}

fn sample_verified_csr() -> VerifiedCsr {
    let (signer, _) = RingSigner::generate().unwrap();
    VerifiedCsr {
        public_key: PublicKey::from_slice(signer.public_key().as_bytes()).unwrap(),
    }
}

proptest! {
    #[test]
    fn issued_noc_roundtrips_for_random_node_id(node_id in any::<u64>()) {
        let fabric = sample_fabric();
        let verified = sample_verified_csr();
        let noc = issue_noc(
            &fabric,
            &verified,
            node_id,
            &[],
            (MatterTime::from_unix_secs(1_700_000_000), MatterTime::NO_EXPIRY),
            &SystemNocRng,
        ).unwrap();
        let tlv = noc.to_tlv().unwrap();
        let parsed = MatterCertificate::from_tlv(&tlv).unwrap();
        prop_assert_eq!(parsed, noc);
    }

    #[test]
    fn issued_noc_roundtrips_for_random_cats(cats in prop::collection::vec(any::<u32>(), 0..=3)) {
        let fabric = sample_fabric();
        let verified = sample_verified_csr();
        let noc = issue_noc(
            &fabric,
            &verified,
            0x4242,
            &cats,
            (MatterTime::from_unix_secs(1_700_000_000), MatterTime::NO_EXPIRY),
            &SystemNocRng,
        ).unwrap();
        let tlv = noc.to_tlv().unwrap();
        let parsed = MatterCertificate::from_tlv(&tlv).unwrap();
        prop_assert_eq!(parsed, noc);
    }
}

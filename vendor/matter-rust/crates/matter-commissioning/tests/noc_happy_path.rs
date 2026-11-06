//! M6.3.2 end-to-end happy-path: synthetic CSR -> verify -> issue NOC.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(unreachable_pub)]

use std::sync::Arc;

use matter_cert::{MatterCertificate, MatterTime};
use matter_codec::{Tag, TlvWriter};
use matter_commissioning::{
    issue_noc, verify_csr_response, FabricRecord, SystemNocRng, VerifiedCsr,
};
use matter_crypto::{RingSigner, Signer};

mod csr_helpers {
    pub fn encode_seq(parts: &[&[u8]]) -> Vec<u8> {
        let mut body = Vec::new();
        for p in parts {
            body.extend_from_slice(p);
        }
        wrap(0x30, &body)
    }
    pub fn encode_oid(arcs: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push((arcs[0] * 40 + arcs[1]) as u8);
        for arc in &arcs[2..] {
            let mut a = *arc;
            let mut digits = vec![(a & 0x7f) as u8];
            a >>= 7;
            while a > 0 {
                digits.push(((a & 0x7f) | 0x80) as u8);
                a >>= 7;
            }
            digits.reverse();
            body.extend_from_slice(&digits);
        }
        wrap(0x06, &body)
    }
    pub fn encode_bit_string(bytes: &[u8]) -> Vec<u8> {
        let mut body = vec![0x00];
        body.extend_from_slice(bytes);
        wrap(0x03, &body)
    }
    pub fn encode_integer_zero() -> Vec<u8> {
        wrap(0x02, &[0x00])
    }
    pub fn encode_implicit_set(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
        let mut body = Vec::new();
        for p in parts {
            body.extend_from_slice(p);
        }
        wrap(0xA0 | tag, &body)
    }
    fn wrap(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&length(body.len()));
        out.extend_from_slice(body);
        out
    }
    fn length(body_len: usize) -> Vec<u8> {
        if body_len < 0x80 {
            vec![body_len as u8]
        } else if body_len <= 0xff {
            vec![0x81, body_len as u8]
        } else if body_len <= 0xffff {
            vec![0x82, (body_len >> 8) as u8, body_len as u8]
        } else {
            panic!("DER length too big for the test helper")
        }
    }
}

fn mint_csr(public_key: &[u8; 65], signing_key: &p256::ecdsa::SigningKey) -> Vec<u8> {
    use csr_helpers::*;
    use p256::ecdsa::{signature::Signer as _, Signature};

    let alg_id = encode_seq(&[
        &encode_oid(&[1, 2, 840, 10045, 2, 1]),
        &encode_oid(&[1, 2, 840, 10045, 3, 1, 7]),
    ]);
    let spki = encode_seq(&[&alg_id, &encode_bit_string(public_key)]);
    let subject = encode_seq(&[]);
    let csr_info = encode_seq(&[
        &encode_integer_zero(),
        &subject,
        &spki,
        &encode_implicit_set(0, &[]),
    ]);
    let sig: Signature = signing_key.sign(&csr_info);
    let sig_der = sig.to_der().as_bytes().to_vec();
    let sig_alg = encode_seq(&[&encode_oid(&[1, 2, 840, 10045, 4, 3, 2])]);
    encode_seq(&[&csr_info, &sig_alg, &encode_bit_string(&sig_der)])
}

fn build_nocsr(csr_der: &[u8], nonce: &[u8; 32]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_bytes(Tag::Context(1), csr_der).unwrap();
    w.put_bytes(Tag::Context(2), nonce).unwrap();
    w.end_container().unwrap();
    buf
}

#[test]
fn end_to_end_csr_verify_then_issue() {
    use p256::ecdsa::{signature::Signer as _, Signature, SigningKey};

    // Build fabric.
    let (root_signer, _) = RingSigner::generate().unwrap();
    let root_signer: Arc<dyn Signer> = Arc::new(root_signer);
    let fabric = FabricRecord::new_root_only(
        0x0000_0000_0000_0001,
        root_signer,
        MatterTime::from_unix_secs(1_700_000_000),
        MatterTime::NO_EXPIRY,
        7,
        &SystemNocRng,
    )
    .unwrap();

    // Device CSR keypair.
    let mut csr_scalar = [0u8; 32];
    csr_scalar[0] = 0x55;
    let csr_key = SigningKey::from_slice(&csr_scalar).unwrap();
    let mut csr_pub = [0u8; 65];
    csr_pub.copy_from_slice(csr_key.verifying_key().to_sec1_point(false).as_bytes());
    let csr_der = mint_csr(&csr_pub, &csr_key);

    // DAC keypair + sign elements || challenge.
    let nonce = [0x42u8; 32];
    let challenge = [0x99u8; 16];
    let elements = build_nocsr(&csr_der, &nonce);
    let mut dac_scalar = [0u8; 32];
    dac_scalar[0] = 0x77;
    let dac_key = SigningKey::from_slice(&dac_scalar).unwrap();
    let mut dac_pub = [0u8; 65];
    dac_pub.copy_from_slice(dac_key.verifying_key().to_sec1_point(false).as_bytes());
    let mut tbs = Vec::with_capacity(elements.len() + 16);
    tbs.extend_from_slice(&elements);
    tbs.extend_from_slice(&challenge);
    let att_sig: Signature = dac_key.sign(&tbs);
    let mut att_sig_arr = [0u8; 64];
    att_sig_arr.copy_from_slice(&att_sig.to_bytes());

    // Verify CSR.
    let verified: VerifiedCsr =
        verify_csr_response(&elements, &att_sig_arr, &nonce, &challenge, &dac_pub).unwrap();

    // Issue NOC.
    let noc: MatterCertificate = issue_noc(
        &fabric,
        &verified,
        0xCAFE_BABE_DEAD_BEEF,
        &[],
        (
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
        ),
        &SystemNocRng,
    )
    .unwrap();

    // NOC's subject pubkey matches the CSR's pubkey.
    assert_eq!(noc.public_key().as_bytes(), &csr_pub);
    // NOC verifies under the fabric's root key.
    noc.verify_signed_by(&fabric.root_public_key).unwrap();
}

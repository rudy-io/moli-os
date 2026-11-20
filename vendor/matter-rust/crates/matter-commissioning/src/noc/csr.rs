//! NOCSR (Node Operational CSR) parsing and verification.
//!
//! Matter Core Spec §11.18.5.6: a device responds to the commissioner's
//! `CSRRequest` with a `CSRResponse` containing two fields —
//!
//! - `nocsr_elements`: a TLV blob carrying the embedded PKCS#10 CSR
//!   bytes, the commissioner-issued `CSRNonce` (echoed back), and three
//!   optional vendor-reserved byte strings.
//! - `attestation_signature`: a 64-byte raw ECDSA-P256-SHA256 signature
//!   produced by the device's DAC private key over
//!   `nocsr_elements || attestation_challenge`.
//!
//! The commissioner verifies all three of:
//!   1. The PKCS#10 CSR's own self-signature (bound by its embedded
//!      public key — this is what binds the to-be-issued NOC pubkey
//!      to a device-held private key).
//!   2. The `csr_nonce` field equals the value the commissioner just
//!      sent in `CSRRequest` (replay-then-substitute defence).
//!   3. The DAC's signature over `nocsr_elements || attestation_challenge`
//!      (kills network-MITM substitution of the CSR mid-flight).
//!
//! All three pass — the caller receives a [`VerifiedCsr`] whose mere
//! existence is proof verification happened.

#![forbid(unsafe_code)]

use matter_cert::PublicKey;

use crate::noc::error::NocError;

/// Decoded `nocsr_elements` TLV (spec §11.18.5.6, table 113).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NocsrElements {
    /// Embedded PKCS#10 `CertificateSigningRequest`, DER-encoded.
    pub csr_der: Vec<u8>,
    /// 32-byte `CSRNonce` the commissioner sent in `CSRRequest` and the
    /// device echoes back.
    pub csr_nonce: [u8; 32],
    /// Optional vendor-reserved byte string (tag 3 in the NOCSR TLV).
    pub vendor_reserved_1: Option<Vec<u8>>,
    /// Optional vendor-reserved byte string (tag 4).
    pub vendor_reserved_2: Option<Vec<u8>>,
    /// Optional vendor-reserved byte string (tag 5).
    pub vendor_reserved_3: Option<Vec<u8>>,
}

/// A parsed PKCS#10 CSR. Construction validates the DER structure and
/// the self-signature.
#[derive(Debug, Clone)]
pub struct ParsedCsr {
    /// The CSR's embedded public key (P-256 SEC1 uncompressed).
    pub public_key: PublicKey,
}

/// A CSR whose **all three** signature checks have passed. The type
/// existing is proof of verification; pass it to
/// [`crate::noc::issue_noc`] to mint the NOC.
#[derive(Debug, Clone)]
pub struct VerifiedCsr {
    /// The device's operational public key — bound to the verified DAC.
    pub public_key: PublicKey,
}

/// Parse a `nocsr_elements` TLV blob.
///
/// # Errors
///
/// Returns [`NocError::NocsrParse`] on malformed input.
pub fn parse_nocsr(elements_tlv: &[u8]) -> Result<NocsrElements, NocError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};

    fn parse_err<T: Into<Box<dyn std::error::Error + Send + Sync + 'static>>>(e: T) -> NocError {
        NocError::NocsrParse(e.into())
    }

    let mut reader = TlvReader::new(elements_tlv);
    match reader.next().map_err(parse_err)? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => {
            return Err(parse_err(
                "NOCSR outer envelope must be anonymous structure",
            ))
        }
    }

    let mut csr_der: Option<Vec<u8>> = None;
    let mut csr_nonce: Option<[u8; 32]> = None;
    let mut vr1: Option<Vec<u8>> = None;
    let mut vr2: Option<Vec<u8>> = None;
    let mut vr3: Option<Vec<u8>> = None;

    loop {
        match reader.next().map_err(parse_err)? {
            None => return Err(parse_err("NOCSR TLV ended without container close")),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(t),
                value: Value::Bytes(b),
            }) => match t {
                1 => {
                    if csr_der.is_some() {
                        return Err(parse_err("duplicate csr field"));
                    }
                    csr_der = Some(b);
                }
                2 => {
                    if csr_nonce.is_some() {
                        return Err(parse_err("duplicate csr_nonce field"));
                    }
                    let arr: [u8; 32] = b
                        .as_slice()
                        .try_into()
                        .map_err(|_| parse_err("csr_nonce must be exactly 32 bytes"))?;
                    csr_nonce = Some(arr);
                }
                3 => {
                    if vr1.is_some() {
                        return Err(parse_err("duplicate vendor_reserved_1"));
                    }
                    vr1 = Some(b);
                }
                4 => {
                    if vr2.is_some() {
                        return Err(parse_err("duplicate vendor_reserved_2"));
                    }
                    vr2 = Some(b);
                }
                5 => {
                    if vr3.is_some() {
                        return Err(parse_err("duplicate vendor_reserved_3"));
                    }
                    vr3 = Some(b);
                }
                other => return Err(parse_err(format!("unknown NOCSR context tag {other}"))),
            },
            Some(_) => return Err(parse_err("unexpected element inside NOCSR")),
        }
    }

    Ok(NocsrElements {
        csr_der: csr_der.ok_or_else(|| parse_err("missing csr field"))?,
        csr_nonce: csr_nonce.ok_or_else(|| parse_err("missing csr_nonce field"))?,
        vendor_reserved_1: vr1,
        vendor_reserved_2: vr2,
        vendor_reserved_3: vr3,
    })
}

/// Parse the embedded PKCS#10 CSR and verify its self-signature.
///
/// # Errors
///
/// - [`NocError::CsrParse`] on malformed DER.
/// - [`NocError::InvalidCsrPublicKey`] if the embedded public key is not P-256.
/// - [`NocError::BadCsrSelfSignature`] if the CSR's signature does not verify.
pub fn parse_and_verify_csr(csr_der: &[u8]) -> Result<ParsedCsr, NocError> {
    use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
    use x509_parser::prelude::*;

    fn parse_err<T: Into<Box<dyn std::error::Error + Send + Sync + 'static>>>(e: T) -> NocError {
        NocError::CsrParse(e.into())
    }

    let (rest, csr) = X509CertificationRequest::from_der(csr_der)
        .map_err(|e| parse_err(format!("PKCS#10 DER parse failed: {e}")))?;
    if !rest.is_empty() {
        return Err(parse_err("trailing bytes after PKCS#10 envelope"));
    }

    // Extract the public key bytes — must be P-256 SEC1 uncompressed (65 bytes, 0x04 prefix).
    let spki = &csr.certification_request_info.subject_pki;
    let pk_bytes = spki.subject_public_key.data.as_ref();
    if pk_bytes.len() != 65 || pk_bytes[0] != 0x04 {
        return Err(NocError::InvalidCsrPublicKey);
    }
    let mut pk_arr = [0u8; 65];
    pk_arr.copy_from_slice(pk_bytes);
    let public_key = PublicKey::new(pk_arr).map_err(|_| NocError::InvalidCsrPublicKey)?;

    // Compute the TBS bytes that the CSR signature covers — for PKCS#10
    // these are the raw DER bytes of `certificationRequestInfo`, which
    // x509-parser surfaces as a slice of the original buffer.
    let tbs = csr.certification_request_info.raw;

    // Signature is ECDSA-P256-SHA256 in ASN.1 DER form (PKCS#10 standard).
    let signature_bytes = csr.signature_value.data.as_ref();
    let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, &pk_arr[..]);
    key.verify(tbs, signature_bytes)
        .map_err(|_| NocError::BadCsrSelfSignature)?;

    Ok(ParsedCsr { public_key })
}

/// Full `CSRResponse` verification — atomic, three signature checks.
///
/// 1. PKCS#10 self-signature on the embedded CSR.
/// 2. `CSRNonce` echo equals `expected_csr_nonce`.
/// 3. DAC attestation signature over `elements_tlv || attestation_challenge`.
///
/// All three must pass.
///
/// # Errors
///
/// See [`NocError`] variants `BadCsrSelfSignature`, `NonceMismatch`,
/// `BadCsrAttestationSignature`, plus the parse variants.
pub fn verify_csr_response(
    elements_tlv: &[u8],
    attestation_signature: &[u8; 64],
    expected_csr_nonce: &[u8; 32],
    attestation_challenge: &[u8; 16],
    dac_public_key: &[u8],
) -> Result<VerifiedCsr, NocError> {
    use crate::attestation::verify_dac_signed_elements;

    // 1. Parse the NOCSR envelope. Failures surface as NocsrParse.
    let elements = parse_nocsr(elements_tlv)?;

    // 2. Parse + verify the embedded PKCS#10 CSR's self-signature.
    let parsed = parse_and_verify_csr(&elements.csr_der)?;

    // 3. Check that the device echoed the commissioner's nonce.
    //    Constant-time compare to avoid leaking the index at which
    //    the bytes diverged.
    if !ct_eq(&elements.csr_nonce, expected_csr_nonce) {
        return Err(NocError::NonceMismatch);
    }

    // 4. Verify the DAC attestation signature over (elements || challenge).
    verify_dac_signed_elements(
        elements_tlv,
        attestation_challenge,
        dac_public_key,
        attestation_signature,
    )
    .map_err(|_| NocError::BadCsrAttestationSignature)?;

    Ok(VerifiedCsr {
        public_key: parsed.public_key,
    })
}

/// Constant-time equality on two 32-byte arrays.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff: u8 = 0;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::manual_assert
)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use matter_codec::{Tag, TlvWriter};

    /// Compose a synthetic NOCSR TLV blob from raw fields.
    fn write_nocsr(
        csr_der: &[u8],
        nonce: &[u8; 32],
        vr1: Option<&[u8]>,
        vr2: Option<&[u8]>,
        vr3: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), csr_der).unwrap();
        w.put_bytes(Tag::Context(2), nonce).unwrap();
        if let Some(b) = vr1 {
            w.put_bytes(Tag::Context(3), b).unwrap();
        }
        if let Some(b) = vr2 {
            w.put_bytes(Tag::Context(4), b).unwrap();
        }
        if let Some(b) = vr3 {
            w.put_bytes(Tag::Context(5), b).unwrap();
        }
        w.end_container().unwrap();
        buf
    }

    #[test]
    fn parse_nocsr_roundtrips_minimal() {
        let csr = b"synthetic-csr-der-bytes".to_vec();
        let nonce = [0x42u8; 32];
        let tlv = write_nocsr(&csr, &nonce, None, None, None);

        let parsed = parse_nocsr(&tlv).unwrap();
        assert_eq!(parsed.csr_der, csr);
        assert_eq!(parsed.csr_nonce, nonce);
        assert_eq!(parsed.vendor_reserved_1, None);
        assert_eq!(parsed.vendor_reserved_2, None);
        assert_eq!(parsed.vendor_reserved_3, None);
    }

    #[test]
    fn parse_nocsr_roundtrips_with_vendor_reserved() {
        let csr = b"more-csr-bytes".to_vec();
        let nonce = [0x99u8; 32];
        let tlv = write_nocsr(&csr, &nonce, Some(b"vr1"), None, Some(b"vr3"));

        let parsed = parse_nocsr(&tlv).unwrap();
        assert_eq!(parsed.csr_der, csr);
        assert_eq!(parsed.csr_nonce, nonce);
        assert_eq!(parsed.vendor_reserved_1.as_deref(), Some(&b"vr1"[..]));
        assert_eq!(parsed.vendor_reserved_2, None);
        assert_eq!(parsed.vendor_reserved_3.as_deref(), Some(&b"vr3"[..]));
    }

    #[test]
    fn parse_nocsr_rejects_missing_csr_field() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(2), &[0u8; 32]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(parse_nocsr(&buf), Err(NocError::NocsrParse(_))));
    }

    #[test]
    fn parse_nocsr_rejects_missing_nonce_field() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), b"some-csr").unwrap();
        w.end_container().unwrap();
        assert!(matches!(parse_nocsr(&buf), Err(NocError::NocsrParse(_))));
    }

    #[test]
    fn parse_nocsr_rejects_wrong_nonce_length() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), b"some-csr").unwrap();
        w.put_bytes(Tag::Context(2), &[0u8; 16]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(parse_nocsr(&buf), Err(NocError::NocsrParse(_))));
    }

    #[test]
    fn parse_nocsr_rejects_truncated_input() {
        let csr = b"hello".to_vec();
        let nonce = [0u8; 32];
        let mut tlv = write_nocsr(&csr, &nonce, None, None, None);
        tlv.pop(); // drop the end-of-container byte
        assert!(matches!(parse_nocsr(&tlv), Err(NocError::NocsrParse(_))));
    }

    /// Mint a synthetic PKCS#10 CSR for testing. Returns
    /// `(csr_der, public_key_bytes)`.
    fn mint_pkcs10_csr(seed: u8) -> (Vec<u8>, [u8; 65]) {
        use p256::ecdsa::{signature::Signer, Signature, SigningKey};

        // Deterministic 32-byte big-endian scalar.
        let mut scalar = [0u8; 32];
        scalar[0] = seed;
        let signing_key = SigningKey::from_slice(&scalar).unwrap();
        let verifying_key = signing_key.verifying_key();
        let encoded = verifying_key.to_sec1_point(false);
        let mut public_key = [0u8; 65];
        public_key.copy_from_slice(encoded.as_bytes());

        let tbs_csr_info = build_pkcs10_csr_info(&public_key);
        let signature: Signature = signing_key.sign(&tbs_csr_info);
        let sig_der = signature.to_der().as_bytes().to_vec();

        let csr_der = wrap_pkcs10_csr(&tbs_csr_info, &sig_der);
        (csr_der, public_key)
    }

    /// Encode CertificationRequestInfo for a P-256 public key with an
    /// empty subject and empty attribute set.
    fn build_pkcs10_csr_info(public_key_sec1: &[u8; 65]) -> Vec<u8> {
        let alg_id = encode_der_sequence(&[
            &encode_der_oid(&[1, 2, 840, 10045, 2, 1]),
            &encode_der_oid(&[1, 2, 840, 10045, 3, 1, 7]),
        ]);
        let pk_bit_string = encode_der_bit_string(public_key_sec1);
        let subject_pk_info = encode_der_sequence(&[&alg_id, &pk_bit_string]);
        let subject = encode_der_sequence(&[]);
        let version = encode_der_integer_zero();
        let attributes = encode_der_context_implicit_set(0, &[]);
        encode_der_sequence(&[&version, &subject, &subject_pk_info, &attributes])
    }

    fn wrap_pkcs10_csr(tbs: &[u8], signature_der: &[u8]) -> Vec<u8> {
        let alg_id = encode_der_sequence(&[&encode_der_oid(&[1, 2, 840, 10045, 4, 3, 2])]);
        let sig_bit_string = encode_der_bit_string(signature_der);
        encode_der_sequence(&[tbs, &alg_id, &sig_bit_string])
    }

    fn encode_der_length(body_len: usize) -> Vec<u8> {
        if body_len < 0x80 {
            vec![body_len as u8]
        } else if body_len <= 0xff {
            vec![0x81, body_len as u8]
        } else if body_len <= 0xffff {
            vec![0x82, (body_len >> 8) as u8, body_len as u8]
        } else {
            panic!("test helper: length too big");
        }
    }

    fn encode_der_tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&encode_der_length(body.len()));
        out.extend_from_slice(body);
        out
    }

    fn encode_der_sequence(parts: &[&[u8]]) -> Vec<u8> {
        let mut body = Vec::new();
        for p in parts {
            body.extend_from_slice(p);
        }
        encode_der_tlv(0x30, &body)
    }

    fn encode_der_integer_zero() -> Vec<u8> {
        encode_der_tlv(0x02, &[0x00])
    }

    fn encode_der_bit_string(bytes: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(bytes.len() + 1);
        body.push(0x00);
        body.extend_from_slice(bytes);
        encode_der_tlv(0x03, &body)
    }

    fn encode_der_context_implicit_set(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
        let mut body = Vec::new();
        for p in parts {
            body.extend_from_slice(p);
        }
        encode_der_tlv(0xA0 | tag, &body)
    }

    fn encode_der_oid(arcs: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        if arcs.len() < 2 {
            panic!("OID needs at least two arcs");
        }
        body.push((arcs[0] * 40 + arcs[1]) as u8);
        for arc in &arcs[2..] {
            let mut a = *arc;
            let mut digits = Vec::new();
            digits.push((a & 0x7f) as u8);
            a >>= 7;
            while a > 0 {
                digits.push(((a & 0x7f) | 0x80) as u8);
                a >>= 7;
            }
            digits.reverse();
            body.extend_from_slice(&digits);
        }
        encode_der_tlv(0x06, &body)
    }

    #[test]
    fn parse_and_verify_csr_accepts_valid_synthetic() {
        let (csr_der, public_key) = mint_pkcs10_csr(0x11);
        let parsed = parse_and_verify_csr(&csr_der).unwrap();
        assert_eq!(parsed.public_key.as_bytes(), &public_key);
    }

    #[test]
    fn parse_and_verify_csr_rejects_flipped_signature_bit() {
        let (mut csr_der, _) = mint_pkcs10_csr(0x12);
        let last = csr_der.len() - 1;
        csr_der[last] ^= 0x01;
        assert!(matches!(
            parse_and_verify_csr(&csr_der),
            Err(NocError::BadCsrSelfSignature)
        ));
    }

    #[test]
    fn parse_and_verify_csr_rejects_garbage_input() {
        assert!(matches!(
            parse_and_verify_csr(&[0x30, 0x00]),
            Err(NocError::CsrParse(_))
        ));
    }

    fn build_signed_csr_response(
        dac_seed: u8,
        csr_seed: u8,
        nonce: &[u8; 32],
        challenge: &[u8; 16],
    ) -> (Vec<u8>, [u8; 64], [u8; 65]) {
        use p256::ecdsa::{signature::Signer, Signature, SigningKey};

        let (csr_der, _) = mint_pkcs10_csr(csr_seed);
        let elements_tlv = write_nocsr(&csr_der, nonce, None, None, None);

        let mut scalar = [0u8; 32];
        scalar[0] = dac_seed;
        let dac_key = SigningKey::from_slice(&scalar).unwrap();
        let dac_pub = dac_key.verifying_key().to_sec1_point(false);
        let mut dac_pub_arr = [0u8; 65];
        dac_pub_arr.copy_from_slice(dac_pub.as_bytes());

        let mut tbs = Vec::with_capacity(elements_tlv.len() + 16);
        tbs.extend_from_slice(&elements_tlv);
        tbs.extend_from_slice(challenge);
        let sig: Signature = dac_key.sign(&tbs);
        let mut att_sig = [0u8; 64];
        att_sig.copy_from_slice(&sig.to_bytes());

        (elements_tlv, att_sig, dac_pub_arr)
    }

    #[test]
    fn verify_csr_response_happy_path() {
        let nonce = [0x33u8; 32];
        let challenge = [0x77u8; 16];
        let (elements, sig, dac_pub) = build_signed_csr_response(0x21, 0x22, &nonce, &challenge);
        let verified = verify_csr_response(&elements, &sig, &nonce, &challenge, &dac_pub).unwrap();
        // The verified CSR's public key must be the CSR's embedded key,
        // not the DAC's.
        assert_ne!(verified.public_key.as_bytes(), &dac_pub);
    }

    #[test]
    fn verify_csr_response_rejects_nonce_mismatch() {
        let nonce_sent = [0x33u8; 32];
        let nonce_echoed = [0x44u8; 32]; // device echoes a different nonce
        let challenge = [0x77u8; 16];
        let (elements, sig, dac_pub) =
            build_signed_csr_response(0x23, 0x24, &nonce_echoed, &challenge);
        let err =
            verify_csr_response(&elements, &sig, &nonce_sent, &challenge, &dac_pub).unwrap_err();
        assert!(matches!(err, NocError::NonceMismatch), "got: {err:?}");
    }

    #[test]
    fn verify_csr_response_rejects_bad_attestation_signature() {
        let nonce = [0x33u8; 32];
        let challenge = [0x77u8; 16];
        let (elements, mut sig, dac_pub) =
            build_signed_csr_response(0x25, 0x26, &nonce, &challenge);
        sig[0] ^= 0x80;
        let err = verify_csr_response(&elements, &sig, &nonce, &challenge, &dac_pub).unwrap_err();
        assert!(
            matches!(err, NocError::BadCsrAttestationSignature),
            "got: {err:?}"
        );
    }

    #[test]
    fn verify_csr_response_rejects_wrong_challenge() {
        let nonce = [0x33u8; 32];
        let challenge = [0x77u8; 16];
        let (elements, sig, dac_pub) = build_signed_csr_response(0x29, 0x2A, &nonce, &challenge);
        // Same elements + sig, but verify with a stale challenge.
        let wrong_challenge = [0u8; 16];
        let err =
            verify_csr_response(&elements, &sig, &nonce, &wrong_challenge, &dac_pub).unwrap_err();
        assert!(
            matches!(err, NocError::BadCsrAttestationSignature),
            "got: {err:?}"
        );
    }
}

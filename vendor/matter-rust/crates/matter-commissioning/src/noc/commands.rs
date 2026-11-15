//! `OperationalCredentials` cluster (`0x003E`) command payload codecs —
//! NOC-issuance subset.
//!
//! Spec §11.18. M6.3 covers only what's needed to walk a device from
//! `CSRRequest` through `AddNOC`. The rest of the cluster
//! (`AttestationRequest` / `AttestationResponse`,
//! `CertificateChainRequest` / `CertificateChainResponse`,
//! `RemoveFabric`, etc.) lives partly in M6.2 (attestation) and partly
//! in M6.4 (state machine wire-up).

#![forbid(unsafe_code)]

use crate::clusters::network_commissioning::truncate_utf8;
use crate::noc::error::NocError;

/// Decoded `CSRResponse` (spec §11.18.5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrResponse {
    /// Opaque `NOCSRElements` TLV blob — pass to
    /// [`crate::noc::verify_csr_response`].
    pub nocsr_elements: Vec<u8>,
    /// 64-byte raw ECDSA-P256-SHA256 signature by the device's DAC over
    /// `nocsr_elements || attestation_challenge`.
    pub attestation_signature: [u8; 64],
}

/// Decoded `NOCResponse` (spec §11.18.5.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NocResponse {
    /// `NodeOperationalCertStatusEnum` (spec §11.18.6.1).
    pub status: u8,
    /// `FabricIndex` assigned to the new fabric (present iff `status == 0`).
    pub fabric_index: Option<u8>,
    /// Optional debug text. Spec §11.18.5.11 caps at 128 chars; capped at
    /// the spec's 512-octet bound at decode. **Device-controlled free
    /// text** — log deliberately.
    pub debug_text: Option<String>,
}

/// Encode `AttestationRequest` (spec §11.18.5.1).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_attestation_request(nonce: &[u8; 32]) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), nonce)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Decode `AttestationResponse` (spec §11.18.5.2).
///
/// Returns the existing `crate::attestation::AttestationResponse`
/// struct — both fields (the opaque `attestation_elements` TLV blob
/// and the 64-byte raw ECDSA signature) are populated from the wire
/// payload's context-tagged fields 0 and 1.
///
/// # Errors
///
/// Returns [`NocError::ClusterCodec`] on malformed input — including a
/// signature payload that is not exactly 64 bytes.
pub fn decode_attestation_response(
    tlv: &[u8],
) -> Result<crate::attestation::AttestationResponse, NocError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => {
            return Err(NocError::MalformedResponse(
                "AttestationResponse: expected an anonymous structure",
            ))
        }
    }
    let mut elements: Option<Vec<u8>> = None;
    let mut sig: Option<[u8; 64]> = None;
    loop {
        match reader.next()? {
            None => {
                return Err(NocError::ClusterCodec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bytes(b),
            }) => {
                if elements.is_some() {
                    return Err(NocError::MalformedResponse(
                        "AttestationResponse: duplicate AttestationElements field",
                    ));
                }
                elements = Some(b);
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Bytes(b),
            }) => {
                if sig.is_some() {
                    return Err(NocError::MalformedResponse(
                        "AttestationResponse: duplicate Signature field",
                    ));
                }
                let arr: [u8; 64] = b.as_slice().try_into().map_err(|_| {
                    NocError::MalformedResponse(
                        "AttestationResponse: Signature is not exactly 64 bytes",
                    )
                })?;
                sig = Some(arr);
            }
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader.skip_container()?,
            // Forward-compat: ignore unknown future scalar fields.
            Some(Element::Scalar { .. }) => {}
            Some(_) => {
                return Err(NocError::MalformedResponse(
                    "AttestationResponse: unexpected element",
                ))
            }
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader.finish()?;
    Ok(crate::attestation::AttestationResponse {
        attestation_elements: elements.ok_or(NocError::MalformedResponse(
            "AttestationResponse: missing AttestationElements field",
        ))?,
        signature: sig.ok_or(NocError::MalformedResponse(
            "AttestationResponse: missing Signature field",
        ))?,
    })
}

/// Argument to `CertificateChainRequest` — `CertificateChainTypeEnum`
/// (spec §11.18.5.2): 1 = DAC, 2 = PAI. Confirmed against a real device
/// (Tapo P110M, M6.6.5 validation).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CertChainType {
    /// DAC (Device Attestation Certificate) — the leaf.
    Dac = 0x01,
    /// PAI (Product Attestation Intermediate) certificate.
    Pai = 0x02,
}

/// Decoded `CertificateChainResponse` (spec §11.18.5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateChainResponse {
    /// Raw DER bytes of the requested certificate.
    pub certificate: Vec<u8>,
}

/// Encode `CertificateChainRequest` (spec §11.18.5.3).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_certificate_chain_request(cert_type: CertChainType) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(cert_type as u8))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Decode `CertificateChainResponse` (spec §11.18.5.4).
///
/// # Errors
///
/// Returns [`NocError::ClusterCodec`] on malformed input.
pub fn decode_certificate_chain_response(tlv: &[u8]) -> Result<CertificateChainResponse, NocError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => {
            return Err(NocError::MalformedResponse(
                "CertificateChainResponse: expected an anonymous structure",
            ))
        }
    }
    let mut cert: Option<Vec<u8>> = None;
    loop {
        match reader.next()? {
            None => {
                return Err(NocError::ClusterCodec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bytes(b),
            }) => {
                if cert.is_some() {
                    return Err(NocError::MalformedResponse(
                        "CertificateChainResponse: duplicate Certificate field",
                    ));
                }
                cert = Some(b);
            }
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader.skip_container()?,
            // Forward-compat: ignore unknown future scalar fields.
            Some(Element::Scalar { .. }) => {}
            Some(_) => {
                return Err(NocError::MalformedResponse(
                    "CertificateChainResponse: unexpected element",
                ))
            }
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader.finish()?;
    Ok(CertificateChainResponse {
        certificate: cert.ok_or(NocError::MalformedResponse(
            "CertificateChainResponse: missing Certificate field",
        ))?,
    })
}

/// Encode `CSRRequest` (spec §11.18.5.5).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_csr_request(nonce: &[u8; 32], is_for_update_noc: bool) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    // Spec §11.18.5.5 fields:
    //   0: csr_nonce         (octet string, 32 bytes)
    //   1: is_for_update_noc (bool, optional; default false)
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), nonce)
        .expect("infallible: vec writer");
    if is_for_update_noc {
        w.put_bool(Tag::Context(1), true)
            .expect("infallible: vec writer");
    }
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Decode `CSRResponse` (spec §11.18.5.7).
///
/// # Errors
///
/// Returns [`NocError::ClusterCodec`] on malformed input.
pub fn decode_csr_response(tlv: &[u8]) -> Result<CsrResponse, NocError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => {
            return Err(NocError::MalformedResponse(
                "CSRResponse: expected an anonymous structure",
            ))
        }
    }

    let mut nocsr: Option<Vec<u8>> = None;
    let mut sig: Option<[u8; 64]> = None;

    loop {
        match reader.next()? {
            None => {
                return Err(NocError::ClusterCodec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bytes(b),
            }) => {
                if nocsr.is_some() {
                    return Err(NocError::MalformedResponse(
                        "CSRResponse: duplicate NOCSRElements field",
                    ));
                }
                nocsr = Some(b);
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Bytes(b),
            }) => {
                if sig.is_some() {
                    return Err(NocError::MalformedResponse(
                        "CSRResponse: duplicate AttestationSignature field",
                    ));
                }
                let arr: [u8; 64] = b.as_slice().try_into().map_err(|_| {
                    NocError::MalformedResponse(
                        "CSRResponse: AttestationSignature is not exactly 64 bytes",
                    )
                })?;
                sig = Some(arr);
            }
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader.skip_container()?,
            // Forward-compat: future-tag tolerance per the interaction-model rules.
            Some(Element::Scalar { .. }) => {}
            Some(_) => {
                return Err(NocError::MalformedResponse(
                    "CSRResponse: unexpected element",
                ))
            }
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader.finish()?;

    Ok(CsrResponse {
        nocsr_elements: nocsr.ok_or(NocError::MalformedResponse(
            "CSRResponse: missing NOCSRElements field",
        ))?,
        attestation_signature: sig.ok_or(NocError::MalformedResponse(
            "CSRResponse: missing AttestationSignature field",
        ))?,
    })
}

/// Encode `AddTrustedRootCertificate` (spec §11.18.5.8).
///
/// `rcac_tlv` is the Matter-TLV-encoded RCAC certificate (the output of
/// `fabric.root_cert.to_tlv()?`).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_add_trusted_root(rcac_tlv: &[u8]) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    // Spec §11.18.5.8: single field, id=0, root_certificate (octet string).
    w.put_bytes(Tag::Context(0), rcac_tlv)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode `AddNOC` (spec §11.18.5.9).
///
/// `icac_tlv` is `None` in M6.3 (RCAC→NOC, no intermediate).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_add_noc(
    noc_tlv: &[u8],
    icac_tlv: Option<&[u8]>,
    ipk: &[u8; 16],
    case_admin_subject: u64,
    admin_vendor_id: u16,
) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    // Spec §11.18.5.9:
    //   0: NOCValue                (octet string)
    //   1: ICACValue               (octet string, optional)
    //   2: IPKValue                (octet string, 16 bytes)
    //   3: CaseAdminSubject        (u64)
    //   4: AdminVendorId           (u16)
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), noc_tlv)
        .expect("infallible: vec writer");
    if let Some(icac) = icac_tlv {
        w.put_bytes(Tag::Context(1), icac)
            .expect("infallible: vec writer");
    }
    w.put_bytes(Tag::Context(2), ipk)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(3), case_admin_subject)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(4), u64::from(admin_vendor_id))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode `UpdateNOC` (spec §11.18.5.10).
///
/// Used only on the re-commission path (multi-admin / NOC renewal).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_update_noc(noc_tlv: &[u8], icac_tlv: Option<&[u8]>) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    // Spec §11.18.5.10:
    //   0: NOCValue   (octet string)
    //   1: ICACValue  (octet string, optional)
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), noc_tlv)
        .expect("infallible: vec writer");
    if let Some(icac) = icac_tlv {
        w.put_bytes(Tag::Context(1), icac)
            .expect("infallible: vec writer");
    }
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Decode `NOCResponse` (spec §11.18.5.11).
///
/// # Errors
///
/// Returns [`NocError::ClusterCodec`] on malformed input.
pub fn decode_noc_response(tlv: &[u8]) -> Result<NocResponse, NocError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => {
            return Err(NocError::MalformedResponse(
                "NOCResponse: expected an anonymous structure",
            ))
        }
    }

    let mut status: Option<u8> = None;
    let mut fabric_index: Option<u8> = None;
    let mut debug_text: Option<String> = None;

    loop {
        match reader.next()? {
            None => {
                return Err(NocError::ClusterCodec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(v),
            }) => {
                let n = u8::try_from(v).map_err(|_| {
                    NocError::MalformedResponse("NOCResponse: StatusCode exceeds u8")
                })?;
                status = Some(n);
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(v),
            }) => {
                let n = u8::try_from(v).map_err(|_| {
                    NocError::MalformedResponse("NOCResponse: FabricIndex exceeds u8")
                })?;
                fabric_index = Some(n);
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Utf8(s),
            }) => {
                // Spec bound (§11.18): DebugText ≤ 512 octets. Device-echoed free text — cap defensively.
                debug_text = Some(truncate_utf8(s, 512));
            }
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader.skip_container()?,
            // Forward-compat: ignore unknown future scalar fields.
            Some(_) => {}
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader.finish()?;

    Ok(NocResponse {
        status: status.ok_or(NocError::MalformedResponse(
            "NOCResponse: missing StatusCode field",
        ))?,
        fabric_index,
        debug_text,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::items_after_statements
)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use matter_codec::{Tag, TlvWriter};

    fn write_csr_response(nocsr: &[u8], att_sig: &[u8; 64]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(0), nocsr).unwrap();
        w.put_bytes(Tag::Context(1), att_sig).unwrap();
        w.end_container().unwrap();
        buf
    }

    #[test]
    fn decode_csr_response_roundtrips() {
        let nocsr = b"opaque nocsr bytes".to_vec();
        let sig = [0xAB; 64];
        let tlv = write_csr_response(&nocsr, &sig);
        let decoded = decode_csr_response(&tlv).unwrap();
        assert_eq!(decoded.nocsr_elements, nocsr);
        assert_eq!(decoded.attestation_signature, sig);
    }

    #[test]
    fn decode_csr_response_rejects_missing_signature() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(0), b"some nocsr").unwrap();
        w.end_container().unwrap();
        // A missing required field is a STRUCTURAL malformation, not a codec EOF.
        assert!(matches!(
            decode_csr_response(&buf),
            Err(NocError::MalformedResponse(_))
        ));
    }

    #[test]
    fn decode_csr_response_rejects_wrong_sig_length() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(0), b"some nocsr").unwrap();
        w.put_bytes(Tag::Context(1), &[0u8; 32]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            decode_csr_response(&buf),
            Err(NocError::MalformedResponse(_))
        ));
    }

    #[test]
    fn decode_csr_response_rejects_non_struct_outer() {
        // A well-formed TLV that is NOT the expected anonymous struct (here a
        // bare unsigned integer) must surface a structural error — NOT the
        // misleading codec `UnexpectedEof`.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 7).unwrap();
        match decode_csr_response(&buf) {
            Err(NocError::MalformedResponse(_)) => {}
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
    }

    #[test]
    fn decode_noc_response_rejects_non_struct_outer() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 7).unwrap();
        match decode_noc_response(&buf) {
            Err(NocError::MalformedResponse(_)) => {}
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
    }

    fn write_noc_response(
        status: u8,
        fabric_index: Option<u8>,
        debug_text: Option<&str>,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), u64::from(status)).unwrap();
        if let Some(fi) = fabric_index {
            w.put_uint(Tag::Context(1), u64::from(fi)).unwrap();
        }
        if let Some(text) = debug_text {
            w.put_utf8(Tag::Context(2), text).unwrap();
        }
        w.end_container().unwrap();
        buf
    }

    #[test]
    fn decode_noc_response_ok_status() {
        let tlv = write_noc_response(0, Some(3), None);
        let r = decode_noc_response(&tlv).unwrap();
        assert_eq!(r.status, 0);
        assert_eq!(r.fabric_index, Some(3));
        assert_eq!(r.debug_text, None);
    }

    #[test]
    fn decode_noc_response_with_debug_text() {
        let tlv = write_noc_response(0, Some(1), Some("welcome to the fabric"));
        let r = decode_noc_response(&tlv).unwrap();
        assert_eq!(r.debug_text.as_deref(), Some("welcome to the fabric"));
    }

    #[test]
    fn decode_noc_response_failure_status_no_index() {
        let tlv = write_noc_response(9, None, Some("invalid NOC"));
        let r = decode_noc_response(&tlv).unwrap();
        assert_eq!(r.status, 9);
        assert_eq!(r.fabric_index, None);
        assert_eq!(r.debug_text.as_deref(), Some("invalid NOC"));
    }

    #[test]
    fn decode_noc_response_caps_debug_text_at_512_bytes() {
        let long_text = "x".repeat(600);
        let tlv = write_noc_response(0, Some(1), Some(&long_text));
        let r = decode_noc_response(&tlv).unwrap();
        assert_eq!(r.debug_text.unwrap().len(), 512, "capped at spec bound");
    }

    #[test]
    fn decode_noc_response_rejects_missing_status() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.end_container().unwrap();
        // Missing required StatusCode field is structural, not a codec EOF.
        assert!(matches!(
            decode_noc_response(&buf),
            Err(NocError::MalformedResponse(_))
        ));
    }

    #[test]
    fn encode_csr_request_then_parse() {
        let nonce = [0x11u8; 32];
        let bytes = encode_csr_request(&nonce, false);
        use matter_codec::{ContainerKind, Element, TlvReader, Value};
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure,
            })
        ));
        match r.next().unwrap() {
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bytes(b),
            }) => assert_eq!(b, nonce.to_vec()),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(r.next().unwrap(), Some(Element::ContainerEnd)));
    }

    #[test]
    fn encode_csr_request_with_update_flag_emits_field_1() {
        let nonce = [0x11u8; 32];
        let bytes = encode_csr_request(&nonce, true);
        use matter_codec::{Element, TlvReader, Value};
        let mut r = TlvReader::new(&bytes);
        let _ = r.next();
        let _ = r.next();
        match r.next().unwrap() {
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Bool(true),
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn attestation_request_emits_anonymous_struct_with_32_byte_nonce() {
        let nonce = [0xAB_u8; 32];
        let bytes = encode_attestation_request(&nonce);
        // Anonymous struct: 0x15 ... 0x18.
        assert_eq!(bytes[0], 0x15);
        assert_eq!(*bytes.last().expect("non-empty"), 0x18);
        // Octet-string at context tag 0, 1-byte length, 32-byte payload.
        // Control octet for "octet-string with 1-byte length" + context tag = 0x30.
        assert_eq!(bytes[1], 0x30);
        assert_eq!(bytes[2], 0x00);
        assert_eq!(bytes[3], 0x20);
        assert_eq!(&bytes[4..4 + 32], &nonce);
    }

    #[test]
    fn attestation_response_round_trips() {
        let elements = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let sig = [0x42_u8; 64];
        // Hand-build: { 0: octet_string(elements), 1: octet_string(sig) }
        let mut tlv = vec![0x15];
        tlv.extend_from_slice(&[0x30, 0x00, 0x04]); // octet-string, ctx 0, len 4
        tlv.extend_from_slice(&elements);
        tlv.extend_from_slice(&[0x30, 0x01, 0x40]); // octet-string, ctx 1, len 64
        tlv.extend_from_slice(&sig);
        tlv.push(0x18);
        let resp = decode_attestation_response(&tlv).expect("decode happy path");
        assert_eq!(resp.attestation_elements, elements);
        assert_eq!(resp.signature, sig);
    }

    #[test]
    fn cert_chain_request_dac_matches_spec_bytes() {
        // Spec §11.18.5.2 CertificateChainTypeEnum: 1 = DACCertificate.
        // Confirmed against a real device (Tapo P110M, M6.6.5): requesting
        // type 1 returns the DAC (leaf), type 2 the PAI (CA).
        let bytes = encode_certificate_chain_request(CertChainType::Dac);
        assert_eq!(bytes, vec![0x15, 0x24, 0x00, 0x01, 0x18]);
    }

    #[test]
    fn cert_chain_request_pai_matches_spec_bytes() {
        // Spec §11.18.5.2 CertificateChainTypeEnum: 2 = PAICertificate.
        let bytes = encode_certificate_chain_request(CertChainType::Pai);
        assert_eq!(bytes, vec![0x15, 0x24, 0x00, 0x02, 0x18]);
    }

    #[test]
    fn cert_chain_response_round_trips_der_payload() {
        // { 0: octet_string([0xAA, 0xBB, 0xCC]) }
        let tlv = vec![0x15, 0x30, 0x00, 0x03, 0xAA, 0xBB, 0xCC, 0x18];
        let resp = decode_certificate_chain_response(&tlv).expect("decode happy path");
        assert_eq!(resp.certificate, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn encode_add_noc_emits_required_fields() {
        let noc = b"noc-bytes".to_vec();
        let ipk = [0x77u8; 16];
        let bytes = encode_add_noc(&noc, None, &ipk, 0xDEAD_BEEF, 0xFFF1);
        use matter_codec::{ContainerKind, Element, TlvReader, Value};
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure,
            })
        ));
        match r.next().unwrap() {
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bytes(b),
            }) => assert_eq!(b, noc),
            other => panic!("unexpected: {other:?}"),
        }
        match r.next().unwrap() {
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Bytes(b),
            }) => assert_eq!(b, ipk.to_vec()),
            other => panic!("unexpected: {other:?}"),
        }
    }
}

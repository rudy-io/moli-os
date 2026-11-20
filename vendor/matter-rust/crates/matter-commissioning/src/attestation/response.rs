//! Matter device attestation response signature verification.
//!
//! This is the second half of Matter Core Spec §6.2 commissioner-side
//! attestation. The first half ([`verify_chain`] in
//! [`crate::attestation::chain`]) proves the DAC chains to a trusted
//! root. This module proves the device *holds the DAC private key*
//! for the current commissioning session by verifying its ECDSA
//! signature over
//! `attestation_elements || attestation_challenge`.
//!
//! Pure sans-I/O — no network, no clock, no internal state. Callers
//! supply the `attestation_challenge` from the active PASE/CASE
//! session — a 16-byte field at offset `[32..48]` of the 48-byte HKDF
//! key blob (Matter §3.5). In our `matter-crypto` API this is exposed
//! as [`matter_crypto::CaseSessionKeys::attestation_challenge`] on the
//! CASE side and [`matter_crypto::PaseSessionKeys::attestation_key`]
//! on the PASE side — both are the same 16-byte slice; only the field
//! name differs.
//!
//! [`verify_chain`]: crate::attestation::verify_chain

#![forbid(unsafe_code)]

use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};

use crate::attestation::error::AttestationError;

/// The decoded attestation-response payload a commissioner receives
/// from a device's `AttestationRequest` cluster response.
///
/// Two fields:
///
/// - `attestation_elements`: an opaque TLV blob whose contents
///   (certification declaration, attestation nonce, timestamp, optional
///   firmware info) M6.2.3 does not parse. M6.4.x will parse it to
///   verify the embedded Certification Declaration. For signature
///   verification, these bytes are simply the first half of the
///   signed input.
/// - `signature`: the device's ECDSA P-256 / SHA-256 signature over
///   `attestation_elements || attestation_challenge`, in raw IEEE P1363
///   fixed-width form (32-byte big-endian `r` || 32-byte big-endian
///   `s`, 64 bytes total). Not ASN.1 DER. This matches Matter Core
///   Spec §3.5.3 ("ECDSA signatures are encoded as fixed-width
///   representations of r and s") and matter.js's
///   `Crypto.signEcdsa` output format.
#[derive(Debug, Clone)]
pub struct AttestationResponse {
    /// Opaque attestation-elements bytes (TLV-encoded by the device;
    /// not parsed by M6.2.3).
    pub attestation_elements: Vec<u8>,
    /// Raw ECDSA P-256 signature, 32-byte `r` followed by 32-byte `s`
    /// (Matter §3.5.3 fixed-width encoding; 64 bytes total).
    pub signature: [u8; 64],
}

/// Verify a device's DAC signature over `elements || attestation_challenge`.
///
/// This is the primitive shared between `verify_attestation_response`
/// (M6.2.3) and M6.3's `noc::csr::verify_csr_response` — both verify a
/// device-private-key signature over a TLV-elements blob concatenated
/// with the 16-byte session attestation challenge.
///
/// - `elements` is the device-supplied opaque TLV blob (attestation
///   elements in M6.2, NOCSR elements in M6.3).
/// - `attestation_challenge` is the 16-byte tail of the session-key
///   derivation (`keys[32..48]`, per Matter Core Spec §3.5).
/// - `dac_public_key` is the raw 65-byte SEC1 uncompressed P-256
///   encoding (leading `0x04`, X, Y) that `ring` consumes directly.
///
/// # Errors
///
/// Returns [`AttestationError::BadResponseSignature`] on any failure —
/// corrupt signature, wrong key, wrong challenge, tampered elements,
/// or malformed key bytes. The variant is deliberately coarse; see
/// its rustdoc.
pub fn verify_dac_signed_elements(
    elements: &[u8],
    attestation_challenge: &[u8; 16],
    dac_public_key: &[u8],
    signature: &[u8; 64],
) -> Result<(), AttestationError> {
    // Compose the to-be-verified blob exactly as the device composed
    // it before signing: elements concatenated with the raw challenge
    // bytes. Length-prefixing or framing would diverge from matter.js
    // and the C++ reference; do not add any.
    let mut tbs = Vec::with_capacity(elements.len() + attestation_challenge.len());
    tbs.extend_from_slice(elements);
    tbs.extend_from_slice(attestation_challenge);

    let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, dac_public_key);
    key.verify(&tbs, signature)
        .map_err(|_| AttestationError::BadResponseSignature)
}

/// Verify a device's `AttestationResponse` signature.
///
/// This is a pure function — no clock reads, no network, no internal
/// state. The caller is responsible for:
///
/// 1. Supplying `attestation_challenge` from the PASE or CASE session
///    that this commissioning exchange is bound to. The challenge is
///    the 16-byte tail of the session-key derivation (`keys[32..48]`,
///    per Matter Core Spec §3.5 and the byte-parity-verified CASE
///    implementation in `matter-crypto`).
/// 2. Supplying `dac_public_key` as the raw SEC1 uncompressed P-256
///    encoding — 65 bytes, leading `0x04`, then 32-byte X and 32-byte
///    Y. This is exactly the byte layout
///    [`crate::attestation::Dac::public_key`] returns and
///    [`crate::attestation::ChainVerification::dac_public_key`]
///    surfaces.
///
/// Thin wrapper over [`verify_dac_signed_elements`]; preserved as a
/// stable public surface for M6.2's existing callers.
///
/// # Errors
///
/// Returns [`AttestationError::BadResponseSignature`] on any
/// verification failure — corrupt signature, wrong key, wrong
/// challenge, tampered elements, or malformed public-key bytes.
/// The variant is deliberately coarse; see the variant's rustdoc.
pub fn verify_attestation_response(
    response: &AttestationResponse,
    attestation_challenge: &[u8; 16],
    dac_public_key: &[u8],
) -> Result<(), AttestationError> {
    verify_dac_signed_elements(
        &response.attestation_elements,
        attestation_challenge,
        dac_public_key,
        &response.signature,
    )
}

/// Fields extracted from an `attestation_elements` TLV blob per Matter
/// Core Spec §6.2.4.
///
/// Used by the M6.4 state machine to thread the CD bytes into the
/// M6.4.3 `verify_certification_declaration` call and to confirm the
/// device echoed back the commissioner's `attestation_nonce`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttestationElementsFields {
    /// Certification Declaration — opaque CMS/PKCS#7 `SignedData` bytes.
    /// M6.4.3's `verify_certification_declaration` consumes this.
    pub certification_declaration: Vec<u8>,
    /// 32-byte nonce echoed from `AttestationRequest`.
    pub attestation_nonce: [u8; 32],
    /// Device's claim of when it produced the response — seconds since
    /// the Matter epoch (`2000-01-01T00:00:00Z`).
    pub timestamp_epoch_seconds: u64,
}

/// Parse the M6.4-relevant fields out of an `attestation_elements`
/// TLV blob (spec §6.2.4).
///
/// Field layout:
/// - Tag 1: `certification_declaration` (octet string).
/// - Tag 2: `attestation_nonce` (32-byte octet string).
/// - Tag 3: `timestamp` (unsigned int, Matter-epoch seconds).
/// - Tag 4: `firmware_information` (octet string, optional — ignored).
/// - Tag 5: `vendor_specific` (octet string, optional — ignored).
///
/// Unknown tags (including future spec additions and the two optional
/// fields above) are skipped so forward-compatible devices do not
/// regress here. An unknown container is skipped whole, nested
/// containers included, so none of its members is read as a top-level
/// field. A known field (tags 1-4) in the wrong shape, or a timestamp
/// above `u32::MAX`, is rejected, as chip rejects it. Bytes after the outer structure's end are ignored, as chip's
/// `DeconstructAttestationElements` ignores them.
///
/// # Errors
///
/// Returns [`AttestationError::ResponseElementsMalformed`] if any of
/// the three required fields is missing, if the TLV outer shape isn't
/// an anonymous structure, if the nonce isn't exactly 32 bytes, or if
/// a required field appears more than once.
#[allow(
    clippy::match_same_arms,
    reason = "the end-of-input arm and the misshapen-field arm both reject the \
     elements, but for different reasons that each carry their own comment."
)]
pub fn extract_attestation_elements_fields(
    tlv: &[u8],
) -> Result<AttestationElementsFields, AttestationError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader
        .next()
        .map_err(|_| AttestationError::ResponseElementsMalformed)?
    {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => return Err(AttestationError::ResponseElementsMalformed),
    }
    let mut cd: Option<Vec<u8>> = None;
    let mut nonce: Option<[u8; 32]> = None;
    let mut ts: Option<u64> = None;
    loop {
        match reader
            .next()
            .map_err(|_| AttestationError::ResponseElementsMalformed)?
        {
            None => return Err(AttestationError::ResponseElementsMalformed),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Bytes(b),
            }) => {
                if cd.is_some() {
                    return Err(AttestationError::ResponseElementsMalformed);
                }
                cd = Some(b);
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Bytes(b),
            }) => {
                if nonce.is_some() {
                    return Err(AttestationError::ResponseElementsMalformed);
                }
                let arr: [u8; 32] = b
                    .as_slice()
                    .try_into()
                    .map_err(|_| AttestationError::ResponseElementsMalformed)?;
                nonce = Some(arr);
            }
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Uint(v),
            }) => {
                if ts.is_some() {
                    return Err(AttestationError::ResponseElementsMalformed);
                }
                // chip reads it as a `uint32_t` (`Get(timestamp)`), which
                // fails above `u32::MAX`.
                u32::try_from(v).map_err(|_| AttestationError::ResponseElementsMalformed)?;
                ts = Some(v);
            }
            // firmware_information: an octet string, otherwise ignored.
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Bytes(_),
            }) => {}
            // A known field (tags 1-4) in any other shape rejects the
            // elements, as chip's `GetByteView` / `Get` fails on the wrong
            // type (`DeviceAttestationConstructor.cpp:120-133`). The
            // well-formed shapes matched the arms above.
            Some(
                Element::ContainerStart {
                    tag: Tag::Context(1..=4),
                    ..
                }
                | Element::Scalar {
                    tag: Tag::Context(1..=4),
                    ..
                },
            ) => return Err(AttestationError::ResponseElementsMalformed),
            // An unknown container (a future field, or a known tag in the
            // wrong shape) is skipped whole, at any depth, so its members are
            // never read as top-level fields. This is what chip's
            // `TLVReader::Next()` does for a container it has not entered.
            Some(Element::ContainerStart { .. }) => reader
                .skip_container()
                .map_err(|_| AttestationError::ResponseElementsMalformed)?,
            // Forward-compat: ignore unknown scalar fields (firmware_info,
            // vendor_specific, future tags).
            Some(_) => {}
        }
    }
    // No strict close here: chip's `DeconstructAttestationElements` never
    // reads past the structure, so bytes after its end are ignored.
    Ok(AttestationElementsFields {
        certification_declaration: cd.ok_or(AttestationError::ResponseElementsMalformed)?,
        attestation_nonce: nonce.ok_or(AttestationError::ResponseElementsMalformed)?,
        timestamp_epoch_seconds: ts.ok_or(AttestationError::ResponseElementsMalformed)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_struct_is_constructible() {
        let r = AttestationResponse {
            attestation_elements: vec![0u8; 0],
            signature: [0u8; 64],
        };
        // Exercise the accessors so they don't dead-code-warn.
        assert_eq!(r.attestation_elements.len(), 0);
        assert_eq!(r.signature.len(), 64);
    }

    use p256::ecdsa::{signature::Signer, Signature, SigningKey};

    /// Mint a fresh P-256 keypair and return (raw SEC1 uncompressed
    /// pubkey, signature over `elements || challenge`).
    ///
    /// This helper exists so each test owns its own deterministic
    /// keypair — no shared global state, no per-test signing-key
    /// fixture file.
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn mint_signed(elements: &[u8], challenge: &[u8; 16], seed: u8) -> ([u8; 65], [u8; 64]) {
        // Deterministic 32-byte scalar so test failures repro byte-
        // identically. P-256 scalars are big-endian; placing `seed` at
        // index 0 (most-significant byte) yields a scalar around
        // `seed * 2^248`, well below the curve order for any
        // `seed <= 0xfe` and far from the pathological scalar 1
        // (which is what `scalar[31] = seed` with `seed == 1` would
        // produce). Any non-zero such scalar is a valid signing key.
        let mut scalar = [0u8; 32];
        scalar[0] = seed;
        let signing_key = SigningKey::from_slice(&scalar)
            .expect("non-zero 32-byte scalar is a valid P-256 scalar");
        let verifying_key = signing_key.verifying_key();
        let pubkey_point = verifying_key.to_sec1_point(false);
        let pubkey_bytes = pubkey_point.as_bytes();
        let mut pubkey_arr = [0u8; 65];
        pubkey_arr.copy_from_slice(pubkey_bytes);

        let mut tbs = Vec::with_capacity(elements.len() + 16);
        tbs.extend_from_slice(elements);
        tbs.extend_from_slice(challenge);
        let sig: Signature = signing_key.sign(&tbs);
        let sig_bytes = sig.to_bytes();
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);

        (pubkey_arr, sig_arr)
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_attestation_response_accepts_valid_signature() {
        let elements = b"opaque attestation_elements TLV blob".to_vec();
        let challenge = [0x42u8; 16];
        let (pubkey, sig) = mint_signed(&elements, &challenge, 0x01);
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        verify_attestation_response(&response, &challenge, &pubkey)
            .expect("valid signature must verify");
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_rejects_flipped_signature_byte() {
        let elements = b"some elements".to_vec();
        let challenge = [0x42u8; 16];
        let (pubkey, mut sig) = mint_signed(&elements, &challenge, 0x02);
        // Flip one bit in `r` (first 32 bytes). `s` would work too;
        // either half being wrong fails verification.
        sig[0] ^= 0x80;
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        let err = verify_attestation_response(&response, &challenge, &pubkey)
            .expect_err("flipped signature byte must fail");
        assert!(matches!(err, AttestationError::BadResponseSignature));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_rejects_wrong_challenge() {
        let elements = b"some elements".to_vec();
        let challenge_correct = [0x42u8; 16];
        let (pubkey, sig) = mint_signed(&elements, &challenge_correct, 0x03);
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        // Same key, same elements, same signature, but a different
        // challenge — must fail.
        let challenge_wrong = [0x43u8; 16];
        let err = verify_attestation_response(&response, &challenge_wrong, &pubkey)
            .expect_err("wrong challenge must fail");
        assert!(matches!(err, AttestationError::BadResponseSignature));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_rejects_tampered_elements() {
        let elements = b"some elements".to_vec();
        let challenge = [0x42u8; 16];
        let (pubkey, sig) = mint_signed(&elements, &challenge, 0x04);
        // Flip the high bit of byte 0 of elements. Anything other than
        // the original input invalidates the signature.
        let mut tampered = elements.clone();
        tampered[0] ^= 0x80;
        let response = AttestationResponse {
            attestation_elements: tampered,
            signature: sig,
        };
        let err = verify_attestation_response(&response, &challenge, &pubkey)
            .expect_err("tampered elements must fail");
        assert!(matches!(err, AttestationError::BadResponseSignature));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_rejects_wrong_public_key() {
        let elements = b"some elements".to_vec();
        let challenge = [0x42u8; 16];
        let (_pubkey_correct, sig) = mint_signed(&elements, &challenge, 0x05);
        // Different seed -> different keypair. Same elements, same
        // challenge, same signature — but verified against the *wrong*
        // public key.
        let (pubkey_wrong, _other_sig) = mint_signed(&elements, &challenge, 0x06);
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        let err = verify_attestation_response(&response, &challenge, &pubkey_wrong)
            .expect_err("wrong public key must fail");
        assert!(matches!(err, AttestationError::BadResponseSignature));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_rejects_malformed_public_key() {
        // Build a valid (elements, challenge, sig) tuple, then point
        // verify at a malformed pubkey blob (wrong length). ring must
        // reject internally — we must surface that as
        // BadResponseSignature, not panic.
        let elements = b"some elements".to_vec();
        let challenge = [0x42u8; 16];
        let (_pubkey, sig) = mint_signed(&elements, &challenge, 0x07);
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        let too_short = vec![0u8; 32];
        let err = verify_attestation_response(&response, &challenge, &too_short)
            .expect_err("malformed pubkey must fail");
        assert!(matches!(err, AttestationError::BadResponseSignature));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_attestation_elements_fields_happy_path() {
        // Hand-build attestation_elements per spec §6.2.4:
        //   { 1: octet_string(cd_bytes),
        //     2: octet_string(nonce_32),
        //     3: uint(timestamp_epoch_seconds) }
        let nonce = [0xAB_u8; 32];
        let mut tlv = vec![0x15];
        // Tag 1 (context), octet-string-1byte-length, 3 bytes "CD"
        tlv.extend_from_slice(&[0x30, 0x01, 0x03, 0xCD, 0xCD, 0xCD]);
        // Tag 2 (context), octet-string-1byte-length, 32-byte nonce
        tlv.extend_from_slice(&[0x30, 0x02, 0x20]);
        tlv.extend_from_slice(&nonce);
        // Tag 3 (context), u64, value 0x42 (le bytes)
        tlv.extend_from_slice(&[0x27, 0x03]);
        tlv.extend_from_slice(&0x42_u64.to_le_bytes());
        tlv.push(0x18);

        let fields = extract_attestation_elements_fields(&tlv).expect("happy path decodes");
        assert_eq!(fields.certification_declaration, vec![0xCD, 0xCD, 0xCD]);
        assert_eq!(fields.attestation_nonce, nonce);
        assert_eq!(fields.timestamp_epoch_seconds, 0x42);
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_attestation_elements_fields_rejects_malformed() {
        let err = extract_attestation_elements_fields(&[0xFF]).expect_err("malformed");
        assert!(matches!(err, AttestationError::ResponseElementsMalformed));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_attestation_elements_fields_rejects_short_nonce() {
        // Same shape as happy path but nonce is only 16 bytes — should fail.
        let mut tlv = vec![0x15];
        tlv.extend_from_slice(&[0x30, 0x01, 0x03, 0xCD, 0xCD, 0xCD]);
        tlv.extend_from_slice(&[0x30, 0x02, 0x10]); // length=16
        tlv.extend_from_slice(&[0u8; 16]);
        tlv.extend_from_slice(&[0x27, 0x03]);
        tlv.extend_from_slice(&0_u64.to_le_bytes());
        tlv.push(0x18);

        let err = extract_attestation_elements_fields(&tlv).expect_err("short nonce");
        assert!(matches!(err, AttestationError::ResponseElementsMalformed));
    }

    /// Encode attestation elements: `{1: cd, 2: nonce, 3: ts}` (each
    /// optional), with `decoy` written inside the outer structure first.
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn elements_with_decoy(
        real: Option<(&[u8], &[u8; 32], u64)>,
        decoy: impl FnOnce(&mut matter_codec::TlvWriter<'_>),
    ) -> Vec<u8> {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        decoy(&mut w);
        if let Some((cd, nonce, ts)) = real {
            w.put_bytes(Tag::Context(1), cd).unwrap();
            w.put_bytes(Tag::Context(2), nonce).unwrap();
            w.put_uint(Tag::Context(3), ts).unwrap();
        }
        w.end_container().unwrap();
        buf
    }

    /// A decoy container three levels deep (ctx 6 → ctx 7 → ctx 8) whose
    /// innermost members carry the three required tags.
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn deep_decoy(w: &mut matter_codec::TlvWriter<'_>) {
        use matter_codec::Tag;
        w.start_structure(Tag::Context(6)).unwrap();
        w.start_list(Tag::Context(7)).unwrap();
        w.start_structure(Tag::Context(8)).unwrap();
        w.put_bytes(Tag::Context(1), &[0xBA, 0xD0]).unwrap();
        w.put_bytes(Tag::Context(2), &[0xEE; 32]).unwrap();
        w.put_uint(Tag::Context(3), 0xBAD).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }

    /// The fields of an unknown container are not top-level fields, at any
    /// depth: chip's `TLVReader::Next()` skips a container it has not
    /// entered. A decoy holding only fake required fields must not satisfy
    /// the parser. Before the fix the fake fields were read as the real ones
    /// and the decoy's first end-of-container ended the structure.
    #[test]
    fn extract_attestation_elements_fields_does_not_read_decoy_container_fields() {
        let tlv = elements_with_decoy(None, deep_decoy);
        assert!(matches!(
            extract_attestation_elements_fields(&tlv),
            Err(AttestationError::ResponseElementsMalformed)
        ));
    }

    /// The real fields after a deep decoy container are the ones returned.
    /// Before the fix the decoy's fake `1` was read first and the real `1`
    /// tripped the duplicate check.
    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_attestation_elements_fields_skips_decoy_container() {
        let nonce = [0xAB_u8; 32];
        let tlv = elements_with_decoy(Some((&[0xCD, 0xCD], &nonce, 0x42)), deep_decoy);
        let fields = extract_attestation_elements_fields(&tlv).expect("decoy skipped");
        assert_eq!(fields.certification_declaration, vec![0xCD, 0xCD]);
        assert_eq!(fields.attestation_nonce, nonce);
        assert_eq!(fields.timestamp_epoch_seconds, 0x42);
    }

    /// A known field in the wrong shape rejects the elements, as chip's
    /// `DeconstructAttestationElements` rejects it (`GetByteView` or `Get`
    /// fails on the wrong type; `DeviceAttestationConstructor.cpp:120-133`,
    /// the same at master 5cd2917a and v1.4.2.0). chip reads the timestamp
    /// as a `uint32_t`, so one above `u32::MAX` is rejected too.
    /// Before the fix it was ignored, so a misshapen field followed by a
    /// well-formed one with the same tag was accepted.
    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_attestation_elements_fields_rejects_a_misshapen_known_field() {
        use matter_codec::Tag;
        type Decoy = fn(&mut matter_codec::TlvWriter<'_>);
        let nonce = [0xAB_u8; 32];
        let cases: [(&str, Decoy); 6] = [
            ("cd as a container", |w| {
                w.start_structure(Tag::Context(1)).unwrap();
                w.end_container().unwrap();
            }),
            ("nonce as an integer", |w| {
                w.put_uint(Tag::Context(2), 7).unwrap();
            }),
            ("timestamp as bytes", |w| {
                w.put_bytes(Tag::Context(3), &[1, 2]).unwrap();
            }),
            ("timestamp signed", |w| {
                w.put_int(Tag::Context(3), 7).unwrap();
            }),
            ("firmware_information as an integer", |w| {
                w.put_uint(Tag::Context(4), 7).unwrap();
            }),
            ("timestamp above u32", |w| {
                w.put_uint(Tag::Context(3), u64::from(u32::MAX) + 1)
                    .unwrap();
            }),
        ];
        for (what, decoy) in cases {
            let tlv = elements_with_decoy(Some((&[0xCD], &nonce, 7)), decoy);
            assert!(
                matches!(
                    extract_attestation_elements_fields(&tlv),
                    Err(AttestationError::ResponseElementsMalformed)
                ),
                "{what} must be rejected"
            );
        }
        // A well-formed firmware_information is still accepted (and ignored).
        let tlv = elements_with_decoy(Some((&[0xCD], &nonce, 7)), |w| {
            w.put_bytes(Tag::Context(4), &[0xF0]).unwrap();
        });
        assert!(extract_attestation_elements_fields(&tlv).is_ok());
    }

    /// Bytes after the outer structure's close are ignored, as chip's
    /// `DeconstructAttestationElements` ignores them (it never reads past
    /// the structure). Pins that the container fix did not add a strict
    /// close here.
    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_attestation_elements_fields_ignores_trailing_bytes() {
        let nonce = [0xAB_u8; 32];
        let mut tlv = elements_with_decoy(Some((&[0xCD], &nonce, 7)), |_| {});
        tlv.extend_from_slice(&[0x24, 0x01, 0x05]);
        let fields = extract_attestation_elements_fields(&tlv).expect("trailing ignored");
        assert_eq!(fields.timestamp_epoch_seconds, 7);
    }
}

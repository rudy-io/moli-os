//! TLV codec for CASE wire messages (Matter Core Spec §4.13).
//!
//! Each message is an anonymous TLV structure with context-tagged fields.
//! Decoders return strongly-typed structs; encoders accept the struct
//! and emit `Vec<u8>` bytes for the wire.
//!
//! # Tag numbers (pinned from matter.js `CaseMessages.ts`, 2026-05-19)
//!
//! ```text
//! Message        Field                       Context tag   Length constraint
//! ─────────────────────────────────────────────────────────────────────────────
//! Sigma1         initiator_random            1             32 bytes
//! Sigma1         initiator_session_id        2             u16
//! Sigma1         dest_id                     3             32 bytes (CRYPTO_HASH_LEN_BYTES)
//! Sigma1         initiator_eph_pub           4             65 bytes (CRYPTO_PUBLIC_KEY_SIZE_BYTES)
//! Sigma1         initiator_session_params    5 (opt)       structure (pass-through)
//! Sigma1         resumption_id               6 (opt)       16 bytes
//! Sigma1         initiator_resume_mic        7 (opt)       16 bytes (CRYPTO_AEAD_MIC_LENGTH_BYTES)
//! ─────────────────────────────────────────────────────────────────────────────
//! Sigma2         responder_random            1             32 bytes
//! Sigma2         responder_session_id        2             u16
//! Sigma2         responder_eph_pub           3             65 bytes (CRYPTO_PUBLIC_KEY_SIZE_BYTES)
//! Sigma2         encrypted                   4             variable bytes (opaque blob)
//! Sigma2         responder_session_params    5 (opt)       structure (pass-through)
//! ─────────────────────────────────────────────────────────────────────────────
//! Sigma2Resume   resumption_id               1             16 bytes
//! Sigma2Resume   resume_mic                  2             16 bytes (CRYPTO_AEAD_MIC_LENGTH_BYTES)
//! Sigma2Resume   responder_session_id        3             u16
//! Sigma2Resume   responder_session_params    4 (opt)       structure (pass-through)
//! ─────────────────────────────────────────────────────────────────────────────
//! Sigma3         encrypted                   1             variable bytes (opaque blob)
//! ─────────────────────────────────────────────────────────────────────────────
//! ```
//!
//! Note: `Sigma3_Resume` does NOT exist as a wire message in matter.js.
//! After the initiator verifies `Sigma2_Resume.resume_mic`, it sends a plain
//! `Status=Success` message (handled by the transport layer, not this codec).
//!
//! # `SessionParams` (M4 pass-through, same as M3)
//!
//! `SessionParams.raw_tlv` holds the raw TLV bytes of the optional
//! `SessionParameters` sub-structure, including the container-start control
//! byte through the end-of-container byte (`0x18`) inclusive. M4 does not
//! interpret the fields; M6 commissioning will decode them when needed.
//!
//! # Dead-code allowance
//!
//! Items here are `pub(crate)` and consumed by the CASE state machines
//! (`initiator.rs`, `responder.rs`).
#![allow(dead_code)]

use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Byte-length constants, pinned from matter.js CryptoConstants.ts:
//   CRYPTO_GROUP_SIZE_BYTES          = 32
//   CRYPTO_PUBLIC_KEY_SIZE_BYTES     = 2 * 32 + 1 = 65   (uncompressed P-256)
//   CRYPTO_HASH_LEN_BYTES            = 32
//   CRYPTO_AEAD_MIC_LENGTH_BYTES     = 16
// ---------------------------------------------------------------------------

/// Byte length of an uncompressed P-256 ephemeral public key (0x04 || X || Y).
const EPH_PUB_LEN: usize = 65;

/// Byte length of the random nonce fields (`initiator_random`, `responder_random`).
const RANDOM_LEN: usize = 32;

/// Byte length of `dest_id` (SHA-256 of the destination identifier input).
const DEST_ID_LEN: usize = 32;

/// Byte length of `resumption_id` and `initiator_resume_mic`.
const RESUMPTION_ID_LEN: usize = 16;

/// Byte length of the AEAD MIC tag used in resumption.
const RESUME_MIC_LEN: usize = 16;

// ---------------------------------------------------------------------------
// TLV end-of-container byte (Matter Core Spec §A.2, element type 0x18).
// Used when closing the outer anonymous structure after appending raw optional
// bytes (same technique as pase/messages.rs).
// ---------------------------------------------------------------------------
const END_CONTAINER_BYTE: u8 = 0x18;

// ---------------------------------------------------------------------------
// Helper: advance past the outer anonymous structure start.
// ---------------------------------------------------------------------------

/// Advance `reader` past the anonymous-structure opening element.
///
/// Returns [`Error::InvalidParameter`] if the first element is not an
/// anonymous TLV structure start.
fn expect_anon_struct_start(reader: &mut TlvReader<'_>) -> Result<()> {
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => Ok(()),
        _ => Err(Error::InvalidParameter),
    }
}

// ---------------------------------------------------------------------------
// Helpers: collect / skip container bodies (re-used for SessionParams capture).
// ---------------------------------------------------------------------------

/// Collect the children of a structure whose `ContainerStart` has already
/// been consumed, returning a `Value::Structure(…)`.
///
/// # Errors
///
/// - [`Error::InvalidParameter`] if the body is not properly terminated.
/// - [`Error::Codec`] on malformed TLV.
fn collect_structure_body(reader: &mut TlvReader<'_>) -> Result<Value> {
    let mut members: Vec<(Tag, Value)> = Vec::new();
    loop {
        match reader.next()? {
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar { tag, value }) => {
                members.push((tag, value));
            }
            Some(Element::ContainerStart { tag, kind }) => {
                let inner = match kind {
                    ContainerKind::Structure => collect_structure_body(reader)?,
                    ContainerKind::Array => Value::Array(collect_array_body(reader)?),
                    ContainerKind::List => Value::List(collect_list_body(reader)?),
                    // `ContainerKind` is `#[non_exhaustive]`; reject unknown kinds.
                    _ => return Err(Error::InvalidParameter),
                };
                members.push((tag, inner));
            }
            // None (unexpected EOF) or unknown element — `Element` is
            // `#[non_exhaustive]` so `Some(_)` must be handled.
            None | Some(_) => return Err(Error::InvalidParameter),
        }
    }
    Ok(Value::Structure(members))
}

/// Collect the elements of an array body (`ContainerStart` already consumed).
fn collect_array_body(reader: &mut TlvReader<'_>) -> Result<Vec<Value>> {
    let mut elems: Vec<Value> = Vec::new();
    loop {
        match reader.next()? {
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar { value, .. }) => elems.push(value),
            Some(Element::ContainerStart { kind, .. }) => {
                let inner = match kind {
                    ContainerKind::Structure => collect_structure_body(reader)?,
                    ContainerKind::Array => Value::Array(collect_array_body(reader)?),
                    ContainerKind::List => Value::List(collect_list_body(reader)?),
                    _ => return Err(Error::InvalidParameter),
                };
                elems.push(inner);
            }
            None | Some(_) => return Err(Error::InvalidParameter),
        }
    }
    Ok(elems)
}

/// Collect the members of a list body (`ContainerStart` already consumed).
fn collect_list_body(reader: &mut TlvReader<'_>) -> Result<Vec<(Tag, Value)>> {
    let mut members: Vec<(Tag, Value)> = Vec::new();
    loop {
        match reader.next()? {
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar { tag, value }) => members.push((tag, value)),
            Some(Element::ContainerStart { tag, kind }) => {
                let inner = match kind {
                    ContainerKind::Structure => collect_structure_body(reader)?,
                    ContainerKind::Array => Value::Array(collect_array_body(reader)?),
                    ContainerKind::List => Value::List(collect_list_body(reader)?),
                    _ => return Err(Error::InvalidParameter),
                };
                members.push((tag, inner));
            }
            None | Some(_) => return Err(Error::InvalidParameter),
        }
    }
    Ok(members)
}

/// Skip the body of an already-opened container (any kind) until its
/// matching `ContainerEnd` is consumed.
fn skip_container_body(reader: &mut TlvReader<'_>) -> Result<()> {
    let mut depth: usize = 1;
    loop {
        match reader.next()? {
            None => return Err(Error::InvalidParameter),
            Some(Element::ContainerStart { .. }) => depth += 1,
            Some(Element::ContainerEnd) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Some(_) => {}
        }
    }
}

/// Forward-compatibly handle a CASE-message element that matched none of the
/// decoder's known tags.
///
/// chip and matter.js accept and *ignore* unknown fields in the Sigma
/// messages, so a strict reject would make a future device that adds a
/// spec-optional field unreachable to us. We therefore skip an unknown
/// container's body and ignore an unknown scalar, rather than failing the whole
/// handshake. A truncated structure (EOF before the matching `ContainerEnd`) is
/// still malformed. Each decoder still enforces its REQUIRED fields via the
/// `Option::ok_or` presence checks after the loop, and the inner
/// cryptographically-authenticated structures (TBEData/TBSData) are unaffected —
/// this only relaxes the outer, unauthenticated message envelope.
fn skip_unknown_field(reader: &mut TlvReader<'_>, element: Option<&Element>) -> Result<()> {
    match element {
        None => Err(Error::InvalidParameter),
        Some(Element::ContainerStart { .. }) => skip_container_body(reader),
        Some(_) => Ok(()),
    }
}

/// After a `ContainerStart` for a sub-structure has been consumed from
/// `full_message_reader`, skip the sub-structure body to the matching
/// `ContainerEnd`, then re-parse `full_message` to recover the raw bytes of
/// that sub-structure (container-start byte through end-of-container byte).
///
/// The re-parse uses [`TlvReader::read_value`] on a fresh reader positioned
/// just before the last context-tagged sub-structure in `full_message`, then
/// re-encodes the value tree via [`TlvWriter::write_value`]. This preserves
/// the TLV encoding faithfully for the values that matter.js actually puts in
/// `SessionParams` (all scalars), which have deterministic minimal encoding.
///
/// # Errors
///
/// - [`Error::InvalidParameter`] if the sub-structure is not properly closed,
///   or no context-tagged sub-structure is found when re-parsing.
/// - [`Error::Codec`] on malformed TLV.
fn skip_and_capture_substructure(
    full_message: &[u8],
    main_reader: &mut TlvReader<'_>,
    context_tag: u8,
) -> Result<Vec<u8>> {
    // Skip the body of the already-opened sub-structure in `main_reader`.
    let mut depth: usize = 1;
    loop {
        match main_reader.next()? {
            None => return Err(Error::InvalidParameter),
            Some(Element::ContainerStart { .. }) => depth += 1,
            Some(Element::ContainerEnd) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Some(_) => {}
        }
    }

    // Re-parse `full_message` on a fresh reader to capture the sub-element's raw bytes.
    let mut reader = TlvReader::new(full_message);
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => return Err(Error::InvalidParameter),
    }

    loop {
        match reader.next()? {
            None | Some(Element::ContainerEnd) => return Err(Error::InvalidParameter),
            Some(Element::ContainerStart {
                tag: Tag::Context(t),
                kind: ContainerKind::Structure,
            }) if t == context_tag => {
                // Found the target. Collect children into a Value tree, then re-encode.
                let child_value = collect_structure_body(&mut reader)?;
                let mut raw = Vec::new();
                let mut w = TlvWriter::new(&mut raw);
                w.write_value(Tag::Context(t), &child_value)?;
                return Ok(raw);
            }
            Some(Element::ContainerStart { .. }) => {
                // A different sub-structure; skip over it.
                skip_container_body(&mut reader)?;
            }
            // Scalars or unknown elements at the outer level.
            // `Element` is `#[non_exhaustive]` so `Some(_)` is required.
            Some(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// SessionParams — opaque TLV bytes pass-through.
// ---------------------------------------------------------------------------

/// Opaque bytes representing the optional `SessionParameters` sub-structure
/// in [`Sigma1`] or [`Sigma2`].
///
/// M4 preserves the bytes byte-for-byte for round-trip fidelity without
/// interpreting the fields (idle/active intervals, data-model revision, …).
/// M6 commissioning will decode the contents when it needs them.
///
/// The bytes cover the entire sub-element: the container-start control byte
/// through the matching end-of-container byte (`0x18`) inclusive, including
/// the context tag byte that identifies this field within the parent structure.
/// Context tag of the `SessionParameters` element inside `Sigma1`.
pub(crate) const SIGMA1_SESSION_PARAMS_TAG: u8 = 5;
/// Context tag of the `SessionParameters` element inside `Sigma2`.
pub(crate) const SIGMA2_SESSION_PARAMS_TAG: u8 = 5;
/// Context tag of the `SessionParameters` element inside `Sigma2_Resume`.
///
/// **Four, not five.** `Sigma2_Resume` has no `responderEphPubKey` or
/// `encrypted` field, so its tag numbering is one short of `Sigma2`'s. Getting
/// this wrong emits an element the peer silently ignores.
pub(crate) const SIGMA2_RESUME_SESSION_PARAMS_TAG: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SessionParams {
    /// Raw TLV bytes of the `SessionParameters` sub-element, context-tag
    /// through end-container, as captured from the wire or built by the encoder.
    pub raw_tlv: Vec<u8>,
}

/// Decode a captured [`SessionParams`] blob into the typed
/// [`SessionParameters`](crate::SessionParameters).
///
/// Returns `Ok(None)` when the peer omitted the optional element entirely,
/// which the spec defines as "assume the defaults" rather than "supports
/// nothing".
///
/// # Errors
///
/// Propagates [`Error::InvalidParameter`] / [`Error::Codec`] if the captured
/// bytes are not a well-formed structure.
fn decode_session_params(raw: Option<&SessionParams>) -> Result<Option<crate::SessionParameters>> {
    raw.map(|sp| crate::SessionParameters::decode(&sp.raw_tlv))
        .transpose()
}

impl From<Vec<u8>> for SessionParams {
    /// Wrap already-encoded sub-element bytes (context-tag control byte through
    /// the matching `0x18`), as produced by
    /// [`SessionParameters::encode`](crate::SessionParameters::encode).
    fn from(raw_tlv: Vec<u8>) -> Self {
        Self { raw_tlv }
    }
}

// ---------------------------------------------------------------------------
// Sigma1
// ---------------------------------------------------------------------------

/// The first CASE message: initiator → responder.
///
/// Opens the new-session CASE handshake. Carries the initiator's ephemeral
/// public key and the `DestinationId` that identifies which fabric and node
/// the initiator wants to talk to.
///
/// The optional fields `resumption_id` + `initiator_resume_mic` are present
/// only on the resumption path (M4.2). When both are present, the responder
/// may choose to respond with `Sigma2_Resume` instead of `Sigma2`.
///
/// Wire format: anonymous TLV structure, context tags 1–4, optional tags 5–7.
///
/// # Matter spec reference
/// Matter Core Spec §4.13.2.3 (Sigma1 TLV structure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sigma1 {
    /// 32-byte random nonce generated by the initiator. Context tag 1.
    pub initiator_random: [u8; RANDOM_LEN],
    /// Session ID proposed by the initiator. Context tag 2.
    pub initiator_session_id: u16,
    /// 32-byte SHA-256 `DestinationId` computed from the target fabric info. Context tag 3.
    pub dest_id: [u8; DEST_ID_LEN],
    /// 65-byte uncompressed P-256 ephemeral public key. Context tag 4.
    pub initiator_eph_pub: [u8; EPH_PUB_LEN],
    /// Optional session-capability advertisement. Context tag 5.
    pub initiator_session_params: Option<SessionParams>,
    /// Optional 16-byte resumption identifier from a prior session. Context tag 6.
    pub resumption_id: Option<[u8; RESUMPTION_ID_LEN]>,
    /// Optional 16-byte AEAD MIC for resumption authentication. Context tag 7.
    ///
    /// Called `initiatorResumeMic` in matter.js (CaseMessages.ts tag 7).
    pub initiator_resume_mic: Option<[u8; RESUME_MIC_LEN]>,
}

impl Sigma1 {
    /// The peer's typed [`SessionParameters`](crate::SessionParameters) from
    /// the optional `initiator_session_params` element (context tag 5), or `None` if
    /// the initiator did not carry one.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidParameter`] / [`Error::Codec`] if the element is
    /// present but malformed.
    pub(crate) fn session_parameters(&self) -> Result<Option<crate::SessionParameters>> {
        decode_session_params(self.initiator_session_params.as_ref())
    }
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(256);
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous)?;
            w.put_bytes(Tag::Context(1), &self.initiator_random)?;
            w.put_uint(Tag::Context(2), u64::from(self.initiator_session_id))?;
            w.put_bytes(Tag::Context(3), &self.dest_id)?;
            w.put_bytes(Tag::Context(4), &self.initiator_eph_pub)?;
            // Writer dropped here; buf is exclusively owned again.
        }
        // Append optional session params sub-element (context tag 5, raw bytes).
        if let Some(sp) = &self.initiator_session_params {
            buf.extend_from_slice(&sp.raw_tlv);
        }
        // Append optional resumption_id (context tag 6).
        if let Some(rid) = &self.resumption_id {
            let mut w = TlvWriter::new(&mut buf);
            w.put_bytes(Tag::Context(6), rid)?;
        }
        // Append optional initiator_resume_mic (context tag 7).
        if let Some(mic) = &self.initiator_resume_mic {
            let mut w = TlvWriter::new(&mut buf);
            w.put_bytes(Tag::Context(7), mic)?;
        }
        // Close the outer anonymous structure.
        buf.push(END_CONTAINER_BYTE);
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Sigma1`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if a required field is absent, an
    ///   unexpected context tag is present, or a byte-string has the wrong
    ///   fixed length.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut initiator_random: Option<[u8; RANDOM_LEN]> = None;
        let mut initiator_session_id: Option<u16> = None;
        let mut dest_id: Option<[u8; DEST_ID_LEN]> = None;
        let mut initiator_eph_pub: Option<[u8; EPH_PUB_LEN]> = None;
        let mut initiator_session_params: Option<SessionParams> = None;
        let mut resumption_id: Option<[u8; RESUMPTION_ID_LEN]> = None;
        let mut initiator_resume_mic: Option<[u8; RESUME_MIC_LEN]> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                // Tag 1: initiator_random (32 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RANDOM_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    initiator_random = Some(arr);
                }

                // Tag 2: initiator_session_id (u16)
                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Uint(v),
                }) => {
                    initiator_session_id =
                        Some(u16::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                // Tag 3: dest_id (32 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; DEST_ID_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    dest_id = Some(arr);
                }

                // Tag 4: initiator_eph_pub (65 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(4),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; EPH_PUB_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    initiator_eph_pub = Some(arr);
                }

                // Tag 5: initiator_session_params (optional structure, pass-through)
                Some(Element::ContainerStart {
                    tag: Tag::Context(5),
                    kind: ContainerKind::Structure,
                }) => {
                    let raw = skip_and_capture_substructure(bytes, &mut reader, 5)?;
                    initiator_session_params = Some(SessionParams { raw_tlv: raw });
                }

                // Tag 6: resumption_id (16 bytes, optional)
                Some(Element::Scalar {
                    tag: Tag::Context(6),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RESUMPTION_ID_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    resumption_id = Some(arr);
                }

                // Tag 7: initiator_resume_mic (16 bytes, optional)
                Some(Element::Scalar {
                    tag: Tag::Context(7),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RESUME_MIC_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    initiator_resume_mic = Some(arr);
                }

                // Forward-compat: skip any unrecognised field instead of
                // rejecting the message (chip/matter.js accept + ignore them).
                other => skip_unknown_field(&mut reader, other.as_ref())?,
            }
        }

        Ok(Self {
            initiator_random: initiator_random.ok_or(Error::InvalidParameter)?,
            initiator_session_id: initiator_session_id.ok_or(Error::InvalidParameter)?,
            dest_id: dest_id.ok_or(Error::InvalidParameter)?,
            initiator_eph_pub: initiator_eph_pub.ok_or(Error::InvalidParameter)?,
            initiator_session_params,
            resumption_id,
            initiator_resume_mic,
        })
    }
}

// ---------------------------------------------------------------------------
// Sigma2
// ---------------------------------------------------------------------------

/// The second CASE message: responder → initiator (new-session path).
///
/// Carries the responder's ephemeral public key and the encrypted TBE
/// (To-Be-Encrypted) blob that contains the responder's NOC chain +
/// ECDSA signature. Decryption + signature verification is the state
/// machine's responsibility, not the codec's.
///
/// Wire format: anonymous TLV structure, context tags 1–4, optional tag 5.
///
/// # Matter spec reference
/// Matter Core Spec §4.13.2.3 (Sigma2 TLV structure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sigma2 {
    /// 32-byte random nonce generated by the responder. Context tag 1.
    pub responder_random: [u8; RANDOM_LEN],
    /// Session ID proposed by the responder. Context tag 2.
    pub responder_session_id: u16,
    /// 65-byte uncompressed P-256 ephemeral public key. Context tag 3.
    pub responder_eph_pub: [u8; EPH_PUB_LEN],
    /// Encrypted TBE blob (AES-128-CCM; variable length). Context tag 4.
    pub encrypted: Vec<u8>,
    /// Optional session-capability advertisement. Context tag 5.
    pub responder_session_params: Option<SessionParams>,
}

impl Sigma2 {
    /// The peer's typed [`SessionParameters`](crate::SessionParameters) from
    /// the optional `responder_session_params` element (context tag 5), or `None` if
    /// the responder did not carry one.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidParameter`] / [`Error::Codec`] if the element is
    /// present but malformed.
    pub(crate) fn session_parameters(&self) -> Result<Option<crate::SessionParameters>> {
        decode_session_params(self.responder_session_params.as_ref())
    }
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(128 + self.encrypted.len());
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous)?;
            w.put_bytes(Tag::Context(1), &self.responder_random)?;
            w.put_uint(Tag::Context(2), u64::from(self.responder_session_id))?;
            w.put_bytes(Tag::Context(3), &self.responder_eph_pub)?;
            w.put_bytes(Tag::Context(4), &self.encrypted)?;
            // Writer dropped here.
        }
        // Append optional session params sub-element (context tag 5, raw bytes).
        if let Some(sp) = &self.responder_session_params {
            buf.extend_from_slice(&sp.raw_tlv);
        }
        // Close the outer anonymous structure.
        buf.push(END_CONTAINER_BYTE);
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Sigma2`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if a required field is absent, an
    ///   unexpected context tag is present, or a byte-string has the wrong
    ///   fixed length.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut responder_random: Option<[u8; RANDOM_LEN]> = None;
        let mut responder_session_id: Option<u16> = None;
        let mut responder_eph_pub: Option<[u8; EPH_PUB_LEN]> = None;
        let mut encrypted: Option<Vec<u8>> = None;
        let mut responder_session_params: Option<SessionParams> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                // Tag 1: responder_random (32 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RANDOM_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    responder_random = Some(arr);
                }

                // Tag 2: responder_session_id (u16)
                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Uint(v),
                }) => {
                    responder_session_id =
                        Some(u16::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                // Tag 3: responder_eph_pub (65 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; EPH_PUB_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    responder_eph_pub = Some(arr);
                }

                // Tag 4: encrypted blob (variable length)
                Some(Element::Scalar {
                    tag: Tag::Context(4),
                    value: Value::Bytes(b),
                }) => {
                    encrypted = Some(b);
                }

                // Tag 5: responder_session_params (optional structure, pass-through)
                Some(Element::ContainerStart {
                    tag: Tag::Context(5),
                    kind: ContainerKind::Structure,
                }) => {
                    let raw = skip_and_capture_substructure(bytes, &mut reader, 5)?;
                    responder_session_params = Some(SessionParams { raw_tlv: raw });
                }

                // Forward-compat: skip any unrecognised field (see `skip_unknown_field`).
                other => skip_unknown_field(&mut reader, other.as_ref())?,
            }
        }

        Ok(Self {
            responder_random: responder_random.ok_or(Error::InvalidParameter)?,
            responder_session_id: responder_session_id.ok_or(Error::InvalidParameter)?,
            responder_eph_pub: responder_eph_pub.ok_or(Error::InvalidParameter)?,
            encrypted: encrypted.ok_or(Error::InvalidParameter)?,
            responder_session_params,
        })
    }
}

// ---------------------------------------------------------------------------
// Sigma2Resume
// ---------------------------------------------------------------------------

/// The resumption response message: responder → initiator.
///
/// Sent instead of [`Sigma2`] when the responder successfully validates
/// `sigma1_resume_mic` from a [`Sigma1`] that carried resumption fields.
///
/// After the initiator verifies `resume_mic` from this message it sends a
/// plain `Status=Success` transport message. There is **no** `Sigma3_Resume`
/// wire message — the protocol ends here.
///
/// Wire format: anonymous TLV structure, context tags 1–3, optional tag 4.
///
/// # matter.js cross-reference
///
/// `TlvCaseSigma2Resume` from `CaseMessages.ts`:
/// ```ts
/// TlvCaseSigma2Resume = TlvObject({
///     1: resumptionId     (bytes, length=16),
///     2: resumeMic        (bytes, length=16),
///     3: responderSessionId (uint16),
///     4: responderSessionParams (optional struct),
/// })
/// ```
///
/// # Matter spec reference
/// Matter Core Spec §4.13.2.3 (`Sigma2_Resume` TLV structure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sigma2Resume {
    /// 16-byte new resumption ID generated by the responder. Context tag 1.
    ///
    /// This replaces the old `resumption_id` in the caller's record after
    /// the handshake completes.
    pub resumption_id: [u8; RESUMPTION_ID_LEN],
    /// 16-byte AES-128-CCM MAC tag computed by the responder. Context tag 2.
    ///
    /// Called `resumeMic` in matter.js. Verifier uses
    /// [`super::sigma::verify_sigma2_resume_mic`].
    pub resume_mic: [u8; RESUME_MIC_LEN],
    /// Session ID proposed by the responder. Context tag 3.
    pub responder_session_id: u16,
    /// Optional session-capability advertisement. Context tag 4.
    pub responder_session_params: Option<SessionParams>,
}

impl Sigma2Resume {
    /// The peer's typed [`SessionParameters`](crate::SessionParameters) from
    /// the optional `responder_session_params` element (context tag 4), or `None` if
    /// the responder did not carry one.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidParameter`] / [`Error::Codec`] if the element is
    /// present but malformed.
    pub(crate) fn session_parameters(&self) -> Result<Option<crate::SessionParameters>> {
        decode_session_params(self.responder_session_params.as_ref())
    }
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(64);
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous)?;
            w.put_bytes(Tag::Context(1), &self.resumption_id)?;
            w.put_bytes(Tag::Context(2), &self.resume_mic)?;
            w.put_uint(Tag::Context(3), u64::from(self.responder_session_id))?;
            // Writer dropped here; buf is exclusively owned again.
        }
        // Append optional session params sub-element (context tag 4, raw bytes).
        if let Some(sp) = &self.responder_session_params {
            buf.extend_from_slice(&sp.raw_tlv);
        }
        // Close the outer anonymous structure.
        buf.push(END_CONTAINER_BYTE);
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Sigma2Resume`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if a required field is absent, an
    ///   unexpected context tag is present, or a byte-string has the wrong
    ///   fixed length.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut resumption_id: Option<[u8; RESUMPTION_ID_LEN]> = None;
        let mut resume_mic: Option<[u8; RESUME_MIC_LEN]> = None;
        let mut responder_session_id: Option<u16> = None;
        let mut responder_session_params: Option<SessionParams> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                // Tag 1: resumption_id (16 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RESUMPTION_ID_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    resumption_id = Some(arr);
                }

                // Tag 2: resume_mic (16 bytes)
                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RESUME_MIC_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    resume_mic = Some(arr);
                }

                // Tag 3: responder_session_id (u16)
                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Uint(v),
                }) => {
                    responder_session_id =
                        Some(u16::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                // Tag 4: responder_session_params (optional structure, pass-through)
                Some(Element::ContainerStart {
                    tag: Tag::Context(4),
                    kind: ContainerKind::Structure,
                }) => {
                    let raw = skip_and_capture_substructure(bytes, &mut reader, 4)?;
                    responder_session_params = Some(SessionParams { raw_tlv: raw });
                }

                // Forward-compat: skip any unrecognised field instead of
                // rejecting the message (chip/matter.js accept + ignore them).
                other => skip_unknown_field(&mut reader, other.as_ref())?,
            }
        }

        Ok(Self {
            resumption_id: resumption_id.ok_or(Error::InvalidParameter)?,
            resume_mic: resume_mic.ok_or(Error::InvalidParameter)?,
            responder_session_id: responder_session_id.ok_or(Error::InvalidParameter)?,
            responder_session_params,
        })
    }
}

// ---------------------------------------------------------------------------
// Sigma3
// ---------------------------------------------------------------------------

/// The third CASE message: initiator → responder (new-session path).
///
/// Carries the encrypted TBE blob that contains the initiator's NOC chain +
/// ECDSA signature. The responder decrypts it and verifies the signature to
/// complete mutual authentication.
///
/// Wire format: anonymous TLV structure, context tag 1 only.
///
/// # Matter spec reference
/// Matter Core Spec §4.13.2.3 (Sigma3 TLV structure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sigma3 {
    /// Encrypted TBE blob (AES-128-CCM; variable length). Context tag 1.
    pub encrypted: Vec<u8>,
}

impl Sigma3 {
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(16 + self.encrypted.len());
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous)?;
        w.put_bytes(Tag::Context(1), &self.encrypted)?;
        w.end_container()?;
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Sigma3`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if the `encrypted` field is absent or an
    ///   unexpected tag is present.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut encrypted: Option<Vec<u8>> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                // Tag 1: encrypted blob (variable length)
                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    encrypted = Some(b);
                }

                // Forward-compat: skip any unrecognised field (see `skip_unknown_field`).
                other => skip_unknown_field(&mut reader, other.as_ref())?,
            }
        }

        Ok(Self {
            encrypted: encrypted.ok_or(Error::InvalidParameter)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper: build a minimal SessionParams raw_tlv at a given context tag.
    // -----------------------------------------------------------------------

    /// Build a minimal `SessionParams` at the given context `tag_num`.
    /// Contains one field: idleInterval (tag 1) = 500, mirroring matter.js.
    fn minimal_session_params(tag_num: u8) -> SessionParams {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Context(tag_num)).unwrap();
        w.put_uint(Tag::Context(1), 500).unwrap();
        w.end_container().unwrap();
        SessionParams { raw_tlv: buf }
    }

    // -----------------------------------------------------------------------
    // Sigma1
    // -----------------------------------------------------------------------

    #[test]
    fn sigma1_roundtrip() {
        let msg = Sigma1 {
            initiator_random: [0x11; 32],
            initiator_session_id: 0x1234,
            dest_id: [0x22; 32],
            initiator_eph_pub: [0x04; 65],
            initiator_session_params: None,
            resumption_id: None,
            initiator_resume_mic: None,
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma1::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma1_with_session_params_roundtrips() {
        let msg = Sigma1 {
            initiator_random: [0xAA; 32],
            initiator_session_id: 0x0001,
            dest_id: [0xBB; 32],
            initiator_eph_pub: [0x04; 65],
            initiator_session_params: Some(minimal_session_params(5)),
            resumption_id: None,
            initiator_resume_mic: None,
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma1::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma1_with_resumption_fields_roundtrips() {
        // Both resumption fields present — this is the M4.2 path.
        let msg = Sigma1 {
            initiator_random: [0x33; 32],
            initiator_session_id: 0xFFFF,
            dest_id: [0x44; 32],
            initiator_eph_pub: [0x04; 65],
            initiator_session_params: None,
            resumption_id: Some([0x55; 16]),
            initiator_resume_mic: Some([0x66; 16]),
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma1::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma1_with_all_optional_fields_roundtrips() {
        // All 3 optional fields present simultaneously.
        let msg = Sigma1 {
            initiator_random: [0x77; 32],
            initiator_session_id: 100,
            dest_id: [0x88; 32],
            initiator_eph_pub: [0x04; 65],
            initiator_session_params: Some(minimal_session_params(5)),
            resumption_id: Some([0x99; 16]),
            initiator_resume_mic: Some([0xAA; 16]),
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma1::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma1_rejects_missing_required_field() {
        // Omit tag 3 (dest_id) — decoder must return InvalidParameter.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x11u8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 1_u64).unwrap();
        // tag 3 intentionally omitted
        w.put_bytes(Tag::Context(4), &[0x04u8; 65]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(Sigma1::decode(&buf), Err(Error::InvalidParameter)));
    }

    #[test]
    fn sigma1_ignores_unknown_forward_compat_fields() {
        // Forward-compat: an unknown scalar tag AND an unknown container tag (a
        // future spec-optional field) must be skipped, not rejected —
        // chip/matter.js accept + ignore them, and strict rejection would make
        // a future device revision unreachable.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x11u8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 7_u64).unwrap();
        w.put_bytes(Tag::Context(3), &[0x22u8; 32]).unwrap();
        w.put_bytes(Tag::Context(4), &[0x04u8; 65]).unwrap();
        w.put_uint(Tag::Context(8), 42_u64).unwrap(); // unknown scalar
        w.start_structure(Tag::Context(9)).unwrap(); // unknown container
        w.put_uint(Tag::Context(0), 1_u64).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        let decoded = Sigma1::decode(&buf).unwrap();
        assert_eq!(decoded.initiator_session_id, 7);
        assert_eq!(decoded.initiator_random, [0x11; 32]);
    }

    #[test]
    fn sigma1_rejects_wrong_random_length() {
        // initiator_random must be exactly 32 bytes; send 31.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x11u8; 31]).unwrap(); // wrong length
        w.put_uint(Tag::Context(2), 1_u64).unwrap();
        w.put_bytes(Tag::Context(3), &[0x22u8; 32]).unwrap();
        w.put_bytes(Tag::Context(4), &[0x04u8; 65]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(Sigma1::decode(&buf), Err(Error::InvalidParameter)));
    }

    #[test]
    fn sigma1_rejects_wrong_eph_pub_length() {
        // initiator_eph_pub must be 65 bytes; send 64.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x11u8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 1_u64).unwrap();
        w.put_bytes(Tag::Context(3), &[0x22u8; 32]).unwrap();
        w.put_bytes(Tag::Context(4), &[0x04u8; 64]).unwrap(); // wrong length
        w.end_container().unwrap();
        assert!(matches!(Sigma1::decode(&buf), Err(Error::InvalidParameter)));
    }

    // -----------------------------------------------------------------------
    // Sigma2
    // -----------------------------------------------------------------------

    #[test]
    fn sigma2_ignores_unknown_forward_compat_field() {
        // Forward-compat: an unknown tag must be skipped, not rejected.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xCCu8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 0x5678_u64).unwrap();
        w.put_bytes(Tag::Context(3), &[0x04u8; 65]).unwrap();
        w.put_bytes(Tag::Context(4), &[0xDEu8; 80]).unwrap();
        w.put_uint(Tag::Context(9), 42_u64).unwrap(); // unknown tag
        w.end_container().unwrap();
        let decoded = Sigma2::decode(&buf).unwrap();
        assert_eq!(decoded.responder_session_id, 0x5678);
    }

    #[test]
    fn sigma2_roundtrip() {
        let msg = Sigma2 {
            responder_random: [0xCC; 32],
            responder_session_id: 0x5678,
            responder_eph_pub: [0x04; 65],
            encrypted: vec![0xDE; 80], // opaque blob
            responder_session_params: None,
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma2::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma2_rejects_short_eph_pub() {
        // responder_eph_pub must be 65 bytes; send 32.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xCCu8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 1_u64).unwrap();
        w.put_bytes(Tag::Context(3), &[0x04u8; 32]).unwrap(); // wrong length (32 not 65)
        w.put_bytes(Tag::Context(4), &[0xDEu8; 80]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(Sigma2::decode(&buf), Err(Error::InvalidParameter)));
    }

    #[test]
    fn sigma2_with_session_params_roundtrips() {
        let msg = Sigma2 {
            responder_random: [0xDD; 32],
            responder_session_id: 42,
            responder_eph_pub: [0x04; 65],
            encrypted: vec![0xEE; 120],
            responder_session_params: Some(minimal_session_params(5)),
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma2::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma2_rejects_missing_encrypted_field() {
        // Omit tag 4 (encrypted).
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xCCu8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 1_u64).unwrap();
        w.put_bytes(Tag::Context(3), &[0x04u8; 65]).unwrap();
        // tag 4 intentionally omitted
        w.end_container().unwrap();
        assert!(matches!(Sigma2::decode(&buf), Err(Error::InvalidParameter)));
    }

    // -----------------------------------------------------------------------
    // Sigma2Resume
    // -----------------------------------------------------------------------

    #[test]
    fn sigma2resume_roundtrip() {
        let msg = Sigma2Resume {
            resumption_id: [0xA1; 16],
            resume_mic: [0xB2; 16],
            responder_session_id: 0x9900,
            responder_session_params: None,
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma2Resume::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma2resume_with_session_params_roundtrips() {
        let msg = Sigma2Resume {
            resumption_id: [0xC3; 16],
            resume_mic: [0xD4; 16],
            responder_session_id: 0x1234,
            responder_session_params: Some(minimal_session_params(4)),
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma2Resume::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma2resume_rejects_missing_resumption_id() {
        // Omit tag 1 (resumption_id).
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        // tag 1 intentionally omitted
        w.put_bytes(Tag::Context(2), &[0xB2u8; 16]).unwrap();
        w.put_uint(Tag::Context(3), 0x9900_u64).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            Sigma2Resume::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn sigma2resume_rejects_missing_resume_mic() {
        // Omit tag 2 (resume_mic).
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xA1u8; 16]).unwrap();
        // tag 2 intentionally omitted
        w.put_uint(Tag::Context(3), 0x9900_u64).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            Sigma2Resume::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn sigma2resume_rejects_wrong_resumption_id_length() {
        // resumption_id must be exactly 16 bytes; send 15.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xA1u8; 15]).unwrap(); // wrong length
        w.put_bytes(Tag::Context(2), &[0xB2u8; 16]).unwrap();
        w.put_uint(Tag::Context(3), 0x9900_u64).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            Sigma2Resume::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn sigma2resume_rejects_wrong_resume_mic_length() {
        // resume_mic must be exactly 16 bytes; send 17.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xA1u8; 16]).unwrap();
        w.put_bytes(Tag::Context(2), &[0xB2u8; 17]).unwrap(); // wrong length
        w.put_uint(Tag::Context(3), 0x9900_u64).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            Sigma2Resume::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn sigma2resume_ignores_unknown_forward_compat_field() {
        // Forward-compat: an unknown tag must be skipped, not rejected.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xA1u8; 16]).unwrap();
        w.put_bytes(Tag::Context(2), &[0xB2u8; 16]).unwrap();
        w.put_uint(Tag::Context(3), 0x9900_u64).unwrap();
        w.put_uint(Tag::Context(5), 42_u64).unwrap(); // unknown tag
        w.end_container().unwrap();
        assert!(
            Sigma2Resume::decode(&buf).is_ok(),
            "unknown forward-compat field must be ignored"
        );
    }

    // -----------------------------------------------------------------------
    // Sigma3
    // -----------------------------------------------------------------------

    #[test]
    fn sigma3_roundtrip() {
        let msg = Sigma3 {
            encrypted: vec![0xFF; 100],
        };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma3::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn sigma3_rejects_missing_encrypted_field() {
        // Empty structure — missing tag 1.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.end_container().unwrap();
        assert!(matches!(Sigma3::decode(&buf), Err(Error::InvalidParameter)));
    }

    #[test]
    fn sigma3_ignores_unknown_forward_compat_field() {
        // Forward-compat: an unknown tag must be skipped, not rejected.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xFFu8; 100]).unwrap();
        w.put_uint(Tag::Context(2), 99_u64).unwrap(); // unknown tag
        w.end_container().unwrap();
        let decoded = Sigma3::decode(&buf).unwrap();
        assert_eq!(decoded.encrypted, vec![0xFF; 100]);
    }

    #[test]
    fn sigma3_empty_encrypted_roundtrips() {
        // Empty encrypted blob is valid (the AES-CCM layer handles zero-length
        // plaintext; this tests the codec boundary only).
        let msg = Sigma3 { encrypted: vec![] };
        let bytes = msg.encode().unwrap();
        let decoded = Sigma3::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}

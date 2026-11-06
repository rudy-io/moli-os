//! TLV codec for the 5 PASE messages (Matter Core Spec §4.14.1.2).
//!
//! Each message is an anonymous TLV structure with context-tagged fields.
//! Decoders return strongly-typed structs; encoders accept the struct
//! and emit `Vec<u8>` bytes for the wire.
//!
//! # Tag numbers (pinned from matter.js `PaseMessages.ts`, 2026-05-19)
//!
//! ```text
//! Message               Field                     Context tag
//! ──────────────────────────────────────────────────────────
//! PbkdfParamRequest     initiator_random          1
//! PbkdfParamRequest     initiator_session_id      2
//! PbkdfParamRequest     passcode_id               3
//! PbkdfParamRequest     has_pbkdf_parameters      4
//! PbkdfParamRequest     initiator_session_params  5 (opt)
//! PbkdfParamResponse    initiator_random          1
//! PbkdfParamResponse    responder_random          2
//! PbkdfParamResponse    responder_session_id      3
//! PbkdfParamResponse    pbkdf_parameters          4 (opt)
//! PbkdfParamResponse    responder_session_params  5 (opt)
//! PbkdfParamsInner      iterations                1
//! PbkdfParamsInner      salt                      2
//! Pake1                 x                         1
//! Pake2                 y                         1
//! Pake2                 verifier                  2
//! Pake3                 verifier                  1
//! ```
//!
//! # `SessionParams` (M3 pass-through)
//!
//! `SessionParams.raw_tlv` holds the raw TLV bytes of the optional
//! `SessionParameters` substructure, including the container-start control
//! byte through the end-of-container byte (`0x18`) inclusive. M3 does not
//! interpret the fields inside; M6 commissioning will decode them.
//!
//! For reference, matter.js (from `TlvSessionParameters` in `PaseMessages.ts`)
//! populates: `idleInterval`(1), `activeInterval`(2), `activeThreshold`(3),
//! `dataModelRevision`(4), `interactionModelRevision`(5), `specificationVersion`(6),
//! `maxPathsPerInvoke`(7), `supportedTransports`(8), `maxTcpMessageSize`(9).
//!
//! # Dead-code allowance
//!
//! All items here are `pub(crate)` and consumed by the PASE state machines
//! (`prover.rs`, `verifier.rs`).
#![allow(dead_code)]

use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Byte-length constants, pinned from matter.js CryptoConstants.ts:
//   CRYPTO_GROUP_SIZE_BYTES = 32
//   CRYPTO_PUBLIC_KEY_SIZE_BYTES = 2 * 32 + 1 = 65  (uncompressed P-256 point)
//   CRYPTO_HASH_LEN_BYTES = 32
// ---------------------------------------------------------------------------

/// Byte length of an uncompressed P-256 public key (0x04 prefix + 32 + 32).
const POINT_LEN: usize = 65;

/// Byte length of a SPAKE2+ confirmation tag (SHA-256 output).
const HASH_LEN: usize = 32;

/// Byte length of the per-session random nonce fields.
const RANDOM_LEN: usize = 32;

/// PBKDF salt minimum length per Matter spec §3.10.3.
const SALT_MIN_LEN: usize = 16;

/// PBKDF salt maximum length per Matter spec §3.10.3.
const SALT_MAX_LEN: usize = 32;

// ---------------------------------------------------------------------------
// TLV end-of-container byte (Matter Core Spec §A.2, element type 0x18).
// This is the raw byte pushed to close a container when the `TlvWriter`
// borrow cannot be held (e.g., because we need to push raw optional bytes
// into the same buffer).
// ---------------------------------------------------------------------------
const END_CONTAINER_BYTE: u8 = 0x18;

// ---------------------------------------------------------------------------
// Helper: advance past the outer anonymous structure start.
// ---------------------------------------------------------------------------

/// Advance `reader` past the anonymous-structure opening element.
///
/// Returns `Error::InvalidParameter` if the first element is not an
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
// Helper: skip a sub-structure body and return its raw TLV bytes.
// ---------------------------------------------------------------------------

/// After a `ContainerStart` for a sub-structure has been consumed from
/// `full_message_reader`, skip the sub-structure body to the matching
/// `ContainerEnd`, then re-parse `full_message` to recover the raw bytes of
/// that sub-structure (container-start byte through end-of-container byte).
///
/// The re-parse uses [`TlvReader::read_value`] on a fresh reader positioned
/// just before the last context-tagged sub-structure in `full_message`, then
/// re-encodes the value tree via [`TlvWriter::write_value`]. This preserves
/// the TLV encoding faithfully for the values that matter.js actually puts in
/// `SessionParams` (all scalars — uints and bitmaps), which have deterministic
/// minimal encoding.
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
    // Track depth so nested structures (possible inside SessionParams) are
    // handled without prematurely stopping.
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
            // Scalars: ignore while skipping. The `_` arm is required because
            // `Element` is `#[non_exhaustive]`.
            Some(_) => {}
        }
    }

    // Re-parse `full_message` to capture the sub-element's raw bytes. We do a
    // fresh reader walk of the outer structure, use `read_value` for each
    // context-tagged container element with the matching tag, and re-encode it.
    let mut reader = TlvReader::new(full_message);
    // Consume the outer anonymous structure start.
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => return Err(Error::InvalidParameter),
    }

    loop {
        match reader.next()? {
            None | Some(Element::ContainerEnd) => {
                // Reached the end without finding the target sub-element.
                return Err(Error::InvalidParameter);
            }
            Some(Element::ContainerStart {
                tag: Tag::Context(t),
                kind: ContainerKind::Structure,
            }) if t == context_tag => {
                // Found it. Collect children into a Value tree, then re-encode.
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
            // Scalars or unknown elements at the outer level: skip.
            // `_` arm required because `Element` is `#[non_exhaustive]`.
            Some(_) => {}
        }
    }
}

/// Collect the children of a structure that has already had its
/// `ContainerStart` consumed, returning a `Value::Structure(…)`.
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
                    ContainerKind::Array => {
                        let elems = collect_array_body(reader)?;
                        Value::Array(elems)
                    }
                    ContainerKind::List => {
                        let inner_list = collect_list_body(reader)?;
                        Value::List(inner_list)
                    }
                    // `ContainerKind` is `#[non_exhaustive]`; reject unknown kinds.
                    _ => return Err(Error::InvalidParameter),
                };
                members.push((tag, inner));
            }
            // None (unexpected EOF) or unknown element type — both are invalid.
            // `Element` is `#[non_exhaustive]` so the `Some(_)` arm is needed.
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

// ---------------------------------------------------------------------------
// SessionParams — opaque TLV bytes passthrough.
// ---------------------------------------------------------------------------

/// Opaque bytes representing one of the optional `SessionParameters`
/// substructures in [`PbkdfParamRequest`] or [`PbkdfParamResponse`].
///
/// M3 preserves the bytes byte-for-byte for round-trip fidelity without
/// interpreting the fields (idle/active intervals, data-model revision, …).
/// M6 commissioning will decode the contents when it needs them.
///
/// The bytes cover the entire sub-element: the container-start control byte
/// through the matching end-of-container byte (`0x18`) inclusive, including
/// the context tag byte that identifies this field within the parent structure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SessionParams {
    /// Raw TLV bytes of the `SessionParameters` sub-element, context-tag
    /// through end-container, as captured from the wire or built by the encoder.
    pub raw_tlv: Vec<u8>,
}

// ---------------------------------------------------------------------------
// PbkdfParamsInner — the nested struct inside PbkdfParamResponse tag 4.
// ---------------------------------------------------------------------------

/// The inner structure encoded at context tag 4 of [`PbkdfParamResponse`].
///
/// Contains the PBKDF2 parameters the responder commits to use when deriving
/// the SPAKE2+ w0/w1 scalars from the passcode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PbkdfParamsInner {
    /// PBKDF2 iteration count. Matter spec §3.10.3 requires ≥ 1000.
    pub iterations: u32,
    /// PBKDF2 salt. Matter spec §3.10.3 requires 16–32 bytes.
    pub salt: Vec<u8>,
}

impl PbkdfParamsInner {
    /// Encode as a context-tagged TLV structure using `outer_tag`.
    ///
    /// `outer_tag` is the context tag used in the *parent* structure (tag 4
    /// in [`PbkdfParamResponse`]). The resulting bytes are self-contained:
    /// container-start through end-of-container.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if the salt length is outside
    ///   \[`SALT_MIN_LEN`, `SALT_MAX_LEN`\].
    /// - [`Error::Codec`] on any codec error.
    pub(crate) fn encode(&self, outer_tag: Tag) -> Result<Vec<u8>> {
        if self.salt.len() < SALT_MIN_LEN || self.salt.len() > SALT_MAX_LEN {
            return Err(Error::InvalidParameter);
        }
        let mut buf = Vec::with_capacity(24 + self.salt.len());
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(outer_tag)?;
        // context tag 1: iterations (u32, widened to u64 for the API).
        w.put_uint(Tag::Context(1), u64::from(self.iterations))?;
        // context tag 2: salt bytes.
        w.put_bytes(Tag::Context(2), &self.salt)?;
        w.end_container()?;
        Ok(buf)
    }

    /// Decode the body of the inner structure from `reader`.
    ///
    /// The caller must have already consumed the `ContainerStart` for this
    /// structure. This function reads until the matching `ContainerEnd`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] on unrecognised context tag, missing
    ///   required field, wrong value type, or salt outside \[16, 32\] bytes.
    /// - [`Error::Codec`] on malformed TLV.
    fn decode_body(reader: &mut TlvReader<'_>) -> Result<Self> {
        let mut iterations: Option<u32> = None;
        let mut salt: Option<Vec<u8>> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Uint(v),
                }) => {
                    iterations = Some(u32::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Bytes(b),
                }) => {
                    if b.len() < SALT_MIN_LEN || b.len() > SALT_MAX_LEN {
                        return Err(Error::InvalidParameter);
                    }
                    salt = Some(b);
                }

                // None (unexpected EOF) or any unrecognised element.
                // `Element` is `#[non_exhaustive]` so the `Some(_)` arm is needed.
                None | Some(_) => return Err(Error::InvalidParameter),
            }
        }

        Ok(Self {
            iterations: iterations.ok_or(Error::InvalidParameter)?,
            salt: salt.ok_or(Error::InvalidParameter)?,
        })
    }
}

// ---------------------------------------------------------------------------
// PbkdfParamRequest
// ---------------------------------------------------------------------------

/// The first PASE message: commissioner → device.
///
/// Sent by the commissioner (initiator) to open PASE negotiation and propose
/// session parameters.
///
/// Wire format: anonymous TLV structure, context tags 1–4, and optional tag 5.
///
/// # Matter spec reference
/// Matter Core Spec §4.14.1.2 (was §3.10.5 in pre-1.3 editions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PbkdfParamRequest {
    /// 32-byte random nonce generated by the initiator.
    pub initiator_random: [u8; RANDOM_LEN],
    /// Session ID proposed by the initiator for this session.
    pub initiator_session_id: u16,
    /// Passcode ID. Controllers typically send 0 (the default passcode).
    pub passcode_id: u16,
    /// `true` if the initiator cached the responder's PBKDF parameters from a
    /// previous interaction (responder may omit them from its response).
    pub has_pbkdf_parameters: bool,
    /// Optional session-capability advertisement from the initiator.
    pub initiator_session_params: Option<SessionParams>,
}

impl PbkdfParamRequest {
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        // Build the fixed fields inside the writer's borrow, then drop the
        // writer so we can push optional raw bytes and the end-container byte.
        let mut buf = Vec::with_capacity(128);
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous)?;
            w.put_bytes(Tag::Context(1), &self.initiator_random)?;
            w.put_uint(Tag::Context(2), u64::from(self.initiator_session_id))?;
            w.put_uint(Tag::Context(3), u64::from(self.passcode_id))?;
            w.put_bool(Tag::Context(4), self.has_pbkdf_parameters)?;
            // Writer is dropped here; buf is exclusively owned again.
        }
        // Append optional session params sub-element (already fully encoded
        // as a context-tagged TLV structure).
        if let Some(sp) = &self.initiator_session_params {
            buf.extend_from_slice(&sp.raw_tlv);
        }
        // Close the outer anonymous structure.
        buf.push(END_CONTAINER_BYTE);
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `PbkdfParamRequest`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if a required field is absent, an
    ///   unexpected context tag is present, or a byte-string has the wrong
    ///   length.
    /// - [`Error::Codec`] if the TLV is malformed.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut initiator_random: Option<[u8; RANDOM_LEN]> = None;
        let mut initiator_session_id: Option<u16> = None;
        let mut passcode_id: Option<u16> = None;
        let mut has_pbkdf_parameters: Option<bool> = None;
        let mut initiator_session_params: Option<SessionParams> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RANDOM_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    initiator_random = Some(arr);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Uint(v),
                }) => {
                    initiator_session_id =
                        Some(u16::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Uint(v),
                }) => {
                    passcode_id = Some(u16::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(4),
                    value: Value::Bool(b),
                }) => {
                    has_pbkdf_parameters = Some(b);
                }

                Some(Element::ContainerStart {
                    tag: Tag::Context(5),
                    kind: ContainerKind::Structure,
                }) => {
                    // Recover raw bytes for byte-exact round-trip (tag 5).
                    let raw = skip_and_capture_substructure(bytes, &mut reader, 5)?;
                    initiator_session_params = Some(SessionParams { raw_tlv: raw });
                }

                // None (unexpected EOF) or any unrecognised element.
                // `Element` is `#[non_exhaustive]` so the `Some(_)` arm is needed.
                None | Some(_) => return Err(Error::InvalidParameter),
            }
        }

        Ok(Self {
            initiator_random: initiator_random.ok_or(Error::InvalidParameter)?,
            initiator_session_id: initiator_session_id.ok_or(Error::InvalidParameter)?,
            passcode_id: passcode_id.ok_or(Error::InvalidParameter)?,
            has_pbkdf_parameters: has_pbkdf_parameters.ok_or(Error::InvalidParameter)?,
            initiator_session_params,
        })
    }
}

// ---------------------------------------------------------------------------
// PbkdfParamResponse
// ---------------------------------------------------------------------------

/// The second PASE message: device → commissioner.
///
/// Carries the responder's random nonce, its chosen session ID, and optionally
/// the PBKDF parameters (omitted when the initiator indicated it had them
/// cached in [`PbkdfParamRequest::has_pbkdf_parameters`]).
///
/// Wire format: anonymous TLV structure, context tags 1–3, and optional tags
/// 4 and 5.
///
/// # Matter spec reference
/// Matter Core Spec §4.14.1.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PbkdfParamResponse {
    /// Echo of the initiator's random nonce (from [`PbkdfParamRequest`]).
    pub initiator_random: [u8; RANDOM_LEN],
    /// 32-byte random nonce generated by the responder.
    pub responder_random: [u8; RANDOM_LEN],
    /// Session ID chosen by the responder.
    pub responder_session_id: u16,
    /// PBKDF2 parameters. Omitted when the initiator indicated it had them cached.
    pub pbkdf_parameters: Option<PbkdfParamsInner>,
    /// Optional session-capability advertisement from the responder.
    pub responder_session_params: Option<SessionParams>,
}

impl PbkdfParamResponse {
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if `pbkdf_parameters` is present and
    ///   the salt length is outside \[16, 32\].
    /// - [`Error::Codec`] on any codec error.
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(160);
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous)?;
            w.put_bytes(Tag::Context(1), &self.initiator_random)?;
            w.put_bytes(Tag::Context(2), &self.responder_random)?;
            w.put_uint(Tag::Context(3), u64::from(self.responder_session_id))?;
            // Writer dropped here.
        }
        // Append optional PBKDF params sub-element (context tag 4).
        if let Some(pbkdf) = &self.pbkdf_parameters {
            let inner_bytes = pbkdf.encode(Tag::Context(4))?;
            buf.extend_from_slice(&inner_bytes);
        }
        // Append optional session params sub-element (context tag 5).
        if let Some(sp) = &self.responder_session_params {
            buf.extend_from_slice(&sp.raw_tlv);
        }
        buf.push(END_CONTAINER_BYTE);
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `PbkdfParamResponse`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if required fields are absent,
    ///   unexpected tags appear, or byte-string lengths are wrong.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut initiator_random: Option<[u8; RANDOM_LEN]> = None;
        let mut responder_random: Option<[u8; RANDOM_LEN]> = None;
        let mut responder_session_id: Option<u16> = None;
        let mut pbkdf_parameters: Option<PbkdfParamsInner> = None;
        let mut responder_session_params: Option<SessionParams> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RANDOM_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    initiator_random = Some(arr);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; RANDOM_LEN] =
                        b.try_into().map_err(|_| Error::InvalidParameter)?;
                    responder_random = Some(arr);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Uint(v),
                }) => {
                    responder_session_id =
                        Some(u16::try_from(v).map_err(|_| Error::InvalidParameter)?);
                }

                Some(Element::ContainerStart {
                    tag: Tag::Context(4),
                    kind: ContainerKind::Structure,
                }) => {
                    // ContainerStart consumed; decode the body directly.
                    let inner = PbkdfParamsInner::decode_body(&mut reader)?;
                    pbkdf_parameters = Some(inner);
                }

                Some(Element::ContainerStart {
                    tag: Tag::Context(5),
                    kind: ContainerKind::Structure,
                }) => {
                    let raw = skip_and_capture_substructure(bytes, &mut reader, 5)?;
                    responder_session_params = Some(SessionParams { raw_tlv: raw });
                }

                None | Some(_) => return Err(Error::InvalidParameter),
            }
        }

        Ok(Self {
            initiator_random: initiator_random.ok_or(Error::InvalidParameter)?,
            responder_random: responder_random.ok_or(Error::InvalidParameter)?,
            responder_session_id: responder_session_id.ok_or(Error::InvalidParameter)?,
            pbkdf_parameters,
            responder_session_params,
        })
    }
}

// ---------------------------------------------------------------------------
// Pake1
// ---------------------------------------------------------------------------

/// The third PASE message: commissioner → device.
///
/// Carries the commissioner's SPAKE2+ share `pA` (the X point).
///
/// Wire format: anonymous TLV structure, context tag 1.
///
/// # Matter spec reference
/// Matter Core Spec §4.14.1.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pake1 {
    /// The commissioner's SPAKE2+ share `pA`. 65 bytes (uncompressed P-256).
    pub x: [u8; POINT_LEN],
}

impl Pake1 {
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(80);
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous)?;
        w.put_bytes(Tag::Context(1), &self.x)?;
        w.end_container()?;
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Pake1`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if `x` is absent, has the wrong length,
    ///   or an unexpected tag is present.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut x: Option<[u8; POINT_LEN]> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; POINT_LEN] = b.try_into().map_err(|_| Error::InvalidParameter)?;
                    x = Some(arr);
                }

                None | Some(_) => return Err(Error::InvalidParameter),
            }
        }

        Ok(Self {
            x: x.ok_or(Error::InvalidParameter)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Pake2
// ---------------------------------------------------------------------------

/// The fourth PASE message: device → commissioner.
///
/// Carries the device's SPAKE2+ share `pB` (the Y point) and its
/// confirmation tag `cB`.
///
/// Wire format: anonymous TLV structure, context tags 1–2.
///
/// # Matter spec reference
/// Matter Core Spec §4.14.1.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pake2 {
    /// The device's SPAKE2+ share `pB`. 65 bytes (uncompressed P-256).
    pub y: [u8; POINT_LEN],
    /// The device's SPAKE2+ confirmation tag `cB`. 32 bytes (SHA-256).
    pub verifier: [u8; HASH_LEN],
}

impl Pake2 {
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(112);
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous)?;
        w.put_bytes(Tag::Context(1), &self.y)?;
        w.put_bytes(Tag::Context(2), &self.verifier)?;
        w.end_container()?;
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Pake2`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if a required field is absent, has the
    ///   wrong length, or an unexpected tag is present.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut y: Option<[u8; POINT_LEN]> = None;
        let mut verifier: Option<[u8; HASH_LEN]> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; POINT_LEN] = b.try_into().map_err(|_| Error::InvalidParameter)?;
                    y = Some(arr);
                }

                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; HASH_LEN] = b.try_into().map_err(|_| Error::InvalidParameter)?;
                    verifier = Some(arr);
                }

                None | Some(_) => return Err(Error::InvalidParameter),
            }
        }

        Ok(Self {
            y: y.ok_or(Error::InvalidParameter)?,
            verifier: verifier.ok_or(Error::InvalidParameter)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Pake3
// ---------------------------------------------------------------------------

/// The fifth PASE message: commissioner → device.
///
/// Carries the commissioner's confirmation tag `cA`. The device checks this
/// to complete mutual authentication.
///
/// Wire format: anonymous TLV structure, context tag 1.
///
/// # Matter spec reference
/// Matter Core Spec §4.14.1.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pake3 {
    /// The commissioner's SPAKE2+ confirmation tag `cA`. 32 bytes (SHA-256).
    pub verifier: [u8; HASH_LEN],
}

impl Pake3 {
    /// Encode this message as wire TLV bytes.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] via [`Error::Codec`].
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(48);
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous)?;
        w.put_bytes(Tag::Context(1), &self.verifier)?;
        w.end_container()?;
        Ok(buf)
    }

    /// Decode wire TLV bytes into a `Pake3`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if the `verifier` field is absent, has
    ///   the wrong length, or an unexpected tag is present.
    /// - [`Error::Codec`] on malformed TLV.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        expect_anon_struct_start(&mut reader)?;

        let mut verifier: Option<[u8; HASH_LEN]> = None;

        loop {
            match reader.next()? {
                Some(Element::ContainerEnd) => break,

                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Bytes(b),
                }) => {
                    let arr: [u8; HASH_LEN] = b.try_into().map_err(|_| Error::InvalidParameter)?;
                    verifier = Some(arr);
                }

                None | Some(_) => return Err(Error::InvalidParameter),
            }
        }

        Ok(Self {
            verifier: verifier.ok_or(Error::InvalidParameter)?,
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
    /// Contains one field: idleInterval (tag 1) = 500.
    fn minimal_session_params(tag_num: u8) -> SessionParams {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Context(tag_num)).unwrap();
        w.put_uint(Tag::Context(1), 500).unwrap();
        w.end_container().unwrap();
        SessionParams { raw_tlv: buf }
    }

    // -----------------------------------------------------------------------
    // PbkdfParamRequest
    // -----------------------------------------------------------------------

    #[test]
    fn pbkdf_param_request_roundtrip_no_session_params() {
        let req = PbkdfParamRequest {
            initiator_random: [0x42; 32],
            initiator_session_id: 0x1234,
            passcode_id: 0,
            has_pbkdf_parameters: false,
            initiator_session_params: None,
        };
        let bytes = req.encode().unwrap();
        let decoded = PbkdfParamRequest::decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn pbkdf_param_request_has_pbkdf_parameters_true_roundtrips() {
        let req = PbkdfParamRequest {
            initiator_random: [0xAB; 32],
            initiator_session_id: 0xFFFF,
            passcode_id: 0x8000,
            has_pbkdf_parameters: true,
            initiator_session_params: None,
        };
        let bytes = req.encode().unwrap();
        let decoded = PbkdfParamRequest::decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn pbkdf_param_request_with_session_params_roundtrips() {
        let req = PbkdfParamRequest {
            initiator_random: [0x11; 32],
            initiator_session_id: 0x0001,
            passcode_id: 0,
            has_pbkdf_parameters: false,
            initiator_session_params: Some(minimal_session_params(5)),
        };
        let bytes = req.encode().unwrap();
        let decoded = PbkdfParamRequest::decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn pbkdf_param_request_rejects_missing_required_field() {
        // Encode with only tags 1, 2, 3 — missing tag 4 (has_pbkdf_parameters).
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0u8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 0).unwrap();
        // tag 4 intentionally omitted.
        w.end_container().unwrap();
        assert!(matches!(
            PbkdfParamRequest::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn pbkdf_param_request_rejects_extra_field() {
        // Tag 6 is outside the spec; decoder must reject it.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0u8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 0).unwrap();
        w.put_bool(Tag::Context(4), false).unwrap();
        w.put_uint(Tag::Context(6), 99).unwrap(); // unknown tag
        w.end_container().unwrap();
        assert!(matches!(
            PbkdfParamRequest::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn pbkdf_param_request_rejects_wrong_random_length() {
        // Tag 1 with 31 bytes instead of 32.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0u8; 31]).unwrap(); // wrong length
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 0).unwrap();
        w.put_bool(Tag::Context(4), false).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            PbkdfParamRequest::decode(&buf),
            Err(Error::InvalidParameter)
        ));
    }

    // -----------------------------------------------------------------------
    // PbkdfParamResponse
    // -----------------------------------------------------------------------

    #[test]
    fn pbkdf_param_response_with_pbkdf_params_roundtrips() {
        let resp = PbkdfParamResponse {
            initiator_random: [0x11; 32],
            responder_random: [0x22; 32],
            responder_session_id: 0xABCD,
            pbkdf_parameters: Some(PbkdfParamsInner {
                iterations: 1000,
                salt: vec![0x55; 16],
            }),
            responder_session_params: None,
        };
        let bytes = resp.encode().unwrap();
        let decoded = PbkdfParamResponse::decode(&bytes).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn pbkdf_param_response_without_pbkdf_params_roundtrips() {
        // When hasPbkdfParameters was true in the request, response omits tag 4.
        let resp = PbkdfParamResponse {
            initiator_random: [0xAA; 32],
            responder_random: [0xBB; 32],
            responder_session_id: 0x0001,
            pbkdf_parameters: None,
            responder_session_params: None,
        };
        let bytes = resp.encode().unwrap();
        let decoded = PbkdfParamResponse::decode(&bytes).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn pbkdf_param_response_with_session_params_roundtrips() {
        let resp = PbkdfParamResponse {
            initiator_random: [0x33; 32],
            responder_random: [0x44; 32],
            responder_session_id: 7,
            pbkdf_parameters: Some(PbkdfParamsInner {
                iterations: 2000,
                salt: vec![0xFF; 32],
            }),
            responder_session_params: Some(minimal_session_params(5)),
        };
        let bytes = resp.encode().unwrap();
        let decoded = PbkdfParamResponse::decode(&bytes).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn pbkdf_params_inner_rejects_short_salt_on_encode() {
        let inner = PbkdfParamsInner {
            iterations: 1000,
            salt: vec![0u8; 15], // too short (< 16)
        };
        assert!(matches!(
            inner.encode(Tag::Context(4)),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn pbkdf_params_inner_rejects_long_salt_on_encode() {
        let inner = PbkdfParamsInner {
            iterations: 1000,
            salt: vec![0u8; 33], // too long (> 32)
        };
        assert!(matches!(
            inner.encode(Tag::Context(4)),
            Err(Error::InvalidParameter)
        ));
    }

    // -----------------------------------------------------------------------
    // Pake1
    // -----------------------------------------------------------------------

    #[test]
    fn pake1_roundtrip() {
        let p = Pake1 { x: [0x04; 65] };
        let bytes = p.encode().unwrap();
        let decoded = Pake1::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn pake1_rejects_wrong_x_length() {
        // 64 bytes instead of 65.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x04u8; 64]).unwrap();
        w.end_container().unwrap();
        assert!(matches!(Pake1::decode(&buf), Err(Error::InvalidParameter)));
    }

    #[test]
    fn pake1_rejects_extra_field() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x04u8; 65]).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap(); // extra unknown tag
        w.end_container().unwrap();
        assert!(matches!(Pake1::decode(&buf), Err(Error::InvalidParameter)));
    }

    // -----------------------------------------------------------------------
    // Pake2
    // -----------------------------------------------------------------------

    #[test]
    fn pake2_roundtrip() {
        let p = Pake2 {
            y: [0x04; 65],
            verifier: [0xCC; 32],
        };
        let bytes = p.encode().unwrap();
        let decoded = Pake2::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn pake2_rejects_wrong_verifier_length() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0x04u8; 65]).unwrap();
        w.put_bytes(Tag::Context(2), &[0xCCu8; 31]).unwrap(); // 31 instead of 32
        w.end_container().unwrap();
        assert!(matches!(Pake2::decode(&buf), Err(Error::InvalidParameter)));
    }

    // -----------------------------------------------------------------------
    // Pake3
    // -----------------------------------------------------------------------

    #[test]
    fn pake3_roundtrip() {
        let p = Pake3 {
            verifier: [0xDE; 32],
        };
        let bytes = p.encode().unwrap();
        let decoded = Pake3::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn pake3_rejects_missing_verifier() {
        // Empty structure — missing tag 1.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.end_container().unwrap();
        assert!(matches!(Pake3::decode(&buf), Err(Error::InvalidParameter)));
    }

    #[test]
    fn pake3_rejects_extra_field() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Context(1), &[0xDEu8; 32]).unwrap();
        w.put_uint(Tag::Context(2), 42).unwrap(); // extra tag
        w.end_container().unwrap();
        assert!(matches!(Pake3::decode(&buf), Err(Error::InvalidParameter)));
    }
}

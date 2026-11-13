//! `WriteRequestMessage` / `WriteResponseMessage` framing — Matter §10.6.

#![forbid(unsafe_code)]

use crate::error::ImError;
use crate::path::AttributePath;
#[cfg(test)]
use crate::read_container_members;
use crate::status::ImStatus;
use crate::{expect_message_struct, skip_container, IM_REVISION};
use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

/// One attribute write: a concrete path plus the pre-encoded data value.
///
/// `Debug` is written by hand so that `value_tlv` prints as its length only
/// (`"<N bytes>"`): the value being written can be a secret (a group key, a
/// credential), so it must never reach a log through a stray `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct AttributeWriteRequest {
    /// Concrete attribute path to write.
    pub path: AttributePath,
    /// The attribute value as a standalone anonymous-tagged TLV element
    /// (e.g. the output of a `matter-clusters` attribute encoder).
    ///
    /// Plaintext: this struct's `Debug` output shows only its length.
    pub value_tlv: Vec<u8>,
}

impl core::fmt::Debug for AttributeWriteRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Same name, fields and order as the derive this replaces; only the
        // value's bytes are withheld.
        f.debug_struct("AttributeWriteRequest")
            .field("path", &self.path)
            .field("value_tlv", &format!("<{} bytes>", self.value_tlv.len()))
            .finish()
    }
}

/// Build a `WriteRequestMessage` for one or more concrete attribute writes.
///
/// `SuppressResponse` and `TimedRequest` are both `false`; `DataVersion`
/// and `MoreChunkedMessages` are omitted (no chunking — single-MTU writes
/// only, per the M7 scope).
///
/// # Panics
///
/// Panics if a `value_tlv` is not a valid anonymous-tagged TLV element
/// (i.e. not the output of a codec encode call). The function is
/// otherwise infallible; `Vec`-backed `TlvWriter` never fails.
#[must_use]
pub fn build_write_request(writes: &[AttributeWriteRequest]) -> Vec<u8> {
    build_write_request_inner(writes, false)
}

/// Like [`build_write_request`] but sets `TimedRequest = true` — the action half
/// of a timed interaction, sent on the same exchange after a `TimedRequest`
/// message (see [`crate::build_timed_request`]).
#[must_use]
pub fn build_write_request_timed(writes: &[AttributeWriteRequest]) -> Vec<u8> {
    build_write_request_inner(writes, true)
}

#[allow(clippy::expect_used)] // Vec-backed TlvWriter is infallible.
fn build_write_request_inner(writes: &[AttributeWriteRequest], timed: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(
        48 + writes
            .iter()
            .map(|wr| 24 + wr.value_tlv.len())
            .sum::<usize>(),
    );
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bool(Tag::Context(0), false)
        .expect("infallible: vec writer"); // SuppressResponse
    w.put_bool(Tag::Context(1), timed)
        .expect("infallible: vec writer"); // TimedRequest
    w.start_array(Tag::Context(2))
        .expect("infallible: vec writer"); // WriteRequests
    for wr in writes {
        w.start_structure(Tag::Anonymous)
            .expect("infallible: vec writer"); // AttributeDataIB
        w.start_list(Tag::Context(1))
            .expect("infallible: vec writer"); // Path (AttributePathIB)
        w.put_uint(Tag::Context(2), u64::from(wr.path.endpoint))
            .expect("infallible: vec writer");
        w.put_uint(Tag::Context(3), u64::from(wr.path.cluster))
            .expect("infallible: vec writer");
        w.put_uint(Tag::Context(4), u64::from(wr.path.attribute))
            .expect("infallible: vec writer");
        w.end_container().expect("infallible: vec writer"); // Path
        w.put_preencoded(Tag::Context(2), &wr.value_tlv)
            .expect("infallible: caller passes a valid anonymous-tagged element"); // Data
        w.end_container().expect("infallible: vec writer"); // AttributeDataIB
    }
    w.end_container().expect("infallible: vec writer"); // WriteRequests array
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer"); // message struct
    buf
}

/// Parse a `WriteResponseMessage` into per-path statuses.
///
/// The write response carries one `AttributeStatusIB` per written path —
/// **including the success case** — so the result is a status per path,
/// not a single message-level status. A message with no `WriteResponses`
/// member yields an empty result.
///
/// # Errors
///
/// Returns [`ImError`] if the message is not a struct, an
/// `AttributeStatusIB` is missing its path or status, or a path value is
/// out of range.
///
/// The message must be complete: end of input before the top-level
/// end-of-container fails with [`ImError::Codec`] wrapping
/// [`matter_codec::Error::UnclosedContainer`], and bytes after it fail with
/// [`matter_codec::Error::TrailingData`] (chip's
/// `MessageParser::ExitContainer` rejects both).
pub fn parse_write_response(bytes: &[u8]) -> Result<Vec<(AttributePath, ImStatus)>, ImError> {
    let mut r = TlvReader::new(bytes);
    expect_message_struct(&mut r)?;

    let mut out = Vec::new();

    // Find WriteResponses [0] (array). Absent → empty result.
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => {
                r.finish()?;
                return Ok(out);
            }
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::Array,
            }) => break,
            Some(Element::ContainerStart { .. }) => skip_container(&mut r)?,
            Some(_) => {}
        }
    }

    // Iterate AttributeStatusIB structs in the array.
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break, // end of array
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            }) => out.push(parse_attribute_status_ib(&mut r)?),
            Some(Element::ContainerStart { .. }) => skip_container(&mut r)?,
            Some(_) => {}
        }
    }
    // The members after the array and the message's own end-of-container;
    // nothing may follow it.
    r.finish()?;

    Ok(out)
}

/// Parse a guarded write acknowledgement with exactly one status per expected path.
///
/// Unlike [`parse_write_response`], rejects duplicate envelope, path and status
/// members, missing or unknown paths, and repeated acknowledgements. Valid
/// failure statuses are returned unchanged, alongside successes.
///
/// # Errors
/// Returns [`ImError`] for malformed or ambiguous replies or incomplete coverage.
pub fn parse_write_response_checked(
    bytes: &[u8],
    expected: &[AttributePath],
) -> Result<Vec<(AttributePath, ImStatus)>, ImError> {
    let mut reader = TlvReader::new(bytes);
    let (tag, message) = reader.read_value()?;
    reader.finish()?;
    if tag != Tag::Anonymous {
        return Err(ImError::UnexpectedValue("WriteResponse must be anonymous"));
    }
    let message = checked_write_struct(&message)?;
    let Some(Value::Array(entries)) = checked_write_member(message, 0) else {
        return Err(ImError::MissingField("WriteResponses"));
    };
    let mut remaining: std::collections::HashSet<_> = expected.iter().copied().collect();
    if remaining.len() != expected.len() || entries.len() != expected.len() {
        return Err(ImError::UnexpectedValue(
            "write acknowledgement coverage mismatch",
        ));
    }
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = checked_write_struct(entry)?;
        let Some(Value::List(path)) = checked_write_member(entry, 0) else {
            return Err(ImError::MissingField("AttributeStatusIB.Path"));
        };
        checked_write_unique(path)?;
        // This API writes whole, concrete attributes, never a wildcard,
        // alternate node or list index. Do not silently discard path qualifiers.
        if path.len() != 3 {
            return Err(ImError::UnexpectedValue("expected concrete attribute path"));
        }
        let number = |tag| match checked_write_member(path, tag) {
            Some(Value::Uint(n)) => Ok(*n),
            _ => Err(ImError::MissingField("AttributePath member")),
        };
        let path = AttributePath {
            endpoint: u16::try_from(number(2)?)
                .map_err(|_| ImError::UnexpectedValue("endpoint exceeds u16"))?,
            cluster: u32::try_from(number(3)?)
                .map_err(|_| ImError::UnexpectedValue("cluster exceeds u32"))?,
            attribute: u32::try_from(number(4)?)
                .map_err(|_| ImError::UnexpectedValue("attribute exceeds u32"))?,
        };
        if !remaining.remove(&path) {
            return Err(ImError::UnexpectedValue(
                "unknown or duplicate write acknowledgement path",
            ));
        }
        let status = checked_write_member(entry, 1)
            .ok_or(ImError::MissingField("AttributeStatusIB.Status"))?;
        let status = checked_write_struct(status)?;
        let Some(Value::Uint(code)) = checked_write_member(status, 0) else {
            return Err(ImError::MissingField("StatusIB.Status"));
        };
        let code = u8::try_from(*code).map_err(|_| ImError::InvalidStatusCode { code: *code })?;
        out.push((path, ImStatus::from_u8(code)));
    }
    if !remaining.is_empty() {
        return Err(ImError::UnexpectedValue(
            "missing write acknowledgement path",
        ));
    }
    Ok(out)
}

fn checked_write_struct(value: &Value) -> Result<&[(Tag, Value)], ImError> {
    let Value::Structure(members) = value else {
        return Err(ImError::UnexpectedValue("expected write structure"));
    };
    checked_write_unique(members)?;
    Ok(members)
}

fn checked_write_unique(members: &[(Tag, Value)]) -> Result<(), ImError> {
    let mut tags = std::collections::HashSet::with_capacity(members.len());
    for (tag, _) in members {
        if !tags.insert(*tag) {
            return Err(ImError::UnexpectedValue("duplicate write member"));
        }
    }
    Ok(())
}

fn checked_write_member(members: &[(Tag, Value)], id: u8) -> Option<&Value> {
    members
        .iter()
        .find_map(|(tag, value)| (*tag == Tag::Context(id)).then_some(value))
}

/// Parse one `AttributeStatusIB` body (reader positioned just after the
/// struct start): `{ 0: Path(list), 1: StatusIB struct { 0: Status } }`.
///
/// Shared with the read path: a `ReportData`'s `AttributeStatus [0]` IB
/// has the identical body, so both the write response and the report path
/// decode per-path status through here.
pub(crate) fn parse_attribute_status_ib(
    r: &mut TlvReader<'_>,
) -> Result<(AttributePath, ImStatus), ImError> {
    let mut path = None;
    let mut status = None;
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::List,
            }) => {
                let (p, _) = crate::path::attribute_path_from_reader(r)?;
                path = Some(p);
            }
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::Structure,
            }) => {
                if let Some(s) = parse_status_ib_body(r)? {
                    status = Some(s);
                }
            }
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
    Ok((
        path.ok_or(ImError::MissingField("AttributeStatusIB.Path"))?,
        status.ok_or(ImError::MissingField("AttributeStatusIB.Status"))?,
    ))
}

/// Consume a `StatusIB` struct body (reader just after its start),
/// returning the last `Status` (context tag 0) seen, mapped to
/// [`ImStatus`]. Out-of-range codes error as `InvalidStatusCode`.
fn parse_status_ib_body(r: &mut TlvReader<'_>) -> Result<Option<ImStatus>, ImError> {
    let mut status = None;
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => return Ok(status),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(n),
            }) => {
                let code = u8::try_from(n).map_err(|_| ImError::InvalidStatusCode { code: n })?;
                status = Some(ImStatus::from_u8(code));
            }
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
}

/// Reserve for the `MoreChunkedMessages`(ctx3) bool we may add after packing.
/// Covers the 2-byte explicit flag element (control byte + tag byte for a
/// boolean) whether it ends up encoding `Some(true)` or `Some(false)` — both
/// cost the same 2 bytes.
const CHUNK_FLAG_RESERVE: usize = 4;

/// Build one or more `WriteRequestMessage`s that write `element_tlvs` (each a
/// pre-encoded anonymous-tagged list element) to `path` as a list, splitting
/// across messages so each stays within `budget` unencrypted bytes.
///
/// Chunk 0 is a `ReplaceAll` (path without `ListIndex`; `Data` = an array of
/// the elements that fit). Remaining elements are emitted as `AppendItem` IBs
/// (path with `ListIndex`=null).
///
/// When the write fits a single message, the result is one `ReplaceAll` with
/// `MoreChunkedMessages` (ctx3) omitted entirely — byte-identical to
/// `build_write_request(&[AttributeWriteRequest{path, value_tlv: <the full
/// array encoded>}])`.
///
/// When the write spans multiple messages, `MoreChunkedMessages` is encoded
/// **explicitly on every chunk**: `true` on all but the last, and an explicit
/// `false` on the last. This is required for chip interop: chip's
/// `WriteHandler::ProcessWriteRequest` (connectedhomeip
/// `src/app/WriteHandler.cpp:649-656`) initializes its parsed
/// `MoreChunkedMessages` from the *previous* chunk's stored state before
/// attempting to read the field, so an absent field on the final chunk
/// silently inherits `true` from chunk N-1 and the device never considers the
/// write transaction finished. Only the single-message (no-chunking) shape
/// omits the field, matching `build_write_request`.
///
/// An empty `element_tlvs` yields a single empty-array `ReplaceAll`.
#[must_use]
pub fn build_list_write_chunks(
    path: AttributePath,
    element_tlvs: &[Vec<u8>],
    budget: usize,
    timed: bool,
) -> Vec<Vec<u8>> {
    // Probe element: a 1-byte anonymous null (0x14). cost = base + wrapper
    // + probe.len() + 1, so wrapper+1 falls out by subtraction.
    const PROBE: &[u8] = &[0x14];

    // Incremental size accounting: containers are delimited by end markers
    // (never length prefixes), so a chunk with elements e_1..e_k encodes to
    // exactly base + Σ cost(e_i). ReplaceAll cost(e) = e.len() (anonymous
    // re-tag rewrites the control byte, same size); AppendItem cost(e) =
    // wrapper + e.len() + 1 (fixed per-path AttributeDataIB wrapper, and the
    // context re-tag adds one byte). Probe both constants from real encodes
    // rather than hand-deriving byte counts; the equivalence proptest and
    // the single-chunk byte-identity test pin correctness.
    let replace_base = encoded_replace_all_len(path, &[], timed);
    let append_base = encoded_append_len(path, &[], timed);
    let append_per_elem_overhead =
        encoded_append_len(path, &[PROBE], timed) - append_base - PROBE.len();

    // 1) Greedily fill chunk 0's ReplaceAll array.
    let mut idx = 0usize;
    let mut first_batch: Vec<&[u8]> = Vec::new();
    let mut size = replace_base;
    while idx < element_tlvs.len() {
        let cost = element_tlvs[idx].len();
        if size + cost + CHUNK_FLAG_RESERVE > budget && !first_batch.is_empty() {
            break;
        }
        size += cost;
        first_batch.push(element_tlvs[idx].as_slice());
        idx += 1;
    }

    // Collect remaining elements as AppendItem batches.
    let mut append_batches: Vec<Vec<&[u8]>> = Vec::new();
    while idx < element_tlvs.len() {
        let mut batch: Vec<&[u8]> = Vec::new();
        let mut size = append_base;
        while idx < element_tlvs.len() {
            let cost = append_per_elem_overhead + element_tlvs[idx].len();
            if size + cost + CHUNK_FLAG_RESERVE > budget && !batch.is_empty() {
                break;
            }
            size += cost;
            batch.push(element_tlvs[idx].as_slice());
            idx += 1;
        }
        append_batches.push(batch);
    }

    // 2) Encode each chunk. Single-message output omits MoreChunkedMessages
    // entirely (None); multi-chunk output sets it explicitly on every chunk —
    // Some(true) on all but the last, Some(false) on the last (chip parity:
    // see the rustdoc above for why the final chunk cannot merely omit it).
    // Note: when chunked is true, append_batches is always non-empty (total =
    // 1 + append_batches.len() > 1), so the first (ReplaceAll) chunk is never
    // the last chunk — it always carries Some(true).
    let total = 1 + append_batches.len();
    let mut messages: Vec<Vec<u8>> = Vec::with_capacity(total);
    let chunked = total > 1;
    let first_more = if chunked { Some(true) } else { None };
    messages.push(encode_replace_all(path, &first_batch, timed, first_more));
    for (i, batch) in append_batches.iter().enumerate() {
        let more = Some(i + 1 < append_batches.len());
        messages.push(encode_append_items(path, batch, timed, more));
    }
    messages
}

/// Encode a `ReplaceAll` `WriteRequestMessage` containing one `AttributeDataIB`
/// whose `Data` is an anonymous array of `elems`.
///
/// `more_chunked`: `None` omits `MoreChunkedMessages` (ctx3) entirely —
/// the single-chunk shape. `Some(v)` encodes it explicitly, `true` or
/// `false` alike — required on every chunk of a multi-chunk sequence (see
/// [`build_list_write_chunks`] rustdoc for why the final chunk cannot omit
/// an explicit `false`).
#[allow(clippy::expect_used)] // Vec-backed TlvWriter is infallible.
fn encode_replace_all(
    path: AttributePath,
    elems: &[&[u8]],
    timed: bool,
    more_chunked: Option<bool>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bool(Tag::Context(0), false)
        .expect("infallible: vec writer"); // SuppressResponse
    w.put_bool(Tag::Context(1), timed)
        .expect("infallible: vec writer"); // TimedRequest
    w.start_array(Tag::Context(2))
        .expect("infallible: vec writer"); // WriteRequests

    // One AttributeDataIB — ReplaceAll (no ListIndex in path).
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.start_list(Tag::Context(1))
        .expect("infallible: vec writer"); // Path (AttributePathIB)
    w.put_uint(Tag::Context(2), u64::from(path.endpoint))
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(3), u64::from(path.cluster))
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(4), u64::from(path.attribute))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer"); // Path
                                                        // Data = anonymous array containing the pre-encoded elements.
    w.start_array(Tag::Context(2))
        .expect("infallible: vec writer");
    for e in elems {
        w.put_preencoded(Tag::Anonymous, e)
            .expect("infallible: caller passes valid anonymous-tagged elements");
    }
    w.end_container().expect("infallible: vec writer"); // Data array
    w.end_container().expect("infallible: vec writer"); // AttributeDataIB

    w.end_container().expect("infallible: vec writer"); // WriteRequests array
    if let Some(v) = more_chunked {
        w.put_bool(Tag::Context(3), v)
            .expect("infallible: vec writer"); // MoreChunkedMessages
    }
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer"); // message struct
    buf
}

/// Encode an `AppendItem` `WriteRequestMessage` containing one
/// `AttributeDataIB` per element — each IB has `ListIndex`=null in its path.
///
/// `more_chunked`: see [`encode_replace_all`] — `None` omits
/// `MoreChunkedMessages`, `Some(v)` encodes it explicitly.
#[allow(clippy::expect_used)] // Vec-backed TlvWriter is infallible.
fn encode_append_items(
    path: AttributePath,
    elems: &[&[u8]],
    timed: bool,
    more_chunked: Option<bool>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bool(Tag::Context(0), false)
        .expect("infallible: vec writer"); // SuppressResponse
    w.put_bool(Tag::Context(1), timed)
        .expect("infallible: vec writer"); // TimedRequest
    w.start_array(Tag::Context(2))
        .expect("infallible: vec writer"); // WriteRequests

    for e in elems {
        w.start_structure(Tag::Anonymous)
            .expect("infallible: vec writer"); // AttributeDataIB
        w.start_list(Tag::Context(1))
            .expect("infallible: vec writer"); // Path (AttributePathIB)
        w.put_uint(Tag::Context(2), u64::from(path.endpoint))
            .expect("infallible: vec writer");
        w.put_uint(Tag::Context(3), u64::from(path.cluster))
            .expect("infallible: vec writer");
        w.put_uint(Tag::Context(4), u64::from(path.attribute))
            .expect("infallible: vec writer");
        w.put_null(Tag::Context(5)).expect("infallible: vec writer"); // ListIndex=null → AppendItem
        w.end_container().expect("infallible: vec writer"); // Path
        w.put_preencoded(Tag::Context(2), e)
            .expect("infallible: caller passes valid anonymous-tagged elements"); // Data
        w.end_container().expect("infallible: vec writer"); // AttributeDataIB
    }

    w.end_container().expect("infallible: vec writer"); // WriteRequests array
    if let Some(v) = more_chunked {
        w.put_bool(Tag::Context(3), v)
            .expect("infallible: vec writer"); // MoreChunkedMessages
    }
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer"); // message struct
    buf
}

/// Size-accounting base: always uses the flag-omitted (`None`) shape. The
/// caller reserves [`CHUNK_FLAG_RESERVE`] bytes on top to cover the explicit
/// flag that may be added afterward.
fn encoded_replace_all_len(path: AttributePath, elems: &[&[u8]], timed: bool) -> usize {
    encode_replace_all(path, elems, timed, None).len()
}

/// Size-accounting base: always uses the flag-omitted (`None`) shape. See
/// [`encoded_replace_all_len`].
fn encoded_append_len(path: AttributePath, elems: &[&[u8]], timed: bool) -> usize {
    encode_append_items(path, elems, timed, None).len()
}

/// Parse the element TLVs out of a sequence of `WriteRequestMessage`s produced
/// by [`build_list_write_chunks`], returning them in order.
///
/// For a `ReplaceAll` IB (no `ListIndex` in path) the `Data` is an array;
/// each anonymous array element is re-encoded and pushed. For `AppendItem`
/// IBs (`ListIndex`=null) the `Data` element (ctx2-tagged) is re-encoded as
/// anonymous and pushed.
///
/// This function is provided for test validation only.
#[cfg(test)]
pub(crate) fn reassemble_list_write(chunks: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for chunk in chunks {
        collect_elements_from_chunk(chunk, &mut out);
    }
    out
}

/// Extract element TLVs from one `WriteRequestMessage` chunk into `out`.
///
/// Uses `read_value` to decode each `AttributeDataIB` as a typed `Value`,
/// then walks the structure to extract elements without needing raw-byte seeks.
#[cfg(test)]
#[allow(clippy::expect_used)]
fn collect_elements_from_chunk(chunk: &[u8], out: &mut Vec<Vec<u8>>) {
    let mut r = TlvReader::new(chunk);
    // Enter anonymous message struct.
    let Ok(Some(Element::ContainerStart {
        tag: Tag::Anonymous,
        kind: ContainerKind::Structure,
    })) = r.next()
    else {
        return;
    };

    // Find WriteRequests (ctx2 array).
    loop {
        match r.next() {
            Ok(Some(Element::ContainerStart {
                tag: Tag::Context(2),
                kind: ContainerKind::Array,
            })) => break,
            Ok(Some(Element::ContainerStart { .. })) => {
                let _ = skip_container(&mut r);
            }
            Ok(Some(Element::ContainerEnd) | None) | Err(_) => return,
            Ok(Some(_)) => {}
        }
    }

    // Iterate AttributeDataIBs — read each as a full Value so we can inspect
    // the path and data without a forward-only byte-position API.
    loop {
        match r.next() {
            Ok(Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            })) => {
                if let Ok(members) = read_container_members(&mut r) {
                    collect_elements_from_ib_members(&members, out);
                }
            }
            Ok(Some(Element::ContainerEnd) | None) => break,
            Ok(Some(Element::ContainerStart { .. })) => {
                let _ = skip_container(&mut r);
            }
            Ok(Some(_)) | Err(_) => {}
        }
    }
}

/// Walk the decoded `AttributeDataIB` members `[(Tag, Value)]` and push
/// re-encoded anonymous element TLVs into `out`.
///
/// An `AttributeDataIB` has:
/// - `ctx1` → Path (list): may include `ctx5 Null` (`ListIndex`=null) for `AppendItem`
/// - `ctx2` → Data
#[cfg(test)]
#[allow(clippy::expect_used)]
fn collect_elements_from_ib_members(members: &[(Tag, Value)], out: &mut Vec<Vec<u8>>) {
    // Determine whether this is ReplaceAll or AppendItem by inspecting path (ctx1).
    let mut is_append = false;
    let mut data_value: Option<&Value> = None;

    for (tag, value) in members {
        match tag {
            Tag::Context(1) => {
                // Path is a list: check for ListIndex=null (ctx5).
                if let Value::List(path_members) = value {
                    for (pt, pv) in path_members {
                        if *pt == Tag::Context(5) && *pv == Value::Null {
                            is_append = true;
                        }
                    }
                }
            }
            Tag::Context(2) => {
                data_value = Some(value);
            }
            _ => {}
        }
    }

    let Some(data) = data_value else { return };

    if is_append {
        // AppendItem: Data is the element itself (re-tagged ctx2 by put_preencoded).
        // Re-encode it as anonymous-tagged.
        let mut elem_bytes = Vec::new();
        let mut w = TlvWriter::new(&mut elem_bytes);
        w.write_value(Tag::Anonymous, data)
            .expect("infallible: vec writer");
        out.push(elem_bytes);
    } else {
        // ReplaceAll: Data is an Array; each element is a list element.
        if let Value::Array(elems) = data {
            for elem in elems {
                let mut elem_bytes = Vec::new();
                let mut w = TlvWriter::new(&mut elem_bytes);
                w.write_value(Tag::Anonymous, elem)
                    .expect("infallible: vec writer");
                out.push(elem_bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

    /// Encode a string as a standalone anonymous TLV element (stand-in for
    /// a matter-clusters attribute encoder).
    fn anon_string(s: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, s).unwrap();
        buf
    }

    fn checked_write_entry(path: AttributePath, code: u64) -> Value {
        Value::Structure(vec![
            (
                Tag::Context(0),
                Value::List(vec![
                    (Tag::Context(2), Value::Uint(u64::from(path.endpoint))),
                    (Tag::Context(3), Value::Uint(u64::from(path.cluster))),
                    (Tag::Context(4), Value::Uint(u64::from(path.attribute))),
                ]),
            ),
            (
                Tag::Context(1),
                Value::Structure(vec![(Tag::Context(0), Value::Uint(code))]),
            ),
        ])
    }

    fn checked_write_bytes(entries: Vec<Value>, extra_array: bool) -> Vec<u8> {
        let mut message = vec![(Tag::Context(0), Value::Array(entries))];
        if extra_array {
            message.push(message[0].clone());
        }
        let mut bytes = vec![];
        TlvWriter::new(&mut bytes)
            .write_value(Tag::Anonymous, &Value::Structure(message))
            .unwrap();
        bytes
    }

    #[test]
    fn checked_write_requires_exact_complete_path_set_and_preserves_failure() {
        let path = AttributePath {
            endpoint: 1,
            cluster: 8,
            attribute: 0,
        };
        let other = AttributePath {
            attribute: 1,
            ..path
        };
        for (code, status) in [
            (0, ImStatus::Success),
            (1, ImStatus::Failure(1)),
            (0x88, ImStatus::Failure(0x88)),
        ] {
            let bytes = checked_write_bytes(vec![checked_write_entry(path, code)], false);
            assert_eq!(
                parse_write_response_checked(&bytes, &[path]).unwrap(),
                vec![(path, status)]
            );
        }
        let bytes = checked_write_bytes(
            vec![checked_write_entry(other, 1), checked_write_entry(path, 0)],
            false,
        );
        assert_eq!(
            parse_write_response_checked(&bytes, &[path, other]).unwrap(),
            vec![(other, ImStatus::Failure(1)), (path, ImStatus::Success)]
        );
        for wrong in [
            AttributePath {
                endpoint: 2,
                ..path
            },
            AttributePath { cluster: 6, ..path },
            other,
        ] {
            assert!(parse_write_response_checked(
                &checked_write_bytes(vec![checked_write_entry(wrong, 0)], false),
                &[path]
            )
            .is_err());
        }
        for entries in [
            vec![],
            vec![checked_write_entry(path, 0), checked_write_entry(path, 0)],
        ] {
            assert!(parse_write_response_checked(
                &checked_write_bytes(entries, false),
                &[path, other]
            )
            .is_err());
        }
        assert!(parse_write_response_checked(
            &checked_write_bytes(vec![checked_write_entry(path, 0)], false),
            &[path, other]
        )
        .is_err());
        assert!(parse_write_response_checked(
            &checked_write_bytes(vec![checked_write_entry(path, 0)], false),
            &[path, path]
        )
        .is_err());
        let bytes = checked_write_bytes(vec![checked_write_entry(path, 0)], false);
        for end in 0..bytes.len() {
            assert!(parse_write_response_checked(&bytes[..end], &[path]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(parse_write_response_checked(&trailing, &[path]).is_err());
    }

    #[test]
    fn checked_write_rejects_duplicate_failure_then_success_and_ambiguous_members() {
        let path = AttributePath {
            endpoint: 1,
            cluster: 8,
            attribute: 0,
        };
        let good = checked_write_entry(path, 0);
        let Value::Structure(members) = &good else {
            unreachable!()
        };
        let mut forged = members.clone();
        forged[1].1 = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(1)),
            (Tag::Context(0), Value::Uint(0)),
        ]);
        let bytes = checked_write_bytes(vec![Value::Structure(forged)], false);
        assert_eq!(
            parse_write_response(&bytes).unwrap(),
            vec![(path, ImStatus::Success)]
        ); // legacy unchanged
        assert!(parse_write_response_checked(&bytes, &[path]).is_err());
        assert!(parse_write_response_checked(
            &checked_write_bytes(vec![good.clone()], true),
            &[path]
        )
        .is_err());
        for member in members {
            let mut changed = members.clone();
            changed.insert(0, member.clone());
            assert!(parse_write_response_checked(
                &checked_write_bytes(vec![Value::Structure(changed)], false),
                &[path]
            )
            .is_err());
        }
        // Separate StatusIBs carrying failure then success are also ambiguous.
        let mut changed = members.clone();
        changed.insert(
            1,
            (
                Tag::Context(1),
                Value::Structure(vec![(Tag::Context(0), Value::Uint(1))]),
            ),
        );
        assert!(parse_write_response_checked(
            &checked_write_bytes(vec![Value::Structure(changed)], false),
            &[path]
        )
        .is_err());
        for tag in 2..=4 {
            for duplicate in [false, true] {
                let mut changed = members.clone();
                let Value::List(fields) = &mut changed[0].1 else {
                    unreachable!()
                };
                if duplicate {
                    fields.insert(0, (Tag::Context(tag), Value::Uint(99)));
                } else {
                    fields.retain(|(member, _)| *member != Tag::Context(tag));
                }
                assert!(parse_write_response_checked(
                    &checked_write_bytes(vec![Value::Structure(changed)], false),
                    &[path]
                )
                .is_err());
            }
        }
        for missing in 0..2 {
            let mut changed = members.clone();
            changed.remove(missing);
            assert!(parse_write_response_checked(
                &checked_write_bytes(vec![Value::Structure(changed)], false),
                &[path]
            )
            .is_err());
        }
    }

    #[test]
    fn write_request_has_expected_structure() {
        let bytes = build_write_request(&[AttributeWriteRequest {
            path: AttributePath {
                endpoint: 0,
                cluster: 0x28,
                attribute: 0x05, // NodeLabel
            },
            value_tlv: anon_string("matter-rust"),
        }]);
        let mut r = TlvReader::new(&bytes);
        // message struct
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure
            })
        ));
        // SuppressResponse [0] = false
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bool(false)
            })
        ));
        // TimedRequest [1] = false
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Bool(false)
            })
        ));
        // WriteRequests [2] array
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Context(2),
                kind: ContainerKind::Array
            })
        ));
        // AttributeDataIB struct
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure
            })
        ));
        // Path [1] list with 2/3/4
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::List
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(0)
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Uint(0x28)
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Uint(0x05)
            })
        ));
    }

    /// Build a `WriteResponseMessage` by hand and parse it back.
    fn echo_write_response(entries: &[(AttributePath, u8)]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(0)).unwrap(); // WriteResponses
        for (p, code) in entries {
            w.start_structure(Tag::Anonymous).unwrap(); // AttributeStatusIB
            w.start_list(Tag::Context(0)).unwrap(); // Path
            w.put_uint(Tag::Context(2), u64::from(p.endpoint)).unwrap();
            w.put_uint(Tag::Context(3), u64::from(p.cluster)).unwrap();
            w.put_uint(Tag::Context(4), u64::from(p.attribute)).unwrap();
            w.end_container().unwrap();
            w.start_structure(Tag::Context(1)).unwrap(); // StatusIB
            w.put_uint(Tag::Context(0), u64::from(*code)).unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap(); // AttributeStatusIB
        }
        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();
        buf
    }

    #[test]
    fn parses_success_and_failure_statuses() {
        let p1 = AttributePath {
            endpoint: 0,
            cluster: 0x28,
            attribute: 0x05,
        };
        let p2 = AttributePath {
            endpoint: 0,
            cluster: 0x28,
            attribute: 0x06,
        };
        let msg = echo_write_response(&[(p1, 0x00), (p2, 0x01)]);
        let statuses = parse_write_response(&msg).unwrap();
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0], (p1, ImStatus::Success));
        assert_eq!(statuses[1], (p2, ImStatus::Failure(0x01)));
    }

    #[test]
    fn missing_status_is_an_error() {
        // AttributeStatusIB with a path but no StatusIB.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(0)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_list(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 0x28).unwrap();
        w.put_uint(Tag::Context(4), 0x05).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let result = parse_write_response(&buf);
        assert!(
            matches!(
                result,
                Err(ImError::MissingField("AttributeStatusIB.Status"))
            ),
            "expected MissingField, got {result:?}"
        );
    }

    #[test]
    fn empty_message_yields_empty_statuses() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();
        let statuses = parse_write_response(&buf).unwrap();
        assert_eq!(
            statuses,
            Vec::<(crate::path::AttributePath, crate::status::ImStatus)>::new()
        );
    }

    /// Drive the private `parse_status_ib_body` over a writer-built `StatusIB`.
    fn parse_status(build: impl FnOnce(&mut TlvWriter<'_>)) -> Result<Option<ImStatus>, ImError> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        build(&mut w);
        w.end_container().unwrap();
        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        parse_status_ib_body(&mut r)
    }

    #[test]
    fn status_ib_body_parses_status_none_and_range_error() {
        assert!(matches!(
            parse_status(|w| w.put_uint(Tag::Context(0), 0).unwrap()),
            Ok(Some(ImStatus::Success))
        ));
        // No status member at all -> Ok(None).
        assert!(matches!(parse_status(|_| {}), Ok(None)));
        // Out-of-range code errors.
        assert!(matches!(
            parse_status(|w| w.put_uint(Tag::Context(0), 0x1_00).unwrap()),
            Err(ImError::InvalidStatusCode { code: 0x100 })
        ));
        // Unknown nested container is skipped, status still found after it.
        assert!(matches!(
            parse_status(|w| {
                w.start_structure(Tag::Context(7)).unwrap();
                w.put_uint(Tag::Context(0), 9).unwrap();
                w.end_container().unwrap();
                w.put_uint(Tag::Context(0), 0).unwrap();
            }),
            Ok(Some(ImStatus::Success))
        ));
    }
}

#[cfg(test)]
mod chunk_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use matter_codec::{Tag, TlvWriter, Value};
    use proptest::prelude::*;

    fn entry_tlv(n: u64) -> Vec<u8> {
        // a small anonymous-tagged struct standing in for an ACL entry
        let mut b = Vec::new();
        let mut w = TlvWriter::new(&mut b);
        w.write_value(
            Tag::Anonymous,
            &Value::Structure(vec![(Tag::Context(1), Value::Uint(n))]),
        )
        .unwrap();
        b
    }

    fn p() -> AttributePath {
        AttributePath {
            endpoint: 0,
            cluster: 0x001F,
            attribute: 0x0000,
        }
    }

    /// Pre-phase-3 packer: re-encodes the whole candidate chunk on every
    /// element to decide whether it still fits `budget`, so it is O(n²) in
    /// elements per chunk. Retained verbatim as a proptest oracle to pin
    /// the incremental packer's output as byte-identical.
    fn build_list_write_chunks_reference(
        path: AttributePath,
        element_tlvs: &[Vec<u8>],
        budget: usize,
        timed: bool,
    ) -> Vec<Vec<u8>> {
        // 1) Greedily fill chunk 0's ReplaceAll array.
        let mut idx = 0usize;
        let mut first_batch: Vec<&[u8]> = Vec::new();
        while idx < element_tlvs.len() {
            let candidate: Vec<&[u8]> = first_batch
                .iter()
                .copied()
                .chain(std::iter::once(element_tlvs[idx].as_slice()))
                .collect();
            if encoded_replace_all_len(path, &candidate, timed) + CHUNK_FLAG_RESERVE > budget
                && !first_batch.is_empty()
            {
                break;
            }
            first_batch.push(element_tlvs[idx].as_slice());
            idx += 1;
        }

        // Collect remaining elements as AppendItem batches.
        let mut append_batches: Vec<Vec<&[u8]>> = Vec::new();
        while idx < element_tlvs.len() {
            let mut batch: Vec<&[u8]> = Vec::new();
            while idx < element_tlvs.len() {
                let candidate: Vec<&[u8]> = batch
                    .iter()
                    .copied()
                    .chain(std::iter::once(element_tlvs[idx].as_slice()))
                    .collect();
                if encoded_append_len(path, &candidate, timed) + CHUNK_FLAG_RESERVE > budget
                    && !batch.is_empty()
                {
                    break;
                }
                batch.push(element_tlvs[idx].as_slice());
                idx += 1;
            }
            append_batches.push(batch);
        }

        // 2) Encode each chunk. Same Option semantics as the incremental
        // packer: None (omitted) for single-message output, Some(true)/
        // Some(false) explicitly on every chunk of a multi-chunk sequence.
        let total = 1 + append_batches.len();
        let mut messages: Vec<Vec<u8>> = Vec::with_capacity(total);
        let chunked = total > 1;
        let first_more = if chunked { Some(true) } else { None };
        messages.push(encode_replace_all(path, &first_batch, timed, first_more));
        for (i, batch) in append_batches.iter().enumerate() {
            let more = Some(i + 1 < append_batches.len());
            messages.push(encode_append_items(path, batch, timed, more));
        }
        messages
    }

    proptest! {
        /// The incremental packer must produce byte-identical chunks to the
        /// pre-phase-3 full-re-encode packer for arbitrary element sets.
        #[test]
        fn incremental_packer_matches_reference(
            lens in proptest::collection::vec(0usize..120, 0..30),
            budget in 60usize..600,
            timed: bool,
        ) {
            let elems: Vec<Vec<u8>> = lens.iter().map(|&n| {
                let mut buf = Vec::new();
                let mut w = TlvWriter::new(&mut buf);
                w.put_bytes(Tag::Anonymous, &vec![0x5A; n]).unwrap();
                buf
            }).collect();
            let p = p(); // the existing test-path helper at the top of the test module
            prop_assert_eq!(
                build_list_write_chunks(p, &elems, budget, timed),
                build_list_write_chunks_reference(p, &elems, budget, timed)
            );
        }
    }

    #[test]
    fn single_chunk_equals_replace_all_build_write_request() {
        let elems = vec![entry_tlv(1), entry_tlv(2)];
        let chunks = build_list_write_chunks(p(), &elems, 4096, false);
        assert_eq!(chunks.len(), 1);
        // Byte-identical to a single ReplaceAll write of the full array.
        let mut arr = Vec::new();
        let mut w = TlvWriter::new(&mut arr);
        w.write_value(
            Tag::Anonymous,
            &Value::Array(vec![
                Value::Structure(vec![(Tag::Context(1), Value::Uint(1))]),
                Value::Structure(vec![(Tag::Context(1), Value::Uint(2))]),
            ]),
        )
        .unwrap();
        let expected = build_write_request(&[AttributeWriteRequest {
            path: p(),
            value_tlv: arr,
        }]);
        assert_eq!(
            chunks[0], expected,
            "single-chunk output must be byte-identical to build_write_request"
        );
    }

    #[test]
    fn overflow_splits_and_sets_more_chunked() {
        // tiny budget forces one element per message
        let elems = vec![entry_tlv(1), entry_tlv(2), entry_tlv(3)];
        let chunks = build_list_write_chunks(p(), &elems, 40, false);
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        // every chunk of a multi-chunk sequence carries MoreChunkedMessages
        // explicitly: Some(true) on all but the last, Some(false) — present,
        // not merely absent — on the last (chip parity: WriteHandler.cpp
        // inherits the previous chunk's value when the field is absent).
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(
                more_chunked_flag(c),
                Some(i + 1 != chunks.len()),
                "chunk {i}"
            );
        }
    }

    #[test]
    fn reassemble_roundtrips() {
        let elems: Vec<Vec<u8>> = (0..7).map(entry_tlv).collect();
        let chunks = build_list_write_chunks(p(), &elems, 48, false);
        assert_eq!(reassemble_list_write(&chunks), elems);
    }

    /// NEW: forces a 3-chunk output and asserts the MoreChunkedMessages(ctx3)
    /// presence/value on each chunk, plus that a single-chunk output carries
    /// no ctx3 element at all.
    #[test]
    fn multi_chunk_carries_explicit_flag_final_chunk_explicit_false() {
        // Budget tight enough that 3 entries each land in their own message:
        // chunk 0 = ReplaceAll[entry 1], chunk 1 = AppendItem[entry 2],
        // chunk 2 = AppendItem[entry 3].
        let elems = vec![entry_tlv(1), entry_tlv(2), entry_tlv(3)];
        let chunks = build_list_write_chunks(p(), &elems, 40, false);
        assert_eq!(
            chunks.len(),
            3,
            "expected exactly 3 chunks, got {}",
            chunks.len()
        );
        assert_eq!(more_chunked_flag(&chunks[0]), Some(true), "chunk 0");
        assert_eq!(more_chunked_flag(&chunks[1]), Some(true), "chunk 1");
        assert_eq!(
            more_chunked_flag(&chunks[2]),
            Some(false),
            "final chunk must carry an EXPLICIT MoreChunkedMessages=false, not omit it"
        );

        // A single-chunk (unsplit) write carries no ctx3 element at all.
        let single = build_list_write_chunks(p(), &[entry_tlv(1)], 4096, false);
        assert_eq!(single.len(), 1);
        assert_eq!(
            more_chunked_flag(&single[0]),
            None,
            "single-chunk output must omit MoreChunkedMessages entirely"
        );
    }

    /// test helper: does this `WriteRequestMessage` carry `MoreChunkedMessages`
    /// (ctx3)? `None` = absent, `Some(v)` = explicitly present with value `v`.
    fn more_chunked_flag(msg: &[u8]) -> Option<bool> {
        use matter_codec::{Element, TlvReader};
        let mut r = TlvReader::new(msg);
        // enter the anonymous message struct
        let _ = r.next();
        loop {
            match r.next() {
                Ok(Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Bool(b),
                })) => return Some(b),
                Ok(Some(Element::ContainerStart { .. })) => {
                    let _ = super::skip_container(&mut r);
                }
                Ok(Some(Element::ContainerEnd) | None) | Err(_) => return None,
                Ok(Some(_)) => {}
            }
        }
    }

    proptest! {
        #[test]
        fn split_reassemble_identity(count in 0usize..30, budget in 30usize..200) {
            let elems: Vec<Vec<u8>> = (0..count as u64).map(entry_tlv).collect();
            let chunks = build_list_write_chunks(p(), &elems, budget, false);
            prop_assert_eq!(reassemble_list_write(&chunks), elems.clone());
            // MoreChunked invariant: explicit on every chunk when multi-chunk,
            // absent entirely when single-chunk.
            for (i, c) in chunks.iter().enumerate() {
                let expected = if chunks.len() > 1 {
                    Some(i + 1 != chunks.len())
                } else {
                    None
                };
                prop_assert_eq!(more_chunked_flag(c), expected);
            }
        }
    }
}

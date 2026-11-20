//! Matter Interaction Model (IM) message framing — Matter Core Spec §10.
//!
//! Builders for the IM message envelopes the wire carries
//! (`InvokeRequestMessage`, `ReadRequestMessage`, `WriteRequestMessage`,
//! `SubscribeRequestMessage`, `StatusResponseMessage`)
//! and parsers for the responses (`InvokeResponseMessage`,
//! `ReportDataMessage`, `WriteResponseMessage`, `SubscribeResponseMessage`).
//! Callers supply already-encoded cluster TLV payloads (e.g. from
//! `matter-clusters` codecs) and compose them with the concrete paths in
//! [`path`].
//!
//! Scope:
//!
//! - **Invoke** — single command, group (broadcast) invoke, and batched
//!   multi-command invoke via [`build_invoke_request_batch`] /
//!   [`parse_invoke_response_batch`]. A controller-side batching verb and
//!   `MaxPathsPerInvoke` gating are not here; the batch builders are.
//! - **Read and subscribe** — concrete and wildcard paths ([`ReadPath`]),
//!   [`SubscribeRequest`] with its response and `StatusResponse` handshake,
//!   and [`ReportAccumulator`] for reassembling chunked reports across
//!   messages and across list boundaries.
//! - **Write** — single-message writes, plus [`build_list_write_chunks`] for
//!   a list attribute too large for one message.
//! - **Events** — event paths and filters in `ReadRequest` and
//!   `SubscribeRequest`, and `EventReportIB` parsing.
//! - **Timed interactions** — the `TimedRequest` message and the
//!   `TimedRequest` flag on writes and invokes.
//! - **Server side** — [`parse_invoke_request`] and the
//!   `build_invoke_response_*` builders, for responding to an invoke rather
//!   than issuing one (used by this workspace's OTA provider).
//!
//! This crate depends only on `matter-codec`: it frames and parses IM
//! messages and does no IO. Byte-parity with matter.js is enforced by
//! `tests/im_byte_parity.rs`
//! against fixtures captured via `cargo xtask capture-im`.

#![forbid(unsafe_code)]

mod accumulator;
#[cfg(test)]
mod debug_redaction_tests;
pub mod error;
pub mod event;
pub mod invoke;
pub mod invoke_server;
pub mod path;
pub mod read;
pub mod status;
pub mod subscription;
pub mod timed;
pub mod write;

pub use accumulator::{ReportAccumulator, DEFAULT_MAX_BYTES, DEFAULT_MAX_ELEMENTS};
pub use error::ImError;
pub use event::{
    EventFilter, EventPath, EventPriority, EventReport, EventReportItem, EventTimestamp,
};
pub use invoke::{
    build_invoke_request, build_invoke_request_batch, build_invoke_request_group,
    build_invoke_request_timed, parse_invoke_response, parse_invoke_response_batch,
    parse_invoke_response_checked, InvokeResponse, InvokeResponseEntry,
};
pub use invoke_server::{
    build_invoke_response_command, build_invoke_response_status, parse_invoke_request,
    InvokedCommand, ParsedInvokeRequest,
};
pub use path::{AttributePath, CommandPath, ReadPath};
pub use read::{
    build_read_request, build_read_request_full, build_read_request_paths, parse_report_data,
    AttributeReportItem, ReportData, ReportOp,
};
pub use status::{parse_status_response, ImStatus};
pub use subscription::{
    build_status_response, build_subscribe_request, parse_subscribe_response, SubscribeRequest,
    SubscribeResponse,
};
pub use timed::build_timed_request;
pub use write::{
    build_list_write_chunks, build_write_request, build_write_request_timed, parse_write_response,
    parse_write_response_checked, AttributeWriteRequest,
};

/// Interaction Model protocol revision emitted at context tag `0xFF` in
/// every top-level IM message. Confirmed against the matter.js byte-parity
/// fixture (see `tests/im_byte_parity.rs`); bump only when a captured
/// fixture proves matter.js changed it.
pub const IM_REVISION: u8 = 11;

use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};

/// Assert the reader's first element is an anonymous message struct and
/// consume its start.
///
/// # Errors
///
/// Returns [`error::ImError::NotAStruct`] if the first element is not an
/// anonymous structure start, or propagates any [`error::ImError::Codec`]
/// error from the reader.
pub fn expect_message_struct(r: &mut TlvReader<'_>) -> Result<(), error::ImError> {
    match r.next()? {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => Ok(()),
        Some(_) | None => Err(error::ImError::NotAStruct),
    }
}

/// Reader positioned just after a container start: consume to its matching
/// end, returning the members as `(tag, value)` pairs (for List/Structure).
/// Calling this with the reader in any other position yields an
/// [`error::ImError`] or misattributed members — never a panic or UB.
///
/// # Errors
///
/// Returns [`error::ImError::Codec`] wrapping
/// [`matter_codec::Error::UnclosedContainer`] if the input ends before a
/// matching end-of-container, or propagates any other codec error.
pub fn read_container_members(r: &mut TlvReader<'_>) -> Result<Vec<(Tag, Value)>, error::ImError> {
    let mut out = Vec::new();
    loop {
        match r.next()? {
            None => {
                return Err(error::ImError::Codec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => return Ok(out),
            Some(Element::Scalar { tag, value }) => out.push((tag, value)),
            Some(Element::ContainerStart { tag, kind }) => {
                let v = read_container_value(r, kind)?;
                out.push((tag, v));
            }
            Some(_) => {}
        }
    }
}

/// Reader positioned just after a container start (of `kind`): read the
/// whole sub-tree into a [`Value`]. Calling this with the reader in any other
/// position yields an [`error::ImError`] or misattributed members — never a
/// panic or UB.
///
/// # Errors
///
/// Propagates any error from [`read_container_members`].
pub fn read_container_value(
    r: &mut TlvReader<'_>,
    kind: ContainerKind,
) -> Result<Value, error::ImError> {
    if matches!(kind, ContainerKind::Array) {
        // Arrays: build the Vec<Value> directly instead of collecting
        // (Tag, Value) members and re-collecting into a second Vec. Tags on
        // array children are discarded, as before (lenient; the codec's
        // tree-builder path is the strict one).
        let mut elements = Vec::new();
        loop {
            match r.next()? {
                None => {
                    return Err(error::ImError::Codec(
                        matter_codec::Error::UnclosedContainer,
                    ))
                }
                Some(Element::ContainerEnd) => return Ok(Value::Array(elements)),
                Some(Element::Scalar { value, .. }) => elements.push(value),
                Some(Element::ContainerStart {
                    kind: inner_kind, ..
                }) => elements.push(read_container_value(r, inner_kind)?),
                Some(_) => {}
            }
        }
    }
    let members = read_container_members(r)?;
    Ok(match kind {
        ContainerKind::Structure => Value::Structure(members),
        // ContainerKind::List and any future non-exhaustive variants: preserve as List.
        _ => Value::List(members),
    })
}

/// Reader positioned just after a container start: discard the whole
/// sub-tree (used to skip fields we do not consume). Calling this with the
/// reader in any other position yields an [`error::ImError`] or misattributed
/// members — never a panic or UB.
///
/// Streaming: the skipped bytes are walked structurally (tags, lengths,
/// depth) but never decoded — string payloads in skipped data are not
/// UTF-8 validated — and it does not charge the codec's tree-builder
/// element budget; a discarded payload is bounded by its input size only.
/// (Deliberate: skipped data is never observed, so checking it would only
/// cost time.)
///
/// # Errors
///
/// Propagates any [`matter_codec::Error`] from the underlying streaming
/// walk (e.g. `UnclosedContainer` on truncated input) as
/// [`error::ImError::Codec`].
pub fn skip_container(r: &mut TlvReader<'_>) -> Result<(), error::ImError> {
    r.skip_container().map_err(error::ImError::Codec)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use matter_codec::{Tag, TlvWriter};

    #[test]
    fn skip_container_leaves_reader_at_next_sibling() {
        // { deep nested container with mixed scalars } followed by a sentinel uint.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(1)).unwrap(); // the container we skip
        w.put_uint(Tag::Context(0), 7).unwrap();
        w.start_array(Tag::Context(1)).unwrap();
        w.put_bytes(Tag::Anonymous, &[0xAA; 40]).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap(); // ctx1 struct
        w.put_uint(Tag::Context(2), 42).unwrap(); // sentinel sibling
        w.end_container().unwrap();

        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        )); // outer
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        )); // ctx1
        skip_container(&mut r).unwrap();
        // Reader must now be positioned at the sentinel.
        match r.next().unwrap() {
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(42),
            }) => {}
            other => panic!("expected sentinel after skip, got {other:?}"),
        }
    }

    #[test]
    fn read_container_value_array_matches_codec_tree_builder_shape() {
        // Array of mixed scalars + a nested array; tags on array children are
        // discarded (lenient, unchanged behavior).
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Anonymous, 1).unwrap();
        w.start_array(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Anonymous, 2).unwrap();
        w.end_container().unwrap();
        w.put_utf8(Tag::Anonymous, "x").unwrap();
        w.end_container().unwrap();

        let mut r = TlvReader::new(&buf);
        let Some(Element::ContainerStart { kind, .. }) = r.next().unwrap() else {
            panic!("expected array start");
        };
        let v = read_container_value(&mut r, kind).unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Uint(1),
                Value::Array(vec![Value::Uint(2)]),
                Value::Utf8(String::from("x")),
            ])
        );
    }
}

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

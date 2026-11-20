//! Server-side `Invoke` framing — the inverse of [`crate::invoke`]'s client
//! codecs. A controller never had to *read* an inbound `InvokeRequestMessage` or
//! *write* an `InvokeResponseMessage`; an OTA Provider (M9-F) does both.

#![forbid(unsafe_code)]

use crate::invoke::{command_path_from_reader, retag_container_anonymous, write_command_path};
use crate::path::CommandPath;
use crate::status::ImStatus;
use crate::{expect_message_struct, skip_container, IM_REVISION};
use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

/// One command parsed out of an inbound `InvokeRequestMessage`.
///
/// `Debug` is written by hand so that `fields_tlv` prints as its length only
/// (`"<N bytes>"`): it is the requester's decrypted command, which for some
/// commands carries a secret, so it must never reach a log through a stray
/// `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct InvokedCommand {
    /// `(endpoint, cluster, command)` the requester invoked.
    pub path: CommandPath,
    /// The requester's original `CommandFields` bytes, **verbatim**, under a
    /// fresh anonymous tag (ready to hand to a `matter-clusters` decoder):
    /// only the container's own control/tag bytes are replaced, the body is
    /// copied unexamined. Original integer widths are preserved, and so is
    /// everything else the peer sent — which has three consequences for
    /// consumers:
    ///
    /// - A localized-string suffix (element type `0x1F`, IS1) survives in the
    ///   blob rather than being dropped by a re-encode. Decoded `Value`s are
    ///   unchanged: the downstream decoder still truncates at the IS1
    ///   separator.
    /// - Invalid UTF-8 inside `CommandFields` is **not** rejected here — the
    ///   copy never decodes it — so it surfaces from your own decoder instead
    ///   of from IM parsing.
    /// - An off-spec `Array` whose children carry non-anonymous tags is copied
    ///   through as-is and fails in your decoder with `NonAnonymousArrayTag`;
    ///   the older decode-then-re-encode path silently normalised those tags
    ///   away.
    ///
    /// Plaintext: this struct's `Debug` output shows only its length.
    pub fields_tlv: Vec<u8>,
    /// The `CommandRef` (`CommandDataIB` tag 2), present only in batched invokes.
    pub command_ref: Option<u16>,
}

impl core::fmt::Debug for InvokedCommand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Same name, fields and order as the derive this replaces; only the
        // command fields' bytes are withheld.
        f.debug_struct("InvokedCommand")
            .field("path", &self.path)
            .field("fields_tlv", &format!("<{} bytes>", self.fields_tlv.len()))
            .field("command_ref", &self.command_ref)
            .finish()
    }
}

/// A parsed inbound `InvokeRequestMessage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedInvokeRequest {
    /// `SuppressResponse` — set for group (multicast) invokes.
    pub suppress_response: bool,
    /// `TimedRequest` — the action half of a timed interaction.
    pub timed: bool,
    /// The invoked commands (one for a normal invoke; more for a batch).
    pub commands: Vec<InvokedCommand>,
}

/// Parse an inbound `InvokeRequestMessage` (Matter Core §10.7) — the message a
/// device sends to a server (e.g. a Requestor's `QueryImage` to an OTA Provider).
///
/// # Errors
///
/// Returns [`crate::ImError`] if the message is not a struct or lacks the
/// `InvokeRequests` array, or a `CommandDataIB` lacks a `CommandPath`.
///
/// The message must be complete: end of input before the end-of-container of
/// the message or of any container in it fails with [`crate::ImError::Codec`]
/// wrapping [`matter_codec::Error::UnclosedContainer`], and bytes after the
/// message fail with [`matter_codec::Error::TrailingData`] (chip's
/// `MessageParser::ExitContainer` rejects both).
pub fn parse_invoke_request(bytes: &[u8]) -> Result<ParsedInvokeRequest, crate::ImError> {
    let mut r = TlvReader::new(bytes);
    expect_message_struct(&mut r)?;

    let mut suppress_response = false;
    let mut timed = false;
    let mut commands = Vec::new();

    loop {
        match r.next()? {
            None => {
                return Err(crate::ImError::Codec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bool(b),
            }) => suppress_response = b,
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Bool(b),
            }) => timed = b,
            Some(Element::ContainerStart {
                tag: Tag::Context(2),
                kind: ContainerKind::Array,
            }) => {
                read_invoke_requests(&mut r, &mut commands)?;
            }
            Some(Element::ContainerStart { .. }) => skip_container(&mut r)?,
            Some(_) => {}
        }
    }
    r.finish()?;

    Ok(ParsedInvokeRequest {
        suppress_response,
        timed,
        commands,
    })
}

/// Read the `InvokeRequests` array body: a sequence of `CommandDataIB` structs,
/// until the array's `ContainerEnd`. End of input first is `UnclosedContainer`.
fn read_invoke_requests(
    r: &mut TlvReader<'_>,
    out: &mut Vec<InvokedCommand>,
) -> Result<(), crate::ImError> {
    loop {
        match r.next()? {
            None => {
                return Err(crate::ImError::Codec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => return Ok(()),
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            }) => out.push(read_command_data(r)?),
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
}

/// Read one `CommandDataIB` body (reader positioned just after its struct start):
/// ctx0 `CommandPath` (list), ctx1 `CommandFields` (struct), opt ctx2 `CommandRef`.
/// End of input before the IB's `ContainerEnd` is `UnclosedContainer`.
fn read_command_data(r: &mut TlvReader<'_>) -> Result<InvokedCommand, crate::ImError> {
    let mut path = None;
    let mut fields_tlv = None;
    let mut command_ref = None;

    loop {
        match r.next()? {
            None => {
                return Err(crate::ImError::Codec(
                    matter_codec::Error::UnclosedContainer,
                ))
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::List,
            }) => {
                path = Some(command_path_from_reader(r)?);
            }
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::Structure,
            }) => {
                fields_tlv = Some(retag_container_anonymous(r, ContainerKind::Structure)?);
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(n),
            }) => {
                command_ref = u16::try_from(n).ok();
            }
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }

    Ok(InvokedCommand {
        path: path.ok_or(crate::ImError::MissingField("CommandDataIB.CommandPath"))?,
        fields_tlv: fields_tlv
            .ok_or(crate::ImError::MissingField("CommandDataIB.CommandFields"))?,
        command_ref,
    })
}

/// Build a single-response `InvokeResponseMessage` carrying a response **command**
/// (`InvokeResponseIB` → ctx0 `Command` → `CommandDataIB`: path + fields).
/// `SuppressResponse = false`.
///
/// `response_fields_tlv` must be an anonymous-tagged struct (a `matter-clusters`
/// response encoder output).
///
/// # Panics
///
/// Panics if `response_fields_tlv` is not valid anonymous-tagged TLV (as
/// [`crate::build_invoke_request`]).
#[must_use]
#[allow(clippy::expect_used)] // Vec-backed TlvWriter is infallible.
pub fn build_invoke_response_command(path: CommandPath, response_fields_tlv: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(48 + response_fields_tlv.len());
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bool(Tag::Context(0), false)
        .expect("infallible: vec writer"); // SuppressResponse
    w.start_array(Tag::Context(1))
        .expect("infallible: vec writer"); // InvokeResponses
    {
        w.start_structure(Tag::Anonymous)
            .expect("infallible: vec writer"); // InvokeResponseIB
        w.start_structure(Tag::Context(0))
            .expect("infallible: vec writer"); // Command = CommandDataIB
        write_command_path(&mut w, Tag::Context(0), path);
        w.put_preencoded(Tag::Context(1), response_fields_tlv)
            .expect("infallible: caller passes a valid anonymous-tagged struct");
        w.end_container().expect("infallible: vec writer"); // CommandDataIB
        w.end_container().expect("infallible: vec writer"); // InvokeResponseIB
    }
    w.end_container().expect("infallible: vec writer"); // InvokeResponses array
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer"); // message struct
    buf
}

/// Build a single-response `InvokeResponseMessage` carrying a bare **status**
/// for `path` (`InvokeResponseIB` → ctx1 `Status` → `CommandStatusIB`: path +
/// `StatusIB` with `Status = status`). `SuppressResponse = false`.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn build_invoke_response_status(path: CommandPath, status: ImStatus) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bool(Tag::Context(0), false)
        .expect("infallible: vec writer"); // SuppressResponse
    w.start_array(Tag::Context(1))
        .expect("infallible: vec writer"); // InvokeResponses
    {
        w.start_structure(Tag::Anonymous)
            .expect("infallible: vec writer"); // InvokeResponseIB
        w.start_structure(Tag::Context(1))
            .expect("infallible: vec writer"); // Status = CommandStatusIB
        write_command_path(&mut w, Tag::Context(0), path);
        w.start_structure(Tag::Context(1))
            .expect("infallible: vec writer"); // StatusIB
        w.put_uint(Tag::Context(0), u64::from(status.to_u8()))
            .expect("infallible: vec writer"); // Status
        w.end_container().expect("infallible: vec writer"); // StatusIB
        w.end_container().expect("infallible: vec writer"); // CommandStatusIB
        w.end_container().expect("infallible: vec writer"); // InvokeResponseIB
    }
    w.end_container().expect("infallible: vec writer"); // InvokeResponses array
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer"); // message struct
    buf
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use crate::invoke::{build_invoke_request, parse_invoke_response, InvokeResponse};

    fn anon_struct_ctx0(value: u64) -> Vec<u8> {
        let mut b = Vec::new();
        let mut w = TlvWriter::new(&mut b);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), value).unwrap();
        w.end_container().unwrap();
        b
    }

    #[test]
    fn parse_invoke_request_roundtrips_builder() {
        let fields = anon_struct_ctx0(0xFFF1);
        let path = CommandPath {
            endpoint: 0,
            cluster: 0x0029,
            command: 0x00,
        };
        let msg = build_invoke_request(path, &fields);
        let parsed = parse_invoke_request(&msg).expect("parse");
        assert!(!parsed.suppress_response);
        assert!(!parsed.timed);
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].path, path);
        assert_eq!(parsed.commands[0].fields_tlv, fields);
        assert_eq!(parsed.commands[0].command_ref, None);
    }

    #[test]
    fn build_invoke_response_command_roundtrips() {
        let fields = anon_struct_ctx0(7);
        let path = CommandPath {
            endpoint: 0,
            cluster: 0x0029,
            command: 0x01,
        };
        let msg = build_invoke_response_command(path, &fields);
        match parse_invoke_response(&msg).expect("parse") {
            InvokeResponse::Command {
                path: p,
                fields_tlv,
            } => {
                assert_eq!(p, path);
                assert_eq!(fields_tlv, fields);
            }
            InvokeResponse::Status(s) => panic!("expected Command, got Status({s:?})"),
        }
    }

    #[test]
    fn command_fields_preserve_device_integer_widths() {
        // Mirror of invoke.rs's test on the server-side (request) parse path.
        // Device encodes CommandFields with a NON-minimal width (uint16 42);
        // span-copy + retag must return those bytes verbatim.
        let nonminimal_fields = [0x15u8, 0x25, 0x00, 0x2A, 0x00, 0x18];
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_bool(Tag::Context(0), false).unwrap(); // SuppressResponse
        w.put_bool(Tag::Context(1), false).unwrap(); // TimedRequest
        w.start_array(Tag::Context(2)).unwrap(); // InvokeRequests
        w.start_structure(Tag::Anonymous).unwrap(); // CommandDataIB
        w.start_list(Tag::Context(0)).unwrap(); // CommandPath
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0x0029).unwrap();
        w.put_uint(Tag::Context(2), 0x00).unwrap();
        w.end_container().unwrap();
        w.put_preencoded(Tag::Context(1), &nonminimal_fields)
            .unwrap();
        w.end_container().unwrap(); // CommandDataIB
        w.end_container().unwrap(); // InvokeRequests array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let parsed = parse_invoke_request(&buf).expect("parse");
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(
            parsed.commands[0].fields_tlv, nonminimal_fields,
            "device widths must be preserved verbatim"
        );
    }

    #[test]
    fn build_invoke_response_status_roundtrips() {
        let path = CommandPath {
            endpoint: 0,
            cluster: 0x0029,
            command: 0x04,
        };
        let msg = build_invoke_response_status(path, ImStatus::Success);
        assert!(matches!(
            parse_invoke_response(&msg),
            Ok(InvokeResponse::Status(ImStatus::Success))
        ));
    }
}

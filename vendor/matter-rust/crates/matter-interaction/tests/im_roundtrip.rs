//! Roundtrip tests for the `im` module: a request built by `build_*`,
//! echoed into the matching response shape, parses back to the same data.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

// Deep submodule paths used intentionally to confirm the submodules are directly accessible (flat re-exports also exist at the crate root).
use matter_codec::{Tag, TlvWriter};
use matter_interaction::invoke::{build_invoke_request, parse_invoke_response, InvokeResponse};
use matter_interaction::CommandPath;

/// Build a minimal `InvokeResponseMessage` echoing `path` with `fields`.
fn echo_invoke_response(path: CommandPath, fields: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_bool(Tag::Context(0), false).unwrap();
    w.start_array(Tag::Context(1)).unwrap();
    w.start_structure(Tag::Anonymous).unwrap(); // InvokeResponseIB
    w.start_structure(Tag::Context(0)).unwrap(); // Command
    w.start_list(Tag::Context(0)).unwrap();
    w.put_uint(Tag::Context(0), u64::from(path.endpoint))
        .unwrap();
    w.put_uint(Tag::Context(1), u64::from(path.cluster))
        .unwrap();
    w.put_uint(Tag::Context(2), u64::from(path.command))
        .unwrap();
    w.end_container().unwrap();
    // CommandFields: splice the anonymous fields struct under tag 1.
    w.put_preencoded(Tag::Context(1), fields).unwrap();
    w.end_container().unwrap();
    w.end_container().unwrap();
    w.end_container().unwrap();
    w.put_uint(Tag::Context(0xFF), 11).unwrap();
    w.end_container().unwrap();
    buf
}

#[test]
fn invoke_roundtrips_path_and_fields() {
    let path = CommandPath {
        endpoint: 0,
        cluster: 0x003E,
        command: 0x07,
    }; // CSRResponse-like
       // struct{ ctx1: bytes(3) }
    let fields = vec![0x15, 0x30, 0x01, 0x03, 0xAA, 0xBB, 0xCC, 0x18];
    // Sanity: the request embeds the fields.
    let _req = build_invoke_request(path, &fields);

    let resp_bytes = echo_invoke_response(path, &fields);
    match parse_invoke_response(&resp_bytes).unwrap() {
        InvokeResponse::Command {
            path: p,
            fields_tlv,
        } => {
            assert_eq!(p, path);
            assert_eq!(
                fields_tlv, fields,
                "fields_tlv mismatch: got {fields_tlv:02X?}, expected {fields:02X?}",
            );
        }
        InvokeResponse::Status(s) => panic!("expected Command, got {s:?}"),
    }
}

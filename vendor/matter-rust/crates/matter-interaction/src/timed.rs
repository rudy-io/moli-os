//! `TimedRequestMessage` framing — Matter §8.7 (timed interactions).
//!
//! IM opcode `0x0a`. Body: `{ 0: TimeoutMs (u16), 0xFF: InteractionModelRevision }`.
//! The client sends this first; the device replies `StatusResponse(SUCCESS)` and
//! then expects the Write/Invoke (with its `TimedRequest` flag set) on the **same
//! exchange** within `timeout_ms`. Byte-parity with matter.js is enforced by
//! `tests/im_byte_parity.rs`.

#![forbid(unsafe_code)]

use crate::IM_REVISION;
use matter_codec::{Tag, TlvWriter};

/// Build a `TimedRequestMessage` requesting `timeout_ms` milliseconds for the
/// follow-up Write/Invoke (which the device expects on the same exchange).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn build_timed_request(timeout_ms: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(timeout_ms))
        .expect("infallible: vec writer"); // TimeoutMs
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use matter_codec::{Element, TlvReader, Value};

    #[test]
    fn timed_request_has_timeout_at_tag_0() {
        let bytes = build_timed_request(10000);
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(10000)
            })
        ));
    }
}

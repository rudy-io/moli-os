//! `SubscribeRequestMessage` / `SubscribeResponseMessage` / `StatusResponseMessage`
//! framing — Matter §10.6 (subscription interaction).
//!
//! Byte-parity with matter.js is enforced by `tests/im_byte_parity.rs`
//! against fixtures captured via `cargo xtask capture-im`.

#![forbid(unsafe_code)]

use crate::error::ImError;
use crate::event::{EventFilter, EventPath};
use crate::path::ReadPath;
use crate::{expect_message_struct, skip_container, IM_REVISION};
use matter_codec::{Element, Tag, TlvReader, TlvWriter};

/// Parameters for a subscription request.
///
/// Encodes as a `SubscribeRequestMessage` (Matter §10.6.6):
/// `keepSubscriptions` (ctx 0), `minIntervalFloor` (ctx 1),
/// `maxIntervalCeiling` (ctx 2), `attributeRequests` array (ctx 3),
/// `isFabricFiltered` (ctx 7), `interactionModelRevision` (ctx 0xFF).
#[derive(Clone, Debug, PartialEq)]
pub struct SubscribeRequest {
    /// Whether to keep existing subscriptions alive when this one is
    /// established. `false` is the typical controller-side value.
    pub keep_subscriptions: bool,
    /// Minimum reporting interval floor in seconds.
    pub min_interval_floor: u16,
    /// Maximum reporting interval ceiling in seconds.
    pub max_interval_ceiling: u16,
    /// Attribute paths to subscribe to. Each [`ReadPath`] field that is
    /// `Some` is emitted as a context-tagged member of the
    /// `AttributePathIB` list (endpoint=2, cluster=3, attribute=4);
    /// `None` fields are omitted (wildcard).
    pub paths: Vec<ReadPath>,
    /// Event paths to subscribe to (`EventRequests`, context tag 4). Empty ⇒
    /// the array is omitted.
    pub event_paths: Vec<EventPath>,
    /// Event filters (`EventFilters`, context tag 5) — report only events with
    /// number `>= event_min`. Empty ⇒ the array is omitted.
    pub event_filters: Vec<EventFilter>,
}

/// Parsed `SubscribeResponseMessage` — the device's subscription confirmation.
///
/// Contains the server-assigned `subscription_id` (opaque, stable for the
/// lifetime of the subscription) and the negotiated `max_interval`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SubscribeResponse {
    /// Server-assigned subscription identifier.
    pub subscription_id: u32,
    /// Negotiated maximum reporting interval in seconds.
    pub max_interval: u16,
}

/// Build a `SubscribeRequestMessage` for the given subscription parameters.
///
/// Encodes to the wire format that byte-matches matter.js
/// `TlvSubscribeRequest.encode(...)` for the same input (verified by
/// `tests/im_byte_parity.rs`).
///
/// The path array reuses the same `AttributePathIB` list encoding as
/// [`crate::read::build_read_request_paths`]: each path is an anonymous list
/// with context tags 2/3/4 for endpoint/cluster/attribute; `None` components
/// are omitted (wildcard).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn build_subscribe_request(req: &SubscribeRequest) -> Vec<u8> {
    let mut buf = Vec::with_capacity(48 + req.paths.len() * 24 + req.event_paths.len() * 24);
    let mut w = TlvWriter::new(&mut buf);

    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");

    // ctx[0]: keepSubscriptions (bool)
    w.put_bool(Tag::Context(0), req.keep_subscriptions)
        .expect("infallible: vec writer");

    // ctx[1]: minIntervalFloorSeconds (uint)
    w.put_uint(Tag::Context(1), u64::from(req.min_interval_floor))
        .expect("infallible: vec writer");

    // ctx[2]: maxIntervalCeilingSeconds (uint)
    w.put_uint(Tag::Context(2), u64::from(req.max_interval_ceiling))
        .expect("infallible: vec writer");

    // ctx[3]: attributeRequests (array of AttributePathIB lists)
    w.start_array(Tag::Context(3))
        .expect("infallible: vec writer");
    for p in &req.paths {
        w.start_list(Tag::Anonymous)
            .expect("infallible: vec writer");
        if let Some(ep) = p.endpoint {
            w.put_uint(Tag::Context(2), u64::from(ep))
                .expect("infallible: vec writer");
        }
        if let Some(cl) = p.cluster {
            w.put_uint(Tag::Context(3), u64::from(cl))
                .expect("infallible: vec writer");
        }
        if let Some(at) = p.attribute {
            w.put_uint(Tag::Context(4), u64::from(at))
                .expect("infallible: vec writer");
        }
        w.end_container().expect("infallible: vec writer");
    }
    w.end_container().expect("infallible: vec writer"); // attributeRequests array

    // ctx[4]: eventRequests (array of EventPathIB lists) — omitted when empty.
    if !req.event_paths.is_empty() {
        w.start_array(Tag::Context(4))
            .expect("infallible: vec writer");
        for p in &req.event_paths {
            p.write(&mut w).expect("infallible: vec writer");
        }
        w.end_container().expect("infallible: vec writer");
    }

    // ctx[5]: eventFilters (array of EventFilterIB structs) — omitted when empty.
    if !req.event_filters.is_empty() {
        w.start_array(Tag::Context(5))
            .expect("infallible: vec writer");
        for f in &req.event_filters {
            f.write(&mut w).expect("infallible: vec writer");
        }
        w.end_container().expect("infallible: vec writer");
    }

    // ctx[7]: isFabricFiltered (bool) — always false for controller-side reads.
    // ctx[4]=eventRequests / ctx[5]=eventFilters are emitted above when present;
    // ctx[6] (dataVersionFilters) is unused.
    w.put_bool(Tag::Context(7), false)
        .expect("infallible: vec writer");

    // ctx[0xFF]: interactionModelRevision (uint)
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");

    w.end_container().expect("infallible: vec writer");
    buf
}

/// Parse a `SubscribeResponseMessage` into a [`SubscribeResponse`].
///
/// Extracts `subscriptionId` (ctx 0, uint32) and `maxInterval` (ctx 2,
/// uint16). The `interactionModelRevision` (ctx 0xFF) is read and discarded.
///
/// # Errors
///
/// Returns [`ImError`] if the message is not a struct, or if
/// `subscriptionId` / `maxInterval` are absent or out of range.
///
/// The message must be complete: end of input before the top-level
/// end-of-container fails with [`ImError::Codec`] wrapping
/// [`matter_codec::Error::UnclosedContainer`], and bytes after it fail with
/// [`matter_codec::Error::TrailingData`] (chip's
/// `MessageParser::ExitContainer` rejects both).
pub fn parse_subscribe_response(bytes: &[u8]) -> Result<SubscribeResponse, ImError> {
    let mut r = TlvReader::new(bytes);
    expect_message_struct(&mut r)?;

    let mut subscription_id: Option<u32> = None;
    let mut max_interval: Option<u16> = None;

    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: matter_codec::Value::Uint(n),
            }) => {
                subscription_id = Some(u32::try_from(n).map_err(|_| {
                    ImError::UnexpectedValue("SubscribeResponse.subscriptionId exceeds u32")
                })?);
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: matter_codec::Value::Uint(n),
            }) => {
                max_interval = Some(u16::try_from(n).map_err(|_| {
                    ImError::UnexpectedValue("SubscribeResponse.maxInterval exceeds u16")
                })?);
            }
            Some(Element::ContainerStart { .. }) => skip_container(&mut r)?,
            Some(_) => {}
        }
    }
    r.finish()?;

    Ok(SubscribeResponse {
        subscription_id: subscription_id
            .ok_or(ImError::MissingField("SubscribeResponse.subscriptionId"))?,
        max_interval: max_interval.ok_or(ImError::MissingField("SubscribeResponse.maxInterval"))?,
    })
}

/// Build a `StatusResponseMessage` with the given `status` code.
///
/// Used to acknowledge a `ReportData` during a subscription. `status = 0`
/// is the success ack. `interactionModelRevision` is always [`IM_REVISION`].
///
/// Encodes to the wire format that byte-matches matter.js
/// `TlvStatusResponse.encode({ status, interactionModelRevision: 11 })`.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn build_status_response(status: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    let mut w = TlvWriter::new(&mut buf);

    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(status))
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use matter_codec::ContainerKind;

    #[test]
    fn status_response_success_has_expected_structure() {
        let bytes = build_status_response(0);
        let mut r = TlvReader::new(&bytes);
        // anon struct
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure
            })
        ));
        // ctx[0] = uint 0
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: matter_codec::Value::Uint(0)
            })
        ));
        // ctx[255] = uint 11
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(0xFF),
                value: matter_codec::Value::Uint(11)
            })
        ));
    }

    #[test]
    fn subscribe_request_has_expected_structure() {
        let req = SubscribeRequest {
            keep_subscriptions: false,
            min_interval_floor: 1,
            max_interval_ceiling: 30,
            paths: vec![ReadPath::concrete(1, 0x06, 0x0000)],
            event_paths: vec![],
            event_filters: vec![],
        };
        let bytes = build_subscribe_request(&req);
        let mut r = TlvReader::new(&bytes);
        // anon struct
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure
            })
        ));
        // ctx[0] = keepSubscriptions = false
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: matter_codec::Value::Bool(false)
            })
        ));
        // ctx[1] = minIntervalFloor = 1
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: matter_codec::Value::Uint(1)
            })
        ));
        // ctx[2] = maxIntervalCeiling = 30
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: matter_codec::Value::Uint(30)
            })
        ));
        // ctx[3] = array of paths
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Context(3),
                kind: ContainerKind::Array
            })
        ));
    }

    #[test]
    fn parse_subscribe_response_roundtrip() {
        // Hand-encode a SubscribeResponse and parse it back.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0x1234_5678_u64).unwrap(); // subscriptionId
        w.put_uint(Tag::Context(2), 30_u64).unwrap(); // maxInterval
        w.put_uint(Tag::Context(0xFF), 11_u64).unwrap(); // revision
        w.end_container().unwrap();

        let result = parse_subscribe_response(&buf).unwrap();
        assert_eq!(result.subscription_id, 0x1234_5678);
        assert_eq!(result.max_interval, 30);
    }
}

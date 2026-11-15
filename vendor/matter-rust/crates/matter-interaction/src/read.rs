//! `ReadRequestMessage` / `ReportDataMessage` framing — Matter §10.6.

#![forbid(unsafe_code)]

use crate::error::ImError;
use crate::event::{EventFilter, EventPath};
pub use crate::path::{AttributePath, ReadPath};
use crate::{expect_message_struct, read_container_value, skip_container, IM_REVISION};
use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

/// Build a `ReadRequestMessage` carrying attribute paths, event paths, and event
/// filters.
///
/// Field order on the wire (Matter §10.6 / `ReadRequestMessage`):
/// `AttributeRequests[0]`, `EventRequests[1]`, `EventFilters[2]`,
/// `IsFabricFiltered[3]`, `InteractionModelRevision[0xFF]`. An empty slice omits
/// its array entirely. Each `AttributePathIB` is a list (endpoint=2, cluster=3,
/// attribute=4); `EventPathIB` is a list and `EventFilterIB` is a struct (see
/// [`EventPath`]/[`EventFilter`]). `Some` fields are emitted, `None` are wildcards.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn build_read_request_full(
    attr_paths: &[ReadPath],
    event_paths: &[EventPath],
    event_filters: &[EventFilter],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32 + attr_paths.len() * 24 + event_paths.len() * 24);
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    if !attr_paths.is_empty() {
        w.start_array(Tag::Context(0))
            .expect("infallible: vec writer"); // AttributeRequests
        for p in attr_paths {
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
        w.end_container().expect("infallible: vec writer"); // AttributeRequests array
    }
    if !event_paths.is_empty() {
        w.start_array(Tag::Context(1))
            .expect("infallible: vec writer"); // EventRequests
        for p in event_paths {
            p.write(&mut w).expect("infallible: vec writer");
        }
        w.end_container().expect("infallible: vec writer");
    }
    if !event_filters.is_empty() {
        w.start_array(Tag::Context(2))
            .expect("infallible: vec writer"); // EventFilters
        for f in event_filters {
            f.write(&mut w).expect("infallible: vec writer");
        }
        w.end_container().expect("infallible: vec writer");
    }
    w.put_bool(Tag::Context(3), false)
        .expect("infallible: vec writer"); // IsFabricFiltered
    w.put_uint(Tag::Context(0xFF), u64::from(IM_REVISION))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Build a `ReadRequestMessage` for the given (possibly wildcard) attribute paths.
///
/// Each [`ReadPath`] field that is `Some` is emitted as a context-tagged member of
/// the `AttributePathIB` list (endpoint=2, cluster=3, attribute=4); `None` fields
/// are omitted (wildcard). `IsFabricFiltered` is `false`. Delegates to
/// [`build_read_request_full`] with no event paths/filters, so the output is
/// byte-identical to the attribute-only encoding.
#[must_use]
pub fn build_read_request_paths(paths: &[ReadPath]) -> Vec<u8> {
    build_read_request_full(paths, &[], &[])
}

/// Build a `ReadRequestMessage` for one or more concrete attribute paths.
///
/// Delegates to [`build_read_request_paths`] so that the output is
/// byte-identical to before: same context tags 2/3/4, same order, same
/// `isFabricFiltered`/`interactionModelRevision`.
#[must_use]
pub fn build_read_request(paths: &[AttributePath]) -> Vec<u8> {
    let read_paths: Vec<ReadPath> = paths.iter().map(|&p| ReadPath::from(p)).collect();
    build_read_request_paths(&read_paths)
}

/// Parsed `ReportDataMessage` (Matter §10.6.4).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ReportData {
    /// Every `AttributeReportIB` carrying `AttributeData`, with the
    /// information needed to reassemble chunked and list-chunked reports.
    ///
    /// For the common single-message (non-chunked) `Replace`-only case, prefer
    /// the [`attributes`](ReportData::attributes) borrowing view over this raw
    /// list.
    pub items: Vec<AttributeReportItem>,
    /// Server-assigned subscription identifier, present only in
    /// subscription `ReportData` messages (context tag 0); `None` in
    /// plain `ReadResponse` messages.
    pub subscription_id: Option<u32>,
    /// `MoreChunkedMessages` (context tag 3): `true` ⇒ more `ReportData`
    /// chunks follow on this exchange and must be solicited with a
    /// `StatusResponse`. Absent on the wire ⇒ `false`.
    pub more_chunked_messages: bool,
    /// `SuppressResponse` (context tag 4): `true` ⇒ the sender does not expect
    /// a `StatusResponse` for this message. Absent on the wire ⇒ `false`.
    pub suppress_response: bool,
    /// Every `EventReportIB` carried in `eventReports` (context tag 2), in wire
    /// order. Empty for attribute-only reports.
    pub events: Vec<crate::event::EventReport>,
    /// Every `AttributeStatusIB` (a per-path status/error, not attribute data),
    /// as `(path, status)` in wire order. These were previously discarded,
    /// so a device reporting e.g. `UnsupportedAttribute` for a requested path was
    /// indistinguishable from the path simply being omitted. Populated by
    /// [`parse_report_data`]; empty for all-data reports and for reports built
    /// via [`ReportData::new`]. Mirrors the write path, which surfaces per-path
    /// status via [`crate::parse_write_response`].
    pub statuses: Vec<(AttributePath, crate::status::ImStatus)>,
}

impl ReportData {
    /// Construct a [`ReportData`] from its decoded parts.
    ///
    /// Provided because the struct is `#[non_exhaustive]`: callers in other
    /// crates cannot use a struct literal, so this constructor is the stable
    /// way to build one (e.g. test fixtures that synthesize a report). Any
    /// future spec-driven field will gain a default here without breaking
    /// existing callers.
    ///
    /// Synthesizes an attribute-only report (`events` empty). Event reports are
    /// populated only by [`parse_report_data`]; an external caller that needs to
    /// synthesize events should construct via the parser from bytes.
    #[must_use]
    pub fn new(
        items: Vec<AttributeReportItem>,
        subscription_id: Option<u32>,
        more_chunked_messages: bool,
        suppress_response: bool,
    ) -> Self {
        Self {
            items,
            subscription_id,
            more_chunked_messages,
            suppress_response,
            events: Vec::new(),
            statuses: Vec::new(),
        }
    }

    /// Borrowing view over the event reports carried in this message
    /// (`eventReports`, context tag 2). Empty for attribute-only reports.
    #[must_use]
    pub fn events(&self) -> &[crate::event::EventReport] {
        &self.events
    }

    /// Borrowing `(path, value)` view over the whole-attribute `Replace` reports
    /// in [`items`](Self::items), as a flattened convenience for the common
    /// single-message (non-chunked) case.
    ///
    /// List-append IBs (`ListIndex` = null, [`ReportOp::Append`]) are **not**
    /// included — use [`items`](Self::items) +
    /// [`ReportAccumulator`](crate::ReportAccumulator) for chunked / list
    /// reassembly. `AttributeStatus` (error) reports never reach `items`, so they
    /// are absent here too.
    ///
    /// This borrows from `items`; it neither allocates nor copies any [`Value`],
    /// unlike materializing an owned `Vec`.
    pub fn attributes(&self) -> impl Iterator<Item = (&AttributePath, &Value)> {
        self.items
            .iter()
            .filter(|it| it.op == ReportOp::Replace)
            .map(|it| (&it.path, &it.value))
    }
}

/// One `AttributeReportIB` carrying `AttributeData`, retaining the list-merge
/// metadata that the [`ReportData::attributes`] convenience view flattens away.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AttributeReportItem {
    /// Concrete `(endpoint, cluster, attribute)`.
    pub path: AttributePath,
    /// Whether this IB replaces the attribute value or appends a list element.
    pub op: ReportOp,
    /// The data value (whole attribute for `Replace`, one element for `Append`).
    pub value: Value,
    /// `DataVersion` (`AttributeData` context tag 0), if present.
    pub data_version: Option<u32>,
}

impl AttributeReportItem {
    /// Construct an [`AttributeReportItem`] from its decoded parts.
    ///
    /// Provided because the struct is `#[non_exhaustive]`: callers in other
    /// crates cannot use a struct literal, so this constructor is the stable
    /// way to build one. Any future spec-driven field will gain a default
    /// here without breaking existing callers.
    #[must_use]
    pub fn new(path: AttributePath, op: ReportOp, value: Value, data_version: Option<u32>) -> Self {
        Self {
            path,
            op,
            value,
            data_version,
        }
    }
}

/// How an [`AttributeReportItem`] merges into accumulated state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReportOp {
    /// Replace the attribute's value (path carried no `ListIndex`).
    Replace,
    /// Append `value` to the attribute's list (path carried `ListIndex` = null).
    Append,
}

/// Parse a `ReportDataMessage` into concrete `(path, value)` pairs.
///
/// Walks the `AttributeReports` array; for each `AttributeReportIB` that
/// carries `AttributeData [1]`, extracts the path (`AttributePathIB [1]`)
/// and the data value (`[2]`). `AttributeStatus` error reports are
/// skipped. A message with no `AttributeReports` yields an empty result.
///
/// # Errors
///
/// Returns [`ImError`] if the message is not a struct, a present
/// `AttributeData` is missing its path or data, or a path value is out of
/// range.
///
/// The message must be complete: end of input before the top-level
/// end-of-container fails with [`ImError::Codec`] wrapping
/// [`matter_codec::Error::UnclosedContainer`], and bytes after it fail with
/// [`matter_codec::Error::TrailingData`] (chip's
/// `MessageParser::ExitContainer` rejects both). A truncated
/// chunk must not decode as a final one (its `MoreChunkedMessages` may be
/// what was cut off).
pub fn parse_report_data(bytes: &[u8]) -> Result<ReportData, ImError> {
    let mut r = TlvReader::new(bytes);
    expect_message_struct(&mut r)?;

    let mut items: Vec<AttributeReportItem> = Vec::new();
    let mut statuses: Vec<(AttributePath, crate::status::ImStatus)> = Vec::new();
    let mut events: Vec<crate::event::EventReport> = Vec::new();
    let mut subscription_id: Option<u32> = None;
    let mut more_chunked_messages = false;
    let mut suppress_response = false;

    // Scan ALL top-level fields. The AttributeReports array (ctx 1) is
    // consumed inline so that the scan continues past it to MoreChunkedMessages
    // (ctx 3) and SuppressResponse (ctx 4), which follow the array on the wire.
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            // subscriptionId [0]
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(n),
            }) => {
                subscription_id = Some(u32::try_from(n).map_err(|_| {
                    ImError::UnexpectedValue("ReportData.subscriptionId exceeds u32")
                })?);
            }
            // attributeReports [1] — consume the array inline.
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::Array,
            }) => parse_attribute_reports(&mut r, &mut items, &mut statuses)?,
            // moreChunkedMessages [3]
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Bool(b),
            }) => more_chunked_messages = b,
            // suppressResponse [4]
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Bool(b),
            }) => suppress_response = b,
            // eventReports [2] — consume the array inline.
            Some(Element::ContainerStart {
                tag: Tag::Context(2),
                kind: ContainerKind::Array,
            }) => crate::event::parse_event_reports(&mut r, &mut events)?,
            // Any other container — skip.
            Some(Element::ContainerStart { .. }) => skip_container(&mut r)?,
            Some(_) => {}
        }
    }
    r.finish()?;

    Ok(ReportData {
        items,
        subscription_id,
        more_chunked_messages,
        suppress_response,
        events,
        statuses,
    })
}

/// One decoded `AttributeReportIB`: either attribute data, a per-path status
/// (`AttributeStatus`), or an empty IB.
enum ReportIb {
    Data(AttributeReportItem),
    Status(AttributePath, crate::status::ImStatus),
    Empty,
}

/// Consume the `AttributeReports` array body (reader positioned just after the
/// array-start at context tag 1), pushing one [`AttributeReportItem`] per IB
/// that carried `AttributeData`, and one `(path, status)` into `statuses` per
/// `AttributeStatus` IB.
fn parse_attribute_reports(
    r: &mut TlvReader<'_>,
    items: &mut Vec<AttributeReportItem>,
    statuses: &mut Vec<(AttributePath, crate::status::ImStatus)>,
) -> Result<(), ImError> {
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => return Ok(()), // end of array
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            }) => match parse_attribute_report_ib(r)? {
                ReportIb::Data(item) => items.push(item),
                ReportIb::Status(path, status) => statuses.push((path, status)),
                ReportIb::Empty => {}
            },
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
}

/// Parse one `AttributeReportIB` body — it carries EITHER `AttributeData [1]`
/// or `AttributeStatus [0]` (the status is surfaced, not skipped).
fn parse_attribute_report_ib(r: &mut TlvReader<'_>) -> Result<ReportIb, ImError> {
    let mut path = None;
    let mut value = None;
    let mut data_version = None;
    let mut append = false;
    let mut status: Option<(AttributePath, crate::status::ImStatus)> = None;
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::Structure,
            }) => {
                // AttributeData = struct { 0:DataVersion?, 1:Path(list), 2:Data }
                parse_attribute_data(r, &mut path, &mut value, &mut data_version, &mut append)?;
            }
            // AttributeStatus [0] = struct { 0:Path(list), 1:StatusIB } — parse
            // the per-path status instead of discarding it. Reuses the
            // write path's identical decoder.
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::Structure,
            }) => {
                status = Some(crate::write::parse_attribute_status_ib(r)?);
            }
            // Any other container → skip.
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
    if let Some((p, s)) = status {
        return Ok(ReportIb::Status(p, s));
    }
    match (path, value) {
        (Some(p), Some(v)) => Ok(ReportIb::Data(AttributeReportItem {
            path: p,
            op: if append {
                ReportOp::Append
            } else {
                ReportOp::Replace
            },
            value: v,
            data_version,
        })),
        (None, None) => Ok(ReportIb::Empty), // no AttributeData/Status present
        (Some(_), None) => Err(ImError::MissingField("AttributeData.Data")),
        (None, Some(_)) => Err(ImError::MissingField("AttributeData.Path")),
    }
}

/// Parse an `AttributeData` body (reader positioned just after the struct
/// start at context tag 1 inside `AttributeReportIB`).
///
/// Populates `path` from the `AttributePathIB` list at tag `[1]`, `value` from
/// the data element at tag `[2]`, `data_version` from tag `[0]`, and sets
/// `append` when the path carried `ListIndex` (tag 5) = null. Either of `path`
/// / `value` may be left `None` if absent; the caller
/// (`parse_attribute_report_ib`) treats a partial result as a protocol error.
fn parse_attribute_data(
    r: &mut TlvReader<'_>,
    path: &mut Option<AttributePath>,
    value: &mut Option<Value>,
    data_version: &mut Option<u32>,
    append: &mut bool,
) -> Result<(), ImError> {
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => return Ok(()),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(n),
            }) => {
                *data_version = Some(u32::try_from(n).map_err(|_| {
                    ImError::UnexpectedValue("AttributeData.DataVersion exceeds u32")
                })?);
            }
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::List,
            }) => {
                let (p, is_append) = crate::path::attribute_path_from_reader(r)?;
                *path = Some(p);
                *append = is_append;
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: v,
            }) => *value = Some(v),
            Some(Element::ContainerStart {
                tag: Tag::Context(2),
                kind,
            }) => *value = Some(read_container_value(r, kind)?),
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};

    #[test]
    fn read_request_has_attribute_requests_array() {
        let bytes = build_read_request(&[AttributePath {
            endpoint: 0,
            cluster: 0x0031,
            attribute: 0xFFFC, // FeatureMap
        }]);
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::Array
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
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
                value: Value::Uint(0x0031)
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Uint(0xFFFC)
            })
        ));
    }

    #[test]
    fn parses_single_attribute_value() {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports
        {
            w.start_structure(Tag::Anonymous).unwrap(); // AttributeReportIB
            w.start_structure(Tag::Context(1)).unwrap(); // AttributeData
            w.start_list(Tag::Context(1)).unwrap(); // Path (AttributePathIB)
            w.put_uint(Tag::Context(2), 0).unwrap();
            w.put_uint(Tag::Context(3), 0x0031).unwrap();
            w.put_uint(Tag::Context(4), 0xFFFC).unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(2), 0x0001).unwrap(); // Data
            w.end_container().unwrap(); // AttributeData
            w.end_container().unwrap(); // AttributeReportIB
        }
        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();
        let attrs: Vec<_> = report.attributes().collect();
        assert_eq!(attrs.len(), 1);
        let (path, value) = attrs[0];
        assert_eq!(path.endpoint, 0);
        assert_eq!(path.cluster, 0x0031);
        assert_eq!(path.attribute, 0xFFFC);
        assert_eq!(*value, matter_codec::Value::Uint(0x0001));
    }

    #[test]
    fn attribute_status_report_is_surfaced() {
        // A per-path AttributeStatus (here UnsupportedAttribute, 0x86)
        // must be surfaced in `statuses`, not silently dropped — a caller must
        // be able to tell "unsupported" from "omitted".
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports
        w.start_structure(Tag::Anonymous).unwrap(); // AttributeReportIB
        w.start_structure(Tag::Context(0)).unwrap(); // AttributeStatus
        w.start_list(Tag::Context(0)).unwrap(); // AttributePathIB
        w.put_uint(Tag::Context(2), 1).unwrap(); // endpoint
        w.put_uint(Tag::Context(3), 0x0006).unwrap(); // cluster OnOff
        w.put_uint(Tag::Context(4), 0x4242).unwrap(); // (bogus) attribute
        w.end_container().unwrap(); // Path
        w.start_structure(Tag::Context(1)).unwrap(); // StatusIB
        w.put_uint(Tag::Context(0), 0x86).unwrap(); // UnsupportedAttribute
        w.end_container().unwrap(); // StatusIB
        w.end_container().unwrap(); // AttributeStatus
        w.end_container().unwrap(); // AttributeReportIB
        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();
        assert_eq!(report.attributes().count(), 0, "no data items");
        assert_eq!(report.statuses.len(), 1, "the status IB must be surfaced");
        let (path, status) = &report.statuses[0];
        assert_eq!(path.endpoint, 1);
        assert_eq!(path.cluster, 0x0006);
        assert_eq!(path.attribute, 0x4242);
        assert_eq!(*status, crate::status::ImStatus::Failure(0x86));
    }

    #[test]
    fn multi_attribute_report_accumulates_all_entries() {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports

        // First AttributeReportIB: endpoint=0, cluster=0x0028, attribute=0x0000, value=42
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(1)).unwrap(); // AttributeData
        w.start_list(Tag::Context(1)).unwrap(); // Path
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 0x0028).unwrap();
        w.put_uint(Tag::Context(4), 0x0000).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 42).unwrap(); // Data
        w.end_container().unwrap(); // AttributeData
        w.end_container().unwrap(); // AttributeReportIB

        // Second AttributeReportIB: endpoint=1, cluster=0x0006, attribute=0x0000, value=1
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(1)).unwrap(); // AttributeData
        w.start_list(Tag::Context(1)).unwrap(); // Path
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 0x0006).unwrap();
        w.put_uint(Tag::Context(4), 0x0000).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap(); // Data
        w.end_container().unwrap(); // AttributeData
        w.end_container().unwrap(); // AttributeReportIB

        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();
        let attrs: Vec<_> = report.attributes().collect();
        assert_eq!(attrs.len(), 2);

        let (path0, val0) = attrs[0];
        assert_eq!(path0.endpoint, 0);
        assert_eq!(path0.cluster, 0x0028);
        assert_eq!(path0.attribute, 0x0000);
        assert_eq!(*val0, matter_codec::Value::Uint(42));

        let (path1, val1) = attrs[1];
        assert_eq!(path1.endpoint, 1);
        assert_eq!(path1.cluster, 0x0006);
        assert_eq!(path1.attribute, 0x0000);
        assert_eq!(*val1, matter_codec::Value::Uint(1));
    }

    #[test]
    fn out_of_range_endpoint_yields_unexpected_value() {
        use crate::error::ImError;
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports
        w.start_structure(Tag::Anonymous).unwrap(); // AttributeReportIB
        w.start_structure(Tag::Context(1)).unwrap(); // AttributeData
        w.start_list(Tag::Context(1)).unwrap(); // Path
        w.put_uint(Tag::Context(2), 0x0001_0000).unwrap(); // endpoint exceeds u16
        w.put_uint(Tag::Context(3), 0x0031).unwrap();
        w.put_uint(Tag::Context(4), 0xFFFC).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 0x0001).unwrap(); // Data
        w.end_container().unwrap(); // AttributeData
        w.end_container().unwrap(); // AttributeReportIB
        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let result = parse_report_data(&buf);
        assert!(
            matches!(result, Err(ImError::UnexpectedValue(_))),
            "expected UnexpectedValue, got {result:?}"
        );
    }

    #[test]
    fn parses_more_chunked_and_suppress_response_flags() {
        use matter_codec::{Tag, TlvWriter};
        // ReportData with attributeReports[1] array THEN moreChunkedMessages[3]=true.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports (empty)
        w.end_container().unwrap();
        w.put_bool(Tag::Context(3), true).unwrap(); // MoreChunkedMessages
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();
        assert!(
            report.more_chunked_messages,
            "tag 3 must be read after the array"
        );
        assert!(!report.suppress_response);
    }

    #[test]
    fn parses_suppress_response_after_array() {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap();
        w.end_container().unwrap();
        w.put_bool(Tag::Context(4), true).unwrap(); // SuppressResponse
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();
        assert!(report.suppress_response);
        assert!(!report.more_chunked_messages);
    }

    #[test]
    fn captures_data_version_and_append_op() {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports
        w.start_structure(Tag::Anonymous).unwrap(); // AttributeReportIB
        w.start_structure(Tag::Context(1)).unwrap(); // AttributeData
        w.put_uint(Tag::Context(0), 7).unwrap(); // DataVersion
        w.start_list(Tag::Context(1)).unwrap(); // Path
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 0x1d).unwrap();
        w.put_uint(Tag::Context(4), 0x0003).unwrap();
        w.put_null(Tag::Context(5)).unwrap(); // ListIndex = null ⇒ append
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 42).unwrap(); // Data (one element)
        w.end_container().unwrap(); // AttributeData
        w.end_container().unwrap(); // AttributeReportIB
        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();
        assert_eq!(report.items.len(), 1);
        let it = &report.items[0];
        assert_eq!(it.op, ReportOp::Append);
        assert_eq!(it.data_version, Some(7));
        assert_eq!(it.value, Value::Uint(42));
        // Append items are excluded from the flattened convenience view.
        assert_eq!(report.attributes().count(), 0);
    }

    /// The borrowing `attributes()` view yields exactly the `Replace` items'
    /// `(path, value)` pairs — same content the removed owned `attributes` Vec
    /// used to deep-clone — and skips `Append` items.
    #[test]
    fn attributes_view_matches_items_filtered_to_replace() {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports

        // Replace: ep0/0x0028/0x0000 = 42
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(1)).unwrap();
        w.start_list(Tag::Context(1)).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 0x0028).unwrap();
        w.put_uint(Tag::Context(4), 0x0000).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 42).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();

        // Append: ep0/0x001d/0x0003 list element (must be excluded from view).
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(1)).unwrap();
        w.start_list(Tag::Context(1)).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 0x001d).unwrap();
        w.put_uint(Tag::Context(4), 0x0003).unwrap();
        w.put_null(Tag::Context(5)).unwrap(); // ListIndex = null ⇒ append
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 7).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();

        // Replace: ep1/0x0006/0x0000 = true
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(1)).unwrap();
        w.start_list(Tag::Context(1)).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 0x0006).unwrap();
        w.put_uint(Tag::Context(4), 0x0000).unwrap();
        w.end_container().unwrap();
        w.put_bool(Tag::Context(2), true).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();

        w.end_container().unwrap(); // array
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();

        let report = parse_report_data(&buf).unwrap();

        // Independently derive the expected pairs from `items`.
        let expected: Vec<(&AttributePath, &Value)> = report
            .items
            .iter()
            .filter(|it| it.op == ReportOp::Replace)
            .map(|it| (&it.path, &it.value))
            .collect();
        let got: Vec<(&AttributePath, &Value)> = report.attributes().collect();
        assert_eq!(got, expected);

        // Concretely: the two Replace values, in order; the Append is excluded.
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, &Value::Uint(42));
        assert_eq!(got[1].1, &Value::Bool(true));
    }
}

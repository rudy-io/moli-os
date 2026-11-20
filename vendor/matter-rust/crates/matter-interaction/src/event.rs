//! Matter event paths, filters, and reports — `EventPathIB` / `EventFilterIB` /
//! `EventDataIB` / `EventReportIB` (Matter §10.6 / Appendix A).
//!
//! Distinct from the attribute path/report code: `EventPathIB` uses tag base 0
//! (Node), not 2. Wire shapes are pinned by the matter.js byte-parity fixtures
//! (`test-vectors/commissioning/im/{read/events_basic_information,report/report_data_event}.json`)
//! and cross-checked against connectedhomeip `src/app/MessageDef/Event*IB.h`:
//! `EventPathIB` is a TLV **list**, `EventFilterIB` is a TLV **structure**.

#![forbid(unsafe_code)]

use crate::error::ImError;
use crate::{read_container_value, skip_container};
use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

/// A read/subscribe event path with optional (wildcard) components. A `None`
/// field is omitted from the encoded `EventPathIB`, which the IM interprets as a
/// wildcard. `node` is normally `None` for a controller addressing the connected
/// node; `is_urgent` requests urgent reporting on a subscription.
///
/// `#[non_exhaustive]`: an event path may gain optional spec components; marking
/// it keeps such additions non-breaking. Build via [`EventPath::concrete`] /
/// [`EventPath::cluster`].
#[derive(Copy, Clone, Debug, Eq, Default, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct EventPath {
    /// Node, or `None` (the connected node / wildcard).
    pub node: Option<u64>,
    /// Endpoint, or `None` for all endpoints.
    pub endpoint: Option<u16>,
    /// Cluster, or `None` for all clusters.
    pub cluster: Option<u32>,
    /// Event, or `None` for all events of the cluster.
    pub event: Option<u32>,
    /// Urgent-reporting hint (subscriptions); omitted when `None`.
    pub is_urgent: Option<bool>,
}

impl EventPath {
    /// A concrete `(endpoint, cluster, event)` path (no node, no urgent flag).
    #[must_use]
    pub const fn concrete(endpoint: u16, cluster: u32, event: u32) -> Self {
        Self {
            node: None,
            endpoint: Some(endpoint),
            cluster: Some(cluster),
            event: Some(event),
            is_urgent: None,
        }
    }

    /// All events of `cluster` on `endpoint`.
    #[must_use]
    pub const fn cluster(endpoint: u16, cluster: u32) -> Self {
        Self {
            node: None,
            endpoint: Some(endpoint),
            cluster: Some(cluster),
            event: None,
            is_urgent: None,
        }
    }

    /// Encode this path as an anonymous-tagged `EventPathIB` **list** element.
    ///
    /// Tags: Node 0, Endpoint 1, Cluster 2, Event 3, `IsUrgent` 4 (Matter
    /// Appendix A). Omitted (`None`) fields are wildcards.
    pub(crate) fn write(&self, w: &mut TlvWriter<'_>) -> Result<(), matter_codec::Error> {
        w.start_list(Tag::Anonymous)?;
        if let Some(n) = self.node {
            w.put_uint(Tag::Context(0), n)?;
        }
        if let Some(e) = self.endpoint {
            w.put_uint(Tag::Context(1), u64::from(e))?;
        }
        if let Some(c) = self.cluster {
            w.put_uint(Tag::Context(2), u64::from(c))?;
        }
        if let Some(ev) = self.event {
            w.put_uint(Tag::Context(3), u64::from(ev))?;
        }
        if let Some(u) = self.is_urgent {
            w.put_bool(Tag::Context(4), u)?;
        }
        w.end_container()
    }
}

/// An `EventFilterIB`: only events with `event_number >= event_min` are reported
/// (used to resume after the last seen event). `node` is omitted when `None`.
///
/// `#[non_exhaustive]`: build via [`EventFilter::from_event_min`]; marking it
/// keeps future optional fields non-breaking.
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct EventFilter {
    /// Node scope, or `None`.
    pub node: Option<u64>,
    /// Minimum event number to report (inclusive).
    pub event_min: u64,
}

impl EventFilter {
    /// A filter reporting events with number `>= event_min`.
    #[must_use]
    pub const fn from_event_min(event_min: u64) -> Self {
        Self {
            node: None,
            event_min,
        }
    }

    /// Encode this filter as an anonymous-tagged `EventFilterIB` element.
    ///
    /// NB: `EventFilterIB` is a TLV **structure** (`0x15`), unlike `EventPathIB`
    /// which is a **list** (`0x17`). Confirmed by the captured matter.js bytes
    /// (`events_basic_information.json`): array[2] holds a struct, not a list.
    pub(crate) fn write(&self, w: &mut TlvWriter<'_>) -> Result<(), matter_codec::Error> {
        w.start_structure(Tag::Anonymous)?;
        if let Some(n) = self.node {
            w.put_uint(Tag::Context(0), n)?;
        }
        w.put_uint(Tag::Context(1), self.event_min)?;
        w.end_container()
    }
}

/// Event priority (Matter §14.3). Unknown values are preserved verbatim so a
/// newer-revision device does not break decoding.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventPriority {
    /// Debug priority (0).
    Debug,
    /// Info priority (1).
    Info,
    /// Critical priority (2).
    Critical,
    /// Any other (future) priority value.
    Unknown(u8),
}

impl EventPriority {
    #[must_use]
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Debug,
            1 => Self::Info,
            2 => Self::Critical,
            other => Self::Unknown(other),
        }
    }
}

/// The timestamp carried by an `EventDataIB`. A report carries exactly one of
/// these (absolute epoch/system, or a delta against the prior event in a
/// subscription stream); [`None`](EventTimestamp::None) if the device omitted all
/// four (tolerated rather than rejected).
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum EventTimestamp {
    /// Milliseconds since the Unix epoch (`EpochTimestamp`, tag 3).
    Epoch(u64),
    /// Milliseconds since boot (`SystemTimestamp`, tag 4).
    System(u64),
    /// Delta-epoch against the prior event in the stream (tag 5).
    DeltaEpoch(u64),
    /// Delta-system against the prior event in the stream (tag 6).
    DeltaSystem(u64),
    /// No timestamp present.
    None,
}

/// One `EventDataIB` (a real event with data).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct EventReportItem {
    /// The event's `(node?, endpoint, cluster, event)` path.
    pub path: EventPath,
    /// Monotonic event number (scoped to priority).
    pub event_number: u64,
    /// Event priority.
    pub priority: EventPriority,
    /// Event timestamp.
    pub timestamp: EventTimestamp,
    /// The event payload (cluster-defined TLV; decode with `matter-clusters`).
    pub value: Value,
}

/// One `EventReportIB`: a real event ([`Data`](EventReport::Data)) or a per-path
/// error ([`Status`](EventReport::Status)).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum EventReport {
    /// An `EventDataIB` carrying a real event.
    Data(EventReportItem),
    /// An `EventStatusIB` carrying a status for a requested event path.
    Status {
        /// The event path the status refers to.
        path: EventPath,
        /// The IM status code (`StatusIB.Status`).
        status: u8,
    },
}

/// Consume an `EventPathIB` list body (reader positioned just after the
/// list's `ContainerStart`) into an [`EventPath`] (tags 0–4), without
/// materialising the members. Field mapping is lenient, as before:
/// wrong-typed or out-of-range members leave the field `None`.
fn event_path_from_reader(r: &mut TlvReader<'_>) -> Result<EventPath, ImError> {
    let mut p = EventPath::default();
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => return Ok(p),
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(n),
            }) => p.node = Some(n),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(n),
            }) => p.endpoint = u16::try_from(n).ok(),
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(n),
            }) => p.cluster = u32::try_from(n).ok(),
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Uint(n),
            }) => p.event = u32::try_from(n).ok(),
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Bool(b),
            }) => p.is_urgent = Some(b),
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
}

/// Parse the body of one `EventReportIB` (reader positioned just after its struct
/// start). Returns the report, or `None` for an empty IB.
///
/// # Errors
///
/// Returns [`ImError`] if the input ends mid-container, or an `EventData` is
/// missing its `Data` member.
fn parse_event_report_ib(r: &mut TlvReader<'_>) -> Result<Option<EventReport>, ImError> {
    let mut out: Option<EventReport> = None;
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            // EventData [1]
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::Structure,
            }) => out = Some(EventReport::Data(parse_event_data(r)?)),
            // EventStatus [0]
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::Structure,
            }) => out = Some(parse_event_status(r)?),
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
    Ok(out)
}

/// Parse an `EventDataIB` body (reader just after the struct start at ctx 1).
///
/// # Errors
///
/// Returns [`ImError::MissingField`] if `Data` (tag 7) is absent, or propagates a
/// codec error.
fn parse_event_data(r: &mut TlvReader<'_>) -> Result<EventReportItem, ImError> {
    let mut path = EventPath::default();
    let mut event_number = 0u64;
    let mut priority = EventPriority::Unknown(0xFF);
    let mut timestamp = EventTimestamp::None;
    let mut value: Option<Value> = None;
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            // Path [0] — EventPathIB list.
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::List,
            }) => {
                path = event_path_from_reader(r)?;
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(n),
            }) => event_number = n,
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(n),
            }) => priority = EventPriority::from_u8(u8::try_from(n).unwrap_or(0xFF)),
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Uint(n),
            }) => timestamp = EventTimestamp::Epoch(n),
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Uint(n),
            }) => timestamp = EventTimestamp::System(n),
            Some(Element::Scalar {
                tag: Tag::Context(5),
                value: Value::Uint(n),
            }) => timestamp = EventTimestamp::DeltaEpoch(n),
            Some(Element::Scalar {
                tag: Tag::Context(6),
                value: Value::Uint(n),
            }) => timestamp = EventTimestamp::DeltaSystem(n),
            // Data [7] — scalar or container.
            Some(Element::Scalar {
                tag: Tag::Context(7),
                value: v,
            }) => value = Some(v),
            Some(Element::ContainerStart {
                tag: Tag::Context(7),
                kind,
            }) => value = Some(read_container_value(r, kind)?),
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
    Ok(EventReportItem {
        path,
        event_number,
        priority,
        timestamp,
        value: value.ok_or(ImError::MissingField("EventData.Data"))?,
    })
}

/// Parse an `EventStatusIB` body (reader just after the struct start at ctx 0).
///
/// # Errors
///
/// Propagates a codec error if the input ends mid-container.
fn parse_event_status(r: &mut TlvReader<'_>) -> Result<EventReport, ImError> {
    let mut path = EventPath::default();
    let mut status = 0u8;
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            // Path [0] — EventPathIB list.
            Some(Element::ContainerStart {
                tag: Tag::Context(0),
                kind: ContainerKind::List,
            }) => {
                path = event_path_from_reader(r)?;
            }
            // Status [1] — StatusIB struct { 0: Status u8, 1: ClusterStatus? }.
            Some(Element::ContainerStart {
                tag: Tag::Context(1),
                kind: ContainerKind::Structure,
            }) => loop {
                match r.next()? {
                    None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
                    Some(Element::ContainerEnd) => break,
                    Some(Element::Scalar {
                        tag: Tag::Context(0),
                        value: Value::Uint(n),
                    }) => status = u8::try_from(n).unwrap_or(0),
                    Some(Element::ContainerStart { .. }) => skip_container(r)?,
                    Some(_) => {}
                }
            },
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
    Ok(EventReport::Status { path, status })
}

/// Parse a `DataReport`'s `eventReports[2]` array body (reader positioned just
/// after the array start at ctx 2), pushing one [`EventReport`] per IB.
///
/// # Errors
///
/// Propagates any [`ImError`] from parsing an individual `EventReportIB`.
pub(crate) fn parse_event_reports(
    r: &mut TlvReader<'_>,
    out: &mut Vec<EventReport>,
) -> Result<(), ImError> {
    loop {
        match r.next()? {
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => return Ok(()),
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            }) => {
                if let Some(rep) = parse_event_report_ib(r)? {
                    out.push(rep);
                }
            }
            Some(Element::ContainerStart { .. }) => skip_container(r)?,
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};

    #[test]
    fn event_path_encodes_as_list_with_tags_1_2_3() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        EventPath::concrete(0, 0x28, 0x00).write(&mut w).unwrap();
        let mut r = TlvReader::new(&buf);
        // EventPathIB is a LIST (not a struct).
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::List
            })
        ));
        // Endpoint=tag 1, Cluster=tag 2, Event=tag 3 (NOT 2/3/4 like AttributePath).
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(0)
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(0x28)
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Uint(0x00)
            })
        ));
    }

    #[test]
    fn event_filter_encodes_as_struct() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        EventFilter::from_event_min(0).write(&mut w).unwrap();
        let mut r = TlvReader::new(&buf);
        // EventFilterIB is a STRUCTURE (not a list) — vectors-confirmed.
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure
            })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(0)
            })
        ));
    }

    #[test]
    fn parses_event_data_ib() {
        // EventReportIB { EventData[1] { Path[0](list){1:ep,2:cl,3:ev}, 1:num,
        // 2:prio, 3:epoch, 7:data } }
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap(); // EventReportIB
        w.start_structure(Tag::Context(1)).unwrap(); // EventData
        w.start_list(Tag::Context(0)).unwrap(); // Path (EventPathIB list)
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 0x28).unwrap();
        w.put_uint(Tag::Context(3), 0x00).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap(); // EventNumber
        w.put_uint(Tag::Context(2), 2).unwrap(); // Priority = Critical
        w.put_uint(Tag::Context(3), 0).unwrap(); // EpochTimestamp
        w.put_uint(Tag::Context(7), 7).unwrap(); // Data (scalar for the test)
        w.end_container().unwrap();
        w.end_container().unwrap();

        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        let rep = parse_event_report_ib(&mut r).unwrap().unwrap();
        match rep {
            EventReport::Data(it) => {
                assert_eq!(it.path.endpoint, Some(0));
                assert_eq!(it.path.cluster, Some(0x28));
                assert_eq!(it.path.event, Some(0x00));
                assert_eq!(it.event_number, 1);
                assert_eq!(it.priority, EventPriority::Critical);
                assert_eq!(it.timestamp, EventTimestamp::Epoch(0));
                assert_eq!(it.value, Value::Uint(7));
            }
            EventReport::Status { .. } => panic!("expected Data, got Status"),
        }
    }

    #[test]
    fn parses_event_status_ib() {
        // EventReportIB { EventStatus[0] { Path[0](list){1:ep,2:cl,3:ev},
        // Status[1](struct){0:status} } }
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap(); // EventReportIB
        w.start_structure(Tag::Context(0)).unwrap(); // EventStatus
        w.start_list(Tag::Context(0)).unwrap(); // Path
        w.put_uint(Tag::Context(1), 1).unwrap();
        w.put_uint(Tag::Context(2), 0x28).unwrap();
        w.put_uint(Tag::Context(3), 0x02).unwrap();
        w.end_container().unwrap();
        w.start_structure(Tag::Context(1)).unwrap(); // Status (StatusIB)
        w.put_uint(Tag::Context(0), 0x86).unwrap(); // UnsupportedEvent (example)
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();

        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        let rep = parse_event_report_ib(&mut r).unwrap().unwrap();
        match rep {
            EventReport::Status { path, status } => {
                assert_eq!(path.endpoint, Some(1));
                assert_eq!(path.event, Some(0x02));
                assert_eq!(status, 0x86);
            }
            EventReport::Data(_) => panic!("expected Status, got Data"),
        }
    }
}

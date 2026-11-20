//! matter.js byte-parity tests for the `im` module.
//!
//! Fixtures live under `test-vectors/commissioning/im/<kind>/*.json`.
//! Each fixture provides the inputs and the matter.js-captured expected
//! IM message bytes (base64). Tests SKIP (not fail) when fixtures are
//! absent, matching the established M6.x pattern — fixtures are captured
//! by `cargo xtask capture-im` (xtask/scripts/capture-im).

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::redundant_closure_for_method_calls,
    clippy::single_match_else,
    clippy::doc_markdown
)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(unreachable_pub)]

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
// Deep submodule paths used intentionally to confirm the submodules are directly accessible (flat re-exports also exist at the crate root).
use matter_interaction::event::{EventFilter, EventPath, EventReport};
use matter_interaction::invoke::build_invoke_request;
use matter_interaction::path::AttributePath;
use matter_interaction::read::{build_read_request_full, build_read_request_paths};
use matter_interaction::subscription::{
    build_status_response, build_subscribe_request, parse_subscribe_response, SubscribeRequest,
};
use matter_interaction::CommandPath;
use matter_interaction::{parse_report_data, ReadPath};
use serde::Deserialize;
use std::{fs, path::PathBuf};

fn fixtures_root() -> PathBuf {
    let mut p: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    p.push("..");
    p.push("..");
    p.push("test-vectors");
    p.push("commissioning");
    p.push("im");
    p
}

fn list_jsons(sub: &str) -> Vec<PathBuf> {
    let dir = fixtures_root().join(sub);
    if !dir.exists() {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    out.sort();
    out
}

#[derive(Deserialize)]
struct InvokeFixture {
    endpoint: u16,
    cluster: u32,
    command: u32,
    command_fields_b64: String,
    expected_message_b64: String,
}

#[derive(Deserialize)]
struct ReadFixture {
    paths: Vec<ReadPathFixture>,
    expected_message_b64: String,
}

#[derive(Deserialize)]
struct ReadPathFixture {
    endpoint: Option<u16>,
    cluster: Option<u32>,
    attribute: Option<u32>,
}

#[test]
fn invoke_request_matches_matter_js() {
    let paths = list_jsons("invoke");
    if paths.is_empty() {
        eprintln!("skipping: no invoke fixtures (run `cargo xtask capture-im`)");
        return;
    }
    for path in paths {
        let bytes = fs::read(&path).unwrap();
        let f: InvokeFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("{path:?}: not an invoke fixture — skipping");
                continue;
            }
        };
        let fields = B64.decode(&f.command_fields_b64).unwrap();
        let ours = build_invoke_request(
            CommandPath {
                endpoint: f.endpoint,
                cluster: f.cluster,
                command: f.command,
            },
            &fields,
        );
        let theirs = B64.decode(&f.expected_message_b64).unwrap();
        assert_eq!(ours, theirs, "InvokeRequest mismatch for {path:?}");
    }
}

#[test]
fn read_request_matches_matter_js() {
    let paths = list_jsons("read");
    if paths.is_empty() {
        eprintln!("skipping: no read fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in paths {
        let bytes = fs::read(&path).unwrap();
        let f: ReadFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("{path:?}: not a read fixture — skipping");
                continue;
            }
        };
        let our_paths: Vec<matter_interaction::ReadPath> = f
            .paths
            .iter()
            .map(|p| matter_interaction::ReadPath::new(p.endpoint, p.cluster, p.attribute))
            .collect();
        let ours = build_read_request_paths(&our_paths);
        let theirs = B64.decode(&f.expected_message_b64).unwrap();
        assert_eq!(ours, theirs, "ReadRequest mismatch for {path:?}");
        asserted += 1;
    }
    assert!(asserted > 0, "no read-request fixtures parsed");
}

#[derive(Deserialize)]
struct EventReadFixture {
    event_paths: Vec<EvPathFixture>,
    event_filters: Vec<EvFilterFixture>,
    expected_message_b64: String,
}

#[derive(Deserialize)]
struct EvPathFixture {
    endpoint: u16,
    cluster: u32,
    event: u32,
}

#[derive(Deserialize)]
struct EvFilterFixture {
    event_min: u64,
}

#[test]
fn read_request_with_event_path_matches_matter_js() {
    let path = fixtures_root()
        .join("read")
        .join("events_basic_information.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no event read fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: EventReadFixture = serde_json::from_str(&raw).unwrap();
    let eps: Vec<EventPath> = f
        .event_paths
        .iter()
        .map(|p| EventPath::concrete(p.endpoint, p.cluster, p.event))
        .collect();
    let efs: Vec<EventFilter> = f
        .event_filters
        .iter()
        .map(|x| EventFilter::from_event_min(x.event_min))
        .collect();
    let ours = build_read_request_full(&[], &eps, &efs);
    let theirs = B64.decode(&f.expected_message_b64).unwrap();
    assert_eq!(
        ours, theirs,
        "event ReadRequest must match matter.js byte-for-byte"
    );
}

#[derive(Deserialize)]
struct EventReportFixture {
    event: EvReportExpect,
    response_message_b64: String,
}

#[derive(Deserialize)]
struct EvReportExpect {
    endpoint: u16,
    cluster: u32,
    event: u32,
    event_number: u64,
    #[allow(dead_code)]
    priority: u8,
}

#[test]
fn parses_event_report_from_matter_js() {
    let path = fixtures_root()
        .join("report")
        .join("report_data_event.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no event report fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: EventReportFixture = serde_json::from_str(&raw).unwrap();
    let bytes = B64.decode(&f.response_message_b64).unwrap();
    let report = parse_report_data(&bytes).unwrap();
    assert_eq!(report.events().len(), 1, "expected one event report");
    match &report.events()[0] {
        EventReport::Data(it) => {
            assert_eq!(it.path.endpoint, Some(f.event.endpoint));
            assert_eq!(it.path.cluster, Some(f.event.cluster));
            assert_eq!(it.path.event, Some(f.event.event));
            assert_eq!(it.event_number, f.event.event_number);
        }
        EventReport::Status { .. } => panic!("expected Data, got Status"),
        _ => panic!("unexpected EventReport variant"),
    }
}

#[derive(Deserialize)]
struct WriteFixture {
    writes: Vec<WriteEntryFixture>,
    expected_message_b64: String,
}

#[derive(Deserialize)]
struct WriteEntryFixture {
    endpoint: u16,
    cluster: u32,
    attribute: u32,
    value_tlv_b64: String,
}

#[derive(Deserialize)]
struct WriteResponseFixture {
    response_message_b64: String,
    expected: Vec<WriteStatusFixture>,
}

#[derive(Deserialize)]
struct WriteStatusFixture {
    endpoint: u16,
    cluster: u32,
    attribute: u32,
    status: u8,
}

#[test]
fn write_request_matches_matter_js() {
    use matter_interaction::write::{build_write_request, AttributeWriteRequest};

    let paths = list_jsons("write");
    if paths.is_empty() {
        eprintln!("skipping: no write fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in paths {
        let bytes = fs::read(&path).unwrap();
        let f: WriteFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue, // response fixtures live in the same dir
        };
        let writes: Vec<AttributeWriteRequest> = f
            .writes
            .iter()
            .map(|w| AttributeWriteRequest {
                path: AttributePath {
                    endpoint: w.endpoint,
                    cluster: w.cluster,
                    attribute: w.attribute,
                },
                value_tlv: B64.decode(&w.value_tlv_b64).unwrap(),
            })
            .collect();
        let ours = build_write_request(&writes);
        let theirs = B64.decode(&f.expected_message_b64).unwrap();
        assert_eq!(
            ours,
            theirs,
            "WriteRequest mismatch for {path:?}\n  ours:   {}\n  theirs: {}",
            hex::encode(&ours),
            hex::encode(&theirs)
        );
        asserted += 1;
    }
    assert!(asserted > 0, "no write-request fixtures parsed");
}

#[test]
fn write_response_parses_matter_js() {
    use matter_interaction::write::parse_write_response;
    use matter_interaction::ImStatus;

    let paths = list_jsons("write");
    if paths.is_empty() {
        eprintln!("skipping: no write fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in paths {
        let bytes = fs::read(&path).unwrap();
        let f: WriteResponseFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue, // request fixtures live in the same dir
        };
        let msg = B64.decode(&f.response_message_b64).unwrap();
        let statuses = parse_write_response(&msg).unwrap();
        assert_eq!(
            statuses.len(),
            f.expected.len(),
            "count mismatch for {path:?}"
        );
        for (got, want) in statuses.iter().zip(&f.expected) {
            assert_eq!(got.0.endpoint, want.endpoint);
            assert_eq!(got.0.cluster, want.cluster);
            assert_eq!(got.0.attribute, want.attribute);
            assert_eq!(got.1, ImStatus::from_u8(want.status));
        }
        asserted += 1;
    }
    assert!(asserted > 0, "no write-response fixtures parsed");
}

// ---------------------------------------------------------------------------
// Subscribe byte-parity fixtures
// ---------------------------------------------------------------------------

/// Fixture for a `SubscribeRequest` message — contains the input parameters
/// and the matter.js-encoded `expected_message_b64`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)] // so the subscribe_with_events fixture (extra event_* fields) is
                              // skipped here and handled by subscribe_request_with_events_matches_matter_js.
struct SubscribeRequestFixture {
    keep_subscriptions: bool,
    min_interval_floor: u16,
    max_interval_ceiling: u16,
    paths: Vec<SubscribePathFixture>,
    expected_message_b64: String,
}

#[derive(Deserialize)]
struct SubscribePathFixture {
    endpoint: Option<u16>,
    cluster: Option<u32>,
    attribute: Option<u32>,
}

/// Fixture for a `SubscribeResponse` message — contains the expected parse
/// results and the matter.js-encoded `response_message_b64`.
#[derive(Deserialize)]
struct SubscribeResponseFixture {
    subscription_id: u32,
    max_interval: u16,
    response_message_b64: String,
}

/// Fixture for a `StatusResponse` message — contains the status code and
/// the matter.js-encoded `expected_message_b64`.
#[derive(Deserialize)]
struct StatusResponseFixture {
    status: u8,
    expected_message_b64: String,
}

/// Fixture for a subscribed `ReportData` — contains the expected
/// `subscription_id`, attribute list, and the matter.js-encoded
/// `response_message_b64`.
#[derive(Deserialize)]
struct ReportDataSubscribedFixture {
    subscription_id: u32,
    attributes: Vec<ReportDataAttributeFixture>,
    response_message_b64: String,
}

#[derive(Deserialize)]
struct ReportDataAttributeFixture {
    endpoint: u16,
    cluster: u32,
    attribute: u32,
    #[serde(rename = "bool")]
    bool_value: Option<bool>,
}

/// `build_subscribe_request` matches matter.js byte-for-byte.
#[test]
fn subscribe_request_matches_matter_js() {
    let paths = list_jsons("subscribe");
    if paths.is_empty() {
        eprintln!("skipping: no subscribe fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in &paths {
        let bytes = fs::read(path).unwrap();
        let f: SubscribeRequestFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue, // not a subscribe-request fixture
        };
        let our_paths: Vec<ReadPath> = f
            .paths
            .iter()
            .map(|p| ReadPath::new(p.endpoint, p.cluster, p.attribute))
            .collect();
        let req = SubscribeRequest {
            keep_subscriptions: f.keep_subscriptions,
            min_interval_floor: f.min_interval_floor,
            max_interval_ceiling: f.max_interval_ceiling,
            paths: our_paths,
            event_paths: vec![],
            event_filters: vec![],
        };
        let ours = build_subscribe_request(&req);
        let theirs = B64.decode(&f.expected_message_b64).unwrap();
        assert_eq!(
            ours,
            theirs,
            "SubscribeRequest mismatch for {path:?}\n  ours:   {}\n  theirs: {}",
            hex::encode(&ours),
            hex::encode(&theirs)
        );
        asserted += 1;
    }
    assert!(asserted > 0, "no subscribe-request fixtures parsed");
}

/// Fixture for a `SubscribeRequest` carrying attribute AND event paths/filters.
#[derive(Deserialize)]
struct SubEventsFixture {
    keep_subscriptions: bool,
    min_interval_floor: u16,
    max_interval_ceiling: u16,
    paths: Vec<SubscribePathFixture>,
    event_paths: Vec<EvPathFixture>,
    event_filters: Vec<EvFilterFixture>,
    expected_message_b64: String,
}

/// `build_subscribe_request` with event paths/filters matches matter.js.
#[test]
fn subscribe_request_with_events_matches_matter_js() {
    let path = fixtures_root()
        .join("subscribe")
        .join("subscribe_with_events.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no subscribe-with-events fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: SubEventsFixture = serde_json::from_str(&raw).unwrap();
    let req = SubscribeRequest {
        keep_subscriptions: f.keep_subscriptions,
        min_interval_floor: f.min_interval_floor,
        max_interval_ceiling: f.max_interval_ceiling,
        paths: f
            .paths
            .iter()
            .map(|p| ReadPath::new(p.endpoint, p.cluster, p.attribute))
            .collect(),
        event_paths: f
            .event_paths
            .iter()
            .map(|p| EventPath::concrete(p.endpoint, p.cluster, p.event))
            .collect(),
        event_filters: f
            .event_filters
            .iter()
            .map(|x| EventFilter::from_event_min(x.event_min))
            .collect(),
    };
    let ours = build_subscribe_request(&req);
    let theirs = B64.decode(&f.expected_message_b64).unwrap();
    assert_eq!(
        ours, theirs,
        "subscribe-with-events must match matter.js byte-for-byte"
    );
}

/// `parse_subscribe_response` parses the matter.js-captured bytes correctly.
#[test]
fn subscribe_response_parses_matter_js() {
    let paths = list_jsons("subscribe");
    if paths.is_empty() {
        eprintln!("skipping: no subscribe fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in &paths {
        let bytes = fs::read(path).unwrap();
        let f: SubscribeResponseFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue, // not a subscribe-response fixture
        };
        let msg = B64.decode(&f.response_message_b64).unwrap();
        let resp = parse_subscribe_response(&msg).unwrap();
        assert_eq!(
            resp.subscription_id, f.subscription_id,
            "subscriptionId mismatch for {path:?}"
        );
        assert_eq!(
            resp.max_interval, f.max_interval,
            "maxInterval mismatch for {path:?}"
        );
        asserted += 1;
    }
    assert!(asserted > 0, "no subscribe-response fixtures parsed");
}

/// `build_status_response` matches matter.js byte-for-byte.
#[test]
fn status_response_matches_matter_js() {
    let paths = list_jsons("subscribe");
    if paths.is_empty() {
        eprintln!("skipping: no subscribe fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in &paths {
        let bytes = fs::read(path).unwrap();
        let f: StatusResponseFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue, // not a status-response fixture
        };
        let ours = build_status_response(f.status);
        let theirs = B64.decode(&f.expected_message_b64).unwrap();
        assert_eq!(
            ours,
            theirs,
            "StatusResponse mismatch for {path:?}\n  ours:   {}\n  theirs: {}",
            hex::encode(&ours),
            hex::encode(&theirs)
        );
        asserted += 1;
    }
    assert!(asserted > 0, "no status-response fixtures parsed");
}

/// `parse_report_data` surfaces `subscription_id` from a subscribed report.
#[test]
fn report_data_subscribed_parses_matter_js() {
    let paths = list_jsons("subscribe");
    if paths.is_empty() {
        eprintln!("skipping: no subscribe fixtures (run `cargo xtask capture-im`)");
        return;
    }
    let mut asserted = 0;
    for path in &paths {
        let bytes = fs::read(path).unwrap();
        let f: ReportDataSubscribedFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue, // not a report-data-subscribed fixture
        };
        let msg = B64.decode(&f.response_message_b64).unwrap();
        let report = parse_report_data(&msg).unwrap();
        assert_eq!(
            report.subscription_id,
            Some(f.subscription_id),
            "subscription_id mismatch for {path:?}"
        );
        let report_attrs: Vec<_> = report.attributes().collect();
        assert_eq!(
            report_attrs.len(),
            f.attributes.len(),
            "attribute count mismatch for {path:?}"
        );
        for (got, want) in report_attrs.iter().zip(&f.attributes) {
            assert_eq!(got.0.endpoint, want.endpoint, "endpoint mismatch");
            assert_eq!(got.0.cluster, want.cluster, "cluster mismatch");
            assert_eq!(got.0.attribute, want.attribute, "attribute mismatch");
            if let Some(expected_bool) = want.bool_value {
                assert_eq!(
                    *got.1,
                    matter_codec::Value::Bool(expected_bool),
                    "value mismatch for {path:?}"
                );
            }
        }
        asserted += 1;
    }
    assert!(asserted > 0, "no report-data-subscribed fixtures parsed");
}

// --- M9-B3 timed interactions ---

#[derive(Deserialize)]
struct TimedRequestFixture {
    timeout_ms: u16,
    expected_message_b64: String,
}

#[test]
fn timed_request_matches_matter_js() {
    let path = fixtures_root().join("timed").join("timed_request.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no timed_request fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: TimedRequestFixture = serde_json::from_str(&raw).unwrap();
    let ours = matter_interaction::build_timed_request(f.timeout_ms);
    let theirs = B64.decode(&f.expected_message_b64).unwrap();
    assert_eq!(
        ours, theirs,
        "TimedRequest must match matter.js byte-for-byte"
    );
}

#[test]
fn write_request_timed_matches_matter_js() {
    use matter_interaction::write::{build_write_request_timed, AttributeWriteRequest};
    let path = fixtures_root()
        .join("timed")
        .join("write_request_timed.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no write_request_timed fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: WriteFixture = serde_json::from_str(&raw).unwrap();
    let writes: Vec<AttributeWriteRequest> = f
        .writes
        .iter()
        .map(|w| AttributeWriteRequest {
            path: AttributePath {
                endpoint: w.endpoint,
                cluster: w.cluster,
                attribute: w.attribute,
            },
            value_tlv: B64.decode(&w.value_tlv_b64).unwrap(),
        })
        .collect();
    let ours = build_write_request_timed(&writes);
    let theirs = B64.decode(&f.expected_message_b64).unwrap();
    assert_eq!(
        ours, theirs,
        "timed WriteRequest must match matter.js byte-for-byte"
    );
}

#[test]
fn invoke_request_timed_matches_matter_js() {
    let path = fixtures_root()
        .join("timed")
        .join("invoke_request_timed.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no invoke_request_timed fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: InvokeFixture = serde_json::from_str(&raw).unwrap();
    let fields = B64.decode(&f.command_fields_b64).unwrap();
    let ours = matter_interaction::build_invoke_request_timed(
        CommandPath {
            endpoint: f.endpoint,
            cluster: f.cluster,
            command: f.command,
        },
        &fields,
    );
    let theirs = B64.decode(&f.expected_message_b64).unwrap();
    assert_eq!(
        ours, theirs,
        "timed InvokeRequest must match matter.js byte-for-byte"
    );
}

#[derive(Deserialize)]
struct StatusResponseParseFixture {
    status: u8,
    response_message_b64: String,
}

#[test]
fn status_needs_timed_parses() {
    let path = fixtures_root()
        .join("timed")
        .join("status_needs_timed.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no status_needs_timed fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: StatusResponseParseFixture = serde_json::from_str(&raw).unwrap();
    let bytes = B64.decode(&f.response_message_b64).unwrap();
    assert_eq!(
        matter_interaction::parse_status_response(&bytes).unwrap(),
        Some(f.status)
    );
}

// --- M9-B5 multi-command invoke ---

#[derive(Deserialize)]
struct BatchRequestFixture {
    commands: Vec<BatchCmdFixture>,
    expected_message_b64: String,
}

#[derive(Deserialize)]
struct BatchCmdFixture {
    endpoint: u16,
    cluster: u32,
    command: u32,
    #[allow(dead_code)]
    command_ref: u16,
}

#[test]
fn batch_invoke_request_matches_matter_js() {
    let path = fixtures_root().join("invoke").join("batch_request.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no batch invoke fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: BatchRequestFixture = serde_json::from_str(&raw).unwrap();
    // Empty anonymous struct as command fields (the fixture uses no fields).
    let empty = [0x15u8, 0x18];
    let cmds: Vec<(CommandPath, &[u8])> = f
        .commands
        .iter()
        .map(|c| {
            (
                CommandPath {
                    endpoint: c.endpoint,
                    cluster: c.cluster,
                    command: c.command,
                },
                &empty[..],
            )
        })
        .collect();
    let ours = matter_interaction::build_invoke_request_batch(&cmds);
    let theirs = B64.decode(&f.expected_message_b64).unwrap();
    assert_eq!(
        ours, theirs,
        "batch InvokeRequest must match matter.js byte-for-byte"
    );
}

#[derive(Deserialize)]
struct BatchResponseFixture {
    expected: Vec<BatchExpectFixture>,
    response_message_b64: String,
}

#[derive(Deserialize)]
struct BatchExpectFixture {
    command_ref: u16,
    status: u8,
}

#[test]
fn batch_invoke_response_parses_matter_js() {
    use matter_interaction::{parse_invoke_response_batch, ImStatus, InvokeResponse};
    let path = fixtures_root().join("invoke").join("batch_response.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        eprintln!("skipping: no batch invoke response fixture (run `cargo xtask capture-im`)");
        return;
    };
    let f: BatchResponseFixture = serde_json::from_str(&raw).unwrap();
    let bytes = B64.decode(&f.response_message_b64).unwrap();
    let entries = parse_invoke_response_batch(&bytes).unwrap();
    assert_eq!(entries.len(), f.expected.len());
    for (entry, exp) in entries.iter().zip(f.expected.iter()) {
        assert_eq!(entry.command_ref, Some(exp.command_ref));
        match &entry.response {
            InvokeResponse::Status(s) => assert_eq!(*s, ImStatus::from_u8(exp.status)),
            InvokeResponse::Command { .. } => panic!("expected Status entries"),
        }
    }
}

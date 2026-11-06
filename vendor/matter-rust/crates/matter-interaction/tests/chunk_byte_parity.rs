//! Byte-parity + reassembly for chunked `ReportData` fixtures captured from
//! matter.js 0.16.11 (`xtask/scripts/capture-im`, subdir `report/`). Skips
//! when no fixtures are present (capture's `node_modules` is gitignored).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use matter_codec::Value;
use matter_interaction::{parse_report_data, ReportAccumulator};
use serde::Deserialize;

fn report_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-vectors/commissioning/im/report")
}

#[derive(Deserialize)]
struct ExpectedPath {
    endpoint: u16,
    cluster: u32,
    attribute: u32,
}

#[derive(Deserialize)]
struct MessageChunkFixture {
    chunks_b64: Vec<String>,
    expected: Vec<ExpectedPath>,
}

#[derive(Deserialize)]
struct ListChunkFixture {
    chunks_b64: Vec<String>,
    expected_path: ExpectedPath,
    expected_list: Vec<u32>,
}

#[test]
fn message_chunked_reassembles_all_attributes() {
    let path = report_dir().join("report_data_chunked_message.json");
    if !path.exists() {
        eprintln!("skipping: no chunk fixtures (run `node xtask/scripts/capture-im/index.js`)");
        return;
    }
    let f: MessageChunkFixture = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

    // First chunk must announce more; last must not.
    let first = parse_report_data(&B64.decode(&f.chunks_b64[0]).unwrap()).unwrap();
    assert!(
        first.more_chunked_messages,
        "chunk 0 should set MoreChunkedMessages"
    );
    let last = parse_report_data(&B64.decode(f.chunks_b64.last().unwrap()).unwrap()).unwrap();
    assert!(
        !last.more_chunked_messages,
        "final chunk must clear MoreChunkedMessages"
    );

    let mut acc = ReportAccumulator::new();
    for c in &f.chunks_b64 {
        acc.push(parse_report_data(&B64.decode(c).unwrap()).unwrap())
            .unwrap();
    }
    let out = acc.finish();
    assert_eq!(out.len(), f.expected.len(), "reassembled attribute count");
    for (got, want) in out.iter().zip(f.expected.iter()) {
        assert_eq!(got.0.endpoint, want.endpoint);
        assert_eq!(got.0.cluster, want.cluster);
        assert_eq!(got.0.attribute, want.attribute);
    }
}

#[test]
fn list_chunked_reassembles_appended_elements() {
    let path = report_dir().join("report_data_chunked_list.json");
    if !path.exists() {
        eprintln!("skipping: no chunk fixtures (run `node xtask/scripts/capture-im/index.js`)");
        return;
    }
    let f: ListChunkFixture = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

    let mut acc = ReportAccumulator::new();
    for c in &f.chunks_b64 {
        acc.push(parse_report_data(&B64.decode(c).unwrap()).unwrap())
            .unwrap();
    }
    let out = acc.finish();
    assert_eq!(out.len(), 1, "one list attribute");
    assert_eq!(out[0].0.endpoint, f.expected_path.endpoint);
    assert_eq!(out[0].0.cluster, f.expected_path.cluster);
    assert_eq!(out[0].0.attribute, f.expected_path.attribute);
    let want: Vec<Value> = f
        .expected_list
        .iter()
        .map(|&n| Value::Uint(u64::from(n)))
        .collect();
    assert_eq!(out[0].1, Value::Array(want));
}

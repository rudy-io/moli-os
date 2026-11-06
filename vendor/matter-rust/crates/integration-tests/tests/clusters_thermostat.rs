// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use matter_codec::Tag;
use matter_controller::{AttributePath, CommandPath, ImStatus, Node, ReadPath, Value};

const THERMOSTAT: u32 = 0x0201;
const CMD_SETPOINT_RAISE_LOWER: u32 = 0x00;
const ATTR_OCCUPIED_HEATING_SETPOINT: u32 = 0x0012;
const MODE_HEAT: u64 = 0;

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Option<Value> {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
}

/// Thermostat behavioral: write a baseline OccupiedHeatingSetpoint (proves the
/// writable setpoint round-trips), then SetpointRaiseLower(Heat, +10) raises it
/// by 1.0 °C (100 centidegrees).
#[tokio::test]
async fn thermostat_setpoint_write_then_raise() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // Baseline: 20.00 °C = 2000 centidegrees (signed int16 → Value::Int).
    let statuses = node
        .write(&[(
            AttributePath {
                endpoint: 1,
                cluster: THERMOSTAT,
                attribute: ATTR_OCCUPIED_HEATING_SETPOINT,
            },
            Value::Int(2000),
        )])
        .await
        .expect("write OccupiedHeatingSetpoint");
    for (_, s) in &statuses {
        assert!(
            matches!(s, ImStatus::Success),
            "setpoint write status not Success: {s:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        read_attr(&node, 1, THERMOSTAT, ATTR_OCCUPIED_HEATING_SETPOINT).await,
        Some(Value::Int(2000)),
        "OccupiedHeatingSetpoint did not round-trip to 2000"
    );

    // SetpointRaiseLower(Heat, +10) → +1.0 °C = +100 centidegrees → 2100.
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: THERMOSTAT,
            command: CMD_SETPOINT_RAISE_LOWER,
        },
        Value::Structure(vec![
            (Tag::Context(0), Value::Uint(MODE_HEAT)), // Mode = Heat
            (Tag::Context(1), Value::Int(10)),         // Amount = +10 (0.1 °C units)
        ]),
    )
    .await
    .expect("invoke SetpointRaiseLower");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        read_attr(&node, 1, THERMOSTAT, ATTR_OCCUPIED_HEATING_SETPOINT).await,
        Some(Value::Int(2100)),
        "OccupiedHeatingSetpoint did not rise to 2100 after SetpointRaiseLower(+10)"
    );
}

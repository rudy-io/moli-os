// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::{
    flow_measurement, illuminance_measurement, pressure_measurement, relative_humidity_measurement,
    temperature_measurement,
};
use matter_clusters::types::Nullable;
use matter_codec::{Tag, TlvWriter};
use matter_controller::{Node, ReadPath, Value};

const ATTR_MEASURED_VALUE: u32 = 0x0000;
const ATTR_MIN_MEASURED_VALUE: u32 = 0x0001;
const ATTR_MAX_MEASURED_VALUE: u32 = 0x0002;

fn value_to_tlv(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.write_value(Tag::Anonymous, value)
        .expect("infallible: Vec-backed TlvWriter");
    buf
}

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Value {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
        .expect("attribute present in report")
}

/// TemperatureMeasurement (0x0402): the three core attrs typed-decode Ok.
/// Min/Max default to 0x8000 (int16 null/invalid sentinel) so they are asserted
/// Ok-only, not exact.
#[tokio::test]
async fn temperature_measurement_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0402;

    let measured = read_attr(&node, 1, C, ATTR_MEASURED_VALUE).await;
    assert!(
        temperature_measurement::decode_measured_value(&value_to_tlv(&measured)).is_ok(),
        "TemperatureMeasurement.MeasuredValue typed-decode failed: {measured:?}"
    );
    let min = read_attr(&node, 1, C, ATTR_MIN_MEASURED_VALUE).await;
    assert!(
        temperature_measurement::decode_min_measured_value(&value_to_tlv(&min)).is_ok(),
        "TemperatureMeasurement.MinMeasuredValue typed-decode failed: {min:?}"
    );
    let max = read_attr(&node, 1, C, ATTR_MAX_MEASURED_VALUE).await;
    assert!(
        temperature_measurement::decode_max_measured_value(&value_to_tlv(&max)).is_ok(),
        "TemperatureMeasurement.MaxMeasuredValue typed-decode failed: {max:?}"
    );
}

/// RelativeHumidityMeasurement (0x0405): MeasuredValue decodes Ok; Min==0, Max==10000.
#[tokio::test]
async fn relative_humidity_measurement_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0405;

    let measured = read_attr(&node, 1, C, ATTR_MEASURED_VALUE).await;
    assert!(
        relative_humidity_measurement::decode_measured_value(&value_to_tlv(&measured)).is_ok(),
        "Humidity.MeasuredValue typed-decode failed: {measured:?}"
    );
    let min = read_attr(&node, 1, C, ATTR_MIN_MEASURED_VALUE).await;
    assert_eq!(
        relative_humidity_measurement::decode_min_measured_value(&value_to_tlv(&min))
            .expect("decode Humidity.Min"),
        Nullable::Value(0),
        "Humidity.MinMeasuredValue != 0"
    );
    let max = read_attr(&node, 1, C, ATTR_MAX_MEASURED_VALUE).await;
    assert_eq!(
        relative_humidity_measurement::decode_max_measured_value(&value_to_tlv(&max))
            .expect("decode Humidity.Max"),
        Nullable::Value(10000),
        "Humidity.MaxMeasuredValue != 10000"
    );
}

/// IlluminanceMeasurement (0x0400): MeasuredValue decodes Ok; Min==1, Max==65534.
#[tokio::test]
async fn illuminance_measurement_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0400;

    let measured = read_attr(&node, 1, C, ATTR_MEASURED_VALUE).await;
    assert!(
        illuminance_measurement::decode_measured_value(&value_to_tlv(&measured)).is_ok(),
        "Illuminance.MeasuredValue typed-decode failed: {measured:?}"
    );
    let min = read_attr(&node, 1, C, ATTR_MIN_MEASURED_VALUE).await;
    assert_eq!(
        illuminance_measurement::decode_min_measured_value(&value_to_tlv(&min))
            .expect("decode Illuminance.Min"),
        Nullable::Value(1),
        "Illuminance.MinMeasuredValue != 1"
    );
    let max = read_attr(&node, 1, C, ATTR_MAX_MEASURED_VALUE).await;
    assert_eq!(
        illuminance_measurement::decode_max_measured_value(&value_to_tlv(&max))
            .expect("decode Illuminance.Max"),
        Nullable::Value(65534),
        "Illuminance.MaxMeasuredValue != 65534"
    );
}

/// PressureMeasurement (0x0403): MeasuredValue and MaxMeasuredValue decode Ok;
/// Min==0.
///
/// MaxMeasuredValue is checked for a successful typed decode only, not a value:
/// it is the DUT's configured default, and all-clusters-app ships 0 in
/// connectedhomeip v1.4.2.0 (the nightly's pin) but 32767 on later `master`.
#[tokio::test]
async fn pressure_measurement_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0403;

    let measured = read_attr(&node, 1, C, ATTR_MEASURED_VALUE).await;
    assert!(
        pressure_measurement::decode_measured_value(&value_to_tlv(&measured)).is_ok(),
        "Pressure.MeasuredValue typed-decode failed: {measured:?}"
    );
    let min = read_attr(&node, 1, C, ATTR_MIN_MEASURED_VALUE).await;
    assert_eq!(
        pressure_measurement::decode_min_measured_value(&value_to_tlv(&min))
            .expect("decode Pressure.Min"),
        Nullable::Value(0),
        "Pressure.MinMeasuredValue != 0"
    );
    let max = read_attr(&node, 1, C, ATTR_MAX_MEASURED_VALUE).await;
    assert!(
        pressure_measurement::decode_max_measured_value(&value_to_tlv(&max)).is_ok(),
        "Pressure.MaxMeasuredValue typed-decode failed: {max:?}"
    );
}

/// FlowMeasurement (0x0404): MeasuredValue decodes Ok; Min==0, Max==100.
#[tokio::test]
async fn flow_measurement_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0404;

    let measured = read_attr(&node, 1, C, ATTR_MEASURED_VALUE).await;
    assert!(
        flow_measurement::decode_measured_value(&value_to_tlv(&measured)).is_ok(),
        "Flow.MeasuredValue typed-decode failed: {measured:?}"
    );
    let min = read_attr(&node, 1, C, ATTR_MIN_MEASURED_VALUE).await;
    assert_eq!(
        flow_measurement::decode_min_measured_value(&value_to_tlv(&min)).expect("decode Flow.Min"),
        Nullable::Value(0),
        "Flow.MinMeasuredValue != 0"
    );
    let max = read_attr(&node, 1, C, ATTR_MAX_MEASURED_VALUE).await;
    assert_eq!(
        flow_measurement::decode_max_measured_value(&value_to_tlv(&max)).expect("decode Flow.Max"),
        Nullable::Value(100),
        "Flow.MaxMeasuredValue != 100"
    );
}

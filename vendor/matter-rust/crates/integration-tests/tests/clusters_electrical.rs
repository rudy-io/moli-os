// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::{electrical_energy_measurement, electrical_power_measurement};
use matter_codec::{Tag, TlvWriter};
use matter_controller::{Node, ReadPath, Value};

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

/// ElectricalPowerMeasurement (0x0090) + ElectricalEnergyMeasurement (0x0091) on
/// evse-app ep1: typed-decode the live device bytes. At rest the readings are
/// null/zero (which decode fine), and the EEM `Accuracy` composite struct +
/// EPM `PowerMode` are populated — so this validates the typed decoders,
/// including the composite struct decoder, against real device bytes.
#[tokio::test]
async fn electrical_measurement_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("evse") {
        eprintln!("skipped: Electrical* test needs the evse-app DUT (`just integration-energy`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // ElectricalPowerMeasurement (0x0090).
    const EPM: u32 = 0x0090;
    let mode = read_attr(&node, 1, EPM, 0x0000).await; // PowerMode
    assert!(
        electrical_power_measurement::decode_power_mode(&value_to_tlv(&mode)).is_ok(),
        "EPM.PowerMode typed-decode failed: {mode:?}"
    );
    let voltage = read_attr(&node, 1, EPM, 0x0004).await; // Voltage
    assert!(
        electrical_power_measurement::decode_voltage(&value_to_tlv(&voltage)).is_ok(),
        "EPM.Voltage typed-decode failed: {voltage:?}"
    );
    let power = read_attr(&node, 1, EPM, 0x0008).await; // ActivePower
    assert!(
        electrical_power_measurement::decode_active_power(&value_to_tlv(&power)).is_ok(),
        "EPM.ActivePower typed-decode failed: {power:?}"
    );

    // ElectricalEnergyMeasurement (0x0091): the composite Accuracy struct + a
    // cumulative-energy reading.
    const EEM: u32 = 0x0091;
    let accuracy = read_attr(&node, 1, EEM, 0x0000).await; // Accuracy (MeasurementAccuracyStruct)
    assert!(
        electrical_energy_measurement::decode_accuracy(&value_to_tlv(&accuracy)).is_ok(),
        "EEM.Accuracy composite typed-decode failed: {accuracy:?}"
    );
    let imported = read_attr(&node, 1, EEM, 0x0001).await; // CumulativeEnergyImported
    assert!(
        electrical_energy_measurement::decode_cumulative_energy_imported(&value_to_tlv(&imported))
            .is_ok(),
        "EEM.CumulativeEnergyImported typed-decode failed: {imported:?}"
    );
}

// ── Energy-reporting events (M9-A3 B1) ───────────────────────────────────────

/// `EnergyReportingTrigger::kFakeReadingsLoadStart_1kW_2s`
/// (EnergyReportingTestEventTriggerHandler.h).
const FAKE_LOAD_START_1KW_2S: u64 = 0x0091_0000_0000_0001;
/// `EnergyReportingTrigger::kFakeReadingsStop`.
const FAKE_READINGS_STOP: u64 = 0x0091_0000_0000_0000;

/// The range one fake reading's periodic imported energy (mWh) falls in.
/// `SetTestEventTrigger_FakeReadingsLoadStart` runs a 1 000 000 mW load with
/// ±20 000 mW randomness (`rand() % 40000 - 20000`, so 980 000..=1 019 999
/// mW) on a 2 s interval, and FakeReadings.cpp computes the energy as
/// `power * 2 / 3600` in integer arithmetic: 544..=566 mWh. The seed is fixed
/// (`srand(1)`) but the sequence is libc's, so only the range is portable.
const FAKE_PERIODIC_IMPORT_MWH: std::ops::RangeInclusive<i64> = 544..=566;

/// Longer than the fake readings' 2 s interval: a quiet period this long
/// after `kFakeReadingsStop` shows no further reading is coming.
const FAKE_READINGS_SETTLE: std::time::Duration = std::time::Duration::from_millis(3000);

/// The highest EEM ep1 `event` number (`None` if none).
async fn eem_latest(node: &Node, event: u32) -> Option<u64> {
    integration_tests::events::latest_event_number(
        node,
        1,
        electrical_energy_measurement::CLUSTER_ID,
        event,
    )
    .await
    .unwrap_or_else(|e| panic!("latest EEM event {event:#04x}: {e}"))
}

/// Every EEM ep1 `event` numbered above `baseline`, in event-number order,
/// as payload TLV.
async fn eem_events_after(node: &Node, event: u32, baseline: Option<u64>) -> Vec<Vec<u8>> {
    use integration_tests::events::{payload_tlv, read_event_items};
    use matter_controller::EventPath;

    let mut items = read_event_items(
        node,
        EventPath::concrete(1, electrical_energy_measurement::CLUSTER_ID, event),
    )
    .await
    .unwrap_or_else(|e| panic!("read EEM event {event:#04x}: {e}"));
    items.retain(|i| baseline.is_none_or(|b| i.event_number > b));
    items.sort_by_key(|i| i.event_number);
    items.iter().map(|i| payload_tlv(&i.value)).collect()
}

/// Poll until at least `count` EEM ep1 `event`s numbered above `baseline`
/// exist or `EVENT_TIMEOUT` passes; returns how many there were.
async fn eem_wait_for_count(node: &Node, event: u32, baseline: Option<u64>, count: usize) -> usize {
    use integration_tests::events::EVENT_TIMEOUT;

    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        let n = eem_events_after(node, event, baseline).await.len();
        if n >= count || std::time::Instant::now() >= deadline {
            return n;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// ElectricalEnergyMeasurement events on evse-app ep1: chip's
/// energy-reporting `TestEventTrigger` starts fake 1 kW import readings every
/// 2 s (EnergyReportingTestEventTriggerHandler.h
/// `kFakeReadingsLoadStart_1kW_2s`), and each reading emits
/// `CumulativeEnergyMeasured` and `PeriodicEnergyMeasured`
/// (ElectricalEnergyMeasurementCluster.cpp `CumulativeEnergySnapshot` /
/// `PeriodicEnergySnapshot`). ElectricalPowerMeasurement's only event,
/// `MeasurementPeriodRanges`, has no chip emitter; anything reported decodes.
///
/// Baselines are taken before the trigger, so only the readings it caused
/// are asserted.
#[tokio::test]
async fn energy_reporting_events_decode() {
    use electrical_energy_measurement::event_id as eem;
    use integration_tests::events::{test_event_trigger, EVENT_TIMEOUT};

    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("evse") {
        eprintln!("skipped: energy event test needs the evse-app DUT (`just integration-energy`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let cumulative_base = eem_latest(&node, eem::CUMULATIVE_ENERGY_MEASURED).await;
    let periodic_base = eem_latest(&node, eem::PERIODIC_ENERGY_MEASURED).await;
    test_event_trigger(&node, FAKE_LOAD_START_1KW_2S)
        .await
        .expect("start fake readings");
    // The first reading is taken inside the trigger; wait for the second
    // (2 s later), so the cumulative total is seen to grow by one reading.
    let readings =
        eem_wait_for_count(&node, eem::CUMULATIVE_ENERGY_MEASURED, cumulative_base, 2).await;
    // Stop before asserting, so a failed assertion cannot leave readings running.
    test_event_trigger(&node, FAKE_READINGS_STOP)
        .await
        .expect("stop fake readings");
    assert!(
        readings >= 2,
        "expected 2 CumulativeEnergyMeasured events within {EVENT_TIMEOUT:?} of the start \
         trigger, got {readings}"
    );

    // StopFakeReadings clears the flag the reading timer checks, on chip's
    // event loop, before the stop invoke is answered: whatever is in the log
    // now is final. Re-read after a quiet period longer than the interval.
    let cumulative =
        eem_events_after(&node, eem::CUMULATIVE_ENERGY_MEASURED, cumulative_base).await;
    let periodic = eem_events_after(&node, eem::PERIODIC_ENERGY_MEASURED, periodic_base).await;
    tokio::time::sleep(FAKE_READINGS_SETTLE).await;
    assert_eq!(
        eem_events_after(&node, eem::CUMULATIVE_ENERGY_MEASURED, cumulative_base)
            .await
            .len(),
        cumulative.len(),
        "a CumulativeEnergyMeasured event arrived after kFakeReadingsStop"
    );
    assert_eq!(
        eem_events_after(&node, eem::PERIODIC_ENERGY_MEASURED, periodic_base)
            .await
            .len(),
        periodic.len(),
        "a PeriodicEnergyMeasured event arrived after kFakeReadingsStop"
    );

    assert_fake_load_readings(&cumulative, &periodic);
    decode_every_energy_event(&node).await;
}

/// The new `CumulativeEnergyMeasured` / `PeriodicEnergyMeasured` payloads
/// (event-number order) from one fake-load run, against FakeReadings.cpp:
/// - every periodic imported energy is one reading, in
///   [`FAKE_PERIODIC_IMPORT_MWH`];
/// - the trigger resets the totals to 0 and takes the first reading at once,
///   so the first cumulative imported energy equals the first periodic one;
/// - each later cumulative event adds exactly one reading, so each
///   increase of the cumulative imported energy is in the same range.
///
/// `EnergyExported` is not asserted: chip 1.4.2 reports a 0 mWh export on
/// every event, while chip master's quieter reporting drops an unchanged
/// value, so whether it is present differs between versions. Nor is the
/// periodic count tied to the cumulative one: master emits a periodic event
/// only when the energy differs from the previous reading's.
fn assert_fake_load_readings(cumulative: &[Vec<u8>], periodic: &[Vec<u8>]) {
    use electrical_energy_measurement::{
        CumulativeEnergyMeasuredEvent, PeriodicEnergyMeasuredEvent,
    };

    let cumulative: Vec<i64> = cumulative
        .iter()
        .map(|tlv| {
            let e = CumulativeEnergyMeasuredEvent::decode(tlv)
                .expect("CumulativeEnergyMeasured decodes");
            e.energy_imported
                .as_ref()
                .unwrap_or_else(|| panic!("a 1 kW load reports imported energy: {e:?}"))
                .energy
        })
        .collect();
    let periodic: Vec<i64> = periodic
        .iter()
        .map(|tlv| {
            let e =
                PeriodicEnergyMeasuredEvent::decode(tlv).expect("PeriodicEnergyMeasured decodes");
            e.energy_imported
                .as_ref()
                .unwrap_or_else(|| panic!("a 1 kW load reports imported energy: {e:?}"))
                .energy
        })
        .collect();
    eprintln!(
        "[events] EEM: cumulative imported {cumulative:?} mWh, periodic imported {periodic:?} mWh"
    );

    for energy in &periodic {
        assert!(
            FAKE_PERIODIC_IMPORT_MWH.contains(energy),
            "periodic imported energy {energy} mWh outside {FAKE_PERIODIC_IMPORT_MWH:?}"
        );
    }
    assert_eq!(
        cumulative.first(),
        periodic.first(),
        "the first reading after the reset is both the total and the period's energy"
    );
    for step in cumulative.windows(2) {
        let added = step[1] - step[0];
        assert!(
            FAKE_PERIODIC_IMPORT_MWH.contains(&added),
            "cumulative imported energy grew by {added} mWh, not one reading \
             ({FAKE_PERIODIC_IMPORT_MWH:?}): {cumulative:?}"
        );
    }
}

/// Every EEM and EPM event evse-app holds on ep1 decodes with its generated
/// decoder; event ids unknown to the 1.4 codegen are skipped.
async fn decode_every_energy_event(node: &Node) {
    use electrical_energy_measurement::event_id as eem;
    use integration_tests::events::{payload_tlv, read_event_items};
    use matter_controller::EventPath;

    let items = read_event_items(
        node,
        EventPath::cluster(1, electrical_energy_measurement::CLUSTER_ID),
    )
    .await
    .expect("read EEM events");
    for item in &items {
        let tlv = payload_tlv(&item.value);
        let decoded = match item.path.event.expect("event id") {
            eem::CUMULATIVE_ENERGY_MEASURED => {
                electrical_energy_measurement::CumulativeEnergyMeasuredEvent::decode(&tlv)
                    .map(|_| ())
            }
            eem::PERIODIC_ENERGY_MEASURED => {
                electrical_energy_measurement::PeriodicEnergyMeasuredEvent::decode(&tlv).map(|_| ())
            }
            other => {
                eprintln!(
                    "[events] EEM: skipping event id {other:#04x} unknown to the 1.4 codegen"
                );
                Ok(())
            }
        };
        decoded.unwrap_or_else(|e| panic!("EEM event failed to decode: {e}; {item:?}"));
    }
    let epm = read_event_items(
        node,
        EventPath::cluster(1, electrical_power_measurement::CLUSTER_ID),
    )
    .await
    .expect("read EPM events");
    for item in &epm {
        electrical_power_measurement::MeasurementPeriodRangesEvent::decode(&payload_tlv(
            &item.value,
        ))
        .unwrap_or_else(|e| panic!("EPM event failed to decode: {e}; {item:?}"));
    }
    eprintln!(
        "[events] EEM: decoded {} event(s); EPM: decoded {} event(s)",
        items.len(),
        epm.len()
    );
}

// ── Energy mode clusters (M9-A3 B2) ──────────────────────────────────────────

/// EnergyEvseMode (0x009D) and DeviceEnergyManagementMode (0x009F) on evse-app
/// ep1 (chip master's all-clusters no longer serves them): every attribute decodes,
/// ChangeToMode(CurrentMode) answers Success with no StatusText, and the
/// supported modes are evse-app's (evse-common/include/energy-evse-modes.h,
/// energy-management/device-energy-management/include/
/// device-energy-management-modes.h).
///
/// evse-app.matter serves exactly SupportedModes and CurrentMode (FeatureMap 0,
/// so no OnMode/StartUpMode) on both clusters; the full id list is asserted so
/// a newly served attribute is noticed rather than skipped.
#[tokio::test]
async fn energy_mode_clusters_decode_and_change_to_current_mode() {
    use integration_tests::sweep::{attribute_ids, attribute_tlv, read_cluster_attributes};
    use matter_clusters::clusters::{device_energy_management_mode, energy_evse_mode};

    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("evse") {
        eprintln!("skipped: energy mode test needs the evse-app DUT (`just integration-energy`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    integration_tests::sweep_mode_base!(&node, 1, energy_evse_mode);
    integration_tests::sweep_mode_base!(&node, 1, device_energy_management_mode);

    let attrs = read_cluster_attributes(&node, 1, energy_evse_mode::CLUSTER_ID)
        .await
        .unwrap();
    {
        use energy_evse_mode::attribute_id::{CURRENT_MODE, SUPPORTED_MODES};
        assert_eq!(attribute_ids(&attrs), [SUPPORTED_MODES, CURRENT_MODE]);
    }
    let evse = energy_evse_mode::decode_supported_modes(attribute_tlv(
        &attrs,
        energy_evse_mode::attribute_id::SUPPORTED_MODES,
    ))
    .unwrap();
    let labels: Vec<(u8, &str)> = evse.iter().map(|m| (m.mode, m.label.as_str())).collect();
    assert_eq!(
        labels,
        [
            (0, "Manual"),
            (1, "Auto-scheduled"),
            (2, "Solar"),
            (3, "Auto-scheduled with Solar charging"),
        ]
    );
    use energy_evse_mode::ModeTag;
    let tags: Vec<Vec<ModeTag>> = evse
        .iter()
        .map(|m| m.mode_tags.iter().map(|t| t.value).collect())
        .collect();
    assert_eq!(
        tags,
        [
            vec![ModeTag::Manual],
            vec![ModeTag::TimeOfUse],
            vec![ModeTag::SolarCharging],
            vec![ModeTag::TimeOfUse, ModeTag::SolarCharging],
        ]
    );

    assert_dem_modes(&node).await;
}

/// DeviceEnergyManagementMode's served attributes and evse-app's five modes
/// (device-energy-management-modes.h), split from the test to keep it short.
async fn assert_dem_modes(node: &Node) {
    use integration_tests::sweep::{attribute_ids, attribute_tlv, read_cluster_attributes};
    use matter_clusters::clusters::device_energy_management_mode::{
        self as dem,
        attribute_id::{CURRENT_MODE, SUPPORTED_MODES},
        ModeTag,
    };

    let attrs = read_cluster_attributes(node, 1, dem::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(attribute_ids(&attrs), [SUPPORTED_MODES, CURRENT_MODE]);
    let modes = dem::decode_supported_modes(attribute_tlv(&attrs, SUPPORTED_MODES)).unwrap();
    let labels: Vec<(u8, &str)> = modes.iter().map(|m| (m.mode, m.label.as_str())).collect();
    assert_eq!(
        labels,
        [
            (0, "No energy management (forecast only)"),
            (1, "Device optimizes (no local or grid control)"),
            (2, "Optimized within building"),
            (3, "Optimized for grid"),
            (4, "Optimized for grid and building"),
        ]
    );
    let tags: Vec<Vec<ModeTag>> = modes
        .iter()
        .map(|m| m.mode_tags.iter().map(|t| t.value).collect())
        .collect();
    assert_eq!(
        tags,
        [
            vec![ModeTag::NoOptimization],
            vec![ModeTag::DeviceOptimization],
            vec![ModeTag::LocalOptimization, ModeTag::DeviceOptimization],
            vec![ModeTag::DeviceOptimization, ModeTag::GridOptimization],
            vec![
                ModeTag::LocalOptimization,
                ModeTag::DeviceOptimization,
                ModeTag::GridOptimization,
            ],
        ]
    );
}

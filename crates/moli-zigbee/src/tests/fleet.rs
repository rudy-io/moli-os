//! The devices, end to end: the real driver, the real hub, a simulated radio.

use moli_runtime::DeviceView;

use super::*;
use crate::devices::tests::lamp;
use crate::sim::{FakeRadio, RadioRequest, incoming_from, lamp_answers, sensor_answers};
use crate::{zcl, zdo};

const LAMP: &str = "0x00124b0000000002";
const LAMP_BYTES: [u8; 8] = [0x02, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
const NWK: u16 = 0x1234;
const OTHER: &str = "0x00124b0000000003";
const OTHER_BYTES: [u8; 8] = [0x03, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
const OTHER_NWK: u16 = 0x2345;

impl Running {
    fn view(&self, ieee: &str) -> Option<DeviceView> {
        self.hub.device(&DeviceId::new(&self.instance, ieee))
    }

    fn value(&self, ieee: &str, key: &str) -> Option<Value> {
        let device = DeviceId::new(&self.instance, ieee);
        self.hub
            .state(&PointId::new(&device, key))
            .map(|sample| sample.value)
    }

    fn online(&self, ieee: &str) -> Option<bool> {
        self.view(ieee).and_then(|view| view.online)
    }

    fn warning(&self) -> Option<String> {
        self.hub
            .snapshot()
            .drivers
            .into_iter()
            .find(|driver| driver.instance == self.instance)
            .and_then(|driver| driver.warning)
    }

    async fn order_to(&self, ieee: &str, key: &str, value: Value) -> Result<(), CommandError> {
        let device = DeviceId::new(&self.instance, ieee);
        self.hub
            .command(&PointId::new(&device, key), value, Origin::Ui, None)
            .await
    }
}

/// Waits until `check` holds, up to 200 s of test time.
async fn until(what: &str, check: impl Fn() -> bool) {
    for _ in 0..20_000 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

/// A device joins and gets through its interview and configuration.
async fn joins(ncp: &FakeNcp, run: &Running, nwk: u16, bytes: [u8; 8], ieee: &str) {
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(nwk, bytes, 1),
    );
    until("interviewed and configured", || {
        run.store()
            .devices
            .get(ieee)
            .is_some_and(|device| device.interviewed && device.configured)
    })
    .await;
}

async fn lamp_on_network() -> (FakeNcp, FakeRadio, Running) {
    let (ncp, run) = running_network().await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    joins(&ncp, &run, NWK, LAMP_BYTES, LAMP).await;
    until("its first values", || {
        run.value(LAMP, "color_temp").is_some()
    })
    .await;
    (ncp, radio, run)
}

/// A store that knows the lamp, interviewed in an earlier run.
fn store_with(record: store::DeviceRecord) -> Setup {
    let mut store = known_store(record_of_network());
    store.devices.insert(record.ieee.clone(), record);
    Setup {
        store: Some(store),
        ..Setup::default()
    }
}

fn record_of_network() -> store::NetworkRecord {
    record(0x1a62)
}

/// A report (or anything) a device sends.
fn from_device(ncp: &FakeNcp, nwk: u16, cluster: u16, frame: &[u8]) {
    ncp.callback(
        ids::INCOMING_MESSAGE_HANDLER,
        &incoming_from(nwk, zcl::HA_PROFILE, cluster, 1, frame),
    );
}

#[tokio::test]
async fn a_device_that_joins_becomes_a_moli_device() {
    let (_ncp, radio, run) = lamp_on_network().await;
    let view = run.view(LAMP).unwrap();
    assert_eq!(view.device.model.as_deref(), Some("Some lamp"));
    assert_eq!(view.device.manufacturer.as_deref(), Some("Some maker"));
    assert_eq!(&*view.device.native_name, "Some lamp");
    assert!(view.device.point("state").unwrap().access.write);
    assert_eq!(view.online, Some(true));
    // What it read at once, in Moli's units.
    assert_eq!(run.value(LAMP, "state"), Some(Value::Bool(true)));
    assert_eq!(run.value(LAMP, "brightness"), Some(Value::Int(100)));
    assert_eq!(run.value(LAMP, "color_temp"), Some(Value::Int(370)));
    assert!(matches!(run.value(LAMP, "color"), Some(Value::Text(_))));
    assert_eq!(run.value(LAMP, "linkquality"), Some(Value::Int(200)));
    // Bound to the coordinator, reports asked for.
    let requests = radio.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.cluster == zdo::BIND_REQUEST)
    );
    assert!(
        requests
            .iter()
            .any(|request| { request.cluster == zcl::ON_OFF && request.payload[2] == 0x06 })
    );
    // Remembered.
    let saved = run.store().devices[LAMP].clone();
    assert_eq!(saved.model.as_deref(), Some("Some lamp"));
    assert_eq!(saved.nwk, Some(NWK));
}

#[tokio::test]
async fn reports_change_values_and_are_answered_when_they_ask() {
    let (ncp, radio, run) = lamp_on_network().await;
    // No default response wanted.
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 0],
    );
    until("off", || {
        run.value(LAMP, "state") == Some(Value::Bool(false))
    })
    .await;
    let before = radio.requests().len();
    // One wanted.
    from_device(
        &ncp,
        NWK,
        zcl::LEVEL,
        &[0x08, 10, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::UINT8, 127],
    );
    until("half", || {
        run.value(LAMP, "brightness") == Some(Value::Int(50))
    })
    .await;
    until("the answer", || radio.requests().len() > before).await;
    let answer = radio.requests()[before].clone();
    assert_eq!(answer.cluster, zcl::LEVEL);
    assert_eq!(
        answer.payload,
        [0x10, 10, zcl::DEFAULT_RESPONSE, 0x0A, 0x00]
    );
    assert_eq!(radio.requests().len(), before + 1, "only that one");
}

#[tokio::test]
async fn orders_reach_the_device_and_show() {
    let (_ncp, radio, run) = lamp_on_network().await;
    run.order_to(LAMP, "state", Value::Bool(false))
        .await
        .unwrap();
    let last = radio.requests().last().unwrap().clone();
    assert_eq!(
        (last.nwk, last.cluster, last.endpoint),
        (NWK, zcl::ON_OFF, 1)
    );
    assert_eq!(last.payload[0], 0x01, "a cluster command");
    assert_eq!(last.payload[2], 0x00, "off");
    assert_eq!(run.value(LAMP, "state"), Some(Value::Bool(false)));

    run.order_to(LAMP, "brightness", Value::Int(50))
        .await
        .unwrap();
    let last = radio.requests().last().unwrap().clone();
    assert_eq!(last.cluster, zcl::LEVEL);
    assert_eq!(last.payload[2..], [0x04, 127, 0, 0, 0, 0]);
    assert_eq!(run.value(LAMP, "brightness"), Some(Value::Int(50)));
    assert_eq!(run.value(LAMP, "state"), Some(Value::Bool(true)));

    run.order_to(LAMP, "color", text("#ff0000")).await.unwrap();
    assert_eq!(radio.requests().last().unwrap().payload[2], 0x07);
    assert_eq!(run.value(LAMP, "color"), Some(text("#ff0000")));
    assert_eq!(run.value(LAMP, "color_temp"), Some(Value::Null));

    run.order_to(LAMP, "color_temp", Value::Int(300))
        .await
        .unwrap();
    assert_eq!(radio.requests().last().unwrap().payload[2], 0x0A);
    assert_eq!(run.value(LAMP, "color_temp"), Some(Value::Int(300)));
}

#[tokio::test]
async fn a_lamp_in_colour_has_no_white() {
    let (ncp, _radio, run) = lamp_on_network().await;
    run.order_to(LAMP, "color", text("#ff0000")).await.unwrap();
    // A real lamp goes on reporting a white, at the end of its range.
    from_device(
        &ncp,
        NWK,
        zcl::COLOR,
        &[
            0x18,
            9,
            zcl::REPORT_ATTRIBUTES,
            0x07,
            0,
            zcl::UINT16,
            0xE8,
            0x03,
        ],
    );
    from_device(
        &ncp,
        NWK,
        zcl::LEVEL,
        &[0x18, 10, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::UINT8, 127],
    );
    until("the last one read", || {
        run.value(LAMP, "brightness") == Some(Value::Int(50))
    })
    .await;
    assert_eq!(run.value(LAMP, "color_temp"), Some(Value::Null));
    // Back in white: its white counts again.
    run.order_to(LAMP, "color_temp", Value::Int(300))
        .await
        .unwrap();
    from_device(
        &ncp,
        NWK,
        zcl::COLOR,
        &[
            0x18,
            11,
            zcl::REPORT_ATTRIBUTES,
            0x07,
            0,
            zcl::UINT16,
            0xFA,
            0x00,
        ],
    );
    until("its white", || {
        run.value(LAMP, "color_temp") == Some(Value::Int(250))
    })
    .await;
}

/// The frames of a real lamp, kept in the integration's package.
fn capture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../integrations/zigbee/fixtures/innr_rb287c.json"
    ))
    .unwrap()
}

fn bytes(hex: &serde_json::Value) -> Vec<u8> {
    let hex = hex.as_str().unwrap();
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

/// A radio answering with the real frames, its transaction numbers put back.
fn answers_of_the_capture(capture: &serde_json::Value, request: &RadioRequest) -> Option<Vec<u8>> {
    let payload = &request.payload;
    let mut frame;
    if request.profile == zdo::PROFILE {
        let zdos = &capture["zdo"];
        frame = match request.cluster {
            zdo::NODE_DESCRIPTOR_REQUEST => bytes(&zdos["node_descriptor"]),
            zdo::ACTIVE_ENDPOINTS_REQUEST => bytes(&zdos["active_endpoints"]),
            zdo::SIMPLE_DESCRIPTOR_REQUEST => {
                bytes(&zdos["simple_descriptors"][payload[3].to_string()])
            }
            _ => vec![0, 0],
        };
        frame[0] = payload[0];
        return Some(frame);
    }
    if payload[2] != 0x00 {
        return Some(vec![0x18, payload[1], zcl::CONFIGURE_RESPONSE, 0x00]);
    }
    let first = u16::from_le_bytes([payload[3], payload[4]]);
    let recorded = capture["read_responses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|read| {
            read["cluster"] == u64::from(request.cluster)
                && read["first_attribute"] == u64::from(first)
        })?;
    frame = bytes(&recorded["frame"]);
    frame[1] = payload[1];
    Some(frame)
}

#[tokio::test]
async fn a_real_lamp_replayed() {
    let capture = capture();
    let (ncp, run) = running_network().await;
    let answers = capture.clone();
    let _radio = FakeRadio::spawn(&ncp, move |request| {
        answers_of_the_capture(&answers, request)
    });
    joins(&ncp, &run, NWK, LAMP_BYTES, LAMP).await;
    let saved = run.store().devices[LAMP].clone();
    assert_eq!(saved.model.as_deref(), Some("RB 287 C"));
    assert_eq!(saved.manufacturer.as_deref(), Some("innr"));
    assert_eq!(saved.sw_build.as_deref(), Some("1.9.64"));
    assert_eq!(saved.manufacturer_code, Some(0x1166));
    assert_eq!(saved.endpoints.len(), 2);
    assert!(saved.mains() && saved.listening());
    let view = run.view(LAMP).unwrap();
    assert_eq!(
        view.device.point("color_temp").unwrap().kind,
        Kind::Numeric {
            min: Some(150.0),
            max: Some(600.0),
            step: Some(1.0),
        }
    );
    assert!(view.device.point("state_242").is_none(), "Green Power");
    // Its first state, as it said it.
    until("its first values", || {
        run.value(LAMP, "color_temp") == Some(Value::Int(370))
    })
    .await;
    assert_eq!(run.value(LAMP, "state"), Some(Value::Bool(true)));
    assert_eq!(run.value(LAMP, "brightness"), Some(Value::Int(100)));
    assert_eq!(run.value(LAMP, "color"), Some(text("#ffce76")));
    // Every report it made, in Moli's units.
    for report in capture["reports"].as_array().unwrap() {
        let cluster = u16::try_from(report["cluster"].as_u64().unwrap()).unwrap();
        from_device(&ncp, NWK, cluster, &bytes(&report["frame"]));
        for (key, value) in report["expect"].as_object().unwrap() {
            let expected = Value::from_json(value);
            until(key, || run.value(LAMP, key).as_ref() == Some(&expected)).await;
        }
    }
}

#[tokio::test]
async fn a_refusal_goes_back_to_whoever_asked() {
    let (_ncp, radio, run) = lamp_on_network().await;
    radio.answer_with(|request| {
        Some(vec![
            0x18,
            request.payload[1],
            zcl::DEFAULT_RESPONSE,
            request.payload[2],
            0x81,
        ])
    });
    let error = run
        .order_to(LAMP, "state", Value::Bool(false))
        .await
        .unwrap_err();
    let CommandError::Driver(reason) = error else {
        panic!("{error:?}");
    };
    assert!(reason.contains("0x81"), "{reason}");
    assert_eq!(
        run.value(LAMP, "state"),
        Some(Value::Bool(true)),
        "nothing changed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_device_that_only_acknowledges_took_the_order() {
    let (_ncp, radio, run) = lamp_on_network().await;
    radio.answer_with(|_| None);
    run.order_to(LAMP, "state", Value::Bool(false))
        .await
        .unwrap();
    assert_eq!(run.value(LAMP, "state"), Some(Value::Bool(false)));
}

#[tokio::test]
async fn orders_leave_in_the_order_received() {
    let (_ncp, radio, run) = lamp_on_network().await;
    let before = radio.requests().len();
    let (on, off, half) = tokio::join!(
        run.order_to(LAMP, "state", Value::Bool(true)),
        run.order_to(LAMP, "state", Value::Bool(false)),
        run.order_to(LAMP, "brightness", Value::Int(50)),
    );
    on.unwrap();
    off.unwrap();
    half.unwrap();
    let sent: Vec<_> = radio.requests()[before..]
        .iter()
        .map(|request| (request.cluster, request.payload[2]))
        .collect();
    assert_eq!(
        sent,
        [(zcl::ON_OFF, 1), (zcl::ON_OFF, 0), (zcl::LEVEL, 0x04)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_device_does_not_block_the_others() {
    let (ncp, radio, run) = lamp_on_network().await;
    joins(&ncp, &run, OTHER_NWK, OTHER_BYTES, OTHER).await;
    radio.deafen(NWK);
    let started = tokio::time::Instant::now();
    let silent = run.order_to(LAMP, "state", Value::Bool(false));
    let heard = async {
        // Once the first is on its way.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let result = run.order_to(OTHER, "state", Value::Bool(false)).await;
        (result, started.elapsed())
    };
    let (silent, (heard, took)) = tokio::join!(silent, heard);
    assert!(silent.is_err(), "{silent:?}");
    heard.unwrap();
    assert!(took < Duration::from_secs(1), "{took:?}");
}

#[tokio::test(start_paused = true)]
async fn an_order_that_does_not_arrive_says_so_and_checks_the_device() {
    let (_ncp, radio, run) = lamp_on_network().await;
    radio.deafen(NWK);
    let error = run
        .order_to(LAMP, "state", Value::Bool(false))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CommandError::Driver(_)),
        "said by the driver, before the hub stops waiting: {error:?}"
    );
    assert_eq!(run.value(LAMP, "state"), Some(Value::Bool(true)));
    until("offline", || run.online(LAMP) == Some(false)).await;
}

/// A sleepy radiator valve, as an interview finds it.
fn valve() -> store::DeviceRecord {
    store::DeviceRecord {
        ieee: OTHER.into(),
        nwk: Some(OTHER_NWK),
        interviewed: true,
        configured: true,
        capability: Some(0x80),
        logical_type: Some(2),
        endpoints: vec![store::EndpointRecord {
            endpoint: 1,
            profile: zcl::HA_PROFILE,
            device_id: 0x0301,
            in_clusters: vec![zcl::BASIC, zcl::THERMOSTAT],
            out_clusters: Vec::new(),
        }],
        ..store::DeviceRecord::default()
    }
}

#[tokio::test(start_paused = true)]
async fn an_order_to_a_sleepy_device_is_handed_over() {
    let (ncp, run) = running_with(&store_with(valve())).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    radio.deafen(OTHER_NWK);
    let mut events = run.hub.subscribe();
    let started = tokio::time::Instant::now();
    run.order_to(OTHER, "occupied_heating_setpoint", Value::Float(20.5))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "nobody waits for its next wake"
    );
    assert_eq!(
        run.value(OTHER, "occupied_heating_setpoint"),
        None,
        "not there yet"
    );
    // It never got it: nothing is shown, and it is not said gone for that.
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(run.value(OTHER, "occupied_heating_setpoint"), None);
    assert_eq!(run.online(OTHER), Some(true));
    // Already told « handed over »: whoever asked hears otherwise.
    let mut told = false;
    while let Ok(event) = events.try_recv() {
        if let moli_core::Event::Notice { notice } = event {
            told = notice.message.contains(OTHER);
        }
    }
    assert!(told, "the people at home are told");
    // It wakes, and takes the next one.
    radio.hear(OTHER_NWK);
    radio.answer_with(|request| Some(vec![0x18, request.payload[1], zcl::WRITE_RESPONSE, 0x00]));
    run.order_to(OTHER, "occupied_heating_setpoint", Value::Float(20.5))
        .await
        .unwrap();
    until("shown once taken", || {
        run.value(OTHER, "occupied_heating_setpoint") == Some(Value::Float(20.5))
    })
    .await;
}

#[tokio::test]
async fn orders_it_cannot_send_are_refused_at_once() {
    let (_ncp, radio, run) = lamp_on_network().await;
    let before = radio.requests().len();
    let error = run
        .order_to(LAMP, "color", text("#000000"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        CommandError::Driver(moli_i18n::tr!("pilotes.zigbee.ordre_valeur"))
    );
    assert_eq!(radio.requests().len(), before, "nothing sent");
}

#[tokio::test]
async fn orders_wait_for_no_network() {
    let (ncp, _radio, run) = lamp_on_network().await;
    ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
    run.wait_point("network", &text("down")).await;
    let error = run
        .order_to(LAMP, "state", Value::Bool(false))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        CommandError::Driver(moli_i18n::tr!("pilotes.zigbee.ordre_reseau_absent"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_interview_is_said_and_tried_again() {
    let (ncp, run) = running_network().await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    radio.lose_answers(usize::MAX);
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 1),
    );
    // There at once, with nothing to control yet.
    until("published", || run.view(LAMP).is_some()).await;
    assert!(run.view(LAMP).unwrap().device.point("state").is_none());
    until("said", || run.warning().is_some()).await;
    assert!(run.warning().unwrap().contains(LAMP));
    assert!(!run.store().devices[LAMP].interviewed);
    // It speaks, a while later, and answers now.
    radio.lose_answers(0);
    tokio::time::sleep(Duration::from_secs(61)).await;
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 0],
    );
    until("interviewed", || run.store().devices[LAMP].interviewed).await;
    assert!(run.view(LAMP).unwrap().device.point("state").is_some());
    until("no more warning", || run.warning().is_none()).await;
}

#[tokio::test(start_paused = true)]
async fn a_lamp_silent_for_ever_about_its_colours_gets_its_controls_in_the_end() {
    let (ncp, run) = running_network().await;
    let _radio = FakeRadio::spawn(&ncp, |request| {
        let capabilities = request.cluster == zcl::COLOR
            && request.payload[2] == 0x00
            && request.payload[3..5] == [0x0A, 0x40];
        if capabilities {
            None
        } else {
            lamp_answers()(request)
        }
    });
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 1),
    );
    until("said", || run.warning().is_some()).await;
    assert!(run.view(LAMP).unwrap().device.point("state").is_none());
    // Three tries, ten minutes apart, then no longer held up.
    tokio::time::sleep(Duration::from_secs(45 * 60)).await;
    assert!(run.store().devices[LAMP].interviewed);
    assert!(run.view(LAMP).unwrap().device.point("state").is_some());
    assert_eq!(run.warning(), None);
}

#[tokio::test(start_paused = true)]
async fn a_silent_device_is_tried_again_on_its_own() {
    let (ncp, run) = running_network().await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    radio.lose_answers(usize::MAX);
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 1),
    );
    until("said", || run.warning().is_some()).await;
    radio.lose_answers(0);
    tokio::time::sleep(Duration::from_secs(11 * 60)).await;
    until("interviewed", || run.store().devices[LAMP].interviewed).await;
}

#[tokio::test(start_paused = true)]
async fn a_known_device_comes_back_without_an_interview() {
    let mut known = lamp();
    known.configured = true;
    let (ncp, run) = running_with(&store_with(known)).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    // Published and online at once, its values read in a moment.
    assert!(run.view(LAMP).unwrap().device.point("state").is_some());
    until("its values", || {
        run.value(LAMP, "color_temp") == Some(Value::Int(370))
    })
    .await;
    assert_eq!(run.online(LAMP), Some(true));
    let requests = radio.requests();
    assert!(
        requests
            .iter()
            .all(|request| request.profile != zdo::PROFILE),
        "no descriptor asked, nothing bound: {requests:?}"
    );
    // Reports are read, and orders sent, with what was remembered.
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 0],
    );
    until("off", || {
        run.value(LAMP, "state") == Some(Value::Bool(false))
    })
    .await;
    run.order_to(LAMP, "state", Value::Bool(true))
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_known_device_not_configured_is_configured() {
    let (ncp, run) = running_with(&store_with(lamp())).await;
    let _radio = FakeRadio::spawn(&ncp, lamp_answers());
    until("configured", || run.store().devices[LAMP].configured).await;
}

#[tokio::test]
async fn a_device_from_before_the_interviews_is_interviewed() {
    // In the store of the previous version: address, nothing more.
    let (ncp, run) = running_with(&store_with(store::DeviceRecord {
        ieee: LAMP.into(),
        nwk: Some(NWK),
        ..store::DeviceRecord::default()
    }))
    .await;
    let _radio = FakeRadio::spawn(&ncp, lamp_answers());
    until("interviewed", || run.store().devices[LAMP].interviewed).await;
    assert!(run.view(LAMP).unwrap().device.point("state").is_some());
}

#[tokio::test]
async fn an_unreadable_message_changes_nothing() {
    let (ncp, _radio, run) = lamp_on_network().await;
    // Cut short, an unknown type, a manufacturer's frame, from a stranger.
    from_device(&ncp, NWK, zcl::ON_OFF, &[0x18]);
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[0x18, 1, zcl::REPORT_ATTRIBUTES, 0, 0, 0xEE, 0],
    );
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[
            0x1C,
            0x34,
            0x12,
            1,
            zcl::REPORT_ATTRIBUTES,
            0,
            0,
            zcl::BOOLEAN,
            0,
        ],
    );
    from_device(
        &ncp,
        0x7777,
        zcl::ON_OFF,
        &[0x18, 1, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 0],
    );
    from_device(
        &ncp,
        NWK,
        zcl::LEVEL,
        &[0x18, 2, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::UINT8, 127],
    );
    until("the last one read", || {
        run.value(LAMP, "brightness") == Some(Value::Int(50))
    })
    .await;
    assert_eq!(
        run.value(LAMP, "state"),
        Some(Value::Bool(true)),
        "none of the others"
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_router_goes_offline_and_comes_back() {
    let mut known = lamp();
    known.configured = true;
    let (ncp, run) = running_with(&store_with(known)).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    until("its values", || run.value(LAMP, "state").is_some()).await;
    // Silent, but it answers: still there.
    tokio::time::sleep(Duration::from_secs(12 * 60)).await;
    assert_eq!(run.online(LAMP), Some(true));
    radio.deafen(NWK);
    tokio::time::sleep(Duration::from_secs(12 * 60)).await;
    until("offline", || run.online(LAMP) == Some(false)).await;
    // A word from it: back.
    radio.hear(NWK);
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 1],
    );
    until("online", || run.online(LAMP) == Some(true)).await;
}

/// A door sensor as an interview finds it.
fn door() -> store::DeviceRecord {
    store::DeviceRecord {
        ieee: OTHER.into(),
        nwk: Some(OTHER_NWK),
        interviewed: true,
        configured: true,
        capability: Some(0x80),
        logical_type: Some(2),
        zone_type: Some(0x0015),
        endpoints: vec![store::EndpointRecord {
            endpoint: 1,
            profile: zcl::HA_PROFILE,
            device_id: 0x0402,
            in_clusters: vec![zcl::BASIC, zcl::POWER_CONFIG, zcl::IAS_ZONE],
            out_clusters: Vec::new(),
        }],
        ..store::DeviceRecord::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_sleepy_device_is_gone_after_25_hours_without_a_word() {
    let (ncp, run) = running_with(&store_with(door())).await;
    assert_eq!(run.online(OTHER), Some(true), "nothing says otherwise");
    tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
    assert_eq!(run.online(OTHER), Some(true));
    tokio::time::sleep(Duration::from_secs(3600 + 120)).await;
    assert_eq!(run.online(OTHER), Some(false));
    from_device(
        &ncp,
        OTHER_NWK,
        zcl::POWER_CONFIG,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0x21, 0, zcl::UINT8, 200],
    );
    until("online", || run.online(OTHER) == Some(true)).await;
    assert_eq!(run.value(OTHER, "battery"), Some(Value::Int(100)));
}

#[tokio::test]
async fn a_door_sensor_enrolls_and_says_open_or_closed() {
    let (ncp, run) = running_network().await;
    let radio = FakeRadio::spawn(&ncp, sensor_answers());
    joins(&ncp, &run, OTHER_NWK, OTHER_BYTES, OTHER).await;
    let saved = run.store().devices[OTHER].clone();
    assert_eq!(saved.zone_type, Some(0x0015));
    assert!(run.view(OTHER).unwrap().device.point("contact").is_some());
    // It asks to enroll: accepted, with its number.
    from_device(
        &ncp,
        OTHER_NWK,
        zcl::IAS_ZONE,
        &[0x09, 5, zcl::ZONE_ENROLL_REQUEST, 0x15, 0x00, 0x34, 0x12],
    );
    until("accepted", || {
        radio
            .requests()
            .iter()
            .any(|request| request.payload == [0x11, 5, 0x00, 0x00, 23])
    })
    .await;
    // Open, then closed.
    from_device(
        &ncp,
        OTHER_NWK,
        zcl::IAS_ZONE,
        &[0x19, 6, zcl::ZONE_STATUS_CHANGE, 0x01, 0x00, 0x00, 23, 0, 0],
    );
    until("open", || {
        run.value(OTHER, "contact") == Some(Value::Bool(false))
    })
    .await;
    from_device(
        &ncp,
        OTHER_NWK,
        zcl::IAS_ZONE,
        &[0x19, 7, zcl::ZONE_STATUS_CHANGE, 0x00, 0x00, 0x00, 23, 0, 0],
    );
    until("closed", || {
        run.value(OTHER, "contact") == Some(Value::Bool(true))
    })
    .await;
}

/// How many attribute reads went out.
fn reads_sent(radio: &FakeRadio) -> usize {
    radio
        .requests()
        .iter()
        .filter(|request| request.profile != zdo::PROFILE && request.payload[2] == 0x00)
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_dead_router_is_read_once_per_silence() {
    let mut known = lamp();
    known.configured = true;
    let (ncp, run) = running_with(&store_with(known)).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    until("its values", || run.value(LAMP, "color_temp").is_some()).await;
    radio.deafen(NWK);
    let before = reads_sent(&radio);
    tokio::time::sleep(Duration::from_secs(35 * 60)).await;
    let sent = reads_sent(&radio) - before;
    assert!((1..=4).contains(&sent), "{sent} reads in 35 minutes");
    assert_eq!(run.online(LAMP), Some(false));
}

#[tokio::test(start_paused = true)]
async fn a_word_during_a_read_keeps_it_online() {
    let mut known = lamp();
    known.configured = true;
    let (ncp, run) = running_with(&store_with(known)).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    until("its values", || run.value(LAMP, "color_temp").is_some()).await;
    radio.deafen(NWK);
    let before = reads_sent(&radio);
    // Its silence reaches 10 minutes just after a look: the next one, a
    // minute later, asks.
    tokio::time::sleep(Duration::from_secs(11 * 60 + 3)).await;
    assert_eq!(reads_sent(&radio), before + 1, "its read is on its way");
    from_device(
        &ncp,
        NWK,
        zcl::ON_OFF,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 1],
    );
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(run.online(LAMP), Some(true), "that read's end says nothing");
}

#[tokio::test(start_paused = true)]
async fn a_default_response_to_a_read_is_an_answer() {
    let mut known = lamp();
    known.configured = true;
    let (ncp, run) = running_with(&store_with(known)).await;
    let _radio = FakeRadio::spawn(&ncp, |request| {
        Some(vec![
            0x18,
            request.payload[1],
            zcl::DEFAULT_RESPONSE,
            0x00,
            0x82,
        ])
    });
    tokio::time::sleep(Duration::from_secs(12 * 60)).await;
    assert_eq!(run.online(LAMP), Some(true));
}

#[tokio::test(start_paused = true)]
async fn a_repeater_is_read_too() {
    let mut repeater = lamp();
    repeater.configured = true;
    repeater.color = None;
    repeater.endpoints[0].in_clusters = vec![zcl::BASIC];
    let (ncp, run) = running_with(&store_with(repeater)).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    radio.deafen(NWK);
    tokio::time::sleep(Duration::from_secs(11 * 60)).await;
    assert!(
        radio
            .requests()
            .iter()
            .any(|request| request.cluster == zcl::BASIC),
        "something to ask it"
    );
    assert_eq!(run.online(LAMP), Some(false));
}

#[tokio::test(start_paused = true)]
async fn a_sleepy_device_missing_its_read_is_not_gone() {
    let (ncp, run) = running_network().await;
    let radio = FakeRadio::spawn(&ncp, sensor_answers());
    joins(&ncp, &run, OTHER_NWK, OTHER_BYTES, OTHER).await;
    // Back to sleep: its first values were asked too late.
    radio.deafen(OTHER_NWK);
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(run.online(OTHER), Some(true));
}

#[tokio::test(start_paused = true)]
async fn silences_go_on_across_a_restart() {
    let long_ago = store::DeviceRecord {
        silent_for: Some(26 * 3600),
        ..door()
    };
    let (_ncp, run) = running_with(&store_with(long_ago)).await;
    assert_eq!(run.online(OTHER), Some(false));
    let (_ncp, run) = running_with(&store_with(store::DeviceRecord {
        silent_for: Some(20 * 3600),
        ..door()
    }))
    .await;
    assert_eq!(run.online(OTHER), Some(true));
    // … and goes off when its 25 hours are up.
    tokio::time::sleep(Duration::from_secs(5 * 3600 + 120)).await;
    assert_eq!(run.online(OTHER), Some(false));
}

#[tokio::test(start_paused = true)]
async fn when_a_device_last_spoke_is_written_down() {
    let (ncp, run) = running_with(&store_with(door())).await;
    from_device(
        &ncp,
        OTHER_NWK,
        zcl::POWER_CONFIG,
        &[0x18, 9, zcl::REPORT_ATTRIBUTES, 0x21, 0, zcl::UINT8, 200],
    );
    until("read", || run.value(OTHER, "battery").is_some()).await;
    tokio::time::sleep(Duration::from_secs(3600 + 60)).await;
    until("written", || {
        run.store().devices[OTHER].silent_for.is_some()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_device_that_left_stays_off_after_a_restart() {
    let mut gone = lamp();
    gone.configured = true;
    gone.left = true;
    let (ncp, run) = running_with(&store_with(gone)).await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    assert_eq!(run.online(LAMP), Some(false));
    tokio::time::sleep(Duration::from_secs(30 * 60)).await;
    assert_eq!(radio.requests(), [], "nothing asked of it");
    assert_eq!(run.online(LAMP), Some(false));
}

#[tokio::test(start_paused = true)]
async fn a_device_leaving_before_its_interview_is_forgotten() {
    let (ncp, run) = running_network().await;
    let radio = FakeRadio::spawn(&ncp, lamp_answers());
    radio.lose_answers(usize::MAX);
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 1),
    );
    until("said", || run.warning().is_some()).await;
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 2),
    );
    until("forgotten", || run.view(LAMP).is_none()).await;
    assert!(!run.store().devices.contains_key(LAMP));
    assert_eq!(run.warning(), None, "nothing left to say");
    let before = radio.requests().len();
    tokio::time::sleep(Duration::from_secs(30 * 60)).await;
    assert_eq!(radio.requests().len(), before, "never tried again");
}

#[tokio::test]
async fn a_device_paired_again_is_asked_everything_again() {
    let (ncp, radio, run) = lamp_on_network().await;
    let asked_again = |from: usize| {
        radio.requests()[from..]
            .iter()
            .any(|request| request.cluster == zdo::NODE_DESCRIPTOR_REQUEST)
    };
    // A rejoin keeps what it knows: nothing to ask.
    let before = radio.requests().len();
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 0),
    );
    until("read again", || radio.requests().len() > before).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!asked_again(before));
    assert!(run.store().devices[LAMP].configured);
    // Reset, then paired again: it has forgotten its bindings and reports.
    let before = radio.requests().len();
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 1),
    );
    until("asked again", || asked_again(before)).await;
    until("bound again", || {
        radio.requests()[before..]
            .iter()
            .any(|request| request.cluster == zdo::BIND_REQUEST)
    })
    .await;
    until("settled", || {
        let device = run.store().devices[LAMP].clone();
        device.interviewed && device.configured
    })
    .await;
    assert!(run.view(LAMP).unwrap().device.point("state").is_some());
    assert_eq!(run.online(LAMP), Some(true));
}

#[tokio::test]
async fn a_broadcast_is_never_answered() {
    let (ncp, radio, run) = lamp_on_network().await;
    let before = radio.requests().len();
    let mut report = incoming_from(
        NWK,
        zcl::HA_PROFILE,
        zcl::LEVEL,
        1,
        &[0x08, 10, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::UINT8, 127],
    );
    report[0] = 0x04;
    ncp.callback(ids::INCOMING_MESSAGE_HANDLER, &report);
    until("read", || {
        run.value(LAMP, "brightness") == Some(Value::Int(50))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(radio.requests().len(), before);
}

#[tokio::test]
async fn a_network_back_is_read_again() {
    let (ncp, radio, run) = lamp_on_network().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
    run.wait_point("network", &text("down")).await;
    let before = reads_sent(&radio);
    ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
    until("read again", || reads_sent(&radio) > before).await;
}

#[tokio::test]
async fn a_device_that_leaves_shows_offline_and_stays_known() {
    let (ncp, radio, run) = lamp_on_network().await;
    assert_eq!(run.online(LAMP), Some(true));
    ncp.callback(
        ids::TRUST_CENTER_JOIN_HANDLER,
        &trust_center_join(NWK, LAMP_BYTES, 2),
    );
    until("offline", || run.online(LAMP) == Some(false)).await;
    assert!(run.store().devices.contains_key(LAMP));
    assert!(run.view(LAMP).unwrap().device.point("state").is_some());
    // It comes back at another address: nothing to ask it again, and orders
    // follow it there.
    ncp.callback(
        ids::INCOMING_MESSAGE_HANDLER,
        &device_announce(0x5678, LAMP_BYTES),
    );
    until("online", || run.online(LAMP) == Some(true)).await;
    assert_eq!(run.store().devices[LAMP].nwk, Some(0x5678));
    assert!(run.store().devices[LAMP].interviewed);
    assert!(!run.store().devices[LAMP].left);
    run.order_to(LAMP, "state", Value::Bool(false))
        .await
        .unwrap();
    assert_eq!(radio.requests().last().unwrap().nwk, 0x5678);
    // What it says from there is read; from its old address, nothing is.
    from_device(
        &ncp,
        0x5678,
        zcl::LEVEL,
        &[0x18, 1, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::UINT8, 127],
    );
    until("half", || {
        run.value(LAMP, "brightness") == Some(Value::Int(50))
    })
    .await;
    from_device(
        &ncp,
        NWK,
        zcl::LEVEL,
        &[0x18, 2, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::UINT8, 254],
    );
    from_device(
        &ncp,
        0x5678,
        zcl::ON_OFF,
        &[0x18, 3, zcl::REPORT_ATTRIBUTES, 0, 0, zcl::BOOLEAN, 1],
    );
    until("on", || run.value(LAMP, "state") == Some(Value::Bool(true))).await;
    assert_eq!(run.value(LAMP, "brightness"), Some(Value::Int(50)));
}

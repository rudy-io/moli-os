use moli_core::{DriverStatus, InstanceId, Origin, PointId, Value};
use moli_homeassistant::{Config, HomeAssistant};
use moli_runtime::{Hub, HubOptions, MasterKey, spawn_driver};
use serde_json::{Value as Json, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    hub: Hub,
    cancel: CancellationToken,
    server: tokio::task::JoinHandle<()>,
    requests: mpsc::UnboundedReceiver<String>,
    raw: Arc<Mutex<(u16, Json)>>,
    configuration: Arc<Mutex<(u16, Json)>>,
    path: std::path::PathBuf,
    latency: Arc<AtomicU64>,
    supervisor: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.server.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn start(writable: &[&str], endpoint_ok: bool, status: u16) -> Fixture {
    start_with_timeout(writable, endpoint_ok, status, Duration::from_secs(5)).await
}

async fn start_with_timeout(
    writable: &[&str],
    endpoint_ok: bool,
    status: u16,
    command_timeout: Duration,
) -> Fixture {
    start_with_source(
        writable,
        endpoint_ok,
        status,
        command_timeout,
        json!({"entity_id":"switch.example","state":"off","attributes":{}}),
    )
    .await
}

async fn start_with_source(
    writable: &[&str],
    endpoint_ok: bool,
    status: u16,
    command_timeout: Duration,
    source: Json,
) -> Fixture {
    start_with_configuration(
        writable,
        endpoint_ok,
        status,
        command_timeout,
        source,
        (200, json!({})),
    )
    .await
}

async fn start_with_configuration(
    writable: &[&str],
    endpoint_ok: bool,
    status: u16,
    command_timeout: Duration,
    source: Json,
    configuration: (u16, Json),
) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let entity_id = source["entity_id"].as_str().unwrap();
    let domain = entity_id.split_once('.').unwrap().0;
    let config:Config=serde_json::from_value(json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"transport":"http","poll_seconds":5,"entities":[{"native_id":"stable","entity_id":entity_id,"domain":domain,"room":"Workshop","safety":"ordinary","writable":writable}]})).unwrap();
    let raw = Arc::new(Mutex::new((status, source)));
    let (tx, requests) = mpsc::unbounded_channel();
    let response = Arc::clone(&raw);
    let latency = Arc::new(AtomicU64::new(0));
    let server_latency = Arc::clone(&latency);
    let configuration = Arc::new(Mutex::new(configuration));
    let server = launch_server(
        listener,
        response,
        server_latency,
        tx,
        Arc::clone(&configuration),
    );
    let path = std::env::temp_dir().join(format!(
        "moli-ha-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let hub = Hub::new(HubOptions {
        secrets: Some((path.clone(), MasterKey::new("01".repeat(32)))),
        command_timeout,
        ..HubOptions::default()
    })
    .unwrap();
    hub.store_secret(
        &InstanceId::from("bridge"),
        "token",
        "synthetic-private-token",
    )
    .await
    .unwrap();
    hub.store_secret(
        &InstanceId::from("bridge"),
        "token_endpoint",
        if endpoint_ok {
            config.token_endpoint()
        } else {
            "wrong endpoint".into()
        }
        .as_str(),
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let supervisor = spawn_driver(
        &hub,
        InstanceId::from("bridge"),
        Arc::new(HomeAssistant::new(config).unwrap()),
        cancel.clone(),
    );
    Fixture {
        hub,
        cancel,
        server,
        requests,
        raw,
        configuration,
        path,
        latency,
        supervisor,
    }
}

fn launch_server(
    listener: TcpListener,
    response: Arc<Mutex<(u16, Json)>>,
    server_latency: Arc<AtomicU64>,
    tx: mpsc::UnboundedSender<String>,
    configuration: Arc<Mutex<(u16, Json)>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            let response = Arc::clone(&response);
            let latency = Arc::clone(&server_latency);
            let configuration = Arc::clone(&configuration);
            tokio::spawn(async move {
                let mut buf = vec![0; 8192];
                let mut used = 0;
                loop {
                    let n = socket.read(&mut buf[used..]).await.unwrap();
                    used += n;
                    let text = String::from_utf8_lossy(&buf[..used]);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if body.len() >= len {
                            break;
                        }
                    }
                    assert!(n > 0 && used < 8192);
                }
                let text = String::from_utf8(buf[..used].to_vec()).unwrap();
                let (status, body) = if text.starts_with("GET /api/config HTTP/1.1\r\n") {
                    {
                        let guard = configuration.lock().unwrap();
                        (guard.0, guard.1.to_string())
                    }
                } else {
                    let guard = response.lock().unwrap();
                    (
                        guard.0,
                        if text.starts_with("GET ") {
                            guard.1.to_string()
                        } else {
                            "[]".into()
                        },
                    )
                };
                let _ = tx.send(text);
                tokio::time::sleep(Duration::from_millis(latency.load(Ordering::Relaxed))).await;
                let reply = format!(
                    "HTTP/1.1 {status} Example\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    })
}

async fn wait_value(f: &Fixture) {
    for _ in 0..200 {
        if f.hub.state(&PointId::from("bridge:stable/on")).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no state: {:?}", f.hub.snapshot().drivers);
}

#[tokio::test]
async fn hub_routes_only_selected_entity_and_does_not_fabricate_write_confirmation() {
    let mut f = start(&["on"], true, 200).await;
    wait_value(&f).await;
    let get = f.requests.recv().await.unwrap();
    assert!(get.starts_with("GET /api/states/switch.example HTTP/1.1"));
    let view = f.hub.snapshot().devices.into_iter().next().unwrap();
    assert_eq!(view.device.model.as_deref(), Some("Home Assistant switch"));
    f.hub
        .command(
            &PointId::from("bridge:stable/on"),
            Value::Bool(true),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
    let post = f.requests.recv().await.unwrap();
    assert!(post.starts_with("POST /api/services/switch/turn_on HTTP/1.1"));
    assert_eq!(
        post.split_once("\r\n\r\n").unwrap().1,
        "{\"entity_id\":\"switch.example\"}"
    );
    assert_eq!(
        f.hub
            .state(&PointId::from("bridge:stable/on"))
            .unwrap()
            .value,
        Value::Bool(false)
    );
    assert!(
        f.hub
            .command(
                &PointId::from("bridge:stable/on"),
                Value::from("toggle"),
                Origin::Ui,
                None
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), f.requests.recv())
            .await
            .is_err()
    );
    assert!(!format!("{:?}", f.hub.journal(10)).contains("synthetic-private-token"));
}

#[tokio::test]
async fn read_only_hub_refuses_before_transport() {
    let mut f = start(&[], true, 200).await;
    wait_value(&f).await;
    f.requests.recv().await.unwrap();
    assert!(
        f.hub
            .command(
                &PointId::from("bridge:stable/on"),
                Value::Bool(true),
                Origin::Ui,
                None
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), f.requests.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn endpoint_change_prevents_any_token_transfer_and_auth_failure_parks() {
    let mut f = start(&["on"], false, 200).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), f.requests.recv())
            .await
            .is_err()
    );
    assert!(matches!(
        f.hub.snapshot().drivers[0].status,
        DriverStatus::Waiting { .. }
    ));
    let mut f = start(&["on"], true, 401).await;
    tokio::time::timeout(Duration::from_secs(2), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(
        f.hub.snapshot().drivers[0].status,
        DriverStatus::Waiting { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), f.requests.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unavailable_entity_goes_offline_refuses_write_and_recovers_by_poll() {
    let mut f = start(&["on"], true, 200).await;
    wait_value(&f).await;
    f.requests.recv().await.unwrap();
    f.raw.lock().unwrap().1["state"] = json!("unavailable");
    tokio::time::timeout(Duration::from_secs(6), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.hub.snapshot().devices[0].online, Some(false));
    assert!(
        f.hub
            .command(
                &PointId::from("bridge:stable/on"),
                Value::Bool(true),
                Origin::Ui,
                None
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), f.requests.recv())
            .await
            .is_err()
    );
    f.raw.lock().unwrap().1["state"] = json!("on");
    tokio::time::timeout(Duration::from_secs(6), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.hub.snapshot().devices[0].online, Some(true));
    assert_eq!(
        f.hub
            .state(&PointId::from("bridge:stable/on"))
            .unwrap()
            .value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn expired_queued_write_is_never_replayed_after_slow_poll() {
    let mut f = start_with_timeout(&["on"], true, 200, Duration::from_millis(50)).await;
    wait_value(&f).await;
    f.requests.recv().await.unwrap();
    f.latency.store(250, Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(6), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        f.hub
            .command(
                &PointId::from("bridge:stable/on"),
                Value::Bool(true),
                Origin::Ui,
                None
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(400), f.requests.recv())
            .await
            .is_err()
    );
    f.cancel.cancel();
    tokio::time::timeout(Duration::from_millis(200), &mut f.supervisor)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn missing_vault_stays_waiting_without_network() {
    let config:Config=serde_json::from_value(json!({"host":"127.0.0.1","port":9,"transport":"http","entities":[{"native_id":"one","entity_id":"switch.example","domain":"switch","room":"Workshop","safety":"ordinary"}]})).unwrap();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    let mut handle = spawn_driver(
        &hub,
        InstanceId::from("bridge"),
        Arc::new(HomeAssistant::new(config).unwrap()),
        cancel.clone(),
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(matches!(
        hub.snapshot().drivers[0].status,
        DriverStatus::Waiting { .. }
    ));
    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(200), &mut handle)
        .await
        .unwrap()
        .unwrap();
}

async fn wait_reading(f: &Fixture, key: &str, expected: Value) {
    let point = PointId::from(format!("bridge:stable/{key}"));
    for _ in 0..200 {
        if f.hub
            .state(&point)
            .is_some_and(|sample| sample.value == expected)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {expected:?} at {point}, found {:?}",
        f.hub.state(&point)
    );
}

async fn next_poll(f: &mut Fixture) {
    let request = tokio::time::timeout(Duration::from_secs(6), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(request.starts_with("GET /api/states/"));
}

#[tokio::test]
async fn cover_position_is_cleared_when_next_available_snapshot_has_null_then_recovers() {
    let mut f=start_with_source(&["control","percent_control"],true,200,Duration::from_secs(5),
        json!({"entity_id":"cover.example","state":"open","attributes":{"device_class":"shutter","supported_features":15,"current_position":75}})).await;
    wait_reading(&f, "percent_control", Value::Float(75.0)).await;
    next_poll(&mut f).await;
    assert!(
        f.hub
            .state(&PointId::from("bridge:stable/control"))
            .is_none()
    );
    f.raw.lock().unwrap().1["attributes"]["current_position"] = Json::Null;
    next_poll(&mut f).await;
    wait_reading(&f, "percent_control", Value::Null).await;
    assert_eq!(f.hub.snapshot().devices[0].online, Some(true));
    assert!(
        f.hub
            .state(&PointId::from("bridge:stable/control"))
            .is_none()
    );
    f.hub
        .command(
            &PointId::from("bridge:stable/percent_control"),
            Value::Float(25.0),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
    let post = f.requests.recv().await.unwrap();
    assert!(post.starts_with("POST /api/services/cover/set_cover_position HTTP/1.1"));
    assert_eq!(
        f.hub
            .state(&PointId::from("bridge:stable/percent_control"))
            .unwrap()
            .value,
        Value::Null
    );
    f.raw.lock().unwrap().1["attributes"]["current_position"] = json!(25);
    next_poll(&mut f).await;
    wait_reading(&f, "percent_control", Value::Float(25.0)).await;
    assert_eq!(f.hub.snapshot().devices[0].online, Some(true));
}

#[tokio::test]
async fn light_missing_invalid_and_null_readings_replace_previous_available_values() {
    let mut f=start_with_source(&[],true,200,Duration::from_secs(5),
        json!({"entity_id":"light.example","state":"on","attributes":{"supported_color_modes":["rgb","color_temp"],"brightness":255,"rgb_color":[255,0,128],"color_temp_kelvin":3000,"min_color_temp_kelvin":2000,"max_color_temp_kelvin":6500}})).await;
    wait_reading(&f, "brightness", Value::Float(100.0)).await;
    wait_reading(&f, "color", Value::from("#ff0080")).await;
    wait_reading(&f, "color_temp", Value::Float(3000.0)).await;
    next_poll(&mut f).await;
    {
        let mut raw = f.raw.lock().unwrap();
        raw.1["attributes"]["brightness"] = json!("invalid");
        raw.1["attributes"]
            .as_object_mut()
            .unwrap()
            .remove("rgb_color");
        raw.1["attributes"]["color_temp_kelvin"] = Json::Null;
    }
    next_poll(&mut f).await;
    for key in ["brightness", "color", "color_temp"] {
        wait_reading(&f, key, Value::Null).await;
    }
    assert_eq!(f.hub.snapshot().devices[0].online, Some(true));
    {
        let mut raw = f.raw.lock().unwrap();
        raw.1["attributes"]["brightness"] = json!(0);
        raw.1["attributes"]["rgb_color"] = json!([0, 0, 0]);
        raw.1["attributes"]["color_temp_kelvin"] = json!(4000);
    }
    next_poll(&mut f).await;
    wait_reading(&f, "brightness", Value::Float(0.0)).await;
    wait_reading(&f, "color", Value::from("#000000")).await;
    wait_reading(&f, "color_temp", Value::Float(4000.0)).await;
}

#[tokio::test]
async fn climate_uses_global_temperature_unit_without_enabling_commands() {
    let mut f = start_with_configuration(
        &[],
        true,
        200,
        Duration::from_secs(5),
        json!({"entity_id":"climate.example","state":"heat","attributes":{
            "current_temperature":20,"temperature":22,"min_temp":7,"max_temp":35,
            "target_temp_step":0.5,"supported_features":1,"hvac_modes":["off","heat"]
        }}),
        (200, json!({"unit_system":{"temperature":"°C"}})),
    )
    .await;
    wait_reading(&f, "temperature", Value::Float(20.0)).await;
    wait_reading(&f, "target_temperature", Value::Float(22.0)).await;
    assert!(
        f.hub.snapshot().devices[0]
            .device
            .points
            .iter()
            .all(|point| !point.access.write)
    );
    let config = f.requests.recv().await.unwrap();
    assert!(config.starts_with("GET /api/config HTTP/1.1\r\n"));
    assert!(
        config
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-private-token")
    );
    next_poll(&mut f).await;
    assert!(
        f.hub
            .command(
                &PointId::from("bridge:stable/target_temperature"),
                Value::Float(23.0),
                Origin::Ui,
                None
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), f.requests.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn climate_refreshes_global_unit_and_removes_temperatures_when_it_becomes_invalid() {
    let mut f = start_with_configuration(
        &[],
        true,
        200,
        Duration::from_secs(5),
        json!({"entity_id":"climate.example","state":"heat","attributes":{
            "current_temperature":20,"temperature":22,"min_temp":7,"max_temp":35}}),
        (200, json!({"unit_system":{"temperature":"°C"}})),
    )
    .await;
    wait_reading(&f, "temperature", Value::Float(20.0)).await;
    f.requests.recv().await.unwrap();
    next_poll(&mut f).await;
    f.configuration.lock().unwrap().1 = json!({"unit_system":{"temperature":"°F"}});
    f.raw.lock().unwrap().1["attributes"] =
        json!({"current_temperature":77,"temperature":68,"min_temp":50,"max_temp":86});
    let config = tokio::time::timeout(Duration::from_secs(6), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(config.starts_with("GET /api/config HTTP/1.1\r\n"));
    next_poll(&mut f).await;
    wait_reading(&f, "temperature", Value::Float(25.0)).await;
    f.configuration.lock().unwrap().1 = json!({"unit_system":{"temperature":"K"}});
    let config = tokio::time::timeout(Duration::from_secs(6), f.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(config.starts_with("GET /api/config HTTP/1.1\r\n"));
    next_poll(&mut f).await;
    for _ in 0..200 {
        if !f.hub.snapshot().devices[0]
            .device
            .points
            .iter()
            .any(|p| p.key.as_ref() == "temperature")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("invalid global unit retained temperature points");
}

#[tokio::test]
async fn climate_config_auth_rejection_parks_without_polling_entity_states() {
    for status in [401, 403] {
        let mut f = start_with_configuration(
            &[],
            true,
            200,
            Duration::from_secs(5),
            json!({"entity_id":"climate.example","state":"heat","attributes":{}}),
            (status, json!({"unit_system":{"temperature":"°C"}})),
        )
        .await;
        let request = tokio::time::timeout(Duration::from_secs(2), f.requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(request.starts_with("GET /api/config HTTP/1.1\r\n"));
        for _ in 0..200 {
            if matches!(
                f.hub.snapshot().drivers[0].status,
                DriverStatus::Waiting { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(matches!(
            f.hub.snapshot().drivers[0].status,
            DriverStatus::Waiting { .. }
        ));
        assert!(f.hub.snapshot().devices.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(80), f.requests.recv())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn climate_config_failure_keeps_explicit_entity_unit_without_guessing_missing_units() {
    for explicit in [false, true] {
        let mut source = json!({"entity_id":"climate.example","state":"heat","attributes":{"current_temperature":21}});
        if explicit {
            source["attributes"]["temperature_unit"] = json!("°C");
        }
        let f = start_with_configuration(
            &[],
            true,
            200,
            Duration::from_secs(5),
            source,
            (503, json!({})),
        )
        .await;
        wait_value(&f).await;
        if explicit {
            wait_reading(&f, "temperature", Value::Float(21.0)).await;
        } else {
            assert!(
                f.hub
                    .state(&PointId::from("bridge:stable/temperature"))
                    .is_none()
            );
        }
        assert_eq!(f.hub.snapshot().devices[0].online, Some(true));
    }
}

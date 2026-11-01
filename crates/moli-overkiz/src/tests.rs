use super::*;
use moli_core::{InstanceId, Origin, PointId, Value};
use moli_runtime::{Hub, HubOptions, MasterKey, spawn_driver};
use rustls::pki_types::PrivatePkcs8KeyDer;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
fn config() -> Config {
    Config {
        host: "gateway-example.local".into(),
        port: 8443,
        poll_secs: 5,
        cert_sha256: None,
        writable_openers: vec![],
    }
}
#[test]
fn construction_is_pure_and_rejects_invalid_config() {
    assert!(Overkiz::new(config()).is_ok());
    for host in [
        "",
        "https://example.org",
        "other/path",
        "user@example.org",
        "example\n.org",
        "foo:8443",
    ] {
        assert!(
            Overkiz::new(Config {
                host: host.into(),
                ..config()
            })
            .is_err()
        );
    }
    assert!(
        Overkiz::new(Config {
            port: 0,
            ..config()
        })
        .is_err()
    );
    assert!(
        Overkiz::new(Config {
            poll_secs: 0,
            ..config()
        })
        .is_err()
    );
    assert!(
        Overkiz::new(Config {
            cert_sha256: Some("bad".into()),
            ..config()
        })
        .is_err()
    );
}
async fn fixture_server(
    hub: &Hub,
    instance: &InstanceId,
    posts: Arc<AtomicUsize>,
    status: Arc<AtomicUsize>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.cert.der().clone()], key.into())
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let observed = hub.clone();
    let native_instance = instance.clone();
    let server = tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(socket).await.unwrap();
            let mut data = vec![0; 8192];
            let Ok(n) = stream.read(&mut data).await else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let request = String::from_utf8_lossy(&data[..n]);
            assert!(request.contains("Bearer synthetic-test-token"));
            assert!(
                observed.secret(&native_instance, "cert_sha256").is_some(),
                "no credential before pin persisted"
            );
            let response = if request
                .starts_with("GET /enduser-mobile-web/1/enduserAPI/setup/devices ")
            {
                let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
                    "../../../integrations/overkiz/fixtures/devices.json"
                ))
                .unwrap();
                if status.load(Ordering::SeqCst) > 0 {
                    fixture[0]["available"] = serde_json::json!(false);
                }
                fixture.to_string()
            } else {
                assert!(request.starts_with("POST /enduser-mobile-web/1/enduserAPI/exec/apply "));
                posts.fetch_add(1, Ordering::SeqCst);
                "{\"execId\":\"example\"}".to_owned()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = stream.shutdown().await;
        }
    });
    (port, server)
}
#[tokio::test]
async fn real_driver_pins_before_token_and_refreshes_before_every_write() {
    let instance = InstanceId::from("overkiz-test");
    let path = std::env::temp_dir().join(format!(
        "overkiz-{}.enc",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hub = Hub::new(HubOptions {
        secrets: Some((path.clone(), MasterKey::new("a".repeat(64)))),
        ..HubOptions::default()
    })
    .unwrap();
    hub.store_secret(&instance, "token", "synthetic-test-token")
        .await
        .unwrap();
    let applies = Arc::new(AtomicUsize::new(0));
    let unavailable = Arc::new(AtomicUsize::new(0));
    let (port, server) = fixture_server(
        &hub,
        &instance,
        Arc::clone(&applies),
        Arc::clone(&unavailable),
    )
    .await;
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(
            Overkiz::new(Config {
                host: "localhost".into(),
                port,
                poll_secs: 300,
                cert_sha256: None,
                writable_openers: vec![],
            })
            .unwrap(),
        ),
        cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while hub.stats().drivers_running != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let entry = model::parse(
        &serde_json::from_str(include_str!(
            "../../../integrations/overkiz/fixtures/devices.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let point = PointId::new(&entry[0].device(&instance).id, "percent_control");
    assert_eq!(hub.state(&point).unwrap().value, Value::Float(75.0));
    hub.command(&point, Value::Float(60.0), Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    assert_eq!(
        hub.state(&point).unwrap().value,
        Value::Float(75.0),
        "acceptance must not fabricate position"
    );
    unavailable.store(1, Ordering::SeqCst);
    assert!(
        hub.command(&point, Value::Float(60.0), Origin::Ui, None)
            .await
            .is_err()
    );
    assert_eq!(
        applies.load(Ordering::SeqCst),
        1,
        "fresh unavailable snapshot forbids second write"
    );
    cancel.cancel();
    server.abort();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let _ = std::fs::remove_file(path);
}
#[tokio::test]
async fn missing_token_waits_for_human_without_network() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    let task = spawn_driver(
        &hub,
        InstanceId::from("overkiz-test"),
        Arc::new(Overkiz::new(config()).unwrap()),
        cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if hub
                .snapshot()
                .drivers
                .iter()
                .any(|d| matches!(d.status, moli_core::DriverStatus::Waiting { .. }))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(hub.snapshot().devices.is_empty());
    assert!(!task.is_finished());
    cancel.cancel();
    task.await.unwrap();
}
#[derive(Debug)]
struct Rig {
    hub: Hub,
    instance: InstanceId,
    path: std::path::PathBuf,
    snapshot: Arc<std::sync::Mutex<serde_json::Value>>,
    posts: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
    preflight_delay: Arc<std::sync::atomic::AtomicU64>,
    payloads: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    cancel: CancellationToken,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.server.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}
async fn test_vault(command_timeout: Duration) -> (std::path::PathBuf, Hub, InstanceId) {
    let path = std::env::temp_dir().join(format!(
        "overkiz-rig-{}.enc",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hub = Hub::new(HubOptions {
        secrets: Some((path.clone(), MasterKey::new("b".repeat(64)))),
        command_timeout,
        ..HubOptions::default()
    })
    .unwrap();
    let instance = InstanceId::from("overkiz-test");
    hub.store_secret(&instance, "token", "synthetic-test-token")
        .await
        .unwrap();
    (path, hub, instance)
}
fn tls_acceptor() -> TlsAcceptor {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.cert.der().clone()], key.into())
    .unwrap();
    TlsAcceptor::from(Arc::new(tls))
}
impl Rig {
    async fn start(
        snapshot: serde_json::Value,
        writable_openers: Vec<ReviewedOpener>,
        command_timeout: Duration,
    ) -> Self {
        let (path, hub, instance) = test_vault(command_timeout).await;
        let snapshot = Arc::new(std::sync::Mutex::new(snapshot));
        let posts = Arc::new(AtomicUsize::new(0));
        let gets = Arc::new(AtomicUsize::new(0));
        let preflight_delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let payloads = Arc::new(std::sync::Mutex::new(vec![]));
        let acceptor = tls_acceptor();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = (
            hub.clone(),
            instance.clone(),
            Arc::clone(&snapshot),
            Arc::clone(&posts),
            Arc::clone(&gets),
            Arc::clone(&preflight_delay),
            Arc::clone(&payloads),
        );
        let server = tokio::spawn(async move {
            let (hub, instance, snapshot, posts, gets, delay, payloads) = shared;
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    continue;
                };
                let Some(request) = read_request(&mut stream).await else {
                    continue;
                };
                assert!(request.contains("Bearer synthetic-test-token"));
                assert!(hub.secret(&instance, "cert_sha256").is_some());
                let response = if request
                    .starts_with("GET /enduser-mobile-web/1/enduserAPI/setup/devices ")
                {
                    gets.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(delay.load(Ordering::SeqCst))).await;
                    snapshot.lock().unwrap().to_string()
                } else {
                    assert!(
                        request.starts_with("POST /enduser-mobile-web/1/enduserAPI/exec/apply ")
                    );
                    let (_, body) = request.split_once("\r\n\r\n").unwrap();
                    payloads
                        .lock()
                        .unwrap()
                        .push(serde_json::from_str(body).unwrap());
                    posts.fetch_add(1, Ordering::SeqCst);
                    "{\"execId\":\"example\"}".to_owned()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            instance.clone(),
            Arc::new(
                Overkiz::new(Config {
                    host: "localhost".into(),
                    port,
                    poll_secs: 300,
                    cert_sha256: None,
                    writable_openers,
                })
                .unwrap(),
            ),
            cancel.clone(),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while hub.stats().drivers_running != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        Self {
            hub,
            instance,
            path,
            snapshot,
            posts,
            gets,
            preflight_delay,
            payloads,
            cancel,
            server,
        }
    }
    fn point(&self, index: usize, key: &str) -> PointId {
        let entries = model::parse(&self.snapshot.lock().unwrap()).unwrap();
        PointId::new(
            &DeviceId::new(&self.instance, &entries[index].native()),
            key,
        )
    }
}
async fn read_request(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> Option<String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = stream.read(&mut buffer).await.ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..n]);
        if bytes.len() > 8192 {
            return None;
        }
        let request = std::str::from_utf8(&bytes).ok()?;
        if let Some((headers, _)) = request.split_once("\r\n\r\n") {
            let content = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= headers.len() + 4 + content {
                return Some(request.to_owned());
            }
        }
    }
}

fn opener_fixture() -> serde_json::Value {
    let mut raw: serde_json::Value = serde_json::from_str(include_str!(
        "../../../integrations/overkiz/fixtures/devices.json"
    ))
    .unwrap();
    raw[2]["definition"]["commands"] = serde_json::json!([
        {"commandName":"open","nparams":0}, {"commandName":"close","nparams":0},
        {"commandName":"stop","nparams":0}, {"commandName":"setClosure","nparams":1}
    ]);
    raw[2]["states"] = serde_json::json!([{"name":"core:ClosureState","value":30}]);
    raw
}
fn gate_policy() -> Vec<ReviewedOpener> {
    vec![ReviewedOpener {
        device_url: "io://example/789".into(),
        ui_class: "Gate".into(),
    }]
}
#[tokio::test]
async fn reviewed_openers_require_human_approval_for_every_write_path() {
    let rig = Rig::start(opener_fixture(), gate_policy(), Duration::from_secs(5)).await;
    let mut held = None;
    for origin in [Origin::Api, Origin::Mcp, Origin::Household] {
        for (key, value) in [
            ("control", Value::from("open")),
            ("percent_control", Value::Float(40.0)),
        ] {
            let point = rig.point(2, key);
            let result = rig.hub.command(&point, value, origin, None).await;
            let Err(moli_runtime::CommandError::NeedsApproval { id, .. }) = result else {
                panic!("unguarded opener: {result:?}");
            };
            held = Some((id, point));
        }
    }
    assert_eq!(rig.posts.load(Ordering::SeqCst), 0);
    rig.snapshot.lock().unwrap()[2]["label"] = serde_json::json!("Renamed blind");
    let (id, point) = held.unwrap();
    rig.hub
        .resolve_approval(id, &point, true, Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(rig.posts.load(Ordering::SeqCst), 1);
    assert_eq!(rig.hub.state(&point).unwrap().value, Value::Float(70.0));
}
#[tokio::test]
async fn changed_opener_class_or_capabilities_forbid_fresh_write() {
    for mutation in ["class", "capabilities"] {
        let rig = Rig::start(opener_fixture(), gate_policy(), Duration::from_secs(5)).await;
        {
            let mut raw = rig.snapshot.lock().unwrap();
            if mutation == "class" {
                raw[2]["definition"]["uiClass"] = serde_json::json!("RollerShutter");
            } else {
                raw[2]["definition"]["commands"] =
                    serde_json::json!([{"commandName":"stop","nparams":0}]);
            }
        }
        assert!(
            rig.hub
                .command(
                    &rig.point(2, "control"),
                    Value::from("open"),
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
        );
        assert_eq!(rig.posts.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn long_native_url_routes_through_the_actual_hub() {
    let mut raw = opener_fixture();
    let url = format!("io://example/{}", "a".repeat(450));
    raw[0]["deviceURL"] = serde_json::json!(url);
    let rig = Rig::start(raw, vec![], Duration::from_secs(5)).await;
    let point = rig.point(0, "control");
    assert!(point.as_str().len() < 256);
    rig.hub
        .command(&point, Value::from("open"), Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(rig.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.payloads.lock().unwrap()[0]["actions"][0]["deviceURL"],
        url
    );
}
#[tokio::test]
async fn preflight_exhaustion_or_caller_cancellation_never_posts() {
    for cancel_caller in [false, true] {
        let rig = Rig::start(
            opener_fixture(),
            vec![],
            if cancel_caller {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(200)
            },
        )
        .await;
        rig.preflight_delay.store(500, Ordering::SeqCst);
        let hub = rig.hub.clone();
        let point = rig.point(0, "control");
        let command = tokio::spawn(async move {
            hub.command(&point, Value::from("open"), Origin::Ui, None)
                .await
        });
        while rig.gets.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        if cancel_caller {
            command.abort();
        } else {
            assert!(command.await.unwrap().is_err());
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(rig.posts.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn opener_review_configuration_is_explicit_and_bounded() {
    for ui_class in ["Unknown", "RollerShutter", "OpenCloseGate4T"] {
        assert!(
            Overkiz::new(Config {
                writable_openers: vec![ReviewedOpener {
                    device_url: "io://example/789".into(),
                    ui_class: ui_class.into()
                }],
                ..config()
            })
            .is_err()
        );
    }
    assert!(
        Overkiz::new(Config {
            writable_openers: vec![gate_policy().remove(0), gate_policy().remove(0)],
            ..config()
        })
        .is_err()
    );
    assert!(
        Overkiz::new(Config {
            writable_openers: vec![ReviewedOpener {
                device_url: "https://example.org".into(),
                ui_class: "Gate".into()
            }],
            ..config()
        })
        .is_err()
    );
}

//! The driver against a fake Proxmox over TLS: a self-signed certificate,
//! GET requests with the token header, the answers of the fixtures.
use super::*;
use moli_core::{DriverStatus, InstanceId, PointId, Value};
use moli_runtime::{Hub, HubOptions, MasterKey, spawn_driver};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

const TOKEN_ID: &str = "moli@pve!moli";
const TOKEN_SECRET: &str = "00000000-1111-2222-3333-444444444444";

#[derive(Default)]
struct Pve {
    /// Every request head received, in order.
    heads: Mutex<Vec<String>>,
    /// The token is refused (401).
    refuse: AtomicBool,
    /// The faulty fixtures: a container stopped that should run, a full one.
    faulty: AtomicBool,
}

impl Pve {
    fn paths(&self, needle: &str) -> usize {
        self.heads
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.lines().next().is_some_and(|l| l.contains(needle)))
            .count()
    }

    fn answer(&self, head: &str) -> (u16, &'static str) {
        self.heads.lock().unwrap().push(head.to_owned());
        if self.refuse.load(Ordering::SeqCst) {
            return (401, r#"{"data":null}"#);
        }
        let path = head.split_whitespace().nth(1).unwrap_or_default();
        let faulty = self.faulty.load(Ordering::SeqCst);
        let body = match path {
            "/api2/json/nodes" => include_str!("../../../integrations/proxmox/fixtures/nodes.json"),
            "/api2/json/nodes/pve/status" => {
                include_str!("../../../integrations/proxmox/fixtures/status.json")
            }
            "/api2/json/nodes/pve/lxc" if faulty => {
                include_str!("../../../integrations/proxmox/fixtures/lxc-faulty.json")
            }
            "/api2/json/nodes/pve/lxc" => {
                include_str!("../../../integrations/proxmox/fixtures/lxc.json")
            }
            "/api2/json/nodes/pve/qemu" => r#"{"data":[]}"#,
            "/api2/json/nodes/pve/lxc/101/config" => {
                include_str!("../../../integrations/proxmox/fixtures/lxc-config.json")
            }
            "/api2/json/nodes/pve/storage" => {
                include_str!("../../../integrations/proxmox/fixtures/storage.json")
            }
            "/api2/json/nodes/pve/tasks?typefilter=vzdump&source=all&limit=20" => {
                include_str!("../../../integrations/proxmox/fixtures/tasks.json")
            }
            "/api2/json/nodes/pve/disks/list" => {
                include_str!("../../../integrations/proxmox/fixtures/disks.json")
            }
            "/api2/json/nodes/pve/apt/update" => {
                include_str!("../../../integrations/proxmox/fixtures/apt.json")
            }
            _ => return (404, r#"{"data":null}"#),
        };
        (200, body)
    }
}

/// Serves `pve` on a free local port; returns the port and the certificate's
/// fingerprint.
async fn serve(pve: Arc<Pve>) -> (u16, String, tokio::task::JoinHandle<()>) {
    use std::fmt::Write as _;
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let mut fingerprint = String::new();
    for byte in ring::digest::digest(&ring::digest::SHA256, certificate.cert.der()).as_ref() {
        write!(fingerprint, "{byte:02x}").unwrap();
    }
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate.cert.der().clone()], key.into())
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let pve = Arc::clone(&pve);
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                // Keep-alive: one request after another on the connection.
                while let Ok(length) = stream.read(&mut buffer).await {
                    if length == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..length]);
                    let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&request[..end]).into_owned();
                    request.clear();
                    assert!(head.starts_with("GET /api2/json/"), "{head}");
                    let (status, reply) = pve.answer(&head);
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    );
                    if stream.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, fingerprint, task)
}

struct Fixture {
    hub: Hub,
    cancel: CancellationToken,
    instance: InstanceId,
    secrets: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.secrets);
    }
}

async fn start(port: u16, node: Option<&str>) -> Fixture {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let secrets = std::env::temp_dir().join(format!(
        "proxmox-{}-{}-{}.enc",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hub = Hub::new(HubOptions {
        secrets: Some((secrets.clone(), MasterKey::new("c".repeat(64)))),
        ..HubOptions::default()
    })
    .unwrap();
    let instance = InstanceId::from("pve");
    hub.store_secret(&instance, SECRET, TOKEN_SECRET)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(
            Proxmox::new(Config {
                host: "127.0.0.1".into(),
                port,
                node: node.map(str::to_owned),
                token_id: TOKEN_ID.into(),
                poll_secs: 5,
                full_percent: 90.0,
                vm_full_percent: None,
                backup_hours: 26.0,
            })
            .unwrap(),
        ),
        cancel.clone(),
    );
    Fixture {
        hub,
        cancel,
        instance,
        secrets,
    }
}

impl Fixture {
    fn value(&self, native: &str, key: &str) -> Option<Value> {
        self.hub
            .state(&PointId::new(&DeviceId::new(&self.instance, native), key))
            .map(|s| s.value)
    }

    async fn until(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        for _ in 0..400 {
            if done(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{what}: {:?}", self.hub.snapshot().drivers);
    }

    fn warning(&self) -> Option<String> {
        self.hub.snapshot().drivers[0].warning.clone()
    }
}

#[tokio::test]
async fn a_node_publishes_its_guests_storages_backup_and_disks() {
    let pve = Arc::new(Pve::default());
    let (port, fingerprint, server) = serve(Arc::clone(&pve)).await;
    let node = start(port, None).await;
    node.until("the disk read", |f| f.value("disk_0", "health").is_some())
        .await;
    assert_eq!(node.value("node", "up"), Some(Value::Bool(true)));
    assert_eq!(
        node.value("node", "version"),
        Some(Value::Text("9.2.31".into()))
    );
    assert_eq!(node.value("node", "updates"), Some(Value::Float(0.0)));
    assert_eq!(node.value("lxc_100", "running"), Some(Value::Bool(true)));
    assert_eq!(node.value("lxc_106", "problem"), Some(Value::Bool(false)));
    assert_eq!(
        node.value("storage_local-lvm", "problem"),
        Some(Value::Bool(false))
    );
    assert_eq!(
        node.value("backup", "last_result"),
        Some(Value::Text("OK".into()))
    );
    assert_eq!(
        node.value("disk_0", "health"),
        Some(Value::Text("PASSED".into()))
    );
    assert_eq!(node.hub.snapshot().drivers[0].status, DriverStatus::Running);
    // The certificate was pinned at first contact.
    assert_eq!(
        node.hub
            .secret(&node.instance, CERT)
            .map(|p| p.replace(':', "").to_ascii_lowercase()),
        Some(fingerprint)
    );
    // The node was found by itself, and every request carried the token in
    // its header, never in the address.
    assert_eq!(pve.paths("/api2/json/nodes "), 1);
    let heads = pve.heads.lock().unwrap().clone();
    assert!(!heads.is_empty());
    for head in &heads {
        let first = head.lines().next().unwrap();
        assert!(!first.contains(TOKEN_SECRET), "{first}");
        assert!(
            head.to_ascii_lowercase().contains(
                &format!("authorization: pveapitoken={TOKEN_ID}={TOKEN_SECRET}")
                    .to_ascii_lowercase()
            ),
            "{head}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn a_stopped_container_that_should_run_is_a_problem() {
    let pve = Arc::new(Pve::default());
    pve.faulty.store(true, Ordering::SeqCst);
    let (port, _, server) = serve(Arc::clone(&pve)).await;
    let node = start(port, Some("pve")).await;
    node.until("the stopped container flagged", |f| {
        f.value("lxc_101", "problem") == Some(Value::Bool(true))
    })
    .await;
    assert_eq!(node.value("lxc_101", "running"), Some(Value::Bool(false)));
    assert_eq!(node.value("lxc_103", "problem"), Some(Value::Bool(true)));
    // Only the stopped container's configuration was read.
    assert_eq!(pve.paths("/lxc/101/config"), 1);
    assert_eq!(pve.paths("/config"), 1);
    // The node was configured: never looked up.
    assert_eq!(pve.paths("/api2/json/nodes "), 0);
    server.abort();
}

#[tokio::test]
async fn a_refused_token_warns_and_is_not_asked_again_at_once() {
    let pve = Arc::new(Pve::default());
    pve.refuse.store(true, Ordering::SeqCst);
    let (port, _, server) = serve(Arc::clone(&pve)).await;
    let node = start(port, Some("pve")).await;
    node.until("the refusal said", |f| f.warning().is_some())
        .await;
    assert_eq!(node.value("node", "up"), Some(Value::Bool(false)));
    assert_eq!(node.value("node", "problem"), Some(Value::Bool(true)));
    let asked = pve.heads.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        pve.heads.lock().unwrap().len(),
        asked,
        "no new try during the pause"
    );
    // Fixed: back after the pause (2 s in test), the warning gone.
    pve.refuse.store(false, Ordering::SeqCst);
    node.until("back", |f| f.value("node", "up") == Some(Value::Bool(true)))
        .await;
    node.until("warning gone", |f| f.warning().is_none()).await;
    server.abort();
}

#[test]
fn a_bad_configuration_is_refused() {
    let good = Config {
        host: "pve.example".into(),
        port: 8006,
        node: None,
        token_id: TOKEN_ID.into(),
        poll_secs: 60,
        full_percent: 90.0,
        vm_full_percent: None,
        backup_hours: 26.0,
    };
    assert!(Proxmox::new(good.clone()).is_ok());
    for bad in [
        Config {
            host: "pve example".into(),
            ..good.clone()
        },
        Config {
            token_id: "moli".into(),
            ..good.clone()
        },
        Config {
            node: Some("a/b".into()),
            ..good.clone()
        },
        Config {
            poll_secs: 1,
            ..good.clone()
        },
        Config {
            full_percent: 0.0,
            ..good.clone()
        },
        Config {
            vm_full_percent: Some(150.0),
            ..good.clone()
        },
    ] {
        assert!(Proxmox::new(bad).is_err());
    }
}

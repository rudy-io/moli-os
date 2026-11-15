//! The driver against a fake DSM over TLS: a self-signed certificate, form
//! posts, the API names and answers of the fixtures.
use super::*;
use moli_core::{DeviceId, DriverStatus, InstanceId, Origin, PointId};
use moli_runtime::{CommandError, Hub, HubOptions, MasterKey, spawn_driver};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Dsm {
    /// Every body received, in order.
    bodies: Mutex<Vec<String>>,
    logins: AtomicUsize,
    /// The next storage read finds the session gone.
    drop_session: AtomicBool,
    refuse_login: AtomicBool,
    /// The next shutdown finds the session gone.
    drop_shutdown: AtomicBool,
    /// Storage answers something that is not DSM's JSON.
    broken_storage: AtomicBool,
    /// The system information answers something that is not DSM's JSON:
    /// the NAS is there, but the poll cannot go through (a proxy in between,
    /// a DSM that is still starting). Never a refused login.
    broken_info: AtomicBool,
    /// Hyper Backup has no task any more (the last one was deleted).
    empty_backups: AtomicBool,
}

impl Dsm {
    fn calls(&self, needle: &str) -> usize {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.contains(needle))
            .count()
    }

    fn answer(&self, body: &str) -> &'static str {
        self.bodies.lock().unwrap().push(body.to_owned());
        let api = |name: &str| body.contains(&format!("api={name}&"));
        if api("SYNO.API.Auth") {
            self.logins.fetch_add(1, Ordering::SeqCst);
            return if self.refuse_login.load(Ordering::SeqCst) {
                r#"{"success":false,"error":{"code":400}}"#
            } else {
                r#"{"success":true,"data":{"sid":"fake-session"}}"#
            };
        }
        if api("SYNO.DSM.Info") && self.broken_info.load(Ordering::SeqCst) {
            return "<html>gateway timeout</html>";
        }
        if api("SYNO.Storage.CGI.Storage") && self.broken_storage.load(Ordering::SeqCst) {
            return "<html>gateway timeout</html>";
        }
        if api("SYNO.Storage.CGI.Storage") && self.drop_session.swap(false, Ordering::SeqCst) {
            return r#"{"success":false,"error":{"code":119}}"#;
        }
        if api("SYNO.Core.System")
            && body.contains("method=shutdown")
            && self.drop_shutdown.swap(false, Ordering::SeqCst)
        {
            return r#"{"success":false,"error":{"code":106}}"#;
        }
        if api("SYNO.DSM.Info") {
            include_str!("../../../integrations/synology/fixtures/info.json")
        } else if api("SYNO.Core.System.Utilization") {
            include_str!("../../../integrations/synology/fixtures/utilization.json")
        } else if api("SYNO.Storage.CGI.Storage") {
            include_str!("../../../integrations/synology/fixtures/storage-faulty.json")
        } else if api("SYNO.Backup.Task") {
            if self.empty_backups.load(Ordering::SeqCst) {
                r#"{"success":true,"data":{"task_list":[]}}"#
            } else {
                include_str!("../../../integrations/synology/fixtures/backup-faulty.json")
            }
        } else if api("SYNO.Core.System") {
            r#"{"success":true}"#
        } else {
            // No right to the rest (the UPS here): that part is left out.
            r#"{"success":false,"error":{"code":105}}"#
        }
    }
}

/// Serves `dsm` on a free local port; returns the port and the certificate's
/// fingerprint.
async fn serve(dsm: Arc<Dsm>) -> (u16, String, tokio::task::JoinHandle<()>) {
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
            let dsm = Arc::clone(&dsm);
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
                    let wanted = head
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    if request.len() < end + 4 + wanted {
                        continue;
                    }
                    let body =
                        String::from_utf8_lossy(&request[end + 4..end + 4 + wanted]).into_owned();
                    request.clear();
                    assert!(head.starts_with("POST /webapi/"), "{head}");
                    let reply = dsm.answer(&body);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
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

async fn start(port: u16, mac: Option<&str>, pin: Option<&str>) -> Fixture {
    // One file per test: the clock alone is too coarse to tell two tests apart.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let secrets = std::env::temp_dir().join(format!(
        "synology-{}-{}-{}.enc",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let hub = Hub::new(HubOptions {
        secrets: Some((secrets.clone(), MasterKey::new("c".repeat(64)))),
        ..HubOptions::default()
    })
    .unwrap();
    let instance = InstanceId::from("nas");
    hub.store_secret(&instance, USERNAME, "moli").await.unwrap();
    hub.store_secret(&instance, PASSWORD, "synthetic-password")
        .await
        .unwrap();
    if let Some(pin) = pin {
        hub.store_secret(&instance, CERT, pin).await.unwrap();
    }
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(
            Synology::new(Config {
                host: "127.0.0.1".into(),
                port,
                poll_secs: 5,
                mac: mac.map(str::to_owned),
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
    fn point(&self, native: &str, key: &str) -> PointId {
        PointId::new(&DeviceId::new(&self.instance, native), key)
    }

    fn value(&self, native: &str, key: &str) -> Option<moli_core::Value> {
        self.hub.state(&self.point(native, key)).map(|s| s.value)
    }

    async fn until(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        for _ in 0..300 {
            if done(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{what}: {:?}", self.hub.snapshot().drivers);
    }

    fn warning(&self) -> Option<String> {
        self.hub.snapshot().drivers[0].warning.clone()
    }

    fn status(&self) -> DriverStatus {
        self.hub.snapshot().drivers[0].status.clone()
    }
}

#[tokio::test]
async fn a_reachable_nas_publishes_its_parts_and_takes_a_shutdown() {
    let dsm = Arc::new(Dsm::default());
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("backups published", |f| {
        f.value("backup_2", "problem") == Some(moli_core::Value::Bool(true))
    })
    .await;
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(true)));
    assert_eq!(
        nas.value("nas", "temperature"),
        Some(moli_core::Value::Float(42.0))
    );
    assert_eq!(
        nas.value("volume_1", "used"),
        Some(moli_core::Value::Float(50.0))
    );
    assert_eq!(
        nas.value("disk_sdb", "problem"),
        Some(moli_core::Value::Bool(true))
    );
    // The UPS answered « no right »: no device, and the rest is unharmed.
    assert!(nas.value("ups", "battery").is_none());
    assert_eq!(nas.status(), DriverStatus::Running);
    assert!(
        nas.hub
            .snapshot()
            .devices
            .iter()
            .all(|d| d.online == Some(true))
    );

    nas.hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("shutdown"),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
    assert_eq!(dsm.calls("method=shutdown"), 1);
    assert_eq!(dsm.calls("force=false"), 1);
    // One login; the password never travels except in it.
    assert_eq!(dsm.logins.load(Ordering::SeqCst), 1);
    assert_eq!(dsm.calls("passwd="), 1);
    assert_eq!(
        dsm.calls("_sid=fake-session"),
        dsm.bodies.lock().unwrap().len() - 1
    );
    server.abort();
}

#[tokio::test]
async fn a_task_that_leaves_the_nas_goes_offline_without_its_old_problem() {
    let dsm = Arc::new(Dsm::default());
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("a backup flagged", |f| {
        f.value("backup_2", "problem") == Some(moli_core::Value::Bool(true))
    })
    .await;
    // The task is deleted on the NAS: the next poll no longer lists it.
    dsm.empty_backups.store(true, Ordering::SeqCst);
    for _ in 0..500 {
        if nas.value("backup_2", "problem") == Some(moli_core::Value::Null) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        nas.value("backup_2", "problem"),
        Some(moli_core::Value::Null)
    );
    assert_eq!(
        nas.value("backup_2", "detail"),
        Some(moli_core::Value::Null)
    );
    let offline = nas
        .hub
        .snapshot()
        .devices
        .iter()
        .find(|d| d.device.id.to_string() == "nas:backup_2")
        .map(|d| d.online);
    assert_eq!(offline, Some(Some(false)));
    // The rest of the NAS is untouched.
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(true)));
    assert_eq!(nas.status(), DriverStatus::Running);
    server.abort();
}

#[tokio::test]
async fn a_dropped_session_is_reopened_once_and_the_read_goes_through() {
    let dsm = Arc::new(Dsm::default());
    dsm.drop_session.store(true, Ordering::SeqCst);
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("volume published", |f| {
        f.value("volume_1", "used").is_some()
    })
    .await;
    assert_eq!(dsm.logins.load(Ordering::SeqCst), 2);
    assert_eq!(nas.status(), DriverStatus::Running);
    server.abort();
}

#[tokio::test]
async fn an_order_met_by_a_lost_session_is_sent_again_after_a_new_login() {
    let dsm = Arc::new(Dsm::default());
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("volume published", |f| {
        f.value("volume_1", "used").is_some()
    })
    .await;
    dsm.drop_shutdown.store(true, Ordering::SeqCst);
    nas.hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("shutdown"),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
    // The first one was refused unrun (session gone), the second ran.
    assert_eq!(dsm.calls("method=shutdown"), 2);
    assert_eq!(dsm.logins.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn refused_credentials_are_not_insisted_on_and_tried_again_later() {
    let dsm = Arc::new(Dsm::default());
    dsm.refuse_login.store(true, Ordering::SeqCst);
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("warned", |f| f.warning().is_some()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(dsm.logins.load(Ordering::SeqCst), 1);
    // The NAS device stays, honestly down, and the driver keeps running.
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(false)));
    assert_eq!(nas.status(), DriverStatus::Running);
    // DSM accepts again (it was starting): the next poll after the
    // cool-down logs in, and the warning goes.
    dsm.refuse_login.store(false, Ordering::SeqCst);
    for _ in 0..500 {
        if nas.value("nas", "up") == Some(moli_core::Value::Bool(true)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(true)));
    assert_eq!(dsm.logins.load(Ordering::SeqCst), 2);
    assert!(nas.warning().is_none());
    server.abort();
}

#[tokio::test]
async fn the_refusal_warning_does_not_outlive_the_refusal() {
    let dsm = Arc::new(Dsm::default());
    dsm.refuse_login.store(true, Ordering::SeqCst);
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("warned", |f| f.warning().is_some()).await;
    // DSM accepts the login again but the poll cannot go through, for a
    // reason that is not a refusal: the warning must go.
    dsm.refuse_login.store(false, Ordering::SeqCst);
    dsm.broken_info.store(true, Ordering::SeqCst);
    for _ in 0..500 {
        if nas.warning().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(nas.warning().is_none(), "{:?}", nas.warning());
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(false)));
    assert_eq!(nas.status(), DriverStatus::Running);
    server.abort();
}

#[tokio::test]
async fn credentials_refused_during_an_order_stop_the_driver_from_trying_again() {
    let dsm = Arc::new(Dsm::default());
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("volume published", |f| {
        f.value("volume_1", "used").is_some()
    })
    .await;
    // The session is gone, and the login that follows is refused.
    dsm.drop_shutdown.store(true, Ordering::SeqCst);
    dsm.refuse_login.store(true, Ordering::SeqCst);
    let refused = nas
        .hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("shutdown"),
            Origin::Ui,
            None,
        )
        .await;
    assert!(refused.is_err());
    nas.until("warned", |f| f.warning().is_some()).await;
    // Still cooling down: an order does not log in again.
    let again = nas
        .hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("reboot"),
            Origin::Ui,
            None,
        )
        .await;
    assert!(again.is_err());
    assert_eq!(dsm.logins.load(Ordering::SeqCst), 2);
    assert_eq!(dsm.calls("method=shutdown"), 1);
    assert_eq!(dsm.calls("method=reboot"), 0);
    server.abort();
}

#[tokio::test]
async fn the_storage_is_read_once_while_the_rest_is_polled() {
    let dsm = Arc::new(Dsm::default());
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("volume published", |f| {
        f.value("volume_1", "used").is_some()
    })
    .await;
    // The poll is every 5 s here: wait for another one.
    nas.until("a second poll", |_| dsm.calls("System.Utilization") >= 2)
        .await;
    assert_eq!(dsm.calls("SYNO.Storage.CGI.Storage"), 1);
    // The volume stays published from the last reading.
    assert_eq!(
        nas.value("volume_1", "used"),
        Some(moli_core::Value::Float(50.0))
    );
    server.abort();
}

#[tokio::test]
async fn a_part_that_cannot_be_read_loses_only_itself() {
    let dsm = Arc::new(Dsm::default());
    dsm.broken_storage.store(true, Ordering::SeqCst);
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, None).await;
    nas.until("backups published", |f| {
        f.value("backup_2", "problem") == Some(moli_core::Value::Bool(true))
    })
    .await;
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(true)));
    assert!(nas.value("volume_1", "used").is_none());
    assert_eq!(nas.status(), DriverStatus::Running);
    server.abort();
}

#[tokio::test]
async fn a_nas_that_sleeps_keeps_the_driver_running_and_its_orders_served() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let nas = start(port, Some("02:00:00:00:00:01"), None).await;
    nas.until("nas published", |f| f.value("nas", "up").is_some())
        .await;
    assert_eq!(nas.value("nas", "up"), Some(moli_core::Value::Bool(false)));
    assert_eq!(nas.status(), DriverStatus::Running);
    // Wake reaches the driver (the packet itself needs a network: either
    // outcome is the driver's answer, never « driver unavailable »).
    let wake = nas
        .hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("wake"),
            Origin::Ui,
            None,
        )
        .await;
    assert!(
        matches!(wake, Ok(()) | Err(CommandError::Driver(_))),
        "{wake:?}"
    );
    // A shutdown cannot reach a NAS that is off: refused, driver untouched.
    let stop = nas
        .hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("shutdown"),
            Origin::Ui,
            None,
        )
        .await;
    assert!(matches!(stop, Err(CommandError::Driver(_))), "{stop:?}");
    assert_eq!(nas.status(), DriverStatus::Running);
}

#[tokio::test]
async fn without_a_mac_address_there_is_no_wake_order() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let nas = start(port, None, None).await;
    nas.until("nas published", |f| f.value("nas", "up").is_some())
        .await;
    let refused = nas
        .hub
        .command(
            &nas.point("nas", "control"),
            moli_core::Value::from("wake"),
            Origin::Ui,
            None,
        )
        .await;
    assert!(
        matches!(refused, Err(CommandError::Invalid(_))),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_changed_certificate_sends_nothing() {
    let dsm = Arc::new(Dsm::default());
    let (port, _, server) = serve(Arc::clone(&dsm)).await;
    let nas = start(port, None, Some(&"0".repeat(64))).await;
    nas.until("waiting", |f| {
        matches!(f.status(), DriverStatus::Waiting { .. })
    })
    .await;
    assert!(dsm.bodies.lock().unwrap().is_empty());
    server.abort();
}

#[test]
fn the_pause_before_another_try_doubles_and_stops_at_the_ceiling() {
    assert_eq!(retry_after(1), RETRY_AFTER_REFUSAL);
    assert_eq!(retry_after(2), RETRY_AFTER_REFUSAL * 2);
    // The third doubles again, up to the ceiling (2 s → 4 s → 8 s in test:
    // the third one already reaches it).
    assert_eq!(
        retry_after(3),
        (RETRY_AFTER_REFUSAL * 4).min(RETRY_AFTER_REFUSAL_MAX)
    );
    // Never past the ceiling, and a counter that kept climbing cannot
    // overflow: a saturated multiplication, not a shift that panics.
    for refusals in 1..=10_000 {
        assert!(retry_after(refusals) <= RETRY_AFTER_REFUSAL_MAX);
    }
    assert_eq!(retry_after(u32::MAX), RETRY_AFTER_REFUSAL_MAX);
}

#[test]
fn the_constructor_checks_the_options_without_the_network() {
    let good = Config {
        host: "nas.example".into(),
        port: 5001,
        poll_secs: 60,
        mac: Some("AA-BB-CC-DD-EE-FF".into()),
    };
    assert!(Synology::new(good.clone()).is_ok());
    assert_eq!(colon("aabbccddeeff"), "aa:bb:cc:dd:ee:ff");
    for host in ["", "http://nas", "nas/path", "nas:5001", " nas", "nas\r\n"] {
        assert!(
            Synology::new(Config {
                host: host.into(),
                ..good.clone()
            })
            .is_err()
        );
    }
    assert!(
        Synology::new(Config {
            port: 0,
            ..good.clone()
        })
        .is_err()
    );
    assert!(
        Synology::new(Config {
            poll_secs: 4,
            ..good.clone()
        })
        .is_err()
    );
    assert!(
        Synology::new(Config {
            poll_secs: 3601,
            ..good.clone()
        })
        .is_err()
    );
    assert!(
        Synology::new(Config {
            mac: Some("nope".into()),
            ..good
        })
        .is_err()
    );
}

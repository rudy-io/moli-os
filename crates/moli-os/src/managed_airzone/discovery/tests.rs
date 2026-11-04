use super::*;
use std::sync::atomic::AtomicBool;

fn host(value: &str) -> Ipv4Addr {
    value.parse().unwrap()
}

fn gateway(ip: &str, port: u16, pin: Option<&str>) -> GatewayStatus {
    GatewayStatus {
        instance: "airzone-demo".into(),
        host: ip.into(),
        port,
        tls_fingerprint: pin.map(str::to_owned),
        mac: Some("aabbccddeeff".into()),
        enabled: false,
        source: "configuration",
        read_only: true,
        running: false,
    }
}

fn report() -> moli_airzone::GatewayProbe {
    moli_airzone::GatewayProbe {
        mac: "aabbccddeeff".into(),
        gateway_type: Some("ws_az".into()),
        api_version: None,
        firmware: Some("4.21".into()),
        systems: 1,
        zones: 2,
        incomplete: false,
    }
}

fn targets(count: u8) -> Plan {
    Plan {
        targets: (1..=count)
            .map(|last| Target {
                host: Ipv4Addr::new(10, 0, 0, last),
                port: 3000,
                tls_fingerprint: None,
            })
            .collect(),
        incomplete: false,
    }
}

#[test]
fn only_private_hosts_not_network_or_broadcast_addresses_can_be_discovered() {
    for value in [
        "127.0.0.1",
        "8.8.8.8",
        "169.254.1.8",
        "10.0.0.0",
        "10.0.0.255",
        "::1",
        "router.local",
        "http://10.0.0.2",
    ] {
        assert_eq!(private_host(value), None, "{value}");
    }
    for value in ["10.0.0.1", "172.16.0.1", "172.31.255.1", "192.168.1.1"] {
        assert_eq!(private_host(value), Some(host(value)));
    }
}

#[test]
fn no_network_hint_is_an_explicit_incomplete_empty_search() {
    let plan = plan_targets(&[], &[], &[]);
    assert!(plan.targets.is_empty());
    assert!(plan.incomplete);
    let arp_only = plan_targets(&[], &[], &[host("172.16.0.1")]);
    assert_eq!(arp_only.targets.len(), 254);
    assert!(
        arp_only.incomplete,
        "a container bridge is not proof of the home's LAN"
    );
}

#[test]
fn configured_network_precedes_a_container_bridge_and_scans_at_most_two_subnets() {
    let known = [gateway("192.168.1.20", 3000, None)];
    let plan = plan_targets(
        &known,
        &[host("10.0.0.5")],
        &[host("172.16.0.1"), host("8.8.8.8")],
    );
    assert_eq!(plan.targets[0].host, host("192.168.1.20"));
    assert_eq!(plan.targets.len(), 508);
    assert!(plan.incomplete);
    assert!(
        plan.targets
            .iter()
            .all(|target| matches!(prefix(target.host), [192, 168, 1] | [10, 0, 0]))
    );
    assert_eq!(
        plan.targets
            .iter()
            .map(|target| (target.host, target.port))
            .collect::<HashSet<_>>()
            .len(),
        508
    );
}

#[test]
fn known_https_and_nonstandard_ports_never_gain_a_plain_http_fallback() {
    let pin = "a".repeat(64);
    let known = [
        gateway("10.0.0.10", 443, Some(&pin)),
        gateway("10.0.0.11", 4000, None),
    ];
    let plan = plan_targets(&known, &[host("10.0.0.10")], &[]);
    assert_eq!(plan.targets.len(), 254);
    assert!(!plan.incomplete);
    assert_eq!(
        plan.targets[0].tls_fingerprint.as_deref(),
        Some(pin.as_str())
    );
    for ip in [host("10.0.0.10"), host("10.0.0.11")] {
        assert_eq!(
            plan.targets
                .iter()
                .filter(|target| target.host == ip)
                .count(),
            1
        );
    }
    assert_eq!(plan.targets[1].port, 4000);
}

#[test]
fn too_many_endpoints_or_networks_are_bounded_and_reported() {
    let many: Vec<_> = (3000..3040)
        .map(|port| gateway("10.0.0.2", port, None))
        .collect();
    let plan = plan_targets(&many, &[host("10.1.0.1")], &[]);
    assert_eq!(plan.targets.len(), MAX_TARGETS);
    assert!(plan.incomplete);
    let separate: Vec<_> = (0..10)
        .map(|n| gateway(&format!("10.{n}.0.2"), 3000, None))
        .collect();
    let plan = plan_targets(&separate, &[], &[]);
    assert_eq!(plan.targets.len(), 508);
    assert_eq!(
        plan.targets
            .iter()
            .map(|target| prefix(target.host))
            .collect::<HashSet<_>>()
            .len(),
        2
    );
}

#[test]
fn one_search_is_shared_with_clones_and_cancelled_searches_keep_the_cooldown() {
    let gate = Arc::new(Gate::default());
    let held = gate.enter().unwrap();
    assert_eq!(gate.clone().enter().err().unwrap().code, "busy");
    gate.0.lock().unwrap().started = Instant::now().checked_sub(COOLDOWN);
    assert_eq!(
        gate.enter().err().unwrap().code,
        "busy",
        "in-flight search still holds the slot"
    );
    drop(held);
    drop(gate.enter().unwrap());
    assert_eq!(gate.enter().err().unwrap().code, "busy");
    assert!(!gate.0.lock().unwrap().running);
}

#[derive(Default)]
struct Activity {
    active: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
}
struct Active<'a>(&'a Activity);
impl Activity {
    fn enter(&self) -> Active<'_> {
        let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(count, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Active(self)
    }
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct Fake {
    connecting: Activity,
    probing: Activity,
    never_finishes: AtomicBool,
    unique: bool,
    first_succeeds: bool,
}
impl Inspector for Fake {
    fn connect<'a>(&'a self, _target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let _active = self.connecting.enter();
            tokio::time::sleep(Duration::from_millis(2)).await;
            true
        })
    }
    fn probe<'a>(
        &'a self,
        target: &'a Target,
    ) -> BoxFuture<'a, Option<moli_airzone::GatewayProbe>> {
        Box::pin(async move {
            let _active = self.probing.enter();
            if self.never_finishes.load(Ordering::SeqCst)
                && !(self.first_succeeds && target.host.octets()[3] == 1)
            {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
            let mut report = report();
            if self.unique {
                report.mac = format!("aabbccddee{:02x}", target.host.octets()[3]);
            }
            Some(report)
        })
    }
}

#[tokio::test]
async fn concurrent_connects_and_validations_have_independent_bounds() {
    let fake = Arc::new(Fake {
        unique: true,
        ..Fake::default()
    });
    let result = run(
        targets(100),
        vec![],
        CancellationToken::new(),
        fake.clone(),
        SEARCH_LIMIT,
    )
    .await
    .unwrap();
    assert_eq!(result.target_count, 100);
    assert_eq!(result.scanned, 100);
    assert_eq!(result.gateways.len(), MAX_PROBES);
    assert!(result.incomplete);
    assert!(fake.connecting.peak.load(Ordering::SeqCst) <= CONNECT_CONCURRENCY);
    assert!(fake.probing.peak.load(Ordering::SeqCst) <= PROBE_CONCURRENCY);
    assert_eq!(fake.probing.calls.load(Ordering::SeqCst), MAX_PROBES);
    assert!(
        result
            .gateways
            .iter()
            .all(|gateway| gateway.read_only && gateway.existing_instance.is_none())
    );
}

#[tokio::test]
async fn duplicate_identity_is_stable_and_a_paused_gateway_keeps_its_owner() {
    let fake = Arc::new(Fake::default());
    let known = vec![gateway("10.0.0.50", 3000, None)];
    let result = run(
        targets(3),
        known,
        CancellationToken::new(),
        fake,
        SEARCH_LIMIT,
    )
    .await
    .unwrap();
    assert_eq!(result.gateways.len(), 1);
    assert_eq!(result.gateways[0].host, "10.0.0.1");
    assert_eq!(
        result.gateways[0].existing_instance.as_deref(),
        Some("airzone-demo")
    );
    assert_eq!(result.gateways[0].existing_enabled, Some(false));
    assert!(!result.incomplete);
}

async fn stopped(fake: &Fake) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while fake.connecting.active.load(Ordering::SeqCst) != 0
            || fake.probing.active.load(Ordering::SeqCst) != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn probing(fake: &Fake) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while fake.probing.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn deadline_returns_partial_results_and_drops_every_pending_probe() {
    let fake = Arc::new(Fake {
        first_succeeds: true,
        ..Fake::default()
    });
    fake.never_finishes.store(true, Ordering::SeqCst);
    let result = run(
        targets(30),
        vec![],
        CancellationToken::new(),
        fake.clone(),
        Duration::from_millis(30),
    )
    .await
    .unwrap();
    assert!(result.incomplete);
    assert!(result.scanned < result.target_count);
    assert_eq!(
        result.gateways.len(),
        1,
        "already validated results survive the deadline"
    );
    stopped(&fake).await;
}

#[tokio::test]
async fn shutdown_and_caller_cancellation_leave_no_detached_probe() {
    for shutdown in [true, false] {
        let fake = Arc::new(Fake::default());
        fake.never_finishes.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            targets(30),
            vec![],
            cancel.clone(),
            fake.clone(),
            SEARCH_LIMIT,
        ));
        probing(&fake).await;
        if shutdown {
            cancel.cancel();
            assert_eq!(task.await.unwrap().unwrap_err().code, "shutting_down");
        } else {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        stopped(&fake).await;
    }
}

#[tokio::test]
async fn real_transport_uses_only_fixed_native_reads() {
    use axum::{
        Router,
        body::Bytes,
        http::{Method, Uri},
        routing::any,
    };
    use serde_json::json;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let app = Router::new().fallback(any(move |method: Method, uri: Uri, body: Bytes| {
        recorded
            .lock()
            .unwrap()
            .push((method.to_string(), uri.to_string(), body.to_vec()));
        async move {
            axum::Json(if uri.path() == "/api/v1/webserver" {
                json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"ws_az","ws_firmware":"4.21"})
            } else {
                json!({"data":[{"systemID":1,"zoneID":1,"on":1,"roomTemp":22.5,"units":0}]})
            })
        }
    }));
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(tcp, app).await.unwrap();
    });
    // Loopback exists only in this injected test plan; production planning rejects it.
    let plan = Plan {
        targets: vec![Target {
            host: host("127.0.0.1"),
            port,
            tls_fingerprint: None,
        }],
        incomplete: false,
    };
    let result = run(
        plan,
        vec![],
        CancellationToken::new(),
        Arc::new(Network),
        SEARCH_LIMIT,
    )
    .await
    .unwrap();
    assert_eq!(result.gateways.len(), 1);
    assert!(result.gateways[0].read_only);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            ("POST".into(), "/api/v1/webserver".into(), b"{}".to_vec()),
            (
                "GET".into(),
                "/api/v1/hvac?systemid=0&zoneid=0".into(),
                vec![]
            ),
        ]
    );
    task.abort();
}

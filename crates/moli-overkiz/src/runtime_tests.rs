use super::*;
use moli_core::{Access, Device, InstanceId, Kind, Origin, PointId, PointSpec, Semantic, Value};
use moli_runtime::{Hub, HubOptions, MasterKey, spawn_driver};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

fn vault(label: &str) -> (Hub, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "overkiz-runtime-{label}-{}-{}.enc",
        std::process::id(),
        moli_core::now_ms()
    ));
    let hub = Hub::new(HubOptions {
        secrets: Some((path.clone(), MasterKey::new("c".repeat(64)))),
        ..HubOptions::default()
    })
    .unwrap();
    (hub, path)
}
fn stale_summary(instance: &InstanceId) -> Device {
    Device {
        id: DeviceId::new(instance, "cloud:summary"),
        instance: instance.clone(),
        native_name: "synthetic summary".into(),
        manufacturer: None,
        model: None,
        description: None,
        native_room: None,
        members: vec![],
        points: vec![PointSpec {
            key: "control".into(),
            label: "synthetic control".into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        }],
    }
}
#[test]
fn cloud_only_config_requires_no_native_host_but_at_least_one_source() {
    let config: Result<Config, _> = serde_json::from_value(serde_json::json!({"cloud": {}}));
    assert!(
        config.is_ok(),
        "cloud-only configuration must deserialize without host"
    );
    assert!(Overkiz::new(config.unwrap()).is_ok());
    let config: Config = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(Overkiz::new(config).is_err());
}
#[derive(Debug)]
struct Probe {
    config: Config,
    guard: bool,
}
impl Driver for Probe {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let host: String = serde_json::to_value(&self.config.host)
                .unwrap()
                .as_str()
                .unwrap()
                .into();
            let probe = PinnedHttps::new(&host, self.config.port, None)?;
            probe.handshake().await?;
            let pin = probe.pinned_fingerprint().unwrap();
            drop(probe);
            ctx.store_secret("cert_sha256", &pin).await?;
            let client = Client::new(
                PinnedHttps::new(&host, self.config.port, Some(pin))?,
                "synthetic-test-token".into(),
            );
            ctx.upsert_device(stale_summary(ctx.instance()));
            if !self.guard {
                refresh(&client, ctx, &self.config).await?;
            }
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                let result = apply(&client, ctx, &command, command.deadline, &self.config).await;
                command.reply(result.map_err(|e| e.to_string()));
            }
            Ok(())
        })
    }
}
async fn probe(
    guard: bool,
) -> (
    Hub,
    InstanceId,
    CancellationToken,
    tokio::task::JoinHandle<()>,
    Arc<AtomicUsize>,
    std::path::PathBuf,
) {
    let (hub, path) = vault(if guard { "guard" } else { "ownership" });
    let instance = InstanceId::from("runtime-test");
    let posts = Arc::new(AtomicUsize::new(0));
    let (port, server) = crate::tests::fixture_server(
        &hub,
        &instance,
        Arc::clone(&posts),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let config =
        serde_json::from_value(serde_json::json!({"host": "localhost", "port": port})).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(Probe { config, guard }),
        cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while hub.stats().drivers_running != 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    (hub, instance, cancel, server, posts, path)
}
#[tokio::test]
async fn native_refresh_keeps_cloud_owned_summary() {
    let (hub, instance, cancel, server, _, path) = probe(false).await;
    assert!(
        hub.device(&DeviceId::new(&instance, "cloud:summary"))
            .is_some(),
        "native source must not remove cloud device"
    );
    cancel.cancel();
    server.abort();
    let _ = std::fs::remove_file(path);
}
#[tokio::test]
async fn every_origin_stale_writable_cloud_metadata_is_refused_before_native_preflight() {
    let (hub, instance, cancel, server, posts, path) = probe(true).await;
    let point = PointId::new(&DeviceId::new(&instance, "cloud:summary"), "control");
    for origin in [
        Origin::Ui,
        Origin::Household,
        Origin::Api,
        Origin::Mcp,
        Origin::Cli,
        Origin::System,
        Origin::Assistant,
        Origin::Automation,
    ] {
        assert_eq!(
            hub.command(&point, Value::Bool(true), origin, None).await,
            Err(moli_runtime::CommandError::Driver(moli_i18n::tr!(
                "overkiz.refused"
            ))),
            "{origin:?}"
        );
    }
    assert!(
        hub.device(&DeviceId::new(&instance, "cloud:summary"))
            .is_some(),
        "a rejected cloud command must not trigger native source reconciliation"
    );
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    cancel.cancel();
    server.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn cloud_only_starts_without_local_credentials_or_certificate() {
    let (hub, path) = vault("cloud-only");
    let instance = InstanceId::from("cloud-only");
    let cancel = CancellationToken::new();
    let config = serde_json::from_value(serde_json::json!({"cloud": {}})).unwrap();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(Overkiz::new(config).unwrap()),
        cancel.clone(),
    );
    tokio::time::sleep(Duration::from_millis(40)).await;
    let summary = hub.device(&DeviceId::new(&instance, "cloud:summary"));
    assert!(
        summary.is_some(),
        "enabled cloud must start independently of missing local credentials"
    );
    assert!(hub.secret(&instance, "cert_sha256").is_none());
    assert!(
        hub.snapshot().drivers[0]
            .warning
            .as_ref()
            .is_some_and(|warning| !warning.contains(&moli_i18n::tr!("overkiz.auth")))
    );
    cancel.cancel();
    let _ = std::fs::remove_file(path);
}

#[derive(Debug)]
struct SeedSource {
    config: Config,
    native: bool,
}
impl Driver for SeedSource {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut device = stale_summary(ctx.instance());
            if self.native {
                device.id = ctx.device_id("previous-native");
            }
            let id = device.id.clone();
            ctx.upsert_device(device);
            ctx.set_availability(&id, true);
            run(&self.config, ctx).await
        })
    }
}
#[tokio::test]
async fn removing_native_source_keeps_only_cloud_summary() {
    let (hub, path) = vault("remove-native");
    let instance = InstanceId::from("remove-native");
    let cancel = CancellationToken::new();
    let config = serde_json::from_value(serde_json::json!({"cloud":{}})).unwrap();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(SeedSource {
            config,
            native: true,
        }),
        cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while hub.device(&cloud::summary_device_id(&instance)).is_none() {
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        hub.device(&DeviceId::new(&instance, "previous-native"))
            .is_none()
    );
    cancel.cancel();
    let _ = std::fs::remove_file(path);
}
#[tokio::test]
async fn disabled_cloud_removes_its_summary_and_never_consumes_pending_input() {
    let (hub, path) = vault("disabled-cloud");
    let instance = InstanceId::from("disabled-cloud");
    hub.store_secret(&instance, "token", "synthetic-test-token")
        .await
        .unwrap();
    hub.store_secret(
        &instance,
        cloud::PENDING_KEY,
        "synthetic input must remain untouched",
    )
    .await
    .unwrap();
    let (port, server) = crate::tests::fixture_server(
        &hub,
        &instance,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let config =
        serde_json::from_value(serde_json::json!({"host":"localhost", "port":port, "poll_secs":1}))
            .unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        instance.clone(),
        Arc::new(SeedSource {
            config,
            native: false,
        }),
        cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while hub.stats().drivers_running != 1 {
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    })
    .await
    .unwrap();
    assert!(hub.device(&cloud::summary_device_id(&instance)).is_none());
    assert_eq!(
        hub.secret(&instance, cloud::PENDING_KEY).as_deref(),
        Some("synthetic input must remain untouched")
    );
    assert!(hub.secret(&instance, cloud::STATE_KEY).is_none());
    assert!(hub.secret(&instance, cloud::SESSION_KEY).is_none());
    cancel.cancel();
    server.abort();
    let _ = std::fs::remove_file(path);
}

use super::*;
use axum::{Json, Router, routing::any};
use moli_core::{Origin, PointId, Value};
use moli_runtime::{Hub, HubOptions, spawn_driver};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

/// Simulate previously admitted writable metadata so hub admission cannot hide
/// an absent executor check. All requests go to a loopback recorder.
struct StaleCapabilities(Airzone);
impl Driver for StaleCapabilities {
    fn kind(&self) -> &'static str {
        "airzone"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let snapshot = Snapshot::parse(&serde_json::from_str(include_str!(
                "../../../integrations/airzone/fixtures/zones.json"
            ))?)?;
            publish_entities(ctx, snapshot.entities(ctx.instance(), "aabbccddeeff"), &[]);
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                let result = self.0.execute(ctx, "aabbccddeeff", &command).await;
                command.reply(result.map_err(|error| error.to_string()));
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn executor_rejects_stale_writable_metadata_before_networking() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = Router::new().fallback(any(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        async { Json(json!({})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for (read_only, write, key, value) in [
        (false, "legacy", "name", Value::from("Zone D")),
        (true, "legacy", "on", Value::Bool(true)),
        (true, "extended", "name", Value::from("Zone D")),
    ] {
        let config = serde_json::from_value(
            json!({"host":"127.0.0.1", "port":port,"read_only":read_only,"write":write}),
        )
        .unwrap();
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("az"),
            Arc::new(StaleCapabilities(Airzone::new(config).unwrap())),
            cancel.clone(),
        );
        for _ in 0..100 {
            if hub.stats().drivers_running == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let id = DeviceId::from("az:aabbccddeeff_s1_z2");
        assert!(
            hub.device(&id)
                .unwrap()
                .device
                .point(key)
                .unwrap()
                .access
                .write
        );
        let error = hub
            .command(&PointId::new(&id, key), value, Origin::Ui, None)
            .await
            .unwrap_err();
        let expected = if read_only {
            api::Failure::ReadOnly
        } else {
            api::Failure::ExtendedWritesDisabled
        };
        assert!(error.to_string().contains(&expected.to_string()), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        cancel.cancel();
    }
    server.abort();
}

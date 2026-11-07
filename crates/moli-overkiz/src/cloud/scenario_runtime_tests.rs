use super::*;
use crate::cloud::execution::{self, Policy};
use moli_core::{Action, Origin, Outcome, PointId};
use moli_runtime::CommandError;

#[derive(Debug)]
struct ExecutionProbe {
    script: Script,
    read_only: bool,
    gate: Arc<PreflightGate>,
}
impl Driver for ExecutionProbe {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let config = super::super::super::Config {
                read_only: self.read_only,
                ..Default::default()
            };
            let mut cloud = Controller::with_transport(
                ctx,
                &config,
                GatedScript {
                    script: self.script.clone(),
                    gate: self.gate.clone(),
                },
            );
            let mut tick = tokio::time::interval(Duration::from_millis(5));
            loop {
                crate::reconcile_status(ctx, false, None, Some(&cloud));
                tokio::select! {
                    _=tick.tick(), if !self.gate.pause_ticks.load(Ordering::SeqCst)=>cloud.tick(ctx),
                    event=cloud.next_event(),if cloud.receiving()=>cloud.handle(ctx,event),
                    command=ctx.next_command()=>{
                        let Some(command)=command else{return Ok(())};
                        cloud.dispatch(ctx,command);
                    }
                }
            }
        })
    }
}
#[derive(Debug, Default)]
struct PreflightGate {
    armed: std::sync::atomic::AtomicBool,
    pause_ticks: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[derive(Clone)]
struct GatedScript {
    script: Script,
    gate: Arc<PreflightGate>,
}
impl Transport for GatedScript {
    fn send(&mut self, request: Request) -> BoxFuture<'_, Result<Response, Error>> {
        Box::pin(async move {
            let endpoint = request.endpoint;
            let response = self.script.send(request).await;
            if endpoint == Endpoint::Collection(CollectionKind::Scenarios)
                && self.gate.armed.swap(false, Ordering::SeqCst)
            {
                self.gate.entered.notify_one();
                self.gate.release.notified().await;
            }
            response
        })
    }
}
async fn connected(label: &str, read_only: bool) -> Rig {
    connected_controlled(label, read_only, Arc::default()).await
}
async fn connected_controlled(label: &str, read_only: bool, gate: Arc<PreflightGate>) -> Rig {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new(label, replies);
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    spawn_driver(
        &rig.hub,
        rig.instance.clone(),
        Arc::new(ExecutionProbe {
            script: rig.script.clone(),
            read_only,
            gate,
        }),
        rig.cancel.clone(),
    );
    let generation = rig
        .wait_state(|s| s.request_id.as_deref() == Some("login"))
        .await
        .generation
        .unwrap();
    rig.select(generation).await;
    rig
}
async fn permission(rig: &Rig, id: &str, enabled: bool) -> ConnectionState {
    let catalog = rig.catalog();
    rig.queue(
        id,
        PendingRequest::ScenarioPermission {
            generation: catalog.connection_generation.clone(),
            revision: execution::catalog_revision(&catalog).unwrap(),
            scenario_id: "example".into(),
            enabled,
        },
    )
    .await;
    rig.wait_state(|s| s.request_id.as_deref() == Some(id))
        .await
}
fn offered(rig: &Rig) -> (PointId, Value) {
    let catalog = rig.catalog();
    let policy = rig
        .hub
        .cloud_secret(&rig.instance, execution::POLICY_KEY)
        .and_then(|s| Policy::decode(s.as_bytes()).ok())
        .unwrap_or_default();
    let status = execution::view(&catalog, &policy, false, true, true)
        .scenarios
        .remove(0);
    (status.point, Value::from(status.value.as_str()))
}
#[tokio::test]
async fn explicit_permission_machine_guard_and_hub_journal_precede_one_cloud_post() {
    let rig = connected("execution-guard", false).await;
    let (point, value) = offered(&rig);
    assert_eq!(
        rig.hub
            .command(&point, value.clone(), Origin::Ui, None)
            .await,
        Err(CommandError::ReadOnly)
    );
    let before = rig.script.calls.lock().unwrap().len();
    assert!(permission(&rig, "permit", true).await.failure.is_none());
    assert_eq!(
        rig.script.calls.lock().unwrap().len(),
        before,
        "consent is local only"
    );
    for origin in [Origin::Mcp, Origin::Household] {
        assert!(matches!(
            rig.hub.command(&point, value.clone(), origin, None).await,
            Err(CommandError::NeedsApproval { .. })
        ));
    }
    assert_eq!(rig.script.calls.lock().unwrap().len(), before);
    let definitions = rig.catalog().collections[&CollectionKind::Scenarios]
        .data
        .as_ref()
        .unwrap()
        .definitions
        .iter()
        .map(|d| d.raw.as_value().clone())
        .collect::<Vec<_>>();
    let mut replies = vec![
        reply(
            Endpoint::Gateways,
            200,
            &serde_json::json!([{"gatewayId":"anchor"}]),
        ),
        reply(
            Endpoint::Collection(CollectionKind::Scenarios),
            200,
            &serde_json::json!(definitions),
        ),
        reply(
            Endpoint::Scenario,
            200,
            &serde_json::json!({"execId":"synthetic-exec"}),
        ),
    ];
    replies.extend(cycle_replies());
    rig.script.replies.lock().unwrap().extend(replies);
    rig.hub
        .command(&point, value, Origin::Ui, Some("synthetic-owner".into()))
        .await
        .unwrap();
    assert_eq!(
        rig.script
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == Endpoint::Scenario)
            .count(),
        1
    );
    assert!(rig.hub.journal(20).iter().any(
        |entry| matches!(&entry.action,Action::Command{point:target,..} if target==&point)
            && entry.outcome == Outcome::Ok
    ));
    let state = permission(&rig, "revoke", false).await;
    assert!(state.failure.is_none());
    let (point, value) = offered(&rig);
    assert_eq!(
        rig.hub.command(&point, value, Origin::Ui, None).await,
        Err(CommandError::ReadOnly)
    );
    assert_eq!(
        rig.script
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == Endpoint::Scenario)
            .count(),
        1
    );
}
#[tokio::test]
async fn controller_rejects_permission_when_global_read_only_even_if_queue_is_injected() {
    let rig = connected("execution-global", true).await;
    assert_eq!(
        permission(&rig, "permit", true).await.failure,
        Some(Error::Forbidden)
    );
    assert!(
        rig.hub
            .cloud_secret(&rig.instance, execution::POLICY_KEY)
            .is_none()
    );
    let (point, value) = offered(&rig);
    assert_eq!(
        rig.hub.command(&point, value, Origin::Ui, None).await,
        Err(CommandError::ReadOnly)
    );
    assert!(
        !rig.script
            .calls
            .lock()
            .unwrap()
            .contains(&Endpoint::Scenario)
    );
}

#[tokio::test]
async fn persisted_consent_cannot_override_global_read_only() {
    let rig = connected("execution-persisted-readonly", true).await;
    let catalog = rig.catalog();
    let mut policy = Policy::default();
    policy.change(&catalog, "example", true).unwrap();
    rig.hub
        .store_secret(
            &rig.instance,
            execution::POLICY_KEY,
            &policy.encode().unwrap(),
        )
        .await
        .unwrap();
    rig.script.replies.lock().unwrap().extend(cycle_replies());
    rig.queue(
        "read-again",
        PendingRequest::Refresh {
            generation: catalog.connection_generation.clone(),
        },
    )
    .await;
    rig.wait_state(|state| state.request_id.as_deref() == Some("read-again"))
        .await;
    let (point, value) = offered(&rig);
    assert_eq!(
        rig.hub.command(&point, value, Origin::Ui, None).await,
        Err(CommandError::ReadOnly)
    );
    assert!(
        !rig.script
            .calls
            .lock()
            .unwrap()
            .contains(&Endpoint::Scenario)
    );
    assert!(permission(&rig, "revoke", false).await.failure.is_none());
    let policy = Policy::decode(
        rig.hub
            .cloud_secret(&rig.instance, execution::POLICY_KEY)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert!(!policy.permits(&rig.catalog(), "example"));
}

fn execution_replies(rig: &Rig) -> Vec<Entry> {
    let definitions = rig.catalog().collections[&CollectionKind::Scenarios]
        .data
        .as_ref()
        .unwrap()
        .definitions
        .iter()
        .map(|d| d.raw.as_value().clone())
        .collect::<Vec<_>>();
    let mut replies = vec![
        reply(
            Endpoint::Gateways,
            200,
            &serde_json::json!([{"gatewayId":"anchor"}]),
        ),
        reply(
            Endpoint::Collection(CollectionKind::Scenarios),
            200,
            &serde_json::json!(definitions),
        ),
        reply(
            Endpoint::Scenario,
            200,
            &serde_json::json!({"execId":"synthetic-exec"}),
        ),
    ];
    replies.extend(cycle_replies());
    replies
}
async fn purge_cloud(rig: &Rig, other_process: bool) {
    let hub = if other_process {
        Hub::new(HubOptions {
            secrets: Some((rig.path.clone(), MasterKey::new("d".repeat(64)))),
            ..HubOptions::default()
        })
        .unwrap()
    } else {
        rig.hub.clone()
    };
    // This is the exact durable operation acknowledged by the DELETE handler.
    assert!(
        hub.purge_private_state(
            &rig.instance,
            [
                SESSION_KEY,
                STATE_KEY,
                inventory::INVENTORY_KEY,
                PENDING_KEY,
                execution::POLICY_KEY,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        )
        .await
        .unwrap()
    );
}
async fn revoked_during_preflight(other_process: bool) {
    let gate = Arc::<PreflightGate>::default();
    let rig = connected_controlled(
        if other_process {
            "preflight-other"
        } else {
            "preflight-local"
        },
        false,
        gate.clone(),
    )
    .await;
    assert!(permission(&rig, "permit", true).await.failure.is_none());
    let (point, value) = offered(&rig);
    rig.script
        .replies
        .lock()
        .unwrap()
        .extend(execution_replies(&rig));
    gate.pause_ticks.store(true, Ordering::SeqCst);
    gate.armed.store(true, Ordering::SeqCst);
    let hub = rig.hub.clone();
    let command = tokio::spawn(async move {
        hub.command(&point, value, Origin::Ui, Some("synthetic-owner".into()))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    purge_cloud(&rig, other_process).await;
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(2), command)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !rig.script
            .calls
            .lock()
            .unwrap()
            .contains(&Endpoint::Scenario),
        "revocation acknowledged during the preparatory GET must prevent the POST"
    );
    assert!(result.is_err());
}
#[tokio::test]
async fn disconnect_during_preflight_prevents_post_without_waiting_for_controller_tick() {
    revoked_during_preflight(false).await;
}
#[tokio::test]
async fn another_process_disconnect_during_preflight_prevents_post() {
    revoked_during_preflight(true).await;
}
#[tokio::test]
async fn dispatch_after_disconnect_immediately_retires_cached_run_capabilities() {
    let gate = Arc::<PreflightGate>::default();
    let rig = connected_controlled("dispatch-revoked", false, gate.clone()).await;
    assert!(permission(&rig, "permit", true).await.failure.is_none());
    let (point, value) = offered(&rig);
    rig.script
        .replies
        .lock()
        .unwrap()
        .extend(execution_replies(&rig));
    gate.pause_ticks.store(true, Ordering::SeqCst);
    purge_cloud(&rig, false).await;
    let before = rig.script.calls.lock().unwrap().len();
    assert!(
        rig.hub
            .command(&point, value, Origin::Ui, Some("synthetic-owner".into()))
            .await
            .is_err()
    );
    assert_eq!(rig.script.calls.lock().unwrap().len(), before);
    let (id, key) = point.split().unwrap();
    let device = rig.hub.device(&id).unwrap();
    assert_eq!(device.online, Some(false));
    assert!(!device.device.point(key).unwrap().access.write);
}

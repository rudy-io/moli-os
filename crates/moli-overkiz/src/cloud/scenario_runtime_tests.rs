use super::*;
use crate::cloud::execution::{self, Policy};
use moli_core::{Action, Origin, Outcome, PointId};
use moli_runtime::CommandError;

#[derive(Debug)]
struct ExecutionProbe {
    script: Script,
    read_only: bool,
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
            let mut cloud = Controller::with_transport(ctx, &config, self.script.clone());
            let mut tick = tokio::time::interval(Duration::from_millis(5));
            loop {
                crate::reconcile_status(ctx, false, None, Some(&cloud));
                tokio::select! {
                    _=tick.tick()=>cloud.tick(ctx),
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
async fn connected(label: &str, read_only: bool) -> Rig {
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

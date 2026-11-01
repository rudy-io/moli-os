use super::super::protocol::{Endpoint, Request, Response};
use super::super::{PENDING_KEY, RequestEnvelope, SESSION_KEY, STATE_KEY, SessionPhase};
use super::*;
use crate::Config;
use crate::inventory::{CollectionKind, CollectionStatus};
use moli_core::{DriverStatus, InstanceId};
use moli_runtime::{Driver, Hub, HubOptions, MasterKey, spawn_driver};
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

type Entry = (Endpoint, Result<Response, Error>, Duration);

#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<VecDeque<Entry>>>,
    calls: Arc<Mutex<Vec<Endpoint>>>,
    active: Arc<AtomicUsize>,
    hub: Hub,
    instance: InstanceId,
    path: std::path::PathBuf,
}
impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Script")
    }
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Transport for Script {
    fn send(&mut self, request: Request) -> BoxFuture<'_, Result<Response, Error>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(request.endpoint);
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(Arc::clone(&self.active));
            if request.endpoint == Endpoint::Accounts {
                assert!(
                    self.hub.secret(&self.instance, PENDING_KEY).is_none(),
                    "password input deleted before use"
                );
                let reopened = Hub::new(HubOptions {
                    secrets: Some((self.path.clone(), MasterKey::new("d".repeat(64)))),
                    ..HubOptions::default()
                })
                .unwrap();
                let marker = reopened.secret(&self.instance, SESSION_KEY).unwrap();
                let binding =
                    super::super::read_session_binding(marker.as_bytes(), self.instance.as_str())
                        .unwrap();
                assert_eq!(
                    binding.phase,
                    SessionPhase::LoginRequired,
                    "durable bootstrap ACK before password POST"
                );
                assert!(binding.scope.is_none());
                assert!(!marker.contains("PASSWORD_SENTINEL"));
            }
            if request.endpoint == Endpoint::Refresh {
                let encoded = self.hub.secret(&self.instance, SESSION_KEY).unwrap();
                assert_eq!(
                    super::super::read_session_binding(encoded.as_bytes(), self.instance.as_str())
                        .unwrap()
                        .phase,
                    SessionPhase::Refreshing
                );
            }
            let next = self.replies.lock().unwrap().pop_front();
            let Some((endpoint, response, delay)) = next else {
                return Err(Error::Network);
            };
            assert_eq!(request.endpoint, endpoint);
            tokio::time::sleep(delay).await;
            response
        })
    }
}
fn reply(
    endpoint: Endpoint,
    status: u16,
    body: &serde_json::Value,
) -> (Endpoint, Result<Response, Error>, Duration) {
    (
        endpoint,
        Ok(Response {
            status,
            retry_after: None,
            body: serde_json::to_vec(body).unwrap(),
        }),
        Duration::ZERO,
    )
}
fn login_replies() -> Vec<Entry> {
    vec![
        reply(
            Endpoint::Accounts,
            200,
            &serde_json::json!({"access_token":"account-token"}),
        ),
        reply(
            Endpoint::Exchange,
            200,
            &serde_json::json!({"access_token":"access-token","refresh_token":"refresh-token","expires_in":3600}),
        ),
        reply(
            Endpoint::Directory,
            200,
            &serde_json::json!({"totalCount":1,"results":[{"siteOID":"parent","name":"PRIVATE_LABEL_SENTINEL","country":"FR","subSites":[{"externalOID":"setup","gateways":[{"gatewayId":"anchor"}]}]}]}),
        ),
    ]
}
fn selected_replies() -> Vec<Entry> {
    vec![
        reply(
            Endpoint::Refresh,
            200,
            &serde_json::json!({"access_token":"scoped-token","refresh_token":"rotated-token","expires_in":3600}),
        ),
        reply(
            Endpoint::Gateways,
            200,
            &serde_json::json!([{"gatewayId":"anchor"}]),
        ),
    ]
}
fn cycle_replies() -> Vec<Entry> {
    let mut replies = vec![reply(
        Endpoint::Gateways,
        200,
        &serde_json::json!([{"gatewayId":"anchor"}]),
    )];
    for kind in CollectionKind::ALL {
        let body = if kind == CollectionKind::Scenarios {
            serde_json::json!([{"oid":"example", "label":"PRIVATE_DEFINITION_SENTINEL", "actions":[{"deviceURL":"io://PRIVATE_TARGET_SENTINEL", "commands":[]}]}])
        } else {
            serde_json::json!([])
        };
        replies.push(reply(Endpoint::Collection(kind), 200, &body));
    }
    replies.push(reply(
        Endpoint::Location,
        200,
        &serde_json::json!({"timeZone":"Europe/Paris"}),
    ));
    replies.push(reply(Endpoint::Devices, 200, &serde_json::json!([])));
    replies
}
struct Rig {
    hub: Hub,
    instance: InstanceId,
    path: std::path::PathBuf,
    script: Script,
    cancel: CancellationToken,
}
#[derive(Debug)]
struct DriverProbe {
    script: Script,
    production: Option<Config>,
}
impl Driver for DriverProbe {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let cloud_config = super::super::Config::default();
            let mut cloud = Controller::with_transport(ctx, &cloud_config, self.script.clone());
            if let Some(config) = &self.production {
                return crate::run_sources(config, ctx, Some(cloud)).await;
            }
            let mut tick = tokio::time::interval(Duration::from_millis(5));
            loop {
                crate::reconcile_status(ctx, false, None, Some(&cloud));
                tokio::select! {
                    _ = tick.tick() => cloud.tick(ctx),
                    event = cloud.next_event(), if cloud.receiving() => cloud.handle(ctx, event),
                    command = ctx.next_command() => {
                        let Some(command) = command else { return Ok(()); };
                        command.reply(Err(moli_i18n::tr!("overkiz.refused")));
                    }
                }
            }
        })
    }
}
impl Rig {
    fn new(label: &str, replies: Vec<Entry>) -> Self {
        let instance = InstanceId::from("cloud-runtime");
        let path = std::env::temp_dir().join(format!(
            "cloud-runtime-{label}-{}-{}.enc",
            std::process::id(),
            moli_core::now_ms()
        ));
        let hub = Hub::new(HubOptions {
            secrets: Some((path.clone(), MasterKey::new("d".repeat(64)))),
            ..HubOptions::default()
        })
        .unwrap();
        let script = Script {
            replies: Arc::new(Mutex::new(replies.into())),
            calls: Arc::new(Mutex::new(vec![])),
            active: Arc::new(AtomicUsize::new(0)),
            hub: hub.clone(),
            instance: instance.clone(),
            path: path.clone(),
        };
        Self {
            hub,
            instance,
            path,
            script,
            cancel: CancellationToken::new(),
        }
    }
    fn start(&self) {
        self.start_config(None);
    }
    fn start_config(&self, config: Option<Config>) {
        spawn_driver(
            &self.hub,
            self.instance.clone(),
            Arc::new(DriverProbe {
                script: self.script.clone(),
                production: config,
            }),
            self.cancel.clone(),
        );
    }
    async fn queue(&self, id: &str, request: PendingRequest) {
        let envelope = RequestEnvelope {
            version: 1,
            request_id: id.into(),
            submitted_at_ms: now_ms(),
            request,
        };
        let bytes = super::super::encode_request(&envelope).unwrap();
        assert!(
            self.hub
                .insert_secret_if_absent(
                    &self.instance,
                    PENDING_KEY,
                    std::str::from_utf8(&bytes).unwrap()
                )
                .await
                .unwrap()
        );
    }
    fn state(&self) -> Option<ConnectionState> {
        self.hub
            .secret(&self.instance, STATE_KEY)
            .and_then(|v| super::super::decode_state(v.as_bytes()).ok())
    }
    async fn wait_state(&self, predicate: impl Fn(&ConnectionState) -> bool) -> ConnectionState {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(state) = self.state()
                    && predicate(&state)
                    && (state.phase != ConnectionPhase::Ready
                        || state.failure.is_some()
                        || self
                            .hub
                            .device(&super::super::summary_device_id(&self.instance))
                            .is_some_and(|summary| summary.online == Some(true)))
                {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn login(&self) -> String {
        self.queue(
            "login",
            PendingRequest::Login {
                username: "user@example.com".into(),
                password: "PASSWORD_SENTINEL".into(),
            },
        )
        .await;
        self.start();
        self.wait_state(|s| s.request_id.as_deref() == Some("login"))
            .await
            .generation
            .unwrap()
    }
    async fn select(&self, generation: String) -> ConnectionState {
        self.queue(
            "select",
            PendingRequest::SelectSite {
                generation,
                site_oid: "setup".into(),
                gateway_id: "anchor".into(),
                region: None,
            },
        )
        .await;
        self.wait_state(|s| s.request_id.as_deref() == Some("select"))
            .await
    }
    fn catalog(&self) -> Catalog {
        inventory::decode_catalog(
            self.hub
                .secret(&self.instance, inventory::INVENTORY_KEY)
                .unwrap()
                .as_bytes(),
        )
        .unwrap()
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn controller_consumes_password_once_acks_sessions_and_publishes_private_safe_summary() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new("success", replies);
    let generation = rig.login().await;
    let state = rig.select(generation).await;
    assert_eq!(state.phase, ConnectionPhase::Ready);
    assert!(state.last_attempt_ms.is_some() && state.last_observation_ms.is_some());
    let summary = rig
        .hub
        .device(&super::super::summary_device_id(&rig.instance))
        .unwrap();
    assert_eq!(summary.online, Some(true));
    let (device, _) = rig
        .hub
        .describe(&super::super::summary_device_id(&rig.instance))
        .unwrap();
    assert!(device.members.is_empty());
    assert!(
        device
            .points
            .iter()
            .all(|p| !p.access.write && p.semantic == Semantic::Other)
    );
    assert!(
        device.point("scenarios_status").is_some(),
        "each collection keeps its independent qualification"
    );
    let bus = serde_json::to_string(&rig.hub.snapshot()).unwrap();
    for sentinel in [
        "PASSWORD_SENTINEL",
        "PRIVATE_LABEL_SENTINEL",
        "PRIVATE_DEFINITION_SENTINEL",
        "PRIVATE_TARGET_SENTINEL",
        "rotated-token",
    ] {
        assert!(!bus.contains(sentinel), "private data leaked: {sentinel}");
    }
    assert_eq!(
        rig.catalog().collections[&CollectionKind::Scenarios]
            .data
            .as_ref()
            .unwrap()
            .definitions
            .len(),
        1
    );
    let raw = std::fs::read(&rig.path).unwrap();
    assert!(!raw.windows(17).any(|w| w == b"PASSWORD_SENTINEL"));
    assert!(rig.script.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn completed_partial_failure_persists_last_good_and_independent_collection_dates() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new("partial", replies);
    let generation = rig.login().await;
    rig.select(generation.clone()).await;
    let old = rig.catalog();
    let mut replies = cycle_replies();
    replies[1] = reply(
        Endpoint::Collection(CollectionKind::Scenarios),
        403,
        &serde_json::json!({}),
    );
    rig.script.replies.lock().unwrap().extend(replies);
    rig.queue("refresh", PendingRequest::Refresh { generation })
        .await;
    let state = rig
        .wait_state(|s| s.request_id.as_deref() == Some("refresh"))
        .await;
    assert!(state.failure.is_some());
    let current = rig.catalog();
    let previous = &old.collections[&CollectionKind::Scenarios];
    let failed = &current.collections[&CollectionKind::Scenarios];
    assert_eq!(failed.status, CollectionStatus::Forbidden);
    assert_eq!(failed.data, previous.data);
    assert_eq!(failed.last_observation_ms, previous.last_observation_ms);
    assert!(
        current.collections[&CollectionKind::CalendarDays].last_observation_ms
            >= old.collections[&CollectionKind::CalendarDays].last_observation_ms
    );
    assert_eq!(
        rig.hub
            .device(&super::super::summary_device_id(&rig.instance))
            .unwrap()
            .online,
        Some(false)
    );
}

#[tokio::test]
async fn new_login_invalidates_old_binding_before_password_and_old_catalog_is_never_relabelled() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new("generation", replies);
    let old_generation = rig.login().await;
    rig.select(old_generation.clone()).await;
    let old = rig.catalog();
    let mut reconnect = login_replies();
    reconnect[0].1 = Err(Error::Network);
    rig.script.replies.lock().unwrap().extend(reconnect);
    rig.queue(
        "new-login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    let state = rig
        .wait_state(|s| s.request_id.as_deref() == Some("new-login"))
        .await;
    assert_ne!(state.generation.as_ref(), Some(&old_generation));
    assert!(state.scope.is_none());
    assert_eq!(
        rig.catalog(),
        old,
        "retained old private catalogue must not be assigned the new generation"
    );
    let binding = super::super::read_session_binding(
        rig.hub
            .secret(&rig.instance, SESSION_KEY)
            .unwrap()
            .as_bytes(),
        rig.instance.as_str(),
    )
    .unwrap();
    assert_ne!(binding.generation, old_generation);
    assert!(binding.scope.is_none());
    assert_eq!(
        rig.hub
            .device(&super::super::summary_device_id(&rig.instance))
            .unwrap()
            .online,
        Some(false)
    );
}

#[tokio::test]
async fn stopped_actor_cancels_pending_transport_and_never_continues_rotating() {
    let mut replies = login_replies();
    replies[0].2 = Duration::from_secs(60);
    let rig = Rig::new("stop", replies);
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    rig.start();
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.script.active.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    rig.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.script.active.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rig.script.calls.lock().unwrap().as_slice(),
        [Endpoint::Accounts]
    );
    assert!(matches!(
        rig.hub.snapshot().drivers[0].status,
        DriverStatus::Stopped
    ));
}

#[test]
fn durable_retry_gate_keeps_provider_long_zero_and_overflow_delays() {
    for seconds in [0, 86_400, u64::MAX] {
        let state = ConnectionState {
            retry_after_secs: Some(seconds),
            last_attempt_ms: Some(now_ms()),
            ..ConnectionState::default()
        };
        assert_eq!(Gate::restore(&state).open(), seconds == 0);
    }
}

#[tokio::test]
async fn reconnect_aborts_old_cycle_and_old_generation_cannot_overwrite_new_session() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new("inflight-generation", replies);
    let generation = rig.login().await;
    rig.select(generation.clone()).await;
    let before = rig.catalog();
    let mut delayed = cycle_replies();
    delayed.truncate(2);
    delayed[1].2 = Duration::from_secs(60);
    rig.script.replies.lock().unwrap().extend(delayed);
    rig.queue(
        "old-read",
        PendingRequest::Refresh {
            generation: generation.clone(),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.script.active.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    rig.script.replies.lock().unwrap().extend(login_replies());
    rig.queue(
        "replace",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    let state = rig
        .wait_state(|s| s.request_id.as_deref() == Some("replace"))
        .await;
    assert_ne!(state.generation.as_ref(), Some(&generation));
    assert_eq!(rig.catalog(), before);
    assert_eq!(rig.script.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn snapshot_storage_failure_never_publishes_a_fresh_observation() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new("storage", replies);
    let generation = rig.login().await;
    rig.select(generation.clone()).await;
    let old = rig.state().unwrap();
    // A real disk failure before the next catalogue commit, after a valid session already exists.
    let mut next = cycle_replies();
    next[1].2 = Duration::from_millis(100);
    rig.script.replies.lock().unwrap().extend(next);
    rig.queue("fail-storage", PendingRequest::Refresh { generation })
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.script.active.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(&rig.path).unwrap();
    std::fs::create_dir(&rig.path).unwrap();
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(
        rig.hub
            .device(&super::super::summary_device_id(&rig.instance))
            .unwrap()
            .online,
        Some(false)
    );
    assert_eq!(
        rig.state().unwrap().last_observation_ms,
        old.last_observation_ms
    );
    assert!(
        rig.hub.snapshot().drivers[0]
            .warning
            .as_ref()
            .is_some_and(|s| s.contains(&moli_i18n::tr!("overkiz.cloud_persistence")))
    );
    std::fs::remove_dir(&rig.path).unwrap();
}

#[derive(Clone, Copy, Debug)]
enum Check {
    StaleSaves,
    Capacity,
    Closed,
    CheckpointFailure,
    Warnings,
    SlowDisk,
    DelayedReconnect,
    NativeAwait,
    ProviderGate,
    WaitingSelection,
}
#[derive(Debug)]
struct CheckProbe {
    script: Script,
    check: Check,
    done: Mutex<Option<oneshot::Sender<()>>>,
}
impl Driver for CheckProbe {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut cloud = Controller::with_transport(
                ctx,
                &super::super::Config::default(),
                self.script.clone(),
            );
            match self.check {
                Check::StaleSaves => Self::check_stale_saves(ctx, &mut cloud),
                Check::Capacity => self.check_capacity(ctx, &mut cloud).await,
                Check::Closed => self.check_closed(ctx, &mut cloud).await,
                Check::CheckpointFailure => self.check_checkpoint_failure(ctx, &mut cloud).await,
                Check::SlowDisk => self.check_slow_disk(ctx, &mut cloud).await,
                Check::DelayedReconnect => self.check_delayed_reconnect(ctx, &mut cloud).await,
                Check::NativeAwait => self.check_native_await(ctx, &mut cloud).await,
                Check::ProviderGate => self.check_provider_gate(ctx, &mut cloud),
                Check::WaitingSelection => {
                    cloud.tick(ctx);
                    assert!(
                        !cloud.busy && cloud.job.is_none(),
                        "saved authenticated choices wait for explicit selection without periodic discovery"
                    );
                }
                Check::Warnings => self.check_warnings(ctx, &mut cloud),
            }
            if let Some(done) = self.done.lock().unwrap().take() {
                done.send(()).unwrap();
            }
            ctx.cancelled().await;
            Ok(())
        })
    }
}
impl CheckProbe {
    fn check_stale_saves(ctx: &DriverCtx, cloud: &mut Controller) {
        let before = ctx.secret(SESSION_KEY).unwrap();
        assert!(matches!(
            cloud.validate_session(ctx, cloud.epoch.wrapping_sub(1), before.as_bytes(), false),
            Err(Error::Persistence)
        ));
        assert!(matches!(
            cloud.validate_session(ctx, cloud.epoch, before.as_bytes(), true),
            Err(Error::Persistence)
        ));
        let mut wrong: serde_json::Value = serde_json::from_str(&before).unwrap();
        wrong["generation"] = serde_json::json!("different-generation");
        assert!(matches!(
            cloud.validate_session(
                ctx,
                cloud.epoch,
                &serde_json::to_vec(&wrong).unwrap(),
                false
            ),
            Err(Error::ScopeMismatch)
        ));
        assert_eq!(ctx.secret(SESSION_KEY).unwrap(), before);
    }
    async fn check_capacity(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        let before = ctx.secret(PENDING_KEY).unwrap();
        let permit = cloud
            .actor
            .as_ref()
            .unwrap()
            .commands
            .clone()
            .try_reserve_owned()
            .unwrap();
        cloud.tick(ctx);
        assert_eq!(
            ctx.secret(PENDING_KEY).as_deref(),
            Some(before.as_str()),
            "full channel cannot consume queued credentials"
        );
        drop(permit);
        cloud.tick(ctx);
        let event = cloud.next_event().await;
        cloud.handle(ctx, event);
        assert!(ctx.secret(PENDING_KEY).is_none());
    }
    async fn check_closed(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        cloud.actor.as_ref().unwrap().task.abort();
        let event = cloud.next_event().await;
        assert!(event.is_none());
        cloud.handle(ctx, event);
        assert!(!cloud.receiving());
        assert!(
            tokio::time::timeout(Duration::from_millis(15), cloud.next_event())
                .await
                .is_err(),
            "closed channel is disabled instead of spinning"
        );
        assert!(self.script.calls.lock().unwrap().is_empty());
    }
    async fn check_checkpoint_failure(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        cloud.tick(ctx);
        let input = cloud.next_event().await;
        cloud.handle(ctx, input);
        let event = cloud.next_event().await;
        assert!(matches!(event, Some(Event::Session { .. })));
        std::fs::remove_file(&self.script.path).unwrap();
        std::fs::create_dir(&self.script.path).unwrap();
        cloud.handle(ctx, event);
        let stored = cloud.next_event().await;
        cloud.handle(ctx, stored);
        let completed = cloud.next_event().await;
        assert!(
            matches!(completed, Some(Event::Complete { ref state, .. }) if state.failure == Some(Error::Persistence))
        );
        assert!(
            self.script.calls.lock().unwrap().is_empty(),
            "failed durable marker must send zero credentials"
        );
        std::fs::remove_dir(&self.script.path).unwrap();
    }
    async fn check_slow_disk(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        cloud.tick(ctx);
        let input = cloud.next_event().await;
        cloud.handle(ctx, input);
        let event = cloud.next_event().await;
        assert!(matches!(event, Some(Event::Session { .. })));
        let temporary = self.script.path.with_extension("tmp");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&temporary)
                .status()
                .unwrap()
                .success()
        );
        let handled = tokio::time::timeout(Duration::from_millis(50), async {
            cloud.handle(ctx, event);
        })
        .await;
        // A reader releases the actual blocked filesystem open, even on RED.
        tokio::task::spawn_blocking(move || std::fs::read(temporary))
            .await
            .unwrap()
            .unwrap();
        assert!(
            handled.is_ok(),
            "a slow cloud disk checkpoint must not occupy the native controller"
        );
        let stored = cloud.next_event().await;
        assert!(
            matches!(
                stored,
                Some(Event::JobFinished {
                    result: Err(Error::Persistence),
                    ..
                })
            ),
            "FIFO file sync failure must propagate"
        );
        cloud.handle(ctx, stored);
        let completed = cloud.next_event().await;
        assert!(
            matches!(completed, Some(Event::Complete { ref state, .. }) if state.failure == Some(Error::Persistence))
        );
        assert!(
            self.script.calls.lock().unwrap().is_empty(),
            "failed fsync cannot authorize a password POST"
        );
    }
    async fn check_delayed_reconnect(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        delayed_reconnect(ctx, cloud, &self.script).await;
    }
    async fn check_native_await(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        cloud.tick(ctx);
        let input = cloud.next_event().await;
        cloud.handle(ctx, input);
        let mut cloud_slot = Some(Controller::with_transport(
            ctx,
            &super::super::Config::default(),
            self.script.clone(),
        ));
        std::mem::swap(cloud_slot.as_mut().unwrap(), cloud);
        during(
            tokio::time::sleep(Duration::from_secs(11)),
            &mut cloud_slot,
            ctx,
        )
        .await;
        let cloud = cloud_slot.as_ref().unwrap();
        assert_eq!(cloud.state.request_id.as_deref(), Some("login"));
        assert!(cloud.state.failure.is_none());
        assert_eq!(self.script.calls.lock().unwrap().len(), 3);
    }
    fn check_provider_gate(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        for _ in 0..5 {
            cloud.tick(ctx);
        }
        assert!(ctx.secret(PENDING_KEY).is_some());
        assert!(self.script.calls.lock().unwrap().is_empty());
        assert!(cloud.job.is_none());
    }
    fn check_warnings(&self, ctx: &DriverCtx, cloud: &mut Controller) {
        cloud.warning = Some("synthetic cloud warning".into());
        crate::reconcile_status(ctx, false, Some("synthetic native warning"), Some(cloud));
        let warning = self.script.hub.snapshot().drivers[0]
            .warning
            .clone()
            .unwrap();
        assert!(warning.contains("native") && warning.contains("cloud"));
        cloud.warning = None;
        crate::reconcile_status(ctx, true, Some("synthetic native warning"), Some(cloud));
        assert_eq!(
            self.script.hub.snapshot().drivers[0].warning.as_deref(),
            Some("synthetic native warning")
        );
        cloud.warning = Some("synthetic cloud warning".into());
        crate::reconcile_status(ctx, true, None, Some(cloud));
        assert_eq!(
            self.script.hub.snapshot().drivers[0].warning.as_deref(),
            Some("synthetic cloud warning")
        );
        cloud.warning = None;
        crate::reconcile_status(ctx, true, None, Some(cloud));
        assert!(self.script.hub.snapshot().drivers[0].warning.is_none());
    }
}

async fn check(rig: &Rig, check: Check) {
    let (done, finished) = oneshot::channel();
    spawn_driver(
        &rig.hub,
        rig.instance.clone(),
        Arc::new(CheckProbe {
            script: rig.script.clone(),
            check,
            done: Mutex::new(Some(done)),
        }),
        rig.cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(15), finished)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn full_command_channel_preserves_pending_slot_until_capacity_is_reserved() {
    let rig = Rig::new("capacity", vec![]);
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    check(&rig, Check::Capacity).await;
}
#[tokio::test]
async fn failed_checkpoint_sends_no_password_or_provider_request() {
    let rig = Rig::new("checkpoint-fail", vec![]);
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    check(&rig, Check::CheckpointFailure).await;
}
#[tokio::test]
async fn closed_actor_channel_is_disabled_without_spinning() {
    let rig = Rig::new("closed", vec![]);
    check(&rig, Check::Closed).await;
}
#[tokio::test]
async fn source_warning_recovery_never_clears_the_other_source_warning() {
    let rig = Rig::new("warnings", vec![]);
    check(&rig, Check::Warnings).await;
}
#[tokio::test]
async fn stale_epoch_generation_or_cancelled_sink_cannot_write_a_session() {
    let original = Rig::new("binding-seed", login_replies());
    original.login().await;
    let rig = Rig::new("stale-saves", vec![]);
    rig.hub
        .store_secret(
            &rig.instance,
            SESSION_KEY,
            &original
                .hub
                .secret(&original.instance, SESSION_KEY)
                .unwrap(),
        )
        .await
        .unwrap();
    check(&rig, Check::StaleSaves).await;
}

#[tokio::test]
async fn corrupt_saved_session_does_not_prevent_owner_reconnection() {
    let rig = Rig::new("corrupt-session", login_replies());
    rig.hub
        .store_secret(&rig.instance, SESSION_KEY, "not a valid session")
        .await
        .unwrap();
    let generation = rig.login().await;
    assert!(!generation.is_empty());
}

#[tokio::test]
async fn missing_native_credentials_do_not_park_cloud_onboarding() {
    let rig = Rig::new("native-missing", login_replies());
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    let config: Config =
        serde_json::from_value(serde_json::json!({"host":"localhost", "cloud": {}})).unwrap();
    rig.start_config(Some(config));
    rig.wait_state(|state| state.request_id.as_deref() == Some("login"))
        .await;
    assert_eq!(rig.script.calls.lock().unwrap().len(), 3);
    let status = &rig.hub.snapshot().drivers[0];
    assert_eq!(status.status, DriverStatus::Running);
    assert!(
        status
            .warning
            .as_ref()
            .unwrap()
            .contains(&moli_i18n::tr!("overkiz.auth"))
    );
    assert!(rig.hub.secret(&rig.instance, "cert_sha256").is_none());
}

#[tokio::test]
async fn slow_cloud_auth_cannot_block_native_polling_or_command_preflight() {
    let mut replies = login_replies();
    replies[0].2 = Duration::from_secs(60);
    let rig = Rig::new("native-independent", replies);
    rig.hub
        .store_secret(&rig.instance, "token", "synthetic-test-token")
        .await
        .unwrap();
    let posts = Arc::new(AtomicUsize::new(0));
    let status = Arc::new(AtomicUsize::new(0));
    let (port, server) = crate::tests::fixture_server(
        &rig.hub,
        &rig.instance,
        Arc::clone(&posts),
        Arc::clone(&status),
    )
    .await;
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    rig.start_config(Some(
        serde_json::from_value(
            serde_json::json!({"host":"localhost", "port":port, "poll_secs":1, "cloud": {}}),
        )
        .unwrap(),
    ));
    let entries = crate::model::parse(
        &serde_json::from_str(include_str!(
            "../../../../integrations/overkiz/fixtures/devices.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let device = entries[0].device(&rig.instance);
    tokio::time::timeout(Duration::from_secs(3), async {
        while rig.hub.device(&device.id).is_none() || rig.script.active.load(Ordering::SeqCst) == 0
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let started = Instant::now();
    let result = rig
        .hub
        .command(
            &moli_core::PointId::new(&device.id, "percent_control"),
            Value::Float(50.0),
            moli_core::Origin::Ui,
            None,
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.script.active.load(Ordering::SeqCst),
        1,
        "native command completed while cloud is still waiting"
    );
    status.store(1, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.hub.device(&device.id).unwrap().online != Some(false) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        rig.hub
            .device(&super::super::summary_device_id(&rig.instance))
            .is_some()
    );
    server.abort();
}

#[derive(Debug)]
struct RestartProbe {
    script: Script,
    runs: AtomicUsize,
}
impl Driver for RestartProbe {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut native = summary(ctx.instance());
                native.id = ctx.device_id("unrefreshed-native");
                let native_id = native.id.clone();
                ctx.upsert_device(native);
                ctx.set_availability(&native_id, true);
                ctx.upsert_device(summary(ctx.instance()));
                ctx.set_availability(&super::super::summary_device_id(ctx.instance()), true);
                ctx.ready();
                anyhow::bail!("synthetic restart");
            }
            let cloud = Controller::with_transport(
                ctx,
                &super::super::Config::default(),
                self.script.clone(),
            );
            let config =
                serde_json::from_value(serde_json::json!({"host":"localhost", "cloud": {}}))
                    .unwrap();
            crate::run_sources(&config, ctx, Some(cloud)).await
        })
    }
}
#[tokio::test]
async fn cold_restart_cloud_recovery_cannot_resurrect_an_unrefreshed_native_source() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let seed = Rig::new("restart-seed", replies);
    let generation = seed.login().await;
    seed.select(generation).await;
    let rig = Rig::new("cold-restart", cycle_replies());
    for key in [SESSION_KEY, STATE_KEY, inventory::INVENTORY_KEY] {
        rig.hub
            .store_secret(
                &rig.instance,
                key,
                &seed.hub.secret(&seed.instance, key).unwrap(),
            )
            .await
            .unwrap();
    }
    spawn_driver(
        &rig.hub,
        rig.instance.clone(),
        Arc::new(RestartProbe {
            script: rig.script.clone(),
            runs: AtomicUsize::new(0),
        }),
        rig.cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while rig.script.calls.lock().unwrap().len() < 9 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig
            .hub
            .device(&super::super::summary_device_id(&rig.instance))
            .unwrap()
            .online
            != Some(true)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rig.hub
            .device(&moli_core::DeviceId::new(
                &rig.instance,
                "unrefreshed-native"
            ))
            .unwrap()
            .online,
        Some(false)
    );
    assert!(
        rig.hub.snapshot().drivers[0]
            .warning
            .as_ref()
            .unwrap()
            .contains(&moli_i18n::tr!("overkiz.auth"))
    );
}

#[tokio::test]
async fn slow_disk_checkpoint_does_not_block_native_controller_progress() {
    let rig = Rig::new("slow-disk", vec![]);
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    check(&rig, Check::SlowDisk).await;
}

async fn delayed_reconnect(ctx: &DriverCtx, cloud: &mut Controller, script: &Script) {
    let old_session = ctx.secret(SESSION_KEY).unwrap();
    let old_catalog = ctx.secret(inventory::INVENTORY_KEY).unwrap();
    let old_disk = std::fs::read(&script.path).unwrap();
    std::fs::remove_file(&script.path).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&script.path)
            .status()
            .unwrap()
            .success()
    );
    let old = ctx.store_secrets_job(vec![
        (inventory::INVENTORY_KEY.into(), old_catalog.clone()),
        (STATE_KEY.into(), "STALE_STATE_WRITE_SENTINEL".into()),
    ]);
    let (opened, reader_present) = oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let path = script.path.clone();
    let writer = tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let mut pipe = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        opened.send(()).unwrap();
        blocked.recv().unwrap();
        pipe.write_all(&old_disk).unwrap();
    });
    reader_present.await.unwrap(); // proves the old transaction is running with its owned vault guard
    old.abort();
    assert!(old.await.unwrap_err().is_cancelled());
    let envelope = RequestEnvelope {
        version: 1,
        request_id: "replacement".into(),
        submitted_at_ms: now_ms(),
        request: PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    };
    let encoded = String::from_utf8(super::super::encode_request(&envelope).unwrap()).unwrap();
    let hub = script.hub.clone();
    let instance = script.instance.clone();
    let queued = tokio::spawn(async move {
        hub.insert_secret_if_absent(&instance, PENDING_KEY, &encoded)
            .await
    });
    release.send(()).unwrap();
    writer.await.unwrap();
    assert!(queued.await.unwrap().unwrap());
    assert_ne!(
        ctx.secret(STATE_KEY).as_deref(),
        Some("STALE_STATE_WRITE_SENTINEL")
    );
    cloud.tick(ctx);
    loop {
        let event = cloud.next_event().await;
        cloud.handle(ctx, event);
        if cloud.state.request_id.as_deref() == Some("replacement") && cloud.job.is_none() {
            break;
        }
    }
    let reopened = Hub::new(HubOptions {
        secrets: Some((script.path.clone(), MasterKey::new("d".repeat(64)))),
        ..HubOptions::default()
    })
    .unwrap();
    let latest = reopened.secret(&script.instance, SESSION_KEY).unwrap();
    assert_ne!(
        super::super::read_session_binding(latest.as_bytes(), script.instance.as_str())
            .unwrap()
            .generation,
        super::super::read_session_binding(old_session.as_bytes(), script.instance.as_str())
            .unwrap()
            .generation
    );
    let state = super::super::decode_state(
        reopened
            .secret(&script.instance, STATE_KEY)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(state.request_id.as_deref(), Some("replacement"));
    assert_eq!(state.generation, cloud.state.generation);
    assert_eq!(
        reopened
            .secret(&script.instance, inventory::INVENTORY_KEY)
            .as_deref(),
        Some(old_catalog.as_str())
    );
}

#[tokio::test]
async fn cancelled_old_batch_cannot_write_state_after_a_new_login_checkpoint() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let seed = Rig::new("delayed-seed", replies);
    let generation = seed.login().await;
    seed.select(generation).await;
    let rig = Rig::new("delayed-reconnect", login_replies());
    for key in [SESSION_KEY, STATE_KEY, inventory::INVENTORY_KEY] {
        rig.hub
            .store_secret(
                &rig.instance,
                key,
                &seed.hub.secret(&seed.instance, key).unwrap(),
            )
            .await
            .unwrap();
    }
    check(&rig, Check::DelayedReconnect).await;
}
#[tokio::test]
async fn cloud_persistence_acks_are_served_during_a_native_wait_longer_than_the_sink_deadline() {
    let rig = Rig::new("native-await", login_replies());
    rig.queue(
        "login",
        PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    )
    .await;
    check(&rig, Check::NativeAwait).await;
}
#[tokio::test]
async fn durable_provider_gate_survives_actor_restart_without_consuming_owner_input() {
    for seconds in [86_400, u64::MAX] {
        let rig = Rig::new(&format!("gate-{seconds}"), login_replies());
        let state = ConnectionState {
            retry_after_secs: Some(seconds),
            last_attempt_ms: Some(now_ms()),
            ..ConnectionState::default()
        };
        let encoded = super::super::encode_state(&state).unwrap();
        rig.hub
            .store_secret(
                &rig.instance,
                STATE_KEY,
                std::str::from_utf8(&encoded).unwrap(),
            )
            .await
            .unwrap();
        rig.queue(
            "login",
            PendingRequest::Login {
                username: "user@example.com".into(),
                password: "PASSWORD_SENTINEL".into(),
            },
        )
        .await;
        check(&rig, Check::ProviderGate).await;
    }
}

#[tokio::test]
async fn manual_refreshes_coalesce_into_one_cycle_with_completion_feedback() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let rig = Rig::new("coalesce", replies);
    let generation = rig.login().await;
    rig.select(generation.clone()).await;
    let previous_calls = rig.script.calls.lock().unwrap().len();
    let mut next = cycle_replies();
    next[1].2 = Duration::from_millis(150);
    rig.script.replies.lock().unwrap().extend(next);
    rig.queue(
        "refresh-one",
        PendingRequest::Refresh {
            generation: generation.clone(),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.script.active.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    rig.queue("refresh-two", PendingRequest::Refresh { generation })
        .await;
    rig.wait_state(|state| state.request_id.as_deref() == Some("refresh-two"))
        .await;
    assert_eq!(rig.script.calls.lock().unwrap().len() - previous_calls, 9);
    assert!(rig.hub.secret(&rig.instance, PENDING_KEY).is_none());
}

#[tokio::test]
async fn persisted_ready_cloud_is_stale_until_this_runtime_observes_even_when_native_is_ready() {
    let mut replies = login_replies();
    replies.extend(selected_replies());
    replies.extend(cycle_replies());
    let seed = Rig::new("freshness-seed", replies);
    let generation = seed.login().await;
    seed.select(generation).await;
    let mut delayed = cycle_replies();
    delayed.truncate(1);
    delayed[0].2 = Duration::from_secs(60);
    let rig = Rig::new("freshness-native", delayed);
    for key in [SESSION_KEY, STATE_KEY, inventory::INVENTORY_KEY] {
        rig.hub
            .store_secret(
                &rig.instance,
                key,
                &seed.hub.secret(&seed.instance, key).unwrap(),
            )
            .await
            .unwrap();
    }
    rig.hub
        .store_secret(&rig.instance, "token", "synthetic-test-token")
        .await
        .unwrap();
    let (port, server) = crate::tests::fixture_server(
        &rig.hub,
        &rig.instance,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    rig.start_config(Some(
        serde_json::from_value(
            serde_json::json!({"host":"localhost", "port":port, "poll_secs":300, "cloud":{}}),
        )
        .unwrap(),
    ));
    let entries = crate::model::parse(
        &serde_json::from_str(include_str!(
            "../../../../integrations/overkiz/fixtures/devices.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let native = entries[0].device(&rig.instance);
    tokio::time::timeout(Duration::from_secs(3), async {
        while rig.script.active.load(Ordering::SeqCst) == 0 || rig.hub.device(&native.id).is_none()
        {
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rig.state().unwrap().phase,
        ConnectionPhase::Ready,
        "saved phase remains historical"
    );
    assert_eq!(rig.hub.snapshot().drivers[0].status, DriverStatus::Running);
    assert_eq!(rig.hub.device(&native.id).unwrap().online, Some(true));
    assert_eq!(
        rig.hub
            .device(&super::super::summary_device_id(&rig.instance))
            .unwrap()
            .online,
        Some(false),
        "native readiness cannot make an unobserved cloud source fresh"
    );
    assert_eq!(
        rig.catalog(),
        seed.catalog(),
        "same-identity last-good cache remains owner-readable as stale"
    );
    server.abort();
}

#[tokio::test]
async fn saved_site_choices_wait_for_owner_without_periodic_provider_discovery() {
    let seed = Rig::new("choices-seed", login_replies());
    seed.login().await;
    let rig = Rig::new("choices-wait", vec![]);
    for key in [SESSION_KEY, STATE_KEY] {
        rig.hub
            .store_secret(
                &rig.instance,
                key,
                &seed.hub.secret(&seed.instance, key).unwrap(),
            )
            .await
            .unwrap();
    }
    check(&rig, Check::WaitingSelection).await;
}

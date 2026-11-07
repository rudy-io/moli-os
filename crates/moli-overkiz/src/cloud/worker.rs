//! A single token owner. Only the controller can access the scoped vault or publish.
use super::client::CloudClient;
use super::protocol::Transport;
use super::session::SessionSink;
use super::{
    ConnectionPhase, ConnectionState, Error, PendingRequest, RequestEnvelope, SessionPhase,
};
use crate::inventory::{self, Catalog, CollectionKind};
use moli_core::{Access, Device, InstanceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, DriverCtx};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

const EVENT_CAPACITY: usize = 2;
const COMMAND_CAPACITY: usize = 1;

pub(crate) enum Work {
    Request(RequestEnvelope),
    Observe,
    Execute(
        moli_runtime::CommandRequest,
        Box<super::execution::Ticket>,
        moli_runtime::PrivateState,
    ),
}
// Never derive Debug: session bytes and catalogues are private.
pub(crate) enum Event {
    Ownership(Result<moli_runtime::DurableLease, Error>),
    Session {
        epoch: u64,
        encoded: Vec<u8>,
        ack: oneshot::Sender<Result<(), Error>>,
    },
    JobFinished {
        kind: Box<JobKind>,
        result: Result<Option<String>, Error>,
    },
    Complete {
        epoch: u64,
        state: Box<ConnectionState>,
        catalog: Option<Box<Catalog>>,
        observed: bool,
    },
}
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ownership(_) => "CloudOwnership([redacted])",
            Self::Session { .. } => "CloudSession([redacted])",
            Self::Complete { .. } => "CloudComplete([redacted])",
            Self::JobFinished { .. } => "CloudStorage([redacted])",
        })
    }
}
struct Sink {
    epoch: u64,
    events: mpsc::Sender<Event>,
}
impl SessionSink for Sink {
    fn save<'a>(&'a mut self, encoded: &'a [u8]) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let (ack, received) = oneshot::channel();
            self.events
                .send(Event::Session {
                    epoch: self.epoch,
                    encoded: encoded.to_vec(),
                    ack,
                })
                .await
                .map_err(|_| Error::Persistence)?;
            received.await.map_err(|_| Error::Persistence)?
        })
    }
}
struct Actor {
    commands: mpsc::Sender<Work>,
    events: mpsc::Receiver<Event>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.task.abort();
    }
}
type Factory = Box<
    dyn Fn(
            &str,
            u64,
            Option<&str>,
            Option<Catalog>,
            Arc<moli_runtime::DurableLease>,
        ) -> Result<Actor, Error>
        + Send
        + Sync,
>;

fn spawn<T: Transport + 'static>(
    mut client: CloudClient<T>,
    epoch: u64,
    session: Option<&str>,
    catalog: Option<Catalog>,
    lease: Arc<moli_runtime::DurableLease>,
) -> Result<Actor, Error> {
    if let Some(session) = session {
        client.restore(session.as_bytes())?;
    }
    let (commands, mut incoming) = mpsc::channel(COMMAND_CAPACITY);
    let (events, received) = mpsc::channel(EVENT_CAPACITY);
    let mut sink = Sink { epoch, events };
    let task = tokio::spawn(async move {
        // Keep protocol ownership until the aborted task is actually dropped.
        let _lease = lease;
        let mut catalog = catalog;
        while let Some(work) = incoming.recv().await {
            let attempted = now_ms();
            let (request_id, result, observe) = match work {
                Work::Request(envelope) => {
                    let id = Some(envelope.request_id);
                    match envelope.request {
                        PendingRequest::Login { username, password } => {
                            (id, client.login(username, password, &mut sink).await, false)
                        }
                        PendingRequest::SelectSite {
                            generation,
                            site_oid,
                            gateway_id,
                            region,
                        } => (
                            id,
                            client
                                .select_site(&generation, &site_oid, &gateway_id, region, &mut sink)
                                .await,
                            true,
                        ),
                        PendingRequest::ScenarioPermission { .. } => {
                            (id, Err(Error::InvalidInput), false)
                        }
                        PendingRequest::Refresh { generation } => {
                            let result = if client.state().generation.as_ref() == Some(&generation)
                            {
                                client.retry_persistence(&mut sink).await
                            } else {
                                Err(Error::ScopeMismatch)
                            };
                            (id, result, true)
                        }
                    }
                }
                Work::Observe => (None, client.retry_persistence(&mut sink).await, true),
                Work::Execute(command, ticket, lifetime) => (
                    None,
                    client
                        .execute_command(command, &ticket, &lifetime, &mut sink)
                        .await,
                    true,
                ),
            };
            let mut observed = false;
            let result = if result.is_ok() && observe {
                if let Ok(identity) = client.selected_identity() {
                    if catalog.as_ref().is_none_or(|c| c.identity() != identity) {
                        catalog = Catalog::new(identity).ok();
                    }
                    if let Some(catalog) = &mut catalog {
                        let result = client.read_cycle(catalog, &mut sink).await;
                        observed = result.is_ok() && client.identity().is_ok();
                        result
                    } else {
                        Err(Error::InvalidResponse)
                    }
                } else if client.can_resume_discovery() {
                    client.discover(&mut sink).await
                } else {
                    Err(Error::LoginRequired)
                }
            } else {
                result
            };
            let mut state = client.state();
            state.request_id = request_id;
            state.last_attempt_ms = Some(attempted);
            if let Err(error) = result {
                state.failure = Some(error.public_failure());
            }
            if sink
                .events
                .send(Event::Complete {
                    epoch,
                    state: Box::new(state),
                    catalog: catalog.clone().map(Box::new),
                    observed,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Ok(Actor {
        commands,
        events: received,
        task,
    })
}

#[derive(Clone, Copy, Debug)]
enum Gate {
    Open,
    Until(Instant),
    Indefinite,
}
impl Gate {
    fn restore(state: &ConnectionState) -> Self {
        let Some(seconds) = state.retry_after_secs else {
            return Self::Open;
        };
        let elapsed = state
            .last_attempt_ms
            .filter(|at| now_ms() >= *at)
            .map_or(0, |at| u64::try_from(now_ms() - at).unwrap_or(0) / 1000);
        let remaining = seconds.min(super::MAX_RETRY_SECS).saturating_sub(elapsed);
        Instant::now()
            .checked_add(Duration::from_secs(remaining))
            .map_or(Self::Indefinite, Self::Until)
    }
    fn open(self) -> bool {
        match self {
            Self::Open => true,
            Self::Until(at) => Instant::now() >= at,
            Self::Indefinite => false,
        }
    }
}

enum JobTask {
    Write(tokio::task::JoinHandle<std::io::Result<()>>),
    Take(tokio::task::JoinHandle<std::io::Result<Option<String>>>),
}
impl JobTask {
    async fn wait(&mut self) -> Result<Option<String>, Error> {
        match self {
            Self::Write(task) => task
                .await
                .map_err(|_| Error::Persistence)?
                .map(|()| None)
                .map_err(|_| Error::Persistence),
            Self::Take(task) => task
                .await
                .map_err(|_| Error::Persistence)?
                .map_err(|_| Error::Persistence),
        }
    }
}
impl Drop for JobTask {
    fn drop(&mut self) {
        match self {
            Self::Write(task) => task.abort(),
            Self::Take(task) => task.abort(),
        }
    }
}
pub(crate) enum JobKind {
    Migration {
        epoch: u64,
    },
    Input {
        epoch: u64,
        permit: Option<mpsc::OwnedPermit<Work>>,
    },
    Rejected {
        epoch: u64,
        state: Box<ConnectionState>,
    },
    Session {
        epoch: u64,
        binding: super::SessionBinding,
        ack: oneshot::Sender<Result<(), Error>>,
    },
    Cycle {
        epoch: u64,
        state: Box<ConnectionState>,
        catalog: Option<Box<Catalog>>,
        observed: bool,
    },
}
struct Job {
    task: JobTask,
    kind: JobKind,
}
#[derive(Clone, Copy)]
enum ScenarioMode {
    ReadOnly,
    Consented,
}
impl ScenarioMode {
    const fn new(read_only: bool) -> Self {
        if read_only {
            Self::ReadOnly
        } else {
            Self::Consented
        }
    }
    const fn read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}
#[derive(Default)]
struct Ownership {
    lease: Option<Arc<moli_runtime::DurableLease>>,
    job: Option<tokio::task::JoinHandle<std::io::Result<moli_runtime::DurableLease>>>,
    resume: bool,
}
#[derive(PartialEq, Eq)]
enum Migration {
    Pending,
    Complete,
}
impl Migration {
    fn current(ctx: &DriverCtx) -> Self {
        if super::storage::has_legacy(ctx) {
            Self::Pending
        } else {
            Self::Complete
        }
    }
}
pub(crate) struct Controller {
    ownership: Ownership,
    storage: Option<moli_runtime::PrivateState>,
    migration: Migration,
    actor: Option<Actor>,
    factory: Factory,
    job: Option<Job>,
    deferred: Option<Event>,
    coalesced_request_id: Option<String>,
    epoch: u64,
    generation: Option<String>,
    accepts_login_marker: bool,
    pub(crate) state: ConnectionState,
    pub(crate) warning: Option<String>,
    pub(crate) live: bool,
    busy: bool,
    next_poll: Instant,
    gate: Gate,
    poll: Duration,
    scenario_mode: ScenarioMode,
}
impl Controller {
    pub(crate) fn new(ctx: &DriverCtx, config: &super::Config) -> Self {
        let read_only = config.read_only;
        Self::with_factory(
            ctx,
            config,
            Box::new(move |instance, epoch, session, catalog, lease| {
                let mut client = CloudClient::new(instance)?;
                client.execution_read_only(read_only);
                spawn(client, epoch, session, catalog, lease)
            }),
        )
    }
    #[cfg(test)]
    pub(crate) fn with_transport<T: Transport + Clone + Send + Sync + 'static>(
        ctx: &DriverCtx,
        config: &super::Config,
        transport: T,
    ) -> Self {
        let read_only = config.read_only;
        Self::with_factory(
            ctx,
            config,
            Box::new(move |instance, epoch, session, catalog, lease| {
                let mut client = CloudClient::with_transport(instance, transport.clone())?;
                client.execution_read_only(read_only);
                spawn(client, epoch, session, catalog, lease)
            }),
        )
    }
    fn with_factory(ctx: &DriverCtx, config: &super::Config, factory: Factory) -> Self {
        let state = super::storage::cached(ctx, super::STATE_KEY)
            .and_then(|v| super::decode_state(v.as_bytes()).ok())
            .unwrap_or_default();
        let generation = ctx
            .secret(super::SESSION_KEY)
            .and_then(|v| super::read_session_binding(v.as_bytes(), ctx.instance().as_str()).ok())
            .map(|binding| binding.generation);
        let gate = Gate::restore(&state);
        let mut this = Self {
            ownership: Ownership::default(),
            storage: ctx.private_state(),
            migration: Migration::current(ctx),
            actor: None,
            factory,
            job: None,
            deferred: None,
            coalesced_request_id: None,
            epoch: 0,
            generation,
            accepts_login_marker: false,
            state,
            warning: Some(moli_i18n::tr!("overkiz.cloud_network")),
            live: false,
            busy: false,
            next_poll: Instant::now(),
            gate,
            poll: Duration::from_secs(config.poll_secs),
            scenario_mode: ScenarioMode::new(config.read_only),
        };
        this.restart(ctx, false);
        this.publish(ctx, None);
        this
    }
    fn restart(&mut self, ctx: &DriverCtx, login: bool) {
        self.job = None; // abort queued later writes; an active vault transaction keeps its lock
        self.deferred = None;
        self.coalesced_request_id = None;
        self.actor = None; // abort old epoch before allowing a new session write
        self.epoch = self.epoch.wrapping_add(1);
        self.busy = false;
        self.live = false;
        self.accepts_login_marker = login;
        let session = if login {
            None
        } else {
            ctx.secret(super::SESSION_KEY)
        };
        let catalog = super::storage::cached(ctx, inventory::INVENTORY_KEY)
            .and_then(|v| inventory::decode_catalog(v.as_bytes()).ok());
        if self.storage.is_none() || ctx.secret(super::CLEANUP_KEY).is_some() {
            self.warning = Some(moli_i18n::tr!("overkiz.cloud_persistence"));
            return;
        }
        if self.ownership.lease.is_none() {
            if self.ownership.job.is_none() {
                let secrets = ctx.durable_secrets();
                self.ownership.job = Some(tokio::task::spawn_blocking(move || {
                    secrets
                        .ok_or_else(|| std::io::Error::other("cloud vault unavailable"))?
                        .try_private_lease("cloud-session")
                }));
            }
            return;
        }
        match (self.factory)(
            ctx.instance().as_str(),
            self.epoch,
            session.as_deref(),
            catalog,
            Arc::clone(self.ownership.lease.as_ref().expect("ownership checked")),
        ) {
            Ok(actor) => self.actor = Some(actor),
            Err(error) => {
                self.warning = Some(error.to_string());
                // Invalid saved state must not prevent an owner from submitting a fresh login.
                self.actor = (self.factory)(
                    ctx.instance().as_str(),
                    self.epoch,
                    None,
                    None,
                    Arc::clone(self.ownership.lease.as_ref().expect("ownership checked")),
                )
                .ok();
            }
        }
    }
    pub(crate) fn receiving(&self) -> bool {
        self.actor.is_some() || self.job.is_some() || self.ownership.job.is_some()
    }
    pub(crate) async fn next_event(&mut self) -> Option<Event> {
        tokio::select! {
            biased;
            result = async { self.ownership.job.as_mut().expect("enabled branch").await }, if self.ownership.job.is_some() => {
                self.ownership.job = None;
                Some(Event::Ownership(result.map_err(|_| Error::Persistence).and_then(|value| value.map_err(|_| Error::Persistence))))
            },
            // A quota already reported by the actor wins over a simultaneously completed take.
            event = async { self.actor.as_mut().expect("enabled branch").events.recv().await }, if self.actor.is_some() => event,
            result = async { self.job.as_mut().expect("enabled branch").task.wait().await }, if self.job.is_some() => {
                let job = self.job.take().expect("completed job");
                Some(Event::JobFinished { kind: Box::new(job.kind), result })
            },
            else => std::future::pending().await,
        }
    }
    pub(crate) fn handle(&mut self, ctx: &DriverCtx, event: Option<Event>) {
        if let Some(Event::Ownership(result)) = event {
            self.ownership(ctx, result);
            return;
        }
        if !self.current_storage(ctx) {
            return;
        }
        let Some(event) = event else {
            self.actor = None;
            self.busy = false;
            self.live = false;
            self.warning = Some(moli_i18n::tr!("overkiz.cloud_network"));
            self.next_poll = Instant::now() + Duration::from_secs(60);
            ctx.set_availability(&super::summary_device_id(ctx.instance()), false);
            return;
        };
        match event {
            Event::Ownership(_) => unreachable!("handled before storage events"),
            Event::Session {
                epoch,
                encoded,
                ack,
            } => match self.validate_session(ctx, epoch, &encoded, ack.is_closed()) {
                Ok(binding) if self.job.is_none() => {
                    let value =
                        String::from_utf8(encoded).expect("validated session is JSON UTF-8");
                    self.job = Some(Job {
                        task: JobTask::Write(self.store_session(value)),
                        kind: JobKind::Session {
                            epoch,
                            binding,
                            ack,
                        },
                    });
                }
                result => {
                    let _ = ack.send(Err(result.err().unwrap_or(Error::Persistence)));
                }
            },
            Event::Complete {
                epoch,
                mut state,
                catalog,
                observed,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if state.retry_after_secs.is_some() {
                    state.last_attempt_ms = Some(now_ms());
                    self.gate = Gate::restore(&state);
                }
                if self.job.is_some() {
                    // A cancelled sink may complete while its disk transaction is finishing.
                    self.deferred = Some(Event::Complete {
                        epoch,
                        state,
                        catalog,
                        observed,
                    });
                } else {
                    self.finish(ctx, *state, catalog, observed);
                }
            }
            Event::JobFinished { kind, result } => self.job_finished(ctx, *kind, result),
        }
    }
    fn ownership(&mut self, ctx: &DriverCtx, result: Result<moli_runtime::DurableLease, Error>) {
        match result {
            Ok(lease) => {
                // try_lease rereads the vault under its process lock. Restore only
                // after ownership: another process may have rotated while we waited.
                self.ownership.lease = Some(Arc::new(lease));
                self.storage = ctx.private_state();
                self.state = super::storage::cached(ctx, super::STATE_KEY)
                    .and_then(|value| super::decode_state(value.as_bytes()).ok())
                    .unwrap_or_default();
                self.gate = Gate::restore(&self.state);
                self.migration = Migration::current(ctx);
                self.generation = ctx
                    .secret(super::SESSION_KEY)
                    .and_then(|value| {
                        super::read_session_binding(value.as_bytes(), ctx.instance().as_str()).ok()
                    })
                    .map(|binding| binding.generation);
                self.restart(ctx, false);
                if self.ownership.resume {
                    self.tick(ctx);
                }
            }
            Err(error) => {
                self.warning = Some(error.to_string());
                self.next_poll = Instant::now() + Duration::from_secs(60);
            }
        }
    }
    fn job_finished(
        &mut self,
        ctx: &DriverCtx,
        kind: JobKind,
        result: Result<Option<String>, Error>,
    ) {
        match kind {
            JobKind::Migration { epoch } => {
                if epoch == self.epoch {
                    self.migration = if result.is_ok() {
                        Migration::Complete
                    } else {
                        Migration::Pending
                    };
                    if let Err(error) = result {
                        self.warning = Some(error.to_string());
                        self.next_poll = Instant::now() + Duration::from_secs(60);
                    } else {
                        self.tick(ctx);
                    }
                }
            }
            JobKind::Input { epoch, permit } => {
                if epoch == self.epoch {
                    match result {
                        Ok(Some(bytes)) => self.consume(ctx, permit, &bytes),
                        Ok(None) => {}
                        Err(error) => self.warning = Some(error.to_string()),
                    }
                }
            }
            JobKind::Rejected { epoch, state } => {
                if epoch == self.epoch {
                    if result.is_ok() {
                        self.state = *state;
                    }
                    self.warning = Some(
                        result
                            .err()
                            .or(self.state.failure)
                            .unwrap_or(Error::Persistence)
                            .to_string(),
                    );
                }
            }
            JobKind::Session {
                epoch,
                binding,
                ack,
            } => {
                if epoch == self.epoch {
                    if result.is_ok() {
                        self.generation = Some(binding.generation);
                        self.accepts_login_marker = false;
                        self.live = false;
                        ctx.set_availability(&super::summary_device_id(ctx.instance()), false);
                    }
                    let _ = ack.send(result.map(|_| ()));
                }
            }
            JobKind::Cycle {
                epoch,
                state,
                catalog,
                observed,
            } => {
                if epoch == self.epoch {
                    self.completed(
                        ctx,
                        *state,
                        catalog.as_deref(),
                        observed,
                        result.map(|_| ()),
                    );
                }
            }
        }
        if self.job.is_none()
            && let Some(Event::Complete {
                epoch,
                state,
                catalog,
                observed,
            }) = self.deferred.take()
            && epoch == self.epoch
        {
            self.finish(ctx, *state, catalog, observed);
        }
    }

    fn validate_session(
        &self,
        ctx: &DriverCtx,
        epoch: u64,
        encoded: &[u8],
        closed: bool,
    ) -> Result<super::SessionBinding, Error> {
        if epoch != self.epoch || closed {
            return Err(Error::Persistence);
        }
        let binding = super::read_session_binding(encoded, ctx.instance().as_str())?;
        if self.accepts_login_marker {
            if binding.phase != SessionPhase::LoginRequired
                || binding.scope.is_some()
                || self.generation.as_ref() == Some(&binding.generation)
            {
                return Err(Error::ScopeMismatch);
            }
        } else if self.generation.as_ref() != Some(&binding.generation) {
            return Err(Error::ScopeMismatch);
        }
        Ok(binding)
    }
    fn finish(
        &mut self,
        ctx: &DriverCtx,
        mut state: ConnectionState,
        catalog: Option<Box<Catalog>>,
        observed: bool,
    ) {
        if let Some(request_id) = self.coalesced_request_id.take() {
            state.request_id = Some(request_id);
        }
        let binding = ctx.secret(super::SESSION_KEY).and_then(|bytes| {
            super::read_session_binding(bytes.as_bytes(), ctx.instance().as_str()).ok()
        });
        let matches = binding.as_ref().is_some_and(|binding| {
            state.generation.as_ref() == Some(&binding.generation) && state.scope == binding.scope
        });
        if state.generation.is_some() && !matches {
            self.busy = false;
            self.live = false;
            self.warning = Some(state.failure.unwrap_or(Error::ScopeMismatch).to_string());
            ctx.set_availability(&super::summary_device_id(ctx.instance()), false);
            return;
        }
        let catalog = catalog.filter(|catalog| {
            matches
                && Some(&catalog.connection_generation) == state.generation.as_ref()
                && Some(&catalog.scope) == state.scope.as_ref()
                && catalog.instance == ctx.instance().as_str()
        });
        state.last_observation_ms = self.state.last_observation_ms.filter(|_| {
            self.state.generation == state.generation && self.state.scope == state.scope
        });
        if observed && catalog.is_some() {
            state.last_observation_ms = Some(now_ms());
        }
        let encoded = super::storage::snapshot(&state, catalog.as_deref());
        self.live = false;
        ctx.set_availability(&super::summary_device_id(ctx.instance()), false);
        match encoded {
            Ok(values) => {
                self.job = Some(Job {
                    task: JobTask::Write(self.store_cache(values)),
                    kind: JobKind::Cycle {
                        epoch: self.epoch,
                        state: Box::new(state),
                        catalog,
                        observed,
                    },
                });
            }
            Err(error) => self.completed(ctx, state, catalog.as_deref(), observed, Err(error)),
        }
    }
    fn completed(
        &mut self,
        ctx: &DriverCtx,
        mut state: ConnectionState,
        catalog: Option<&Catalog>,
        observed: bool,
        stored: Result<(), Error>,
    ) {
        self.busy = false;
        self.live = stored.is_ok() && observed && catalog.is_some();
        if let Err(error) = stored {
            state.phase = ConnectionPhase::PersistencePending;
            state.failure = Some(error.public_failure());
            state.last_observation_ms = self.state.last_observation_ms;
        }
        self.warning = state
            .failure
            .map(|error| error.to_string())
            .or_else(|| (!self.live).then(|| moli_i18n::tr!("overkiz.cloud_login")));
        self.gate = Gate::restore(&state);
        self.next_poll = Instant::now()
            + if self.live {
                self.poll
            } else {
                Duration::from_secs(60)
            };
        self.state = state;
        self.publish(ctx, catalog);
    }
    pub(crate) fn tick(&mut self, ctx: &DriverCtx) {
        // Exactly one tracked disk job. Native polls/commands remain selectable throughout.
        if !self.current_storage(ctx)
            || self.job.is_some()
            || ctx.secret(super::CLEANUP_KEY).is_some()
        {
            return;
        }
        if self.ownership.lease.is_none() {
            self.ownership.resume = true;
            if self.ownership.job.is_none() && Instant::now() >= self.next_poll {
                self.restart(ctx, false);
            }
            return;
        }
        if self.migration == Migration::Pending {
            self.migrate_legacy(ctx);
            return;
        }
        if self.actor.is_none() && Instant::now() >= self.next_poll {
            self.restart(ctx, false);
        }
        if let Some(pending) = self
            .storage
            .as_ref()
            .and_then(|storage| storage.get(super::PENDING_KEY))
        {
            if self.busy
                && super::decode_request(pending.as_bytes()).is_ok_and(|request| {
                    matches!(request.request, PendingRequest::ScenarioPermission { .. })
                })
            {
                return;
            }
            let permit = if self.gate.open() {
                let Some(actor) = &self.actor else {
                    return;
                };
                let Ok(permit) = actor.commands.clone().try_reserve_owned() else {
                    return;
                };
                Some(permit)
            } else {
                // No actor dispatch for rejected input. The single disk job is its capacity.
                None
            };
            self.job = Some(Job {
                task: JobTask::Take(self.take_pending()),
                kind: JobKind::Input {
                    epoch: self.epoch,
                    permit,
                },
            });
            return;
        }
        if !self.gate.open() {
            return;
        }
        if matches!(
            self.state.phase,
            ConnectionPhase::ReconnectRequired | ConnectionPhase::Disconnected
        ) || (self.state.phase == ConnectionPhase::SelectingSite && self.state.scope.is_none())
        {
            return;
        }
        if !self.busy
            && Instant::now() >= self.next_poll
            && let Some(actor) = &self.actor
            && actor.commands.try_send(Work::Observe).is_ok()
        {
            self.busy = true;
        }
    }
    fn migrate_legacy(&mut self, ctx: &DriverCtx) {
        if Instant::now() >= self.next_poll {
            match super::storage::legacy(ctx) {
                Ok(entries) => {
                    let storage = self.storage.clone();
                    self.job = Some(Job {
                        task: JobTask::Write(tokio::spawn(async move {
                            let storage = storage.ok_or_else(|| {
                                std::io::Error::other("private storage unavailable")
                            })?;
                            storage
                                .migrate(
                                    entries,
                                    [
                                        super::STATE_KEY,
                                        inventory::INVENTORY_KEY,
                                        super::PENDING_KEY,
                                        super::execution::POLICY_KEY,
                                    ]
                                    .into_iter()
                                    .map(str::to_owned)
                                    .collect(),
                                )
                                .await
                        })),
                        kind: JobKind::Migration { epoch: self.epoch },
                    });
                }
                Err(error) => {
                    self.warning = Some(error.to_string());
                    self.next_poll = Instant::now() + Duration::from_secs(60);
                }
            }
        }
    }
    fn consume(&mut self, ctx: &DriverCtx, permit: Option<mpsc::OwnedPermit<Work>>, bytes: &str) {
        match super::decode_request(bytes.as_bytes()) {
            Ok(request) => {
                if matches!(request.request, PendingRequest::ScenarioPermission { .. }) {
                    drop(permit);
                    self.permission(ctx, request);
                    return;
                }
                let Some(permit) = permit.filter(|_| self.gate.open()) else {
                    self.reject_input(ctx, request.request_id);
                    return;
                };
                if let PendingRequest::Refresh { generation } = &request.request
                    && self.generation.as_ref() != Some(generation)
                {
                    self.warning = Some(Error::ScopeMismatch.to_string());
                    return;
                }
                if !matches!(request.request, PendingRequest::Refresh { .. }) {
                    drop(permit);
                    let login = matches!(request.request, PendingRequest::Login { .. });
                    self.restart(ctx, login);
                    if let Some(actor) = &self.actor
                        && actor.commands.try_send(Work::Request(request)).is_ok()
                    {
                        self.busy = true;
                    }
                } else if !self.busy {
                    permit.send(Work::Request(request));
                    self.busy = true;
                } else {
                    self.coalesced_request_id = Some(request.request_id);
                }
                self.live = false;
                ctx.set_availability(&super::summary_device_id(ctx.instance()), false);
            }
            Err(error) => self.warning = Some(error.to_string()),
        }
    }
    fn reject_input(&mut self, ctx: &DriverCtx, request_id: String) {
        // If quota completed while take was pending, save that cycle's partial catalogue
        // and this request outcome together, once; never discard its quota or observations.
        if matches!(self.deferred, Some(Event::Complete { .. })) {
            self.coalesced_request_id = Some(request_id);
            return;
        }
        let mut state = self.state.clone();
        state.request_id = Some(request_id);
        state.phase = ConnectionPhase::Unavailable;
        state.failure = Some(Error::RateLimited {
            retry_after_secs: state.retry_after_secs.unwrap_or(u64::MAX),
        });
        // Keep retry_after_secs + last_attempt_ms unchanged: rejecting a request must
        // neither bypass nor extend the original durable provider gate.
        match super::storage::snapshot(&state, None) {
            Ok(values) => {
                self.job = Some(Job {
                    task: JobTask::Write(self.store_cache(values)),
                    kind: JobKind::Rejected {
                        epoch: self.epoch,
                        state: Box::new(state),
                    },
                });
            }
            Err(error) => self.warning = Some(error.to_string()),
        }
        self.live = false;
        let summary = super::summary_device_id(ctx.instance());
        ctx.set_state(&summary, "status", Value::from("stale"));
        ctx.set_availability(&summary, false);
    }
    fn current_storage(&mut self, ctx: &DriverCtx) -> bool {
        if self.storage.is_none() {
            return false;
        }
        if self
            .storage
            .as_ref()
            .is_some_and(moli_runtime::PrivateState::is_current)
        {
            return true;
        }
        self.storage = ctx.private_state();
        self.state = super::storage::cached(ctx, super::STATE_KEY)
            .and_then(|value| super::decode_state(value.as_bytes()).ok())
            .unwrap_or_default();
        self.generation = ctx
            .secret(super::SESSION_KEY)
            .and_then(|value| {
                super::read_session_binding(value.as_bytes(), ctx.instance().as_str()).ok()
            })
            .map(|binding| binding.generation);
        self.migration = Migration::current(ctx);
        self.gate = Gate::restore(&self.state);
        self.next_poll = Instant::now();
        self.restart(ctx, false);
        self.publish(ctx, None);
        false
    }

    fn store_session(&self, value: String) -> tokio::task::JoinHandle<std::io::Result<()>> {
        let storage = self.storage.clone();
        tokio::spawn(async move {
            storage
                .ok_or_else(|| std::io::Error::other("private storage unavailable"))?
                .store_secret(super::SESSION_KEY, &value)
                .await
        })
    }

    fn store_cache(
        &self,
        values: Vec<(String, String, String)>,
    ) -> tokio::task::JoinHandle<std::io::Result<()>> {
        let storage = self.storage.clone();
        tokio::spawn(async move {
            storage
                .ok_or_else(|| std::io::Error::other("private storage unavailable"))?
                .set_if_changed(values)
                .await
                .map(|_| ())
        })
    }

    fn take_pending(&self) -> tokio::task::JoinHandle<std::io::Result<Option<String>>> {
        let storage = self.storage.clone();
        tokio::spawn(async move {
            storage
                .ok_or_else(|| std::io::Error::other("private storage unavailable"))?
                .take(super::PENDING_KEY)
                .await
        })
    }

    fn publish(&self, ctx: &DriverCtx, catalog: Option<&Catalog>) {
        super::execution::publish(
            ctx,
            catalog,
            self.scenario_mode.read_only(),
            self.live,
            self.poll.as_secs(),
        );
        let id = super::summary_device_id(ctx.instance());
        ctx.upsert_device(summary(ctx.instance()));
        ctx.set_state(
            &id,
            "status",
            Value::from(match self.state.phase {
                ConnectionPhase::Ready if self.live => "available",
                ConnectionPhase::Disconnected => "disconnected",
                ConnectionPhase::SelectingSite => "selecting_site",
                ConnectionPhase::ReconnectRequired => "reconnect_required",
                ConnectionPhase::PersistencePending => "persistence_pending",
                _ => "stale",
            }),
        );
        for kind in CollectionKind::ALL {
            let prefix = collection_key(kind);
            let state = catalog.and_then(|c| c.collections.get(&kind));
            ctx.set_state(
                &id,
                &format!("{prefix}_status"),
                Value::from(match state.map(|s| s.status) {
                    Some(inventory::CollectionStatus::Available) => "available",
                    Some(inventory::CollectionStatus::Unsupported) => "unsupported",
                    Some(inventory::CollectionStatus::Forbidden) => "forbidden",
                    Some(inventory::CollectionStatus::Unauthorized) => "unauthorized",
                    Some(inventory::CollectionStatus::RateLimited) => "rate_limited",
                    Some(inventory::CollectionStatus::Unavailable) => "unavailable",
                    Some(inventory::CollectionStatus::InvalidResponse) => "invalid_response",
                    _ => "not_observed",
                }),
            );
            ctx.set_state(
                &id,
                &format!("{prefix}_count"),
                Value::Int(
                    i64::try_from(
                        state
                            .and_then(|s| s.data.as_ref())
                            .map_or(0, |data| data.definitions.len()),
                    )
                    .unwrap_or(0),
                ),
            );
            ctx.set_state(
                &id,
                &format!("{prefix}_observed_ms"),
                state
                    .and_then(|s| s.last_observation_ms)
                    .map_or(Value::Null, Value::Int),
            );
        }
        ctx.set_availability(&id, self.live);
    }
}

pub(crate) async fn during<F: Future>(
    future: F,
    cloud: &mut Option<Controller>,
    ctx: &DriverCtx,
) -> F::Output {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            event = async { cloud.as_mut().expect("enabled branch").next_event().await }, if cloud.as_ref().is_some_and(Controller::receiving) => {
                if let Some(cloud) = cloud { cloud.handle(ctx, event); }
            }
        }
    }
}
fn collection_key(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Scenarios => "scenarios",
        CollectionKind::CalendarDays => "calendar_days",
        CollectionKind::CalendarRules => "calendar_rules",
        CollectionKind::Triggers => "triggers",
        CollectionKind::Schedule => "schedule",
        CollectionKind::DelayedSchedule => "delayed_schedule",
    }
}
fn summary(instance: &InstanceId) -> Device {
    let mut points = vec![PointSpec {
        key: "status".into(),
        label: moli_i18n::tr!("overkiz.cloud_status").into(),
        kind: Kind::Text,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    }];
    for kind in CollectionKind::ALL {
        points.push(PointSpec {
            key: format!("{}_status", collection_key(kind)).into(),
            label: moli_i18n::tr!("overkiz.cloud_status").into(),
            kind: Kind::Text,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Other,
        });
        for suffix in ["count", "observed_ms"] {
            points.push(PointSpec {
                key: format!("{}_{suffix}", collection_key(kind)).into(),
                label: moli_i18n::tr!("overkiz.cloud_status").into(),
                kind: Kind::Numeric {
                    min: Some(0.0),
                    max: None,
                    step: Some(1.0),
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Other,
            });
        }
    }
    Device {
        id: super::summary_device_id(instance),
        instance: instance.clone(),
        native_name: moli_i18n::tr!("overkiz.cloud_summary").into(),
        manufacturer: Some("Somfy".into()),
        model: None,
        description: None,
        native_room: None,
        members: vec![],
        points,
    }
}
fn now_ms() -> i64 {
    i64::try_from(moli_core::now_ms()).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

#[path = "worker_execution.rs"]
mod scenario_execution;

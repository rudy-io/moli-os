//! The hub: single source of truth for the live model of the home.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use moli_core::{
    Action, ApprovalRequest, Decision, Device, DeviceId, DriverStatus, Event, InstanceId,
    JournalEntry, Label, Origin, Outcome, PointId, Sample, Value, ValueError, now_ms, validate,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::driver::CommandRequest;
use crate::guard::{GuardPolicy, GuardStatus, Verdict};
use crate::journal_log::JournalLog;
use crate::secrets::{self, MasterKey, SecretStore};
use crate::state_cache::{self, States};
use crate::{Stats, labels};

const COMMAND_QUEUE: usize = 64;
/// Secret namespace of the hub itself; reserved as a driver id.
pub const CORE_NAMESPACE: &str = "core";

#[derive(Clone, Debug)]
pub struct HubOptions {
    /// Where user labels are persisted. `None` keeps them in memory only.
    pub labels_path: Option<PathBuf>,
    /// Where last known values are cached across restarts. `None` disables it.
    pub state_path: Option<PathBuf>,
    /// Append-only journal file (JSON Lines). `None` keeps it in memory only.
    pub journal_path: Option<PathBuf>,
    /// Entries kept in memory (and reloaded at startup).
    pub journal_capacity: usize,
    /// Encrypted store for credentials drivers obtain at runtime, with its
    /// 64-hex-digit master key. `None` disables it (pairing impossible).
    pub secrets: Option<(PathBuf, MasterKey)>,
    /// Protected rooms and quiet hours (from the configuration file only).
    pub guard: GuardPolicy,
    pub event_capacity: usize,
    pub command_timeout: Duration,
}

impl Default for HubOptions {
    fn default() -> Self {
        Self {
            labels_path: None,
            state_path: None,
            journal_path: None,
            journal_capacity: 500,
            secrets: None,
            guard: GuardPolicy::default(),
            event_capacity: 1024,
            command_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("unknown point")]
    UnknownPoint,
    #[error("point is read-only")]
    ReadOnly,
    #[error("invalid value: {0}")]
    Invalid(#[from] ValueError),
    #[error("driver unavailable")]
    DriverUnavailable,
    #[error("driver busy")]
    Busy,
    #[error("driver did not answer in time")]
    Timeout,
    #[error("driver error: {0}")]
    Driver(String),
    /// Held by the guard: a human must approve request `id`.
    #[error("awaiting human approval #{id}: {reason}")]
    NeedsApproval { id: u64, reason: String },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ApprovalError {
    #[error("only a human, from the dashboard, can decide")]
    HumanOnly,
    #[error("unknown or already decided request")]
    Unknown,
    #[error("request expired")]
    Expired,
    #[error("this request is about another point than the one shown")]
    Mismatch,
    #[error("approved, but the order failed: {0}")]
    Command(CommandError),
}

/// How long a held order waits for a human.
const APPROVAL_TTL_MS: u64 = 10 * 60 * 1000;
/// Orders waiting for a human, at most: a flood of requests must not bury
/// the dashboard (the oldest gives way, and one per point and origin).
const MAX_PENDING: usize = 50;
/// A point id is `<instance>:<native id>/<key>`: anything longer is noise.
const MAX_POINT: usize = 256;
/// Who acts, as declared by an agent: a name, not a message.
const MAX_ACTOR: usize = 64;

/// An agent's declared name, as journaled: bounded, no control characters.
fn clean_actor(actor: Option<String>) -> Option<String> {
    actor
        .map(|a| {
            a.chars()
                .filter(|c| !c.is_control())
                .take(MAX_ACTOR)
                .collect::<String>()
        })
        .filter(|a| !a.trim().is_empty())
}

/// An order resolved and validated, ready for its driver.
struct Prepared {
    point: PointId,
    device: Arc<Device>,
    key: Arc<str>,
    value: Value,
    commands: mpsc::Sender<CommandRequest>,
}

#[derive(Debug, Default)]
struct Approvals {
    next_id: u64,
    pending: BTreeMap<u64, Arc<ApprovalRequest>>,
}

#[derive(Debug, thiserror::Error)]
pub enum LabelError {
    #[error("unknown device")]
    UnknownDevice,
    #[error("could not persist labels: {0}")]
    Persist(#[from] std::io::Error),
    #[error("{0}")]
    Guarded(String),
}

/// A partial label update: `None` keeps a field, `Some("")` clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct LabelPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub room: Option<String>,
}

/// Longest name or room a label may hold: a label is read by people, by
/// agents and by Moli's prompts (never a place to hide instructions).
pub const MAX_LABEL: usize = 60;

/// A label as stored: no control characters, single spaces, bounded.
fn clean_label(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_LABEL)
        .collect()
}

impl LabelPatch {
    fn apply(self, current: Label) -> Label {
        let merge = |patch: Option<String>, current: Option<String>| match patch {
            None => current,
            Some(s) => Some(clean_label(&s)).filter(|s| !s.is_empty()),
        };
        Label {
            name: merge(self.name, current.name),
            room: merge(self.room, current.room),
        }
    }
}

/// Everything known about one device, as served to the surfaces.
#[derive(Clone, Debug, Serialize)]
pub struct DeviceView {
    #[serde(flatten)]
    pub device: Arc<Device>,
    pub label: Label,
    pub online: Option<bool>,
    pub state: BTreeMap<Arc<str>, Sample>,
    /// Images on demand (`/api/devices/{id}/snapshot`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub camera: bool,
    /// Prints documents (`/api/devices/{id}/print`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub printer: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DriverView {
    pub instance: InstanceId,
    pub kind: &'static str,
    /// The catalogue package it runs (see [`crate::Driver::integration`]).
    pub integration: Arc<str>,
    pub status: DriverStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub devices: Vec<DeviceView>,
    pub drivers: Vec<DriverView>,
    pub approvals: Vec<Arc<ApprovalRequest>>,
    pub guard: GuardStatus,
}

/// Cheap, clonable handle to the hub.
#[derive(Clone, Debug)]
pub struct Hub(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    model: RwLock<Model>,
    journal: Mutex<Journal>,
    approvals: Mutex<Approvals>,
    events: broadcast::Sender<Event>,
    /// Serializes label writes so the file always matches the model.
    label_write: tokio::sync::Mutex<()>,
    journal_log: Option<JournalLog>,
    secrets: Option<SecretStore>,
    /// Why there is no secret store, in words a human can act on.
    secrets_problem: Option<String>,
    /// Values changed since the state cache was last written.
    state_dirty: AtomicBool,
    /// The last image of each camera, shared by every viewer for a second.
    images: Mutex<HashMap<DeviceId, LastImage>>,
    /// Short-lived media a device fetches back (an announcement's sound).
    published: Mutex<HashMap<String, (Instant, crate::media::Image)>>,
    opts: HubOptions,
    started: Instant,
}

/// Two screens showing the same camera live cost one fetch, not two.
const IMAGE_FRESH: Duration = Duration::from_secs(1);

/// How long a published media stays reachable, and how many at most.
const PUBLISHED_TTL: Duration = Duration::from_secs(5 * 60);
/// (A spoken announcement weighs ~2 MB, 8 MiB at most: 32 MiB at worst.)
const MAX_PUBLISHED: usize = 4;

/// A camera's last image and when it was taken; its viewers wait on this lock.
type LastImage = Arc<tokio::sync::Mutex<Option<(Instant, crate::media::Image)>>>;

#[derive(Debug, Default)]
struct Model {
    devices: BTreeMap<DeviceId, Arc<Device>>,
    states: States,
    /// Cached values not yet claimed by a device (its driver has not
    /// published it yet in this run).
    restored: States,
    availability: HashMap<DeviceId, bool>,
    /// Devices taken offline because their driver failed, with what they
    /// were before: given back when the driver runs again, unless it said
    /// something itself in between (a group that nothing ever re-announces
    /// must not stay offline forever).
    held_offline: HashMap<DeviceId, Option<bool>>,
    drivers: BTreeMap<InstanceId, DriverSlot>,
    labels: BTreeMap<DeviceId, Label>,
    snapshots: HashMap<DeviceId, Arc<dyn crate::media::SnapshotSource>>,
    printers: HashMap<DeviceId, Arc<dyn crate::media::PrintSink>>,
}

#[derive(Debug)]
struct DriverSlot {
    kind: &'static str,
    integration: Arc<str>,
    status: DriverStatus,
    commands: mpsc::Sender<CommandRequest>,
}

#[derive(Debug)]
struct Journal {
    entries: VecDeque<Arc<JournalEntry>>,
    next_id: u64,
}

impl Hub {
    /// Creates a hub, loading persisted labels and cached values if configured.
    /// A corrupt state cache is discarded (it is only a cache); corrupt
    /// labels are an error (they are user data).
    pub fn new(opts: HubOptions) -> std::io::Result<Self> {
        let labels = match &opts.labels_path {
            Some(path) => labels::load(path)?,
            None => BTreeMap::new(),
        };
        let restored = match &opts.state_path {
            Some(path) => state_cache::load(path).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "state cache unreadable, starting empty");
                States::new()
            }),
            None => States::new(),
        };
        let (journal_log, past) = match &opts.journal_path {
            Some(path) => {
                let (log, past) = JournalLog::open(path, opts.journal_capacity)?;
                (Some(log), past)
            }
            None => (None, Vec::new()),
        };
        let next_id = past.last().map_or(1, |e| e.id + 1);
        // An unreadable store (wrong or rotated master key, corrupt file)
        // must not take the whole home down: only drivers needing secrets
        // wait, and they say why.
        let (secrets, secrets_problem) = match &opts.secrets {
            Some((path, master)) => match secrets::master_key_bytes(master)
                .and_then(|key| SecretStore::open(path.clone(), &key))
            {
                Ok(store) => (Some(store), None),
                Err(e) => {
                    tracing::error!(error = %e, "secret store unavailable");
                    (
                        None,
                        Some(moli_i18n::tr!("serveur.coffre.illisible", error = e)),
                    )
                }
            },
            None => (None, Some(moli_i18n::tr!("serveur.coffre.cle_absente"))),
        };
        let (events, _) = broadcast::channel(opts.event_capacity.max(16));
        Ok(Self(Arc::new(Inner {
            model: RwLock::new(Model {
                restored,
                labels,
                ..Model::default()
            }),
            journal: Mutex::new(Journal {
                entries: past.into(),
                next_id,
            }),
            approvals: Mutex::new(Approvals::default()),
            journal_log,
            secrets,
            secrets_problem,
            events,
            label_write: tokio::sync::Mutex::new(()),
            state_dirty: AtomicBool::new(false),
            images: Mutex::new(HashMap::new()),
            published: Mutex::new(HashMap::new()),
            opts,
            started: Instant::now(),
        })))
    }

    // ---- reading -----------------------------------------------------------

    /// Subscribe to every subsequent event. Subscribe *before* taking a
    /// snapshot so nothing falls between the two.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        // Both take the model lock: never while holding it (a waiting
        // writer would deadlock a nested read).
        let guard = self.guard_status();
        let approvals = self.approvals();
        let model = self.read();
        Snapshot {
            devices: model.devices.values().map(|d| model.view(d)).collect(),
            drivers: model
                .drivers
                .iter()
                .map(|(instance, slot)| DriverView {
                    instance: instance.clone(),
                    kind: slot.kind,
                    integration: slot.integration.clone(),
                    status: slot.status.clone(),
                })
                .collect(),
            approvals,
            guard,
        }
    }

    #[must_use]
    pub fn device(&self, id: &DeviceId) -> Option<DeviceView> {
        let model = self.read();
        model.devices.get(id).map(|d| model.view(d))
    }

    /// A device's description and label, without copying its state: for
    /// callers after names and points only (the automation checker reads
    /// hundreds at each listing).
    #[must_use]
    pub fn describe(&self, id: &DeviceId) -> Option<(Arc<Device>, Label)> {
        let model = self.read();
        let device = model.devices.get(id)?;
        Some((
            Arc::clone(device),
            model.labels.get(id).cloned().unwrap_or_default(),
        ))
    }

    #[must_use]
    pub fn state(&self, point: &PointId) -> Option<Sample> {
        let (device, key) = point.split()?;
        self.read().states.get(&device)?.get(key).cloned()
    }

    /// Most recent journal entries first.
    #[must_use]
    pub fn journal(&self, limit: usize) -> Vec<Arc<JournalEntry>> {
        let journal = self
            .0
            .journal
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        journal.entries.iter().rev().take(limit).cloned().collect()
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        let model = self.read();
        Stats {
            uptime_ms: u64::try_from(self.0.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            devices: model.devices.len(),
            points: model.devices.values().map(|d| d.points.len()).sum(),
            drivers: model.drivers.len(),
            drivers_running: model
                .drivers
                .values()
                .filter(|s| s.status == DriverStatus::Running)
                .count(),
            rss_bytes: crate::stats::rss_bytes(),
        }
    }

    // ---- writing (surfaces) --------------------------------------------------

    /// Validates a write, routes it to the owning driver, waits for its answer
    /// and journals the attempt — successful or not.
    pub async fn command(
        &self,
        point: &PointId,
        value: Value,
        origin: Origin,
        actor: Option<String>,
    ) -> Result<(), CommandError> {
        let started = Instant::now();
        // Absurd inputs are refused before anything, and journaled without
        // their bulk (the journal is not a place to store what an agent sent).
        let oversized = point.as_str().len() > MAX_POINT
            || matches!(&value, Value::Text(t) if t.chars().count() > moli_core::MAX_TEXT);
        if oversized {
            let short: String = point.as_str().chars().take(MAX_POINT).collect();
            let error = CommandError::Invalid(ValueError::TooLong);
            self.record(
                origin,
                actor,
                Action::Command {
                    point: PointId::from(short),
                    value: Value::Null,
                },
                Outcome::Err(error.to_string()),
                started,
            );
            return Err(error);
        }
        let result = match self.prepare(point, &value) {
            Err(e) => Err(e),
            Ok(prepared) => match self.verdict(point, origin) {
                Verdict::Allow => self.send(prepared).await,
                Verdict::Approval(reason) => {
                    let id = self.hold(point, &value, origin, actor.clone(), &reason);
                    Err(CommandError::NeedsApproval { id, reason })
                }
            },
        };
        let outcome =
            match &result {
                Ok(()) => Outcome::Ok,
                Err(CommandError::NeedsApproval { id, reason }) => Outcome::Pending(
                    moli_i18n::tr!("serveur.garde.en_attente", id = id, reason = reason),
                ),
                Err(e) => Outcome::Err(e.to_string()),
            };
        self.record(
            origin,
            actor,
            Action::Command {
                point: point.clone(),
                value,
            },
            outcome,
            started,
        );
        result
    }

    /// What the guard says about an order on this point from this origin.
    fn verdict(&self, point: &PointId, origin: Origin) -> Verdict {
        // Stopping a machine at work (a print) is a human's call, whatever
        // the room: an agent asks.
        let agent = !matches!(origin, Origin::Ui | Origin::System | Origin::Automation);
        if agent && self.is_control(point) {
            return Verdict::Approval(moli_i18n::tr!("serveur.garde.machine"));
        }
        let rooms = point
            .split()
            .map(|(device, _)| self.rooms_of(&device))
            .unwrap_or_default();
        let rooms: Vec<&str> = rooms.iter().map(String::as_str).collect();
        self.0
            .opts
            .guard
            .check(&rooms, origin, jiff::Timestamp::now())
    }

    /// Whether the point is an order to a machine at work.
    fn is_control(&self, point: &PointId) -> bool {
        let Some((device, key)) = point.split() else {
            return false;
        };
        self.read()
            .devices
            .get(&device)
            .and_then(|d| d.point(key))
            .is_some_and(|p| p.semantic == moli_core::Semantic::Control)
    }

    /// Every room an order on this device reaches: its own (user label and
    /// source system's room) and, for a group, those of each member. All
    /// count for protection.
    fn rooms_of(&self, device: &DeviceId) -> Vec<String> {
        let model = self.read();
        let own = |id: &DeviceId| {
            let label = model.labels.get(id).and_then(|l| l.room.clone());
            let native = model
                .devices
                .get(id)
                .and_then(|d| d.native_room.as_deref().map(str::to_owned));
            label.into_iter().chain(native)
        };
        let members = model
            .devices
            .get(device)
            .map(|d| d.members.clone())
            .unwrap_or_default();
        let mut rooms: Vec<String> = own(device).chain(members.iter().flat_map(own)).collect();
        rooms.sort();
        rooms.dedup();
        rooms
    }

    /// Protected rooms that match no device's room: renamed in the source
    /// system? Then protection silently stopped applying — say so.
    #[must_use]
    pub fn unmatched_protected_rooms(&self) -> Vec<String> {
        let known: Vec<String> = {
            let model = self.read();
            model
                .labels
                .values()
                .filter_map(|l| l.room.clone())
                .chain(
                    model
                        .devices
                        .values()
                        .filter_map(|d| d.native_room.as_deref().map(str::to_owned)),
                )
                .collect()
        };
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        self.0
            .opts
            .guard
            .protected_rooms
            .iter()
            .filter(|p| !known.iter().any(|k| crate::guard::same_room(p, k)))
            .cloned()
            .collect()
    }

    /// Holds an order for a human; returns the request id.
    fn hold(
        &self,
        point: &PointId,
        value: &Value,
        origin: Origin,
        actor: Option<String>,
        reason: &str,
    ) -> u64 {
        let now = now_ms();
        let (device_name, native_name, rooms) = point
            .split()
            .map(|(device, _)| {
                let (label, native) = {
                    let model = self.read();
                    let native = model
                        .devices
                        .get(&device)
                        .map_or_else(|| device.to_string(), |d| d.native_name.to_string());
                    let label = model.labels.get(&device).and_then(|l| l.name.clone());
                    (label, native)
                };
                (
                    label.unwrap_or_else(|| native.clone()),
                    native,
                    self.rooms_of(&device),
                )
            })
            .unwrap_or_default();
        let replaced;
        let request = {
            let mut approvals = self
                .0
                .approvals
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // Ids never repeat across restarts: a card left on a screen can
            // never approve a different, newer request.
            approvals.next_id = approvals.next_id.max(now).saturating_add(1);
            // A newer request for the same point, from the same origin,
            // replaces the older one; past the cap, the oldest gives way.
            let mut superseded: Vec<Arc<ApprovalRequest>> = approvals
                .pending
                .values()
                .filter(|r| &r.point == point && r.origin == origin)
                .cloned()
                .collect();
            for r in &superseded {
                approvals.pending.remove(&r.id);
            }
            while approvals.pending.len() >= MAX_PENDING {
                let Some((&oldest, _)) = approvals.pending.iter().next() else {
                    break;
                };
                if let Some(r) = approvals.pending.remove(&oldest) {
                    superseded.push(r);
                }
            }
            replaced = superseded;
            let request = Arc::new(ApprovalRequest {
                id: approvals.next_id,
                ts: now,
                expires: now + APPROVAL_TTL_MS,
                point: point.clone(),
                value: value.clone(),
                origin,
                actor,
                reason: reason.to_owned(),
                device_name,
                native_name,
                rooms,
            });
            approvals.pending.insert(request.id, Arc::clone(&request));
            request
        };
        let id = request.id;
        for old in replaced {
            self.resolved(
                &old,
                Decision::Expired,
                Origin::System,
                None,
                Outcome::Err(moli_i18n::tr!("serveur.garde.remplacee")),
                Instant::now(),
            );
        }
        self.emit(Event::ApprovalRequested { request });
        id
    }

    /// Orders waiting for a human, oldest first.
    #[must_use]
    pub fn approvals(&self) -> Vec<Arc<ApprovalRequest>> {
        self.0
            .approvals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .values()
            .cloned()
            .collect()
    }

    /// A human approves (the order runs now) or denies a held order.
    /// Only the dashboard (`Origin::Ui`) may decide, and it must name the
    /// point it saw: a decision never lands on a request it did not show.
    pub async fn resolve_approval(
        &self,
        id: u64,
        expected_point: &PointId,
        approve: bool,
        origin: Origin,
        actor: Option<String>,
    ) -> Result<(), ApprovalError> {
        if origin != Origin::Ui {
            return Err(ApprovalError::HumanOnly);
        }
        let started = Instant::now();
        let request = {
            let mut approvals = self
                .0
                .approvals
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match approvals.pending.get(&id) {
                None => return Err(ApprovalError::Unknown),
                Some(r) if &r.point != expected_point => return Err(ApprovalError::Mismatch),
                Some(_) => approvals
                    .pending
                    .remove(&id)
                    .ok_or(ApprovalError::Unknown)?,
            }
        };
        if request.expires <= now_ms() {
            self.resolved(
                &request,
                Decision::Expired,
                Origin::System,
                None,
                Outcome::Err(moli_i18n::tr!("serveur.garde.expiree")),
                started,
            );
            return Err(ApprovalError::Expired);
        }
        let (decision, outcome, result) = if approve {
            let sent = match self.prepare(&request.point, &request.value) {
                Ok(prepared) => self.send(prepared).await,
                Err(e) => Err(e),
            };
            match sent {
                Ok(()) => (Decision::Approved, Outcome::Ok, Ok(())),
                Err(e) => (
                    Decision::Approved,
                    Outcome::Err(e.to_string()),
                    Err(ApprovalError::Command(e)),
                ),
            }
        } else {
            (Decision::Denied, Outcome::Ok, Ok(()))
        };
        self.resolved(&request, decision, origin, actor, outcome, started);
        result
    }

    /// Drops held orders nobody decided on in time.
    pub fn expire_approvals(&self) {
        let now = now_ms();
        let expired: Vec<_> = {
            let mut approvals = self
                .0
                .approvals
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let ids: Vec<u64> = approvals
                .pending
                .values()
                .filter(|r| r.expires <= now)
                .map(|r| r.id)
                .collect();
            ids.iter()
                .filter_map(|id| approvals.pending.remove(id))
                .collect()
        };
        for request in expired {
            let outcome = Outcome::Err(moli_i18n::tr!("serveur.garde.expiree_sans_decision"));
            self.resolved(
                &request,
                Decision::Expired,
                Origin::System,
                None,
                outcome,
                Instant::now(),
            );
        }
    }

    fn resolved(
        &self,
        request: &ApprovalRequest,
        decision: Decision,
        origin: Origin,
        actor: Option<String>,
        outcome: Outcome,
        started: Instant,
    ) {
        self.emit(Event::ApprovalResolved {
            id: request.id,
            decision,
        });
        let action = Action::Approval {
            request: request.id,
            point: request.point.clone(),
            value: request.value.clone(),
            decision,
        };
        self.record(origin, actor, action, outcome, started);
    }

    /// Journals a change to an automation (they live in their own crate;
    /// the audit trail is one).
    pub fn record_automation(
        &self,
        origin: Origin,
        actor: Option<String>,
        id: &str,
        name: &str,
        change: moli_core::AutomationChange,
        fingerprint: Option<String>,
    ) {
        self.record(
            origin,
            actor,
            Action::Automation {
                id: id.to_owned(),
                name: name.to_owned(),
                change,
                fingerprint,
            },
            Outcome::Ok,
            Instant::now(),
        );
    }

    /// Every writable point with this key (`announce`…), without copying
    /// the model: cheap enough for a check run on every automation.
    #[must_use]
    pub fn writable_points(&self, key: &str) -> Vec<PointId> {
        self.read()
            .devices
            .values()
            .filter(|d| d.point(key).is_some_and(|p| p.access.write))
            .map(|d| PointId::new(&d.id, key))
            .collect()
    }

    /// Publishes a short media for a device to fetch back (Sonos plays an
    /// announcement from a URL): reachable under an unguessable name for a
    /// few minutes, then gone. Returns the name (`<128 random bits>.<ext>`).
    pub fn publish_media(&self, extension: &str, image: crate::media::Image) -> Option<String> {
        use ring::rand::SecureRandom as _;
        let mut raw = [0u8; 16];
        // The name is the only key to it: never a guessable one.
        ring::rand::SystemRandom::new().fill(&mut raw).ok()?;
        let mut name = raw.iter().fold(String::with_capacity(40), |mut name, b| {
            use std::fmt::Write as _;
            let _ = write!(name, "{b:02x}");
            name
        });
        name.push('.');
        name.push_str(extension);
        let mut published = self
            .0
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        published.retain(|_, (at, _)| at.elapsed() < PUBLISHED_TTL);
        while published.len() >= MAX_PUBLISHED {
            let Some(oldest) = published
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            published.remove(&oldest);
        }
        published.insert(name.clone(), (Instant::now(), image));
        Some(name)
    }

    /// A media published a few minutes ago, by its name.
    #[must_use]
    pub fn published_media(&self, name: &str) -> Option<crate::media::Image> {
        self.0
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .filter(|(at, _)| at.elapsed() < PUBLISHED_TTL)
            .map(|(_, image)| image.clone())
    }

    /// Journal entries that could not be written to disk since the start.
    #[must_use]
    pub fn journal_failures(&self) -> u64 {
        self.0.journal_log.as_ref().map_or(0, JournalLog::failures)
    }

    /// How the dashboard is recognized as the household.
    #[must_use]
    pub fn dashboard(&self) -> crate::guard::Dashboard {
        self.0.opts.guard.dashboard
    }

    #[must_use]
    pub fn guard_status(&self) -> GuardStatus {
        let mut status = self.0.opts.guard.status(jiff::Timestamp::now());
        status.unmatched_protected_rooms = self.unmatched_protected_rooms();
        status
    }

    /// Updates the user label of a device. Each field of the patch is
    /// independent: `None` keeps the current value, an empty string clears
    /// it. Merging happens under the label lock, so concurrent patches of
    /// different fields never overwrite each other.
    pub async fn set_label(
        &self,
        device: &DeviceId,
        patch: LabelPatch,
        origin: Origin,
        actor: Option<String>,
    ) -> Result<Label, LabelError> {
        let started = Instant::now();
        let result = match self.label_verdict(device, &patch, origin) {
            Some(reason) => Err(LabelError::Guarded(reason)),
            None => self.write_label(device, patch).await,
        };
        let outcome = match &result {
            Ok(_) => Outcome::Ok,
            Err(e) => Outcome::Err(e.to_string()),
        };
        let label = result.as_ref().ok().cloned().unwrap_or_default();
        self.record(
            origin,
            actor,
            Action::Label {
                device: device.clone(),
                label,
            },
            outcome,
            started,
        );
        result
    }

    /// Only a human may move a device out of (or between) protected rooms:
    /// otherwise an agent could relabel a bedroom lamp, then command it.
    fn label_verdict(
        &self,
        device: &DeviceId,
        patch: &LabelPatch,
        origin: Origin,
    ) -> Option<String> {
        if matches!(origin, Origin::Ui | Origin::System) {
            return None;
        }
        // An agent may name a device nobody named yet (onboarding), never
        // rename one: the sentence a person approves speaks of that name.
        if patch.name.is_some()
            && self
                .read()
                .labels
                .get(device)
                .is_some_and(|l| l.name.is_some())
        {
            return Some(moli_i18n::tr!("serveur.garde.renommer_humain"));
        }
        patch.room.as_ref()?;
        let rooms = self.rooms_of(device);
        let rooms: Vec<&str> = rooms.iter().map(String::as_str).collect();
        self.0
            .opts
            .guard
            .room_change_needs_human(&rooms)
            .map(|why| moli_i18n::tr!("serveur.garde.changer_piece_humain", why = why))
    }

    async fn write_label(&self, device: &DeviceId, patch: LabelPatch) -> Result<Label, LabelError> {
        let _guard = self.0.label_write.lock().await;
        let mut next = {
            let model = self.read();
            if !model.devices.contains_key(device) {
                return Err(LabelError::UnknownDevice);
            }
            model.labels.clone()
        };
        let label = patch.apply(next.get(device).cloned().unwrap_or_default());
        if label.is_empty() {
            next.remove(device);
        } else {
            next.insert(device.clone(), label.clone());
        }
        if let Some(path) = &self.0.opts.labels_path {
            labels::save(path, &next).await?;
        }
        self.write().labels = next;
        self.emit(Event::LabelChanged {
            device: device.clone(),
            label: label.clone(),
        });
        Ok(label)
    }

    /// Resolves and validates an order without sending it.
    fn prepare(&self, point: &PointId, value: &Value) -> Result<Prepared, CommandError> {
        let (device_id, key) = point.split().ok_or(CommandError::UnknownPoint)?;
        let (device, key, kind, commands) = {
            let model = self.read();
            let device = model
                .devices
                .get(&device_id)
                .ok_or(CommandError::UnknownPoint)?;
            let spec = device.point(key).ok_or(CommandError::UnknownPoint)?;
            if !spec.access.write {
                return Err(CommandError::ReadOnly);
            }
            let slot = model
                .drivers
                .get(&device.instance)
                .ok_or(CommandError::DriverUnavailable)?;
            (
                Arc::clone(device),
                spec.key.clone(),
                spec.kind.clone(),
                slot.commands.clone(),
            )
        };
        let value = validate(&kind, value)?;
        Ok(Prepared {
            point: point.clone(),
            device,
            key,
            value,
            commands,
        })
    }

    /// Hands a prepared order to its driver and waits for the answer.
    async fn send(&self, prepared: Prepared) -> Result<(), CommandError> {
        let Prepared {
            point,
            device,
            key,
            value,
            commands,
        } = prepared;
        let (reply, answer) = oneshot::channel();
        commands
            .try_send(CommandRequest {
                point,
                device,
                key,
                value,
                deadline: Instant::now() + self.0.opts.command_timeout,
                reply,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => CommandError::Busy,
                mpsc::error::TrySendError::Closed(_) => CommandError::DriverUnavailable,
            })?;
        match tokio::time::timeout(self.0.opts.command_timeout, answer).await {
            Err(_) => Err(CommandError::Timeout),
            Ok(Err(_)) => Err(CommandError::Driver("command dropped".into())),
            Ok(Ok(result)) => result.map_err(CommandError::Driver),
        }
    }

    fn record(
        &self,
        origin: Origin,
        actor: Option<String>,
        action: Action,
        outcome: Outcome,
        started: Instant,
    ) {
        let actor = clean_actor(actor);
        let entry = {
            let mut journal = self
                .0
                .journal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let entry = Arc::new(JournalEntry {
                id: journal.next_id,
                ts: now_ms(),
                origin,
                actor,
                action,
                outcome,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
            journal.next_id += 1;
            if journal.entries.len() == self.0.opts.journal_capacity {
                journal.entries.pop_front();
            }
            journal.entries.push_back(Arc::clone(&entry));
            entry
        };
        if let Some(log) = &self.0.journal_log {
            log.append(Arc::clone(&entry));
        }
        tracing::info!(id = entry.id, origin = ?entry.origin, outcome = ?entry.outcome, "journal");
        self.emit(Event::Journal { entry });
    }

    // ---- core secrets (the hub's own, e.g. human sessions) ------------------

    /// A secret of the hub itself (namespace `core`, reserved: no driver
    /// instance may be called that).
    #[must_use]
    pub fn core_secret(&self, name: &str) -> Option<String> {
        self.secret(&InstanceId::from(CORE_NAMESPACE), name)
    }

    pub async fn store_core_secret(&self, name: &str, value: &str) -> std::io::Result<()> {
        self.store_secret(&InstanceId::from(CORE_NAMESPACE), name, value)
            .await
    }

    /// Forgets a secret of the hub itself; `true` if it existed.
    pub async fn forget_core_secret(&self, name: &str) -> std::io::Result<bool> {
        self.forget_secret(&InstanceId::from(CORE_NAMESPACE), name)
            .await
    }

    // ---- secrets (drivers only, namespaced by instance) ----------------------

    /// Whether secrets can be kept (a master key was given).
    #[must_use]
    pub fn has_secret_store(&self) -> bool {
        self.0.secrets.is_some()
    }

    pub(crate) fn secrets_problem(&self) -> Option<&str> {
        self.0.secrets_problem.as_deref()
    }

    pub(crate) async fn forget_secret(
        &self,
        instance: &InstanceId,
        name: &str,
    ) -> std::io::Result<bool> {
        match &self.0.secrets {
            Some(store) => store.remove(&format!("{instance}/{name}")).await,
            None => Ok(false),
        }
    }

    /// A secret of a driver instance, or of a surface (the assistant's API
    /// key lives under `assistant/api_key`).
    #[must_use]
    pub fn secret(&self, instance: &InstanceId, name: &str) -> Option<String> {
        self.0.secrets.as_ref()?.get(&format!("{instance}/{name}"))
    }

    /// Files a secret for a driver instance: drivers through their context,
    /// surfaces when a person gives keys for an integration (never read
    /// back by them).
    pub async fn store_secret(
        &self,
        instance: &InstanceId,
        name: &str,
        value: &str,
    ) -> std::io::Result<()> {
        let store = self.0.secrets.as_ref().ok_or_else(|| {
            std::io::Error::other("no secret store: MOLI_MASTER_KEY is not configured")
        })?;
        store.set(&format!("{instance}/{name}"), value).await
    }

    // ---- writing (drivers, via DriverCtx) -----------------------------------

    pub(crate) fn register_driver(
        &self,
        instance: &InstanceId,
        kind: &'static str,
        integration: Arc<str>,
    ) -> mpsc::Receiver<CommandRequest> {
        let (commands, rx) = mpsc::channel(COMMAND_QUEUE);
        self.write().drivers.insert(
            instance.clone(),
            DriverSlot {
                kind,
                integration,
                status: DriverStatus::Starting,
                commands,
            },
        );
        self.emit(Event::DriverStatus {
            instance: instance.clone(),
            status: DriverStatus::Starting,
        });
        rx
    }

    pub(crate) fn set_driver_status(&self, instance: &InstanceId, status: DriverStatus) {
        let (changed, back) = {
            let mut model = self.write();
            let changed = match model.drivers.get_mut(instance) {
                Some(slot) if slot.status != status => {
                    slot.status = status.clone();
                    true
                }
                _ => false,
            };
            let mut back = Vec::new();
            if changed && status == DriverStatus::Running {
                let held: Vec<DeviceId> = model
                    .held_offline
                    .keys()
                    .filter(|d| {
                        model
                            .devices
                            .get(*d)
                            .is_some_and(|d| &d.instance == instance)
                    })
                    .cloned()
                    .collect();
                for device in held {
                    let before = model.held_offline.remove(&device).flatten();
                    match before {
                        Some(online) => model.availability.insert(device.clone(), online),
                        None => model.availability.remove(&device),
                    };
                    back.push(device);
                }
            }
            (changed, back)
        };
        for device in back {
            self.emit(Event::Availability {
                device,
                online: true,
            });
        }
        if changed {
            self.emit(Event::DriverStatus {
                instance: instance.clone(),
                status,
            });
        }
    }

    pub(crate) fn upsert_device(&self, device: Device) {
        let device = Arc::new(device);
        let restored = {
            let mut model = self.write();
            if model
                .devices
                .get(&device.id)
                .is_some_and(|d| **d == *device)
            {
                return;
            }
            // Claim cached values for the points this definition declares.
            // The rest stays cached: a device first published without its
            // definition (interview in progress) must not lose its history.
            let mut cached = model.restored.remove(&device.id).unwrap_or_default();
            let states = model.states.entry(device.id.clone()).or_default();
            states.retain(|key, _| device.point(key).is_some());
            let mut restored = Vec::new();
            for point in &device.points {
                if let Some(sample) = cached.remove(&point.key)
                    && !states.contains_key(&point.key)
                {
                    restored.push(Event::State {
                        point: PointId::new(&device.id, &point.key),
                        value: sample.value.clone(),
                        ts: sample.ts,
                    });
                    states.insert(point.key.clone(), sample);
                }
            }
            if !cached.is_empty() {
                model.restored.insert(device.id.clone(), cached);
            }
            model.devices.insert(device.id.clone(), Arc::clone(&device));
            restored
        };
        self.emit(Event::DeviceUpserted { device });
        for event in restored {
            self.emit(event);
        }
    }

    /// Writes the state cache if anything changed since the last write.
    /// Returns whether a write happened.
    pub async fn persist_state(&self) -> std::io::Result<bool> {
        let Some(path) = &self.0.opts.state_path else {
            return Ok(false);
        };
        if !self.0.state_dirty.swap(false, Ordering::AcqRel) {
            return Ok(false);
        }
        let all = {
            let model = self.read();
            let mut all = model.restored.clone();
            all.extend(model.states.iter().map(|(k, v)| (k.clone(), v.clone())));
            all
        };
        if let Err(e) = state_cache::save(path, &all).await {
            self.0.state_dirty.store(true, Ordering::Release);
            return Err(e);
        }
        Ok(true)
    }

    pub(crate) fn remove_device(&self, id: &DeviceId) {
        let removed = {
            let mut model = self.write();
            model.states.remove(id);
            model.availability.remove(id);
            model.snapshots.remove(id);
            model.printers.remove(id);
            model.devices.remove(id).is_some()
        };
        self.0
            .images
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if removed {
            self.emit(Event::DeviceRemoved { device: id.clone() });
        }
    }

    pub(crate) fn provide_snapshots(
        &self,
        device: &DeviceId,
        source: Arc<dyn crate::media::SnapshotSource>,
    ) {
        self.write().snapshots.insert(device.clone(), source);
    }

    pub(crate) fn provide_printing(
        &self,
        device: &DeviceId,
        sink: Arc<dyn crate::media::PrintSink>,
    ) {
        self.write().printers.insert(device.clone(), sink);
    }

    /// Hands a document to a printer (queued there: this returns once it is
    /// taken, not printed). The sink is called without the model lock.
    pub async fn print(
        &self,
        device: &DeviceId,
        document: crate::media::Document,
    ) -> Result<(), crate::media::PrintError> {
        use crate::media::PrintError;
        let sink = {
            let model = self.read();
            if !model.devices.contains_key(device) {
                return Err(PrintError::UnknownDevice);
            }
            model
                .printers
                .get(device)
                .cloned()
                .ok_or(PrintError::NotAPrinter)?
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), sink.print(document)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(PrintError::Refused(format!("{e:#}"))),
            Err(_) => Err(PrintError::Refused(moli_i18n::tr!(
                "serveur.impression.refusee"
            ))),
        }
    }

    /// A fresh image of a camera (at most a second old: viewers of the same
    /// camera share one fetch). The source is called without holding the
    /// model lock; viewers of one camera wait for the same fetch.
    pub async fn camera_image(
        &self,
        device: &DeviceId,
    ) -> Result<crate::media::Image, crate::media::SnapshotError> {
        use crate::media::SnapshotError;
        let source = {
            let model = self.read();
            if !model.devices.contains_key(device) {
                return Err(SnapshotError::UnknownDevice);
            }
            model
                .snapshots
                .get(device)
                .cloned()
                .ok_or(SnapshotError::NotACamera)?
        };
        let slot = Arc::clone(
            self.0
                .images
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(device.clone())
                .or_default(),
        );
        let mut last = slot.lock().await;
        if let Some((at, image)) = last.as_ref()
            && at.elapsed() < IMAGE_FRESH
        {
            return Ok(image.clone());
        }
        // Battery cameras wake up for an image: be patient.
        match tokio::time::timeout(std::time::Duration::from_secs(20), source.snapshot()).await {
            Ok(Ok(image)) => {
                *last = Some((Instant::now(), image.clone()));
                Ok(image)
            }
            Ok(Err(e)) => Err(SnapshotError::Failed(format!("{e:#}"))),
            Err(_) => Err(SnapshotError::Timeout),
        }
    }

    /// The protected room this device (or a member) is in, if any: images
    /// of it are for humans only. With `protect_unassigned`, a device in no
    /// room is protected too (as the guard does for commands).
    #[must_use]
    pub fn protected_room(&self, device: &DeviceId) -> Option<String> {
        let rooms = self.rooms_of(device);
        let rooms: Vec<&str> = rooms.iter().map(String::as_str).collect();
        let guard = &self.0.opts.guard;
        if let Some(room) = guard.protected(&rooms) {
            return Some(room.to_owned());
        }
        (guard.protect_unassigned && rooms.is_empty())
            .then(|| moli_i18n::tr!("serveur.garde.appareil_sans_piece"))
    }

    pub(crate) fn device_ids_of(&self, instance: &InstanceId) -> Vec<DeviceId> {
        self.read()
            .devices
            .values()
            .filter(|d| &d.instance == instance)
            .map(|d| d.id.clone())
            .collect()
    }

    /// Records a value. Unknown devices/points are ignored (the model only
    /// holds what drivers declared). Only actual changes are broadcast.
    pub(crate) fn set_state(&self, device: &DeviceId, key: &str, value: Value) {
        self.record_state(device, key, value, false);
    }

    /// Like `set_state`, but always broadcast: for points that are
    /// occurrences (button presses), where the same value twice is two events.
    pub(crate) fn pulse_state(&self, device: &DeviceId, key: &str, value: Value) {
        self.record_state(device, key, value, true);
    }

    fn record_state(&self, device: &DeviceId, key: &str, value: Value, always: bool) {
        let ts = now_ms();
        let event = {
            let mut model = self.write();
            let Some(spec) = model.devices.get(device).and_then(|d| d.point(key)) else {
                return;
            };
            let key = spec.key.clone();
            self.0.state_dirty.store(true, Ordering::Release);
            let states = model.states.entry(device.clone()).or_default();
            let changed = always || states.get(&key).is_none_or(|s| s.value != value);
            states.insert(
                key.clone(),
                Sample {
                    value: value.clone(),
                    ts,
                },
            );
            changed.then(|| Event::State {
                point: PointId::new(device, &key),
                value,
                ts,
            })
        };
        if let Some(event) = event {
            self.emit(event);
        }
    }

    pub(crate) fn set_availability(&self, device: &DeviceId, online: bool) {
        let changed = {
            let mut model = self.write();
            if !model.devices.contains_key(device) {
                return;
            }
            // The driver speaks for it again.
            model.held_offline.remove(device);
            model.availability.insert(device.clone(), online) != Some(online)
        };
        if changed {
            self.emit(Event::Availability {
                device: device.clone(),
                online,
            });
        }
    }

    /// Every device of `instance` goes offline: its driver is down, its last
    /// values are no longer the truth (an automation must not act on them).
    pub(crate) fn mark_instance_offline(&self, instance: &InstanceId) {
        let gone: Vec<DeviceId> = {
            let mut model = self.write();
            let ids: Vec<DeviceId> = model
                .devices
                .values()
                .filter(|d| &d.instance == instance)
                .map(|d| d.id.clone())
                .collect();
            ids.into_iter()
                .filter(|id| {
                    let before = model.availability.insert(id.clone(), false);
                    if before == Some(false) {
                        return false;
                    }
                    model.held_offline.entry(id.clone()).or_insert(before);
                    true
                })
                .collect()
        };
        for device in gone {
            self.emit(Event::Availability {
                device,
                online: false,
            });
        }
    }

    /// A message for the people at home, shown by the dashboards.
    pub fn notice(&self, notice: moli_core::Notice) {
        self.emit(Event::Notice { notice });
    }

    // ---- internals -----------------------------------------------------------

    fn emit(&self, event: Event) {
        // No subscriber is not an error.
        let _ = self.0.events.send(event);
    }

    fn read(&self) -> RwLockReadGuard<'_, Model> {
        self.0.model.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Model> {
        self.0.model.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Model {
    fn view(&self, device: &Arc<Device>) -> DeviceView {
        DeviceView {
            device: Arc::clone(device),
            label: self.labels.get(&device.id).cloned().unwrap_or_default(),
            online: self.availability.get(&device.id).copied(),
            state: self.states.get(&device.id).cloned().unwrap_or_default(),
            camera: self.snapshots.contains_key(&device.id),
            printer: self.printers.contains_key(&device.id),
        }
    }
}

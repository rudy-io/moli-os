//! Automations: « the n8n of the house ».
//!
//! A graph of triggers, conditions and actions, drawn in the dashboard or
//! drafted by Moli, run by a deterministic engine **once a human approved
//! that exact version**. Approved automations act as `Origin::Automation`:
//! the guard lets them through (a human's intent). The language model
//! creates, explains and writes messages; it never decides at run time.
//! See `docs/AUTOMATIONS.md`.

mod check;
mod engine;
mod model;
mod run;
mod store;
pub mod sun;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Instant;

use moli_core::{AutomationChange, Notice, Origin};
use moli_i18n::tr;
use moli_runtime::{BoxFuture, Hub};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub use check::{Abilities, Check, Level, Problem, check, describe};
pub use model::{
    Author, Automation, Channel, DayNight, Edge, Graph, Mode, Node, Op, Rule, Step, SunEvent,
    Version, minutes_of,
};
pub use run::{Live, Run, RunStatus, StepLog, StepStatus};

/// `[automations]` in moli.toml.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// For sunrise and sunset (rounded is fine).
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_timezone() -> String {
    "Europe/Paris".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            latitude: None,
            longitude: None,
            timezone: default_timezone(),
        }
    }
}

/// Moli writing a text for a « Moli écrit » node (words, never decisions).
pub trait Writer: std::fmt::Debug + Send + Sync + 'static {
    fn write(&self, prompt: String) -> BoxFuture<'_, anyhow::Result<String>>;
}

/// Who changes an automation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor {
    /// A person in a dashboard human session (PIN).
    pub human: bool,
    pub author: Author,
}

impl Actor {
    /// Where the change came from, for the hub's journal.
    #[must_use]
    pub fn origin(self) -> Origin {
        match (self.human, self.author) {
            (true, _) | (false, Author::Human) => Origin::Ui,
            (false, Author::Assistant) => Origin::Assistant,
            (false, Author::Agent) => Origin::Api,
            (false, Author::Import) => Origin::System,
        }
    }

    /// Who, in words a person reads in the journal or a notice.
    #[must_use]
    pub fn who(self) -> String {
        match (self.human, self.author) {
            (true, _) | (false, Author::Human) => tr!("moteur.qui.personne"),
            (false, Author::Assistant) => tr!("moteur.qui.moli"),
            (false, Author::Agent) => tr!("moteur.qui.agent"),
            (false, Author::Import) => tr!("moteur.qui.import"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("unknown automation")]
    Unknown,
    #[error("only a person at home may do this")]
    NeedsHuman,
    #[error("{0}")]
    Invalid(String),
    /// The automation changed since the person looked at it.
    #[error("this automation changed since you looked at it")]
    Conflict,
    #[error("cannot save: {0}")]
    Io(String),
}

/// Handle on the automations: cheap to clone.
#[derive(Clone, Debug)]
pub struct Automations(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    hub: Hub,
    config: Config,
    tz: jiff::tz::TimeZone,
    path: Option<PathBuf>,
    runs_path: Option<PathBuf>,
    list: RwLock<Vec<Automation>>,
    /// The approved, switched-on ones: what the engine looks at on every
    /// change and every second. Recomputed only when the list changes
    /// (fingerprints are hashes: not something to redo 87 times a second).
    live_set: RwLock<Arc<Vec<Automation>>>,
    /// Fingerprints of the stored versions, by id, for listings (hashing
    /// serializes the graph). Forgotten at every write, under the list's
    /// write lock; approving and running always hash afresh.
    fingerprints: Mutex<HashMap<String, String>>,
    runs: Mutex<VecDeque<Run>>,
    next_run: AtomicU64,
    live: broadcast::Sender<Live>,
    /// The run in progress per automation (restart / single).
    active: Mutex<HashMap<String, (u64, CancellationToken)>>,
    /// Per automation, the parent of every run's token: switching off
    /// cancels them all, waiting ones included.
    stops: Mutex<HashMap<String, CancellationToken>>,
    /// Runs waiting their turn (queued), per automation.
    waiting: Mutex<HashMap<String, usize>>,
    /// One writer of the runs file at a time.
    runs_writing: tokio::sync::Mutex<()>,
    /// Moli is stopping: a run cancelled now says it was interrupted.
    shutting_down: std::sync::atomic::AtomicBool,
    /// One at a time per automation (queued).
    queues: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// « Held for N seconds » timers, per (automation, trigger).
    holds: Mutex<HashMap<(String, String), tokio::task::AbortHandle>>,
    /// Last value seen per point: `from` and threshold crossings.
    last: Mutex<HashMap<String, moli_core::Value>>,
    /// Last minute a clock trigger fired, per (automation, trigger).
    fired: Mutex<HashMap<(String, String), String>>,
    /// Recent starts per automation, against runaway loops.
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    writer: RwLock<Option<Arc<dyn Writer>>>,
    saving: tokio::sync::Mutex<()>,
    cancel: CancellationToken,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A number as the house writes it: `21.5` or `21,5`.
pub(crate) fn decimal(number: &str) -> String {
    number.replace('.', &tr!("moteur.nombre.separateur"))
}

/// The items joined two by two by the template `key` (`{debut}`, `{fin}`).
pub(crate) fn fold(items: impl IntoIterator<Item = String>, key: &str) -> String {
    items
        .into_iter()
        .reduce(|debut, fin| tr!(key, debut = debut, fin = fin))
        .unwrap_or_default()
}

/// `a, b and c`: all but the last joined by commas, then the template `key`
/// (`{debut}`, `{fin}`) puts the last one on.
pub(crate) fn join_last(items: &[String], key: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => tr!(key, debut = rest.join(", "), fin = last),
    }
}

/// The time of day as the house writes it: `07h30` or `07:30`.
pub(crate) fn clock(hour: i8, minute: i8) -> String {
    tr!(
        "moteur.heure.courte",
        heure = format!("{hour:02}"),
        minute = format!("{minute:02}")
    )
}

/// "yes" / "no" in the house's language.
pub(crate) fn yes_no(yes: bool) -> String {
    tr!(if yes {
        "moteur.valeur.oui"
    } else {
        "moteur.valeur.non"
    })
}

impl Automations {
    /// Loads the automations and starts the engine (until `cancel`).
    pub async fn start(
        hub: &Hub,
        config: Config,
        data_dir: Option<&std::path::Path>,
        cancel: CancellationToken,
    ) -> anyhow::Result<(Self, tokio::task::JoinHandle<()>)> {
        let tz = jiff::tz::TimeZone::get(&config.timezone)?;
        let path = data_dir.map(|d| d.join("automations.json"));
        let runs_path = data_dir.map(|d| d.join("automation-runs.jsonl"));
        let list = store::load(path.as_deref()).await?;
        let runs = store::load_runs(runs_path.as_deref()).await;
        let next = runs.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        let (live, _) = broadcast::channel(256);
        let this = Self(Arc::new(Inner {
            hub: hub.clone(),
            config,
            tz,
            path,
            runs_path,
            live_set: RwLock::new(Arc::new(
                list.iter().filter(|a| a.is_live()).cloned().collect(),
            )),
            list: RwLock::new(list),
            fingerprints: Mutex::default(),
            runs: Mutex::new(runs),
            next_run: AtomicU64::new(next),
            live,
            active: Mutex::default(),
            stops: Mutex::default(),
            waiting: Mutex::default(),
            runs_writing: tokio::sync::Mutex::new(()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            queues: Mutex::default(),
            holds: Mutex::default(),
            last: Mutex::default(),
            fired: Mutex::default(),
            rate: Mutex::default(),
            writer: RwLock::new(None),
            saving: tokio::sync::Mutex::new(()),
            cancel,
        }));
        let task = tokio::spawn(engine::run(this.clone()));
        Ok((this, task))
    }

    pub fn set_writer(&self, writer: Arc<dyn Writer>) {
        *self
            .0
            .writer
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(writer);
    }

    fn writer(&self) -> Option<Arc<dyn Writer>> {
        self.0
            .writer
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn abilities(&self) -> Abilities {
        Abilities {
            telegram: !self.messengers().is_empty(),
            writer: self.writer().is_some(),
            voice: !self.speakers().is_empty(),
            phone: !self.phones().is_empty(),
        }
    }

    /// Every point that says words aloud (Sonos `announce`).
    pub(crate) fn speakers(&self) -> Vec<moli_core::PointId> {
        self.0.hub.writable_points("announce")
    }

    /// Every phone that can be notified (the Moli app's `push`).
    pub(crate) fn phones(&self) -> Vec<moli_core::PointId> {
        self.0.hub.writable_points("push")
    }

    /// Every point that sends a written message (Telegram `notify`).
    pub(crate) fn messengers(&self) -> Vec<moli_core::PointId> {
        self.0.hub.writable_points("notify")
    }

    pub fn hub(&self) -> &Hub {
        &self.0.hub
    }

    pub fn check(&self, graph: &Graph) -> Check {
        self.check_with(graph, self.abilities())
    }

    /// `check` with abilities computed once (a list checks every automation).
    pub fn check_with(&self, graph: &Graph, can: Abilities) -> Check {
        let mut c = check(graph, &self.0.hub, can);
        let uses_sun = graph.nodes.iter().any(|n| {
            matches!(n.step, Step::AtSun { .. })
                || matches!(&n.step, Step::If { rules, .. } if rules.iter().any(|r| matches!(r, Rule::Sun { .. })))
        });
        if uses_sun && (self.0.config.latitude.is_none() || self.0.config.longitude.is_none()) {
            c.problems.push(Problem {
                node: None,
                level: Level::Warning,
                message: tr!("moteur.verif.position_inconnue"),
            });
        }
        c
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Live> {
        self.0.live.subscribe()
    }

    pub fn list(&self) -> Vec<Automation> {
        self.0
            .list
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every automation with its fingerprint, kept between writes: a listing
    /// of a hundred would otherwise hash a hundred graphs each time.
    pub fn list_fingerprinted(&self) -> Vec<(Automation, String)> {
        let list = self.0.list.read().unwrap_or_else(PoisonError::into_inner);
        let mut known = lock(&self.0.fingerprints);
        list.iter()
            .map(|a| {
                let fingerprint = known
                    .entry(a.id.clone())
                    .or_insert_with(|| a.fingerprint())
                    .clone();
                (a.clone(), fingerprint)
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Automation> {
        self.0
            .list
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    /// Most recent first; one automation's or all.
    pub fn runs(&self, automation: Option<&str>, limit: usize) -> Vec<Run> {
        lock(&self.0.runs)
            .iter()
            .rev()
            .filter(|r| automation.is_none_or(|a| r.automation == a))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Creates or replaces an automation. Saving never approves nor switches
    /// on (only `approve` does, for the version the person saw); anyone but
    /// a person may only switch off. A change of logic stops what the
    /// previous version had started.
    pub async fn save(&self, mut a: Automation, by: Actor) -> Result<Automation, AutomationError> {
        let now = moli_core::now_ms();
        if a.name.trim().is_empty() {
            return Err(AutomationError::Invalid("give it a name".into()));
        }
        a.name = tidy_name(&a.name);
        tidy(&mut a.graph)?;
        let (saved, changed_logic, before) = {
            let mut list = self.0.list.write().unwrap_or_else(PoisonError::into_inner);
            let existing = list.iter().position(|x| x.id == a.id && !a.id.is_empty());
            if let Some(i) = existing {
                let old = &list[i];
                let changed_logic = old.fingerprint() != a.fingerprint();
                let before = Some((old.enabled, old.is_approved()));
                a.created = old.created;
                a.author = old.author;
                a.approved.clone_from(&old.approved);
                a.approved_version.clone_from(&old.approved_version);
                a.enabled = if by.human {
                    old.enabled
                } else {
                    old.enabled && a.enabled
                };
                a.updated = now;
                list[i] = a.clone();
                lock(&self.0.fingerprints).remove(&a.id);
                (a, changed_logic, before)
            } else {
                if list.len() >= MAX_AUTOMATIONS {
                    return Err(AutomationError::Invalid(format!(
                        "at most {MAX_AUTOMATIONS} automations"
                    )));
                }
                a.id = new_id(&a.name, &list);
                a.created = now;
                a.updated = now;
                a.author = by.author;
                a.approved = None;
                a.approved_version = None;
                a.enabled = false;
                list.push(a.clone());
                lock(&self.0.fingerprints).remove(&a.id);
                (a, false, None)
            }
        };
        self.refresh_live();
        let switched_off = before.is_some_and(|(was_on, _)| was_on) && !saved.enabled;
        // Off means off: its runs (waiting ones too) end now.
        if changed_logic || switched_off {
            self.stop(&saved.id);
        }
        self.persist().await?;
        self.changed(&saved.id);
        let change = if before.is_some() {
            AutomationChange::Updated
        } else {
            AutomationChange::Created
        };
        self.journal(&saved, change, by);
        if !by.human && before.is_some_and(|(_, approved)| approved) && changed_logic {
            self.tell(
                &saved,
                tr!(
                    "moteur.notice.perdu_titre",
                    nom = saved.name,
                    qui = by.who()
                ),
                tr!("moteur.notice.perdu"),
            );
        } else if !by.human && switched_off {
            self.tell(
                &saved,
                tr!(
                    "moteur.notice.coupe_titre",
                    nom = saved.name,
                    qui = by.who()
                ),
                tr!("moteur.notice.coupe"),
            );
        }
        Ok(saved)
    }

    /// The audit trail: every change, who made it, which version.
    fn journal(&self, a: &Automation, change: AutomationChange, by: Actor) {
        self.0.hub.record_automation(
            by.origin(),
            Some(by.who()),
            &a.id,
            &a.name,
            change,
            Some(a.fingerprint()),
        );
    }

    /// The house hears it when someone other than a person changes what a
    /// person approved.
    fn tell(&self, a: &Automation, title: String, message: String) {
        self.0.hub.notice(Notice {
            title: Some(title),
            message,
            from: Some(a.name.clone()),
            ts: moli_core::now_ms(),
        });
    }

    /// A person approves the version they saw (`fingerprint`) and switches
    /// it on. Another version in between: refused, they must look again.
    pub async fn approve(
        &self,
        id: &str,
        fingerprint: &str,
        by: Actor,
    ) -> Result<Automation, AutomationError> {
        if !by.human {
            return Err(AutomationError::NeedsHuman);
        }
        let current = self.get(id).ok_or(AutomationError::Unknown)?;
        if current.fingerprint() != fingerprint {
            return Err(AutomationError::Conflict);
        }
        if !self.check(&current.graph).ok() {
            return Err(AutomationError::Invalid("fix the problems first".into()));
        }
        let a = self.update(id, |a| {
            if a.fingerprint() == fingerprint {
                a.approved = Some(a.fingerprint());
                a.approved_version = Some(Version {
                    mode: a.mode,
                    graph: a.graph.clone(),
                });
                a.enabled = true;
                a.note = None;
            }
        })?;
        if !a.is_live() {
            return Err(AutomationError::Conflict);
        }
        // Timers of the previous version must not fire into this one.
        self.stop_holds(id);
        self.persist().await?;
        self.changed(id);
        self.journal(&a, AutomationChange::Approved, by);
        Ok(a)
    }

    /// Off: anyone, any time. On: a person approving what they saw.
    pub async fn set_enabled(
        &self,
        id: &str,
        on: bool,
        fingerprint: Option<&str>,
        by: Actor,
    ) -> Result<Automation, AutomationError> {
        if on {
            let fingerprint =
                fingerprint.ok_or_else(|| AutomationError::Invalid("which version?".into()))?;
            return self.approve(id, fingerprint, by).await;
        }
        let a = self.update(id, |a| a.enabled = false)?;
        self.stop(id);
        self.persist().await?;
        self.changed(id);
        self.journal(&a, AutomationChange::SwitchedOff, by);
        if !by.human {
            self.tell(
                &a,
                tr!("moteur.notice.coupe_titre", nom = a.name, qui = by.who()),
                tr!("moteur.notice.coupe"),
            );
        }
        Ok(a)
    }

    /// Back to the version a person approved, after someone else changed it.
    pub async fn restore(&self, id: &str, by: Actor) -> Result<Automation, AutomationError> {
        if !by.human {
            return Err(AutomationError::NeedsHuman);
        }
        let version = self
            .get(id)
            .ok_or(AutomationError::Unknown)?
            .approved_version
            .ok_or_else(|| AutomationError::Invalid("no approved version".into()))?;
        let a = self.update(id, |a| {
            a.mode = version.mode;
            a.graph = version.graph;
        })?;
        self.stop(id);
        self.persist().await?;
        self.changed(id);
        self.journal(&a, AutomationChange::Restored, by);
        Ok(a)
    }

    pub async fn delete(&self, id: &str, by: Actor) -> Result<(), AutomationError> {
        if !by.human {
            return Err(AutomationError::NeedsHuman);
        }
        let gone = {
            let mut list = self.0.list.write().unwrap_or_else(PoisonError::into_inner);
            let i = list
                .iter()
                .position(|a| a.id == id)
                .ok_or(AutomationError::Unknown)?;
            lock(&self.0.fingerprints).remove(id);
            list.remove(i)
        };
        self.refresh_live();
        self.stop(id);
        self.persist().await?;
        self.changed(id);
        self.journal(&gone, AutomationChange::Deleted, by);
        Ok(())
    }

    /// Runs it now. Dry (anyone): conditions read for real, nothing acts,
    /// nothing waits. For real: a person, on the approved version they saw.
    pub async fn run_now(
        &self,
        id: &str,
        dry: bool,
        fingerprint: Option<&str>,
        by: Actor,
    ) -> Result<Run, AutomationError> {
        let a = self.get(id).ok_or(AutomationError::Unknown)?;
        if !dry {
            if !by.human {
                return Err(AutomationError::NeedsHuman);
            }
            if fingerprint != Some(a.fingerprint().as_str()) {
                return Err(AutomationError::Conflict);
            }
            if !a.is_approved() || !self.check(&a.graph).ok() {
                return Err(AutomationError::Invalid(
                    "approve this version first".into(),
                ));
            }
            // A switched-off automation's runs are cancelled at birth: say so
            // instead of answering « running » for a run that never happens.
            if !a.enabled {
                return Err(AutomationError::Invalid("switch it on first".into()));
            }
        }
        let start = a
            .graph
            .nodes
            .iter()
            .find(|n| matches!(n.step, Step::Manual))
            .or_else(|| a.graph.triggers().next())
            .map(|n| n.id.clone())
            .ok_or_else(|| AutomationError::Invalid("no trigger".into()))?;
        if dry {
            let id = self.next_id();
            Ok(run::execute(
                self,
                &a,
                &start,
                tr!("moteur.pourquoi.essai"),
                true,
                CancellationToken::new(),
                id,
            )
            .await)
        } else {
            Ok(self.start_run(&a, &start, tr!("moteur.pourquoi.main"), true))
        }
    }

    fn next_id(&self) -> u64 {
        self.0
            .next_run
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn update(
        &self,
        id: &str,
        f: impl FnOnce(&mut Automation),
    ) -> Result<Automation, AutomationError> {
        let mut list = self.0.list.write().unwrap_or_else(PoisonError::into_inner);
        let a = list
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or(AutomationError::Unknown)?;
        f(a);
        a.updated = moli_core::now_ms();
        lock(&self.0.fingerprints).remove(id);
        let a = a.clone();
        drop(list);
        self.refresh_live();
        Ok(a)
    }

    fn refresh_live(&self) {
        let live: Vec<Automation> = self
            .list()
            .into_iter()
            .filter(Automation::is_live)
            .collect();
        *self
            .0
            .live_set
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(live);
    }

    /// The automations that run when triggered (cached).
    pub(crate) fn live(&self) -> Arc<Vec<Automation>> {
        self.0
            .live_set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn persist(&self) -> Result<(), AutomationError> {
        let _one = self.0.saving.lock().await;
        let list = self.list();
        store::save(self.0.path.as_deref(), &list)
            .await
            .map_err(|e| AutomationError::Io(e.to_string()))
    }

    fn changed(&self, id: &str) {
        let _ = self.0.live.send(Live::Changed {
            automation: id.to_owned(),
        });
    }

    /// Cancels every run of it (waiting ones included) and its pending
    /// « held for » timers.
    fn stop(&self, id: &str) {
        if let Some(all) = lock(&self.0.stops).remove(id) {
            all.cancel();
        }
        lock(&self.0.active).remove(id);
        self.stop_holds(id);
    }

    fn stop_holds(&self, id: &str) {
        lock(&self.0.holds).retain(|(a, _), handle| {
            let keep = a != id;
            if !keep {
                handle.abort();
            }
            keep
        });
    }
}

/// A name as saving stores it: trimmed, 80 characters at most.
pub fn tidy_name(name: &str) -> String {
    name.trim()
        .chars()
        .take(MAX_NAME)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Bounds on what is stored: duplicate edges dropped, long texts cut.
/// Public for what is shown to Moli without being saved (a draft's current
/// version): the same bounds, not a copy of them.
pub fn tidy(graph: &mut Graph) -> Result<(), AutomationError> {
    if graph.nodes.len() > check::MAX_NODES {
        return Err(AutomationError::Invalid("too many nodes".into()));
    }
    let mut seen = std::collections::HashSet::new();
    graph
        .edges
        .retain(|e| seen.insert((e.from.clone(), e.port.clone(), e.to.clone())));
    if graph.edges.len() > check::MAX_EDGES {
        return Err(AutomationError::Invalid("too many links".into()));
    }
    let cut = |s: &mut String| {
        if s.chars().count() > MAX_TEXT {
            *s = s.chars().take(MAX_TEXT).collect();
        }
    };
    for n in &mut graph.nodes {
        n.id = n.id.chars().take(40).collect();
        match &mut n.step {
            Step::Notify { title, message, .. } => {
                cut(message);
                if let Some(t) = title {
                    cut(t);
                }
            }
            Step::Write { prompt } => cut(prompt),
            _ => {}
        }
    }
    Ok(())
}

/// An import checks it before spending anything on translations.
pub const MAX_AUTOMATIONS: usize = 400;
const MAX_NAME: usize = 80;
const MAX_TEXT: usize = 2_000;

/// `entree-la-nuit`, `entree-la-nuit-2`…
/// Fixed routes under `/api/automations/` would shadow these ids.
const RESERVED: [&str; 5] = ["check", "draft", "import", "runs", "live"];

fn new_id(name: &str, list: &[Automation]) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        let c = match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(40).collect();
    let slug = if slug.is_empty() {
        "automatisme".to_owned()
    } else {
        slug
    };
    let mut id = slug.clone();
    let mut n = 2;
    while RESERVED.contains(&id.as_str()) || list.iter().any(|a| a.id == id) {
        id = format!("{slug}-{n}");
        n += 1;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_readable_and_unique() {
        let a = |id: &str| Automation {
            id: id.into(),
            name: String::new(),
            enabled: false,
            mode: Mode::Restart,
            graph: Graph::default(),
            author: Author::Human,
            approved: None,
            approved_version: None,
            note: None,
            created: 0,
            updated: 0,
        };
        assert_eq!(new_id("Entrée, la nuit !", &[]), "entree-la-nuit");
        assert_eq!(
            new_id("Entrée la nuit", &[a("entree-la-nuit")]),
            "entree-la-nuit-2"
        );
        assert_eq!(new_id("  ", &[]), "automatisme");
        assert_eq!(new_id("Live", &[]), "live-2");
        assert_eq!(new_id("Runs", &[a("runs-2")]), "runs-3");
    }
}

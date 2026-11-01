//! What the conversation proposes and a person has not decided yet: a change
//! of configuration (rename a device, move it, hide it, rename a room). The
//! model never changes anything by itself: Moli keeps the proposal for a few
//! minutes, says it, and applies it only when a person confirms.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use moli_core::DeviceId;
use serde::Serialize;

/// How long a proposal waits for a person.
pub(crate) const LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Proposals waiting at once: the oldest give way.
const MAX_PENDING: usize = 20;

/// A change of configuration, resolved to what Moli will do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    RenameDevice {
        device: DeviceId,
        name: String,
    },
    MoveDevice {
        device: DeviceId,
        room: String,
    },
    SetEnabled {
        device: DeviceId,
        enabled: bool,
    },
    RenameRoom {
        from: String,
        to: String,
    },
    /// A member with a name and nothing else.
    AddPerson {
        name: String,
    },
    /// Takes back an addition (only while nobody built on it).
    RemovePerson {
        id: String,
        name: String,
    },
    /// The house's position; `None`: forget what a person chose, `moli.toml` speaks again.
    SetLocation {
        place: Option<crate::places::Place>,
    },
}

/// What the screen shows under the reply: a sentence, and the way to decide.
#[derive(Clone, Debug, Serialize)]
pub struct Proposal {
    pub id: u64,
    pub text: String,
    /// Seconds left to decide.
    pub expires_in: u64,
}

#[derive(Debug)]
pub(crate) struct Pending {
    pub(crate) change: Change,
    pub(crate) text: String,
    /// This proposal takes back the confirmed change numbered so (a history entry).
    pub(crate) undo: Option<u64>,
    /// Who asked for it (`Turn::asker`): « annule » only refuses its own.
    pub(crate) asker: Option<String>,
}

/// A change a person confirmed, and what takes it back.
#[derive(Clone, Debug)]
pub(crate) struct Applied {
    /// Its place in the history: an undo only goes through if this one is still the latest.
    pub(crate) id: u64,
    pub(crate) inverse: Change,
    /// The inverse, in words (« renommer « B » en « A » »).
    pub(crate) text: String,
    /// When it was confirmed: « annule » takes back the latest of this and what was planned.
    pub(crate) at: Instant,
}

/// Confirmed changes remembered, the latest last: « annule » takes the latest back.
const MAX_APPLIED: usize = 10;

#[derive(Debug)]
struct Entry {
    id: u64,
    until: Instant,
    pending: Pending,
}

#[derive(Debug)]
pub(crate) struct Proposals {
    lifetime: Duration,
    state: Mutex<(u64, Vec<Entry>)>,
    applied: Mutex<Vec<Applied>>,
    applied_next: std::sync::atomic::AtomicU64,
}

impl Default for Proposals {
    fn default() -> Self {
        Self::lasting(LIFETIME)
    }
}

impl Proposals {
    pub(crate) fn lasting(lifetime: Duration) -> Self {
        Self {
            lifetime,
            // Numbers do not start over at every start: an open page that still
            // shows an old proposal must never confirm a new one.
            state: Mutex::new((moli_core::now_ms(), Vec::new())),
            applied: Mutex::new(Vec::new()),
            applied_next: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, Vec<Entry>)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps a proposal; what to show of it.
    pub(crate) fn add(
        &self,
        change: Change,
        text: String,
        undo: Option<u64>,
        asker: Option<String>,
    ) -> Proposal {
        let mut state = self.lock();
        let now = Instant::now();
        state.1.retain(|e| e.until > now);
        while state.1.len() >= MAX_PENDING {
            state.1.remove(0);
        }
        state.0 += 1;
        let id = state.0;
        state.1.push(Entry {
            id,
            until: now + self.lifetime,
            pending: Pending {
                change,
                text: text.clone(),
                undo,
                asker,
            },
        });
        Proposal {
            id,
            text,
            expires_in: self.lifetime.as_secs(),
        }
    }

    /// Remembers a change a person confirmed, to take it back on request.
    pub(crate) fn record(&self, inverse: Change, text: String) {
        let id = self
            .applied_next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut list = self
            .applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        list.push(Applied {
            id,
            inverse,
            text,
            at: Instant::now(),
        });
        if list.len() > MAX_APPLIED {
            list.remove(0);
        }
    }

    /// The latest confirmed change, not yet taken back.
    pub(crate) fn last_applied(&self) -> Option<Applied> {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .cloned()
    }

    /// The newest proposal of `asker` still waiting for a person (another
    /// screen's, or another person's, is not theirs to refuse).
    pub(crate) fn newest_pending(&self, asker: Option<&str>) -> Option<u64> {
        let state = self.lock();
        let now = Instant::now();
        state
            .1
            .iter()
            .rev()
            .find(|e| e.until > now && e.pending.asker.as_deref() == asker)
            .map(|e| e.id)
    }

    /// Forgets the latest one: it was taken back.
    pub(crate) fn pop_applied(&self) {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop();
    }

    /// Gives a proposal back that could not be applied: it waits again.
    pub(crate) fn put_back(&self, id: u64, pending: Pending) {
        let mut state = self.lock();
        state.1.retain(|e| e.id != id);
        state.1.push(Entry {
            id,
            until: Instant::now() + self.lifetime,
            pending,
        });
    }

    /// Takes a proposal out: it is decided, whatever the decision.
    pub(crate) fn take(&self, id: u64) -> Option<Pending> {
        let mut state = self.lock();
        let now = Instant::now();
        let at = state.1.iter().position(|e| e.id == id)?;
        let entry = state.1.remove(at);
        (entry.until > now).then_some(entry.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change() -> Change {
        Change::SetEnabled {
            device: "a:1".into(),
            enabled: false,
        }
    }

    #[test]
    fn a_proposal_is_taken_once() {
        let proposals = Proposals::default();
        let p = proposals.add(change(), "masquer".into(), None, None);
        assert_eq!(proposals.take(p.id).unwrap().change, change());
        assert!(proposals.take(p.id).is_none());
        assert!(proposals.take(999).is_none());
    }

    #[test]
    fn a_proposal_put_back_can_be_decided_again() {
        let proposals = Proposals::default();
        let p = proposals.add(change(), "masquer".into(), None, None);
        let taken = proposals.take(p.id).unwrap();
        assert!(proposals.take(p.id).is_none());
        proposals.put_back(p.id, taken);
        assert!(proposals.take(p.id).is_some());
    }

    #[test]
    fn numbers_do_not_start_over_at_every_start() {
        let first = Proposals::default()
            .add(change(), "a".into(), None, None)
            .id;
        assert!(
            first > 1_000,
            "an open page must not mistake a new proposal for an old one: {first}"
        );
    }

    #[test]
    fn a_proposal_nobody_decided_expires() {
        let proposals = Proposals::lasting(Duration::ZERO);
        let p = proposals.add(change(), "masquer".into(), None, None);
        assert!(proposals.take(p.id).is_none());
    }

    #[test]
    fn only_the_latest_proposals_are_kept() {
        let proposals = Proposals::default();
        let first = proposals.add(change(), "premier".into(), None, None).id;
        for _ in 0..MAX_PENDING {
            proposals.add(change(), "autre".into(), None, None);
        }
        assert!(proposals.take(first).is_none(), "the oldest gave way");
    }
}

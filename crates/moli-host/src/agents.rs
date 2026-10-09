//! The Moli agent on the house's computers (`agent/windows/` for a PC): it
//! reports what the machine does every few seconds (processor, graphics card,
//! memory, temperatures, disks, busiest processes, its Claude and Codex apps)
//! and takes orders back in the answer (shut down, sleep, open Claude and
//! Codex for the phone). Outbound only from the PC: nothing listens there.
//!
//! The agent proves itself with a token; Moli only keeps its SHA-256
//! (`agent_sha256` in the machine's config), never the token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure};
use ring::digest::{SHA256, digest};
use serde_json::{Value as Json, json};
use tokio::sync::mpsc;

/// An agent silent this long is gone (the PC asleep, off, or the agent stopped).
pub const GONE: Duration = Duration::from_secs(20);
/// An order nobody took this long is dropped (a PC that never woke).
const ORDER_TTL: Duration = Duration::from_secs(15 * 60);
/// How often the agent reports.
const EVERY: u64 = 5;
const MAX_ORDERS: usize = 16;

/// A report, as the driver gets it.
#[derive(Debug)]
pub struct Report {
    pub machine: String,
    pub body: Json,
}

#[derive(Debug, Default)]
struct Slot {
    sha256: String,
    seen: Option<Instant>,
    seen_ms: u64,
    last: Option<Json>,
    orders: Vec<(Json, Instant)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentError {
    UnknownMachine,
    BadToken,
    Invalid(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMachine => f.write_str(&moli_i18n::tr!("pilotes.host.machine_inconnue")),
            Self::BadToken => f.write_str(&moli_i18n::tr!("pilotes.host.jeton_refuse")),
            Self::Invalid(why) => f.write_str(why),
        }
    }
}

/// What the API and the driver share about the agents.
#[derive(Clone, Debug)]
pub struct Agents {
    slots: Arc<Mutex<HashMap<String, Slot>>>,
    reports: mpsc::Sender<Report>,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[must_use]
pub fn sha256(token: &str) -> String {
    hex(digest(&SHA256, token.as_bytes()).as_ref())
}

fn order_id() -> String {
    use ring::rand::{SecureRandom as _, SystemRandom};
    let mut raw = [0u8; 6];
    let _ = SystemRandom::new().fill(&mut raw);
    format!("o-{}", hex(&raw))
}

impl Agents {
    /// The agents of `machines` (id, token hash); the receiver is the driver's.
    #[must_use]
    pub fn new(
        machines: impl IntoIterator<Item = (String, String)>,
    ) -> (Self, mpsc::Receiver<Report>) {
        let (tx, rx) = mpsc::channel(64);
        let slots = machines
            .into_iter()
            .map(|(id, sha)| {
                (
                    id,
                    Slot {
                        sha256: sha.to_lowercase(),
                        ..Slot::default()
                    },
                )
            })
            .collect();
        (
            Self {
                slots: Arc::new(Mutex::new(slots)),
                reports: tx,
            },
            rx,
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A report from the agent of `machine`: checked, kept, handed to the
    /// driver; the answer carries the orders not yet done.
    pub fn report(&self, machine: &str, token: &str, body: Json) -> Result<Json, AgentError> {
        let orders = {
            let mut slots = self.lock();
            let slot = slots.get_mut(machine).ok_or(AgentError::UnknownMachine)?;
            // Hashes compared, not tokens: timing could only leak a hash.
            if sha256(token) != slot.sha256 {
                return Err(AgentError::BadToken);
            }
            if !body.is_object() {
                return Err(AgentError::Invalid(moli_i18n::tr!(
                    "pilotes.host.objet_json"
                )));
            }
            let done: Vec<&str> = body["done"]
                .as_array()
                .map(|d| d.iter().filter_map(Json::as_str).collect())
                .unwrap_or_default();
            slot.orders.retain(|(o, at)| {
                at.elapsed() < ORDER_TTL && !done.contains(&o["id"].as_str().unwrap_or_default())
            });
            slot.seen = Some(Instant::now());
            slot.seen_ms = moli_core::now_ms();
            slot.last = Some(body.clone());
            slot.orders
                .iter()
                .map(|(o, _)| o.clone())
                .collect::<Vec<_>>()
        };
        let _ = self.reports.try_send(Report {
            machine: machine.to_owned(),
            body,
        });
        Ok(json!({ "orders": orders, "every": EVERY }))
    }

    /// Whether the agent of `machine` spoke lately.
    #[must_use]
    pub fn connected(&self, machine: &str) -> bool {
        self.lock()
            .get(machine)
            .and_then(|s| s.seen)
            .is_some_and(|at| at.elapsed() < GONE)
    }

    #[must_use]
    pub fn has(&self, machine: &str) -> bool {
        self.lock().contains_key(machine)
    }

    /// What the agent last said (processes, disks, runs…), for the dashboard.
    #[must_use]
    pub fn view(&self, machine: &str) -> Option<Json> {
        let slots = self.lock();
        let slot = slots.get(machine)?;
        Some(json!({
            "connected": slot.seen.is_some_and(|at| at.elapsed() < GONE),
            "seen": slot.seen_ms,
            "report": slot.last.clone().unwrap_or(Json::Null),
            "pending": slot.orders.iter().map(|(o, _)| o.clone()).collect::<Vec<_>>(),
        }))
    }

    /// An order for the agent (delivered at its next report). Its id.
    pub fn order(&self, machine: &str, mut order: Json) -> anyhow::Result<String> {
        let id = order_id();
        order["id"] = Json::String(id.clone());
        let mut slots = self.lock();
        let Some(slot) = slots.get_mut(machine) else {
            bail!("{}", AgentError::UnknownMachine);
        };
        slot.orders.retain(|(_, at)| at.elapsed() < ORDER_TTL);
        ensure!(
            slot.orders.len() < MAX_ORDERS,
            moli_i18n::tr!("pilotes.host.trop_d_ordres")
        );
        slot.orders.push((order, Instant::now()));
        Ok(id)
    }

    /// The remote mode: the PC opens its Claude and Codex apps (when they are
    /// not), so the phone reaches them. Asked again while one waits (the PC
    /// still waking up): the same order.
    pub fn remote(&self, machine: &str) -> anyhow::Result<String> {
        let waiting = self.lock().get(machine).and_then(|s| {
            s.orders
                .iter()
                .find(|(o, at)| o["kind"] == "remote" && at.elapsed() < ORDER_TTL)
                .and_then(|(o, _)| o["id"].as_str().map(str::to_owned))
        });
        if let Some(id) = waiting {
            return Ok(id);
        }
        self.order(machine, json!({ "kind": "remote" }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> (Agents, mpsc::Receiver<Report>) {
        Agents::new([("pc-bureau".to_owned(), sha256("secret-token"))])
    }

    #[tokio::test]
    async fn a_report_needs_the_right_token_and_reaches_the_driver() {
        let (agents, mut rx) = agents();
        assert_eq!(
            agents.report("pc-bureau", "wrong", json!({})).unwrap_err(),
            AgentError::BadToken
        );
        assert_eq!(
            agents
                .report("other", "secret-token", json!({}))
                .unwrap_err(),
            AgentError::UnknownMachine
        );
        assert!(!agents.connected("pc-bureau"));
        let answer = agents
            .report("pc-bureau", "secret-token", json!({ "cpu": 12.0 }))
            .unwrap();
        assert_eq!(answer["orders"], json!([]));
        assert!(agents.connected("pc-bureau"));
        assert_eq!(rx.recv().await.unwrap().body["cpu"], json!(12.0));
    }

    #[test]
    fn orders_go_until_the_agent_says_done() {
        let (agents, _rx) = agents();
        let id = agents
            .order("pc-bureau", json!({ "kind": "sleep" }))
            .unwrap();
        let answer = agents
            .report("pc-bureau", "secret-token", json!({}))
            .unwrap();
        assert_eq!(answer["orders"][0]["id"], json!(id));
        // Not acknowledged: sent again.
        let again = agents
            .report("pc-bureau", "secret-token", json!({}))
            .unwrap();
        assert_eq!(again["orders"].as_array().unwrap().len(), 1);
        let after = agents
            .report("pc-bureau", "secret-token", json!({ "done": [id] }))
            .unwrap();
        assert_eq!(after["orders"], json!([]));
    }

    #[test]
    fn a_remote_order_waits_once() {
        let (agents, _rx) = agents();
        let first = agents.remote("pc-bureau").unwrap();
        // Asked twice while the PC wakes: one order, not two.
        assert_eq!(agents.remote("pc-bureau").unwrap(), first);
        let view = agents.view("pc-bureau").unwrap();
        assert_eq!(view["pending"].as_array().unwrap().len(), 1);
        assert_eq!(view["pending"][0]["kind"], json!("remote"));
        assert!(agents.remote("other").is_err());
    }
}

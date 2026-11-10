//! LIFX Wi-Fi lights, 100 % local, through LIFX's LAN protocol (UDP
//! 56700): power, brightness, and colour temperature or colour where the
//! model has them. No account, no secret.
//!
//! One instance runs every bulb it finds: at the addresses of `hosts`, and
//! by a `GetService` broadcast unless `discovery = false`. A bulb is known
//! by its serial number, never by its address.
//!
//! The protocol has no authentication: any device of the network can drive
//! the bulbs, and nothing here can change that. What the driver does is not
//! believe just any datagram (see `link`): a reply counts only from the
//! address asked, a bulb moves only once it is silent where it was, and a
//! flood of made-up serials costs a bounded time per search.

mod bulb;
mod link;
mod products;
mod proto;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{DeviceId, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use tokio::time::Instant;

use bulb::Bulb;
use link::{Answer, Link};
use proto::Serial;

/// Consecutive missed polls before a bulb is shown offline: a LIFX bulb
/// often misses one and answers the next.
const MISSES: u32 = 3;
/// An offline bulb is asked once every this many polls (it would delay
/// the others' and the orders' turn at each one).
const OFFLINE_EVERY: u32 = 6;
/// A new search while a bulb is missing or offline, or none is known…
const SEARCH_MISSING: Duration = Duration::from_secs(60);
/// …and otherwise, for new bulbs and new addresses.
const SEARCH_AGAIN: Duration = Duration::from_secs(300);
/// Bulbs one instance runs at most.
const MAX_BULBS: usize = 100;
/// New bulbs waiting to be met at most (the others at a next search).
const MEET_PER_SEARCH: usize = 8;
const MAX_TRANSITION_MS: u32 = 60_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Addresses asked directly (bulbs with a fixed address, or on a network
    /// the broadcast does not reach).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Also look for bulbs with a broadcast on the local network.
    #[serde(default = "yes")]
    pub discovery: bool,
    /// Seconds between two readings of each bulb.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    /// Fade, in milliseconds (0-60 000), of every change.
    #[serde(default)]
    pub transition_ms: u32,
}

fn yes() -> bool {
    true
}

fn default_poll() -> u64 {
    5
}

#[derive(Debug)]
pub struct Lifx {
    /// Where bulbs listen: port 56700 (another port in tests only).
    hosts: Vec<SocketAddrV4>,
    /// Where the search is broadcast, unless `discovery = false`.
    broadcast: Option<SocketAddrV4>,
    poll: Duration,
    transition: u32,
    search_missing: Duration,
    search_again: Duration,
    max_bulbs: usize,
}

impl Lifx {
    /// # Errors
    /// An address that is not IPv4, nothing to look for, or a poll period
    /// or a fade out of bounds.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let Config {
            hosts,
            discovery,
            poll_secs,
            transition_ms,
        } = config;
        let hosts = hosts
            .iter()
            .map(|h| {
                h.trim()
                    .parse::<Ipv4Addr>()
                    .map(|ip| SocketAddrV4::new(ip, proto::PORT))
                    .with_context(|| moli_i18n::tr!("pilotes.lifx.adresse_invalide", host = h))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if hosts.is_empty() && !discovery {
            bail!(moli_i18n::tr!("pilotes.lifx.rien_a_chercher"));
        }
        if !(1..=3600).contains(&poll_secs) {
            bail!(moli_i18n::tr!("pilotes.lifx.poll_secs"));
        }
        if transition_ms > MAX_TRANSITION_MS {
            bail!(moli_i18n::tr!("pilotes.lifx.transition_ms"));
        }
        Ok(Self {
            hosts,
            broadcast: discovery.then_some(SocketAddrV4::new(Ipv4Addr::BROADCAST, proto::PORT)),
            poll: Duration::from_secs(poll_secs),
            transition: transition_ms,
            search_missing: SEARCH_MISSING,
            search_again: SEARCH_AGAIN,
            max_bulbs: MAX_BULBS,
        })
    }
}

impl Driver for Lifx {
    fn kind(&self) -> &'static str {
        "lifx"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// Work a search leaves, done one piece at a time between the orders.
#[derive(Debug, PartialEq, Eq)]
enum Job {
    /// A serial heard for the first time.
    Meet(Serial, SocketAddrV4),
    /// A known bulb heard elsewhere.
    Move(DeviceId, SocketAddrV4),
    /// Model, firmware or group still to ask.
    Complete(DeviceId),
    Signal(DeviceId),
    /// One reading of one bulb.
    Read(DeviceId),
}

struct Bulbs {
    by_id: HashMap<DeviceId, Bulb>,
    jobs: VecDeque<Job>,
    /// Serials left out: the instance is full.
    full: bool,
    max: usize,
    /// Serials that did not answer as a light, not asked again before.
    failed: HashMap<Serial, Instant>,
    retry_after: Duration,
}

impl Bulbs {
    /// Looks for bulbs and queues what it heard.
    async fn search(&mut self, lifx: &Lifx, link: &mut Link) {
        let heard = link.search(&lifx.hosts, lifx.broadcast).await;
        let queued = self
            .jobs
            .iter()
            .filter(|j| matches!(j, Job::Meet(..)))
            .count();
        let mut meet = MEET_PER_SEARCH.saturating_sub(queued);
        let mut room = self.max.saturating_sub(self.by_id.len() + queued);
        // A bulb has one serial: one new serial per address and search, so
        // a device announcing many costs one meeting each time.
        let mut used: Vec<SocketAddrV4> = Vec::new();
        let now = Instant::now();
        self.failed.retain(|_, until| *until > now);
        self.full = false;
        for (serial, addresses) in heard {
            if let Some(bulb) = self.by_id.values().find(|b| b.serial == serial) {
                if !addresses.contains(&bulb.at) {
                    self.queue(Job::Move(bulb.id.clone(), pick(lifx, &addresses)));
                }
                continue;
            }
            if self
                .jobs
                .iter()
                .any(|j| matches!(j, Job::Meet(s, _) if *s == serial))
            {
                continue;
            }
            if room == 0 {
                self.full = true;
                continue;
            }
            let at = pick(lifx, &addresses);
            if meet == 0 || used.contains(&at) || self.failed.contains_key(&serial) {
                continue;
            }
            used.push(at);
            self.queue(Job::Meet(serial, at));
            meet -= 1;
            room -= 1;
        }
        let ids: Vec<(DeviceId, bool)> = self
            .by_id
            .values()
            .filter(|b| b.online)
            .map(|b| (b.id.clone(), b.incomplete()))
            .collect();
        for (id, incomplete) in ids {
            if incomplete {
                self.queue(Job::Complete(id.clone()));
            }
            self.queue(Job::Signal(id));
        }
    }

    fn queue(&mut self, job: Job) {
        if !self.jobs.contains(&job) {
            self.jobs.push_back(job);
        }
    }

    /// One piece of work. Whether a bulb went offline.
    async fn work(&mut self, job: Job, link: &mut Link, ctx: &DriverCtx) -> bool {
        let mut lost = false;
        match job {
            Job::Meet(serial, at) => self.meet(serial, at, link, ctx).await,
            Job::Move(id, at) => {
                if let Some(bulb) = self.by_id.get_mut(&id) {
                    follow(bulb, at, link, ctx).await;
                }
            }
            Job::Complete(id) => {
                if let Some(bulb) = self.by_id.get_mut(&id)
                    && bulb.complete(link, false).await
                {
                    ctx.upsert_device(bulb.device(ctx));
                }
            }
            Job::Signal(id) => {
                if let Some(bulb) = self.by_id.get_mut(&id)
                    && let Some(rssi) = bulb.signal(link).await
                {
                    ctx.set_state(&bulb.id, "rssi", Value::Int(rssi));
                }
            }
            Job::Read(id) => {
                if let Some(bulb) = self.by_id.get_mut(&id) {
                    lost = read(bulb, link, ctx).await;
                }
            }
        }
        self.warnings(ctx);
        lost
    }

    /// A serial heard for the first time: it must answer a `LightGet` as a
    /// light; model, firmware and group as far as they answer.
    async fn meet(&mut self, serial: Serial, at: SocketAddrV4, link: &mut Link, ctx: &DriverCtx) {
        let state = link
            .ask(
                at,
                serial,
                (proto::LIGHT_GET, &[]),
                (proto::Reply::Response, proto::LIGHT_STATE),
                (Instant::now() + link::READ, true),
            )
            .await
            .message()
            .and_then(|m| proto::light_state(&m.payload));
        let Some(state) = state else {
            self.failed
                .insert(serial, Instant::now() + self.retry_after);
            return;
        };
        let mut bulb = Bulb::new(ctx, serial, at, &state);
        bulb.complete(link, true).await;
        let rssi = bulb.signal(link).await;
        ctx.upsert_device(bulb.device(ctx));
        ctx.set_availability(&bulb.id, true);
        for (key, value) in bulb.values(&state) {
            ctx.set_state(&bulb.id, key, value);
        }
        if let Some(rssi) = rssi {
            ctx.set_state(&bulb.id, "rssi", Value::Int(rssi));
        }
        tracing::info!(instance = %ctx.instance(), bulb = %bulb.id, at = %at, "lifx bulb met");
        self.by_id.insert(bulb.id.clone(), bulb);
    }

    fn warnings(&self, ctx: &DriverCtx) {
        if self.full {
            ctx.warn(moli_i18n::tr!("pilotes.lifx.trop", max = self.max));
        } else if let Some(bulb) = self.by_id.values().find(|b| b.product().is_none()) {
            let pid = bulb
                .version
                .map_or_else(|| "?".to_owned(), |(_, p)| p.to_string());
            ctx.warn(moli_i18n::tr!("pilotes.lifx.modele_inconnu", pid = pid));
        } else {
            ctx.clear_warning();
        }
    }

    /// A poll: one reading per bulb queued (a bulb fading, or offline
    /// outside its turn, is skipped). Each reading is its own piece of
    /// work, so silent bulbs never hold an order longer than one reading.
    fn poll(&mut self) {
        let mut reads = Vec::new();
        let now = Instant::now();
        for bulb in self.by_id.values_mut() {
            bulb.polls = bulb.polls.wrapping_add(1);
            if now < bulb.settle_until || (!bulb.online && bulb.polls % OFFLINE_EVERY != 1) {
                continue;
            }
            reads.push(Job::Read(bulb.id.clone()));
        }
        for job in reads {
            self.queue(job);
        }
    }

    /// Whether to search again soon: none known, one known before this run
    /// or at an address of `hosts` not met, or one offline.
    fn missing(&self, lifx: &Lifx, known: &BTreeSet<DeviceId>) -> bool {
        let answered = |at: &SocketAddrV4| self.by_id.values().any(|b| b.at == *at);
        self.by_id.is_empty()
            || self.by_id.values().any(|b| !b.online)
            || known.iter().any(|id| !self.by_id.contains_key(id))
            || !lifx.hosts.iter().all(answered)
    }

    fn status(&self, ctx: &DriverCtx) {
        if self.by_id.is_empty() {
            ctx.wait_for(moli_i18n::tr!("pilotes.lifx.aucune"));
        } else {
            ctx.ready();
        }
    }
}

/// Of the addresses a serial answered from: the first one of `hosts`, in
/// their order, else the first heard.
fn pick(lifx: &Lifx, addresses: &[SocketAddrV4]) -> SocketAddrV4 {
    lifx.hosts
        .iter()
        .find(|h| addresses.contains(h))
        .copied()
        .unwrap_or(addresses[0])
}

/// One `LightGet`, sent once (the next poll is its retry). Whether the bulb
/// went offline.
async fn read(bulb: &mut Bulb, link: &mut Link, ctx: &DriverCtx) -> bool {
    if Instant::now() < bulb.settle_until {
        return false;
    }
    let state = bulb
        .ask(
            link,
            (proto::LIGHT_GET, proto::LIGHT_STATE),
            (Instant::now() + link::ONCE, false),
        )
        .await
        .message()
        .and_then(|m| proto::light_state(&m.payload));
    let Some(state) = state else {
        bulb.misses += 1;
        if bulb.online && bulb.misses >= MISSES {
            bulb.online = false;
            ctx.set_availability(&bulb.id, false);
            tracing::warn!(instance = %ctx.instance(), bulb = %bulb.id, "lifx unreachable");
            return true;
        }
        return false;
    };
    seen(bulb, &state, ctx);
    false
}

/// A reading of a bulb: online, values, and its LIFX name if it changed.
fn seen(bulb: &mut Bulb, state: &proto::LightState, ctx: &DriverCtx) {
    bulb.misses = 0;
    if !bulb.online {
        bulb.online = true;
        ctx.set_availability(&bulb.id, true);
    }
    bulb.last = Some((state.color, state.on));
    if let Some(label) = &state.label
        && *label != bulb.name
    {
        // Renamed in the LIFX app: the suggestion follows.
        label.clone_into(&mut bulb.name);
        ctx.upsert_device(bulb.device(ctx));
    }
    for (key, value) in bulb.values(state) {
        ctx.set_state(&bulb.id, key, value);
    }
}

/// A known bulb heard at another address: it moves there only if it no
/// longer answers where it was, and does answer there. A bulb that still
/// answers cannot be taken away by a datagram claiming its serial.
async fn follow(bulb: &mut Bulb, at: SocketAddrV4, link: &mut Link, ctx: &DriverCtx) {
    // Silent means silent through the resends: one lost datagram must not
    // hand the bulb to whoever claims its serial.
    let here = bulb
        .ask(
            link,
            (proto::LIGHT_GET, proto::LIGHT_STATE),
            (Instant::now() + link::READ, true),
        )
        .await;
    if let Answer::Got(_) = here {
        tracing::info!(instance = %ctx.instance(), bulb = %bulb.id, claimed = %at, "lifx bulb still answers where it is: not moved");
        return;
    }
    let there = link
        .ask(
            at,
            bulb.serial,
            (proto::LIGHT_GET, &[]),
            (proto::Reply::Response, proto::LIGHT_STATE),
            (Instant::now() + link::READ, true),
        )
        .await
        .message()
        .and_then(|m| proto::light_state(&m.payload));
    let Some(state) = there else {
        return;
    };
    tracing::info!(instance = %ctx.instance(), bulb = %bulb.id, old = %bulb.at, new = %at, "lifx found at a new address");
    bulb.at = at;
    bulb.polls = 0;
    seen(bulb, &state, ctx);
}

async fn run(lifx: &Lifx, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Restored readings are stale until a bulb answers in this run.
    let known: BTreeSet<DeviceId> = ctx.known_devices().into_iter().collect();
    for id in &known {
        ctx.set_availability(id, false);
    }
    let mut link = Link::open().await?;
    let mut bulbs = Bulbs {
        by_id: HashMap::new(),
        jobs: VecDeque::new(),
        full: false,
        max: lifx.max_bulbs,
        failed: HashMap::new(),
        retry_after: lifx.search_again,
    };
    bulbs.search(lifx, &mut link).await;
    let mut tick = tokio::time::interval(lifx.poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let after = |bulbs: &Bulbs| {
        Instant::now()
            + if bulbs.missing(lifx, &known) {
                lifx.search_missing
            } else {
                lifx.search_again
            }
    };
    let mut next_search = after(&bulbs);
    let mut announced = false;
    loop {
        // The first search's bulbs are met before saying ready or waiting.
        if !announced && bulbs.jobs.iter().all(|j| !matches!(j, Job::Meet(..))) {
            bulbs.status(ctx);
            announced = true;
        }
        tokio::select! {
            // Orders first, then the search when it is due, then the poll
            // (it only queues readings), then the queued work one piece at
            // a time: an order waits at most for the piece under way, and a
            // queue that never empties cannot hold the search back.
            biased;
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = match bulbs.by_id.get_mut(&command.device.id) {
                    Some(bulb) => bulb.apply(&mut link, &command.key, &command.value, lifx.transition).await,
                    None => Err(moli_i18n::tr!("pilotes.lifx.injoignable")),
                };
                let result = result.map(|values| {
                    for (key, value) in values {
                        ctx.set_state(&command.device.id, key, value);
                    }
                });
                command.reply(result);
            }
            () = tokio::time::sleep_until(next_search) => {
                bulbs.search(lifx, &mut link).await;
                if announced {
                    bulbs.status(ctx);
                }
                next_search = after(&bulbs);
            }
            _ = tick.tick() => bulbs.poll(),
            Some(job) = async { bulbs.jobs.pop_front() }, if !bulbs.jobs.is_empty() => {
                if bulbs.work(job, &mut link, ctx).await {
                    next_search = next_search.min(Instant::now() + lifx.search_missing);
                }
                if announced {
                    bulbs.status(ctx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

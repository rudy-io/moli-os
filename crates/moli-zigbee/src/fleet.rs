//! The devices on the network, alive: who is at which short address, their
//! interviews, their reports and their orders, whether they still answer.
//!
//! The driver's loop stays the only reader of the callbacks; anything that
//! waits for a device (an interview, a delivery, a read) runs in the
//! [`JoinSet`] and comes back as a [`Done`], so that a silent device never
//! holds up the others. Dropped with the driver: nothing outlives a run.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::time::Duration;

use moli_core::{DeviceId, Value};
use moli_runtime::{CommandRequest, DriverCtx};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::aps::{Aps, ApsFrame, Delivery, Incoming, SENT_TIMEOUT};
use crate::devices::{self, ColorState, Refused};
use crate::exchange::{Expected, Link, zcl_frame};
use crate::interview::{Failed, ZONE_ID};
use crate::store::{DeviceRecord, Store};
use crate::{interview, network, zcl};

/// A device that just joined is busy with its keys: its interview waits.
const INTERVIEW_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_secs(2)
};
/// How often the paced chores go: one device read every half second, so that
/// they never crowd the network (and orders always find room).
pub const CHORE_PACE: Duration = Duration::from_millis(500);
/// How often the devices' silences are looked at.
pub const UPKEEP_PERIOD: Duration = Duration::from_secs(60);
/// Zigbee2MQTT's values: a listening device silent this long is read…
const LISTENING_SILENCE: Duration = Duration::from_secs(10 * 60);
/// … and a sleepy one silent this long is taken as gone.
const SLEEPY_SILENCE: Duration = Duration::from_secs(25 * 3600);
/// A failed interview, tried again without any sign from the device.
const INTERVIEW_RETRY: Duration = Duration::from_secs(10 * 60);
/// A failed interview, tried again when the device speaks: a sleepy one only
/// listens then.
const INTERVIEW_RETRY_ON_MESSAGE: Duration = Duration::from_secs(60);
/// Bindings and reports that were not answered, tried again.
const CONFIGURE_RETRY: Duration = Duration::from_secs(3600);
/// An order is answered this long before the hub stops waiting: whoever
/// asked hears what happened, not a timeout.
const ORDER_MARGIN: Duration = Duration::from_millis(300);
/// How long a sleepy device gets to take an order and answer it: nobody waits
/// for it (it gets it at its next wake), its outcome only corrects the values.
const SLEEPY_ORDER: Duration = SENT_TIMEOUT.saturating_add(Duration::from_secs(4));

/// Work that comes back to the loop.
#[derive(Debug)]
pub enum Done {
    Nothing,
    Interview {
        ieee: String,
        result: Result<DeviceRecord, Box<Failed>>,
    },
    Configured {
        ieee: String,
        answered: bool,
    },
    Read {
        ieee: String,
        answered: bool,
        asked_at: Instant,
    },
    Order {
        /// `None` once answered: a sleepy device's.
        order: Option<CommandRequest>,
        ieee: String,
        outcome: OrderOutcome,
        effects: Vec<(String, Value)>,
    },
}

/// How an order ended.
#[derive(Debug, PartialEq, Eq)]
pub enum OrderOutcome {
    Taken,
    /// It never reached the device.
    Lost(String),
    /// The device answered with this ZCL status.
    Refused(u8),
}

/// A paced chore.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chore {
    /// Every current value of a device.
    Read(String),
    /// One attribute: does it still answer?
    Ping(String),
    Configure(String),
}

/// What is known of a device while Moli runs; nothing here survives it.
#[derive(Debug)]
struct Tracked {
    last_seen: Instant,
    online: Option<bool>,
    colors: HashMap<u8, ColorState>,
    interview_tried: Option<Instant>,
    configure_tried: Option<Instant>,
    ping_tried: Option<Instant>,
}

impl Tracked {
    fn new(now: Instant) -> Self {
        Self {
            last_seen: now,
            online: None,
            colors: HashMap::new(),
            interview_tried: None,
            configure_tried: None,
            ping_tried: None,
        }
    }
}

/// An order ready to leave: what the NCP is handed, and the answer awaited.
#[derive(Debug)]
pub struct Prepared {
    nwk: u16,
    aps_frame: ApsFrame,
    frame: Vec<u8>,
    expected: Expected,
    ieee: String,
    listening: bool,
    endpoint: u8,
    color_mode: Option<u64>,
    effects: Vec<(String, Value)>,
}

impl Prepared {
    /// Hands it to the NCP.
    ///
    /// # Errors
    ///
    /// The link failing, or the NCP refusing it.
    pub async fn submit(&self, aps: &Aps) -> anyhow::Result<Delivery> {
        aps.submit(self.nwk, &self.aps_frame, aps.next_tag(), &self.frame)
            .await
    }
}

/// The devices of the running coordinator.
#[derive(Debug)]
pub struct Fleet {
    link: Link,
    /// The coordinator's address as it travels, once the network is ours.
    coordinator: Option<[u8; 8]>,
    work: JoinSet<Done>,
    by_nwk: HashMap<u16, String>,
    tracked: HashMap<String, Tracked>,
    interviews: VecDeque<String>,
    interviewing: Option<String>,
    chores: VecDeque<Chore>,
    /// Whose interview failed last: what the driver's warning names.
    failing: BTreeSet<String>,
}

impl Fleet {
    #[must_use]
    pub fn new(aps: Aps) -> Self {
        Self {
            link: Link::new(aps),
            coordinator: None,
            work: JoinSet::new(),
            by_nwk: HashMap::new(),
            tracked: HashMap::new(),
            interviews: VecDeque::new(),
            interviewing: None,
            chores: VecDeque::new(),
            failing: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn aps(&self) -> &Aps {
        &self.link.aps
    }

    /// The network is ours: its known devices published and given a chance
    /// to say how they are; what was not interviewed yet is. Their silences
    /// go on from before the restart (`unix_now`: the wall clock).
    pub fn start(&mut self, ctx: &DriverCtx, store: &Store, unix_now: u64) {
        self.coordinator = store
            .network
            .as_ref()
            .and_then(|network| network::eui64(&network.coordinator_ieee));
        let now = Instant::now();
        for (ieee, record) in &store.devices {
            let id = ctx.device_id(ieee);
            if let Some(nwk) = record.nwk {
                self.by_nwk.insert(nwk, ieee.clone());
            }
            let silence = record.last_seen.map_or(Duration::ZERO, |seen| {
                Duration::from_secs(unix_now.saturating_sub(seen))
            });
            let mut tracked = Tracked::new(now.checked_sub(silence).unwrap_or(now));
            let settled = record.interviewed && !record.left && record.listening();
            if settled && !record.configured {
                tracked.configure_tried = Some(now);
            }
            self.tracked.insert(ieee.clone(), tracked);
            if record.interviewed {
                ctx.upsert_device(devices::device(ctx.instance(), &id, record));
                // Nothing says otherwise yet; a listening device that does
                // not answer its read goes offline then.
                let gone = record.left || (!record.listening() && silence >= SLEEPY_SILENCE);
                self.set_online(ctx, ieee, !gone);
                if settled {
                    self.chores.push_back(Chore::Read(ieee.clone()));
                    if !record.configured {
                        self.chores.push_back(Chore::Configure(ieee.clone()));
                    }
                }
            } else {
                ctx.upsert_device(devices::pending_device(ctx.instance(), &id, record));
                if record.nwk.is_some() {
                    self.queue_interview(ieee);
                }
            }
        }
        self.next_interview(store);
    }

    fn id_of(ctx: &DriverCtx, ieee: &str) -> DeviceId {
        ctx.device_id(ieee)
    }

    fn set_online(&mut self, ctx: &DriverCtx, ieee: &str, online: bool) {
        if let Some(tracked) = self.tracked.get_mut(ieee)
            && tracked.online != Some(online)
        {
            tracked.online = Some(online);
            ctx.set_availability(&Self::id_of(ctx, ieee), online);
        }
    }

    fn queued(&self, ieee: &str) -> bool {
        self.interviewing.as_deref() == Some(ieee) || self.interviews.iter().any(|q| q == ieee)
    }

    fn queue_interview(&mut self, ieee: &str) {
        if !self.queued(ieee) {
            self.interviews.push_back(ieee.to_owned());
        }
    }

    /// One interview at a time: the next one, if none runs.
    fn next_interview(&mut self, store: &Store) {
        if self.interviewing.is_some() {
            return;
        }
        let Some(coordinator) = self.coordinator else {
            return;
        };
        while let Some(ieee) = self.interviews.pop_front() {
            let Some((record, nwk)) = store
                .devices
                .get(&ieee)
                .and_then(|record| record.nwk.map(|nwk| (record.clone(), nwk)))
            else {
                continue;
            };
            let link = self.link.clone();
            self.interviewing = Some(ieee.clone());
            // Its first one only: a sleepy device trying again is awake now,
            // and not for long.
            let first = self
                .tracked
                .get_mut(&ieee)
                .is_some_and(|tracked| tracked.interview_tried.replace(Instant::now()).is_none());
            self.work.spawn(async move {
                if first {
                    tokio::time::sleep(INTERVIEW_DELAY).await;
                }
                let result = interview::interview(&link, nwk, record, coordinator).await;
                Done::Interview { ieee, result }
            });
            return;
        }
    }

    /// A device joined or announced itself at `nwk` (already in `store`);
    /// `reset`: what was known of it was just forgotten.
    pub fn arrived(&mut self, ctx: &DriverCtx, store: &Store, ieee: &str, nwk: u16, reset: bool) {
        self.by_nwk
            .retain(|&known, who| known == nwk || who != ieee);
        self.by_nwk.insert(nwk, ieee.to_owned());
        let now = Instant::now();
        let tracked = self
            .tracked
            .entry(ieee.to_owned())
            .or_insert_with(|| Tracked::new(now));
        tracked.last_seen = now;
        if reset {
            *tracked = Tracked::new(now);
        }
        let Some(record) = store.devices.get(ieee) else {
            return;
        };
        if record.interviewed {
            self.set_online(ctx, ieee, true);
            if record.listening() {
                self.read_again(ieee);
            }
        } else {
            if reset || !self.queued(ieee) {
                let id = Self::id_of(ctx, ieee);
                ctx.upsert_device(devices::pending_device(ctx.instance(), &id, record));
                self.set_online(ctx, ieee, true);
            }
            self.queue_interview(ieee);
            self.next_interview(store);
        }
    }

    /// A device left the network: kept if it was interviewed (it may come
    /// back, its name and its automations with it), shown offline. One that
    /// never was is forgotten: it had no controls, and would be tried again,
    /// and warned about, for ever.
    pub fn left(&mut self, ctx: &DriverCtx, store: &mut Store, path: &Path, ieee: &str) {
        let Some(record) = store.devices.get_mut(ieee) else {
            return;
        };
        if record.interviewed {
            record.left = true;
            save(store, path);
            self.set_online(ctx, ieee, false);
            return;
        }
        store.devices.remove(ieee);
        save(store, path);
        ctx.remove_device(&Self::id_of(ctx, ieee));
        self.tracked.remove(ieee);
        self.by_nwk.retain(|_, who| who != ieee);
        self.interviews.retain(|queued| queued != ieee);
        self.failing.remove(ieee);
        self.say_failures(ctx);
    }

    /// The network is back: what happened meanwhile is unknown.
    pub fn read_everyone(&mut self, store: &Store) {
        for (ieee, record) in &store.devices {
            if record.interviewed && record.listening() && !record.left {
                self.read_again(ieee);
            }
        }
    }

    /// Writes down when each device last spoke, for after a restart.
    /// Whether anything changed (the store then needs saving).
    pub fn remember_seen(&self, store: &mut Store, unix_now: u64) -> bool {
        let now = Instant::now();
        let mut changed = false;
        for (ieee, tracked) in &self.tracked {
            if let Some(record) = store.devices.get_mut(ieee) {
                let silence = now.duration_since(tracked.last_seen).as_secs();
                let seen = Some(unix_now.saturating_sub(silence));
                changed |= record.last_seen != seen;
                record.last_seen = seen;
            }
        }
        changed
    }

    /// An incoming message: an answer to whoever awaits it, and what it says
    /// of its device. Whatever it is, the device is alive.
    pub fn on_message(&mut self, ctx: &DriverCtx, store: &Store, message: &Incoming) {
        self.link.exchanges.deliver(message);
        let Some(ieee) = self.by_nwk.get(&message.nwk).cloned() else {
            tracing::debug!(
                nwk = format_args!("{:#06x}", message.nwk),
                "a message from a device Moli does not know at this address"
            );
            return;
        };
        let now = Instant::now();
        let tracked = self
            .tracked
            .entry(ieee.clone())
            .or_insert_with(|| Tracked::new(now));
        tracked.last_seen = now;
        self.set_online(ctx, &ieee, true);
        ctx.set_state(
            &Self::id_of(ctx, &ieee),
            "linkquality",
            Value::Int(message.lqi.into()),
        );
        let Some(record) = store.devices.get(&ieee) else {
            return;
        };
        if matches!(message.frame.profile, zcl::HA_PROFILE | zcl::ZLL_PROFILE) {
            self.on_zcl(ctx, record, &ieee, message);
        }
        self.retry_on_message(store, &ieee, now);
    }

    /// A device that speaks and was not settled yet: now is a good time.
    fn retry_on_message(&mut self, store: &Store, ieee: &str, now: Instant) {
        let Some(record) = store.devices.get(ieee) else {
            return;
        };
        let Some(tracked) = self.tracked.get_mut(ieee) else {
            return;
        };
        let due = |tried: Option<Instant>, after| {
            tried.is_none_or(|tried| now.duration_since(tried) >= after)
        };
        if record.interviewed {
            if !record.configured && due(tracked.configure_tried, CONFIGURE_RETRY) {
                tracked.configure_tried = Some(now);
                self.spawn_configure(store, ieee);
            }
        } else if due(tracked.interview_tried, INTERVIEW_RETRY_ON_MESSAGE) {
            self.queue_interview(ieee);
            self.next_interview(store);
        }
    }

    fn on_zcl(&mut self, ctx: &DriverCtx, record: &DeviceRecord, ieee: &str, message: &Incoming) {
        let (endpoint, cluster) = (message.frame.src_ep, message.frame.cluster);
        let Ok((header, payload)) = zcl::Header::parse(&message.payload) else {
            tracing::debug!(%ieee, len = message.payload.len(), "an unreadable ZCL frame");
            return;
        };
        if header.manufacturer.is_some() {
            // A brand's own language: not the generic devices'.
            tracing::debug!(
                %ieee,
                cluster = format_args!("{cluster:#06x}"),
                command = header.command,
                "a manufacturer's frame"
            );
            return;
        }
        let id = Self::id_of(ctx, ieee);
        let mut answer_default = false;
        if header.specific {
            match (cluster, header.command) {
                (zcl::IAS_ZONE, zcl::ZONE_ENROLL_REQUEST) => {
                    tracing::info!(%ieee, zone = ?zcl::enroll_request(payload), "a sensor enrolls");
                    let frame = zcl::enroll_response(ZONE_ID).frame(header.seq);
                    self.send(message.nwk, endpoint, cluster, frame);
                }
                (zcl::IAS_ZONE, zcl::ZONE_STATUS_CHANGE) => {
                    if let Some(status) = zcl::zone_status_change(payload) {
                        for (key, value) in devices::zone_points(record, endpoint, status) {
                            ctx.set_state(&id, &key, value);
                        }
                    }
                    answer_default = true;
                }
                _ => {
                    tracing::debug!(
                        %ieee,
                        cluster = format_args!("{cluster:#06x}"),
                        command = header.command,
                        "a command from a device"
                    );
                    answer_default = true;
                }
            }
        } else {
            let attributes = match header.command {
                zcl::REPORT_ATTRIBUTES => {
                    answer_default = true;
                    zcl::report_attributes(payload)
                }
                zcl::READ_RESPONSE => zcl::read_response(payload).0,
                _ => Vec::new(),
            };
            if let Some(tracked) = self.tracked.get_mut(ieee) {
                let color = tracked.colors.entry(endpoint).or_default();
                for attribute in &attributes {
                    for (key, value) in devices::decode(record, color, endpoint, cluster, attribute)
                    {
                        ctx.set_state(&id, &key, value);
                    }
                }
            }
        }
        if answer_default && !header.no_default_response && !message.broadcast {
            let frame = zcl::default_response_frame(&header, zcl::STATUS_SUCCESS);
            self.send(message.nwk, endpoint, cluster, frame);
        }
    }

    /// A frame that needs no answer, without waiting for it to arrive.
    fn send(&mut self, nwk: u16, endpoint: u8, cluster: u16, frame: Vec<u8>) {
        let aps = self.link.aps.clone();
        self.work.spawn(async move {
            if let Err(error) = aps
                .send_unicast(nwk, &zcl_frame(endpoint, cluster), &frame)
                .await
            {
                tracing::debug!(%error, "a response did not reach its device");
            }
            Done::Nothing
        });
    }

    fn spawn_configure(&mut self, store: &Store, ieee: &str) {
        let (Some(coordinator), Some(record)) = (self.coordinator, store.devices.get(ieee)) else {
            return;
        };
        let Some(nwk) = record.nwk else {
            return;
        };
        let link = self.link.clone();
        let record = record.clone();
        let ieee = ieee.to_owned();
        self.work.spawn(async move {
            let answered = interview::configure(&link, nwk, &record, coordinator).await;
            Done::Configured { ieee, answered }
        });
    }

    /// Whether work is out: `next_done` has something to wait for.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.work.is_empty()
    }

    /// The next piece of work that ended. Cancel safe.
    pub async fn next_done(&mut self) -> Option<Done> {
        match self.work.join_next().await? {
            Ok(done) => Some(done),
            Err(error) => {
                tracing::error!(%error, "a zigbee task ended badly");
                self.interviewing = None;
                Some(Done::Nothing)
            }
        }
    }

    /// Work that ended, in the loop: the store and the hub told.
    pub fn finished(&mut self, ctx: &DriverCtx, store: &mut Store, path: &Path, done: Done) {
        match done {
            Done::Nothing => {}
            Done::Interview { ieee, result } => self.interviewed(ctx, store, path, ieee, result),
            Done::Configured { ieee, answered } => {
                if let Some(record) = store.devices.get_mut(&ieee)
                    && record.configured != answered
                {
                    record.configured = answered;
                    save(store, path);
                }
            }
            Done::Read {
                ieee,
                answered,
                asked_at,
            } => {
                // A sleepy device not answering is asleep: only its long
                // silence says something. One that spoke meanwhile is there.
                let listening = store
                    .devices
                    .get(&ieee)
                    .is_some_and(DeviceRecord::listening);
                let spoke = self
                    .tracked
                    .get(&ieee)
                    .is_some_and(|tracked| tracked.last_seen >= asked_at);
                if !answered && listening && !spoke {
                    tracing::info!(%ieee, "a device does not answer");
                    self.set_online(ctx, &ieee, false);
                }
            }
            Done::Order {
                order,
                ieee,
                outcome,
                effects,
            } => self.ordered(ctx, order, &ieee, outcome, effects),
        }
    }

    fn interviewed(
        &mut self,
        ctx: &DriverCtx,
        store: &mut Store,
        path: &Path,
        ieee: String,
        result: Result<DeviceRecord, Box<Failed>>,
    ) {
        self.interviewing = None;
        // It may have moved meanwhile, or left.
        let Some(known_nwk) = store.devices.get(&ieee).map(|known| known.nwk) else {
            tracing::debug!(device = %ieee, "an interview ended for a device that left");
            self.next_interview(store);
            return;
        };
        match result {
            Ok(mut record) => {
                tracing::info!(
                    device = %ieee,
                    manufacturer = ?record.manufacturer,
                    model = ?record.model,
                    endpoints = record.endpoints.len(),
                    "a device was interviewed"
                );
                record.nwk = known_nwk.or(record.nwk);
                let id = Self::id_of(ctx, &ieee);
                ctx.upsert_device(devices::device(ctx.instance(), &id, &record));
                self.set_online(ctx, &ieee, true);
                store.devices.insert(ieee.clone(), record);
                save(store, path);
                self.failing.remove(&ieee);
                self.say_failures(ctx);
                self.chores.push_back(Chore::Read(ieee.clone()));
                if let Some(tracked) = self.tracked.get_mut(&ieee) {
                    tracked.configure_tried = Some(Instant::now());
                }
                self.spawn_configure(store, &ieee);
            }
            Err(failed) => {
                let Failed { mut record, error } = *failed;
                tracing::warn!(device = %ieee, %error, "an interview failed: tried again later");
                // The next try goes on from what it learnt.
                record.nwk = known_nwk.or(record.nwk);
                store.devices.insert(ieee.clone(), record);
                save(store, path);
                self.failing.insert(ieee);
                self.say_failures(ctx);
            }
        }
        self.next_interview(store);
    }

    fn say_failures(&self, ctx: &DriverCtx) {
        match self.failing.iter().next() {
            Some(device) => ctx.warn(moli_i18n::tr!(
                "pilotes.zigbee.interview_echec",
                device = device.as_str()
            )),
            None => ctx.clear_warning(),
        }
    }

    /// An order to a device: what to send, or why it cannot be.
    ///
    /// # Errors
    ///
    /// The device or the point is unknown, or the value cannot be sent: the
    /// reason, for whoever asked.
    pub fn prepare_order(&self, store: &Store, order: &CommandRequest) -> Result<Prepared, String> {
        let unknown = || moli_i18n::tr!("pilotes.zigbee.ordre_appareil_inconnu");
        let ieee = order
            .device
            .id
            .as_str()
            .split_once(':')
            .map_or(order.device.id.as_str(), |(_, native)| native);
        let record = store.devices.get(ieee).ok_or_else(unknown)?;
        let nwk = record.nwk.ok_or_else(unknown)?;
        let command =
            devices::command(record, &order.key, &order.value).map_err(
                |refused| match refused {
                    Refused::UnknownPoint => moli_i18n::tr!("pilotes.zigbee.ordre_point_inconnu"),
                    Refused::BadValue => moli_i18n::tr!("pilotes.zigbee.ordre_valeur"),
                },
            )?;
        let (expected, frame) = self
            .link
            .zcl_request(nwk, command.cluster, &command.request);
        Ok(Prepared {
            nwk,
            aps_frame: zcl_frame(command.endpoint, command.cluster),
            frame,
            expected,
            ieee: ieee.to_owned(),
            listening: record.listening(),
            endpoint: command.endpoint,
            color_mode: command.color_mode,
            effects: command.effects,
        })
    }

    /// The NCP took the order: how it ends comes back as a [`Done`], while
    /// the loop goes on.
    pub fn follow_order(&mut self, order: CommandRequest, prepared: Prepared, delivery: Delivery) {
        let Prepared {
            expected,
            ieee,
            listening,
            endpoint,
            color_mode,
            effects,
            ..
        } = prepared;
        // At once: its reports can come before its answer.
        if let (Some(mode), Some(tracked)) = (color_mode, self.tracked.get_mut(&ieee)) {
            tracked.colors.entry(endpoint).or_default().set_mode(mode);
        }
        // A sleepy device gets it at its next wake, maybe long after the hub
        // stopped waiting: handed over is all that can be said in time.
        let (order, budget) = if listening {
            let left = order
                .deadline
                .saturating_duration_since(std::time::Instant::now())
                .saturating_sub(ORDER_MARGIN);
            (Some(order), left)
        } else {
            order.reply(Ok(()));
            (None, SLEEPY_ORDER)
        };
        self.work.spawn(async move {
            let outcome = order_outcome(delivery, expected, budget).await;
            Done::Order {
                order,
                ieee,
                outcome,
                effects,
            }
        });
    }

    fn ordered(
        &mut self,
        ctx: &DriverCtx,
        order: Option<CommandRequest>,
        ieee: &str,
        outcome: OrderOutcome,
        effects: Vec<(String, Value)>,
    ) {
        let reply = match outcome {
            OrderOutcome::Taken => {
                let id = Self::id_of(ctx, ieee);
                for (key, value) in effects {
                    ctx.set_state(&id, &key, value);
                }
                Ok(())
            }
            OrderOutcome::Refused(status) => {
                tracing::info!(%ieee, status, "a device refused an order");
                // What it really shows, now that a colour mode may be wrong.
                self.read_again(ieee);
                Err(moli_i18n::tr!(
                    "pilotes.zigbee.ordre_refuse",
                    status = format!("{status:#04x}")
                ))
            }
            OrderOutcome::Lost(error) => {
                tracing::info!(%ieee, %error, "an order did not arrive");
                // Is it still there, and how? Its answers will say.
                self.read_again(ieee);
                Err(moli_i18n::tr!("pilotes.zigbee.ordre_echec", error = error))
            }
        };
        if let Some(order) = order {
            order.reply(reply);
        }
    }

    fn read_again(&mut self, ieee: &str) {
        let read = Chore::Read(ieee.to_owned());
        if !self.chores.contains(&read) {
            self.chores.push_back(read);
        }
    }

    /// Whether a paced chore waits.
    #[must_use]
    pub fn has_chores(&self) -> bool {
        !self.chores.is_empty()
    }

    /// The next paced chore: its answers come back as messages like any
    /// others, and say whether the device is there.
    pub fn next_chore(&mut self, store: &Store) {
        let Some(chore) = self.chores.pop_front() else {
            return;
        };
        let (ieee, ping) = match chore {
            Chore::Configure(ieee) => {
                self.spawn_configure(store, &ieee);
                return;
            }
            Chore::Read(ieee) => (ieee, false),
            Chore::Ping(ieee) => (ieee, true),
        };
        let Some(record) = store.devices.get(&ieee) else {
            return;
        };
        let Some(nwk) = record.nwk else {
            return;
        };
        let plans = if ping {
            devices::ping(record).into_iter().collect()
        } else {
            devices::reads(record)
        };
        let link = self.link.clone();
        let asked_at = Instant::now();
        self.work.spawn(async move {
            let mut answered = plans.is_empty();
            for plan in plans {
                match link
                    .read(nwk, plan.endpoint, plan.cluster, &plan.attributes)
                    .await
                {
                    Ok(_) => answered = true,
                    Err(error) => tracing::debug!(%ieee, %error, "a read was not answered"),
                }
            }
            Done::Read {
                ieee,
                answered,
                asked_at,
            }
        });
    }

    /// Who has been silent too long: a listening device is read (it goes
    /// offline if it does not answer), a sleepy one taken as gone; an
    /// interview that failed is tried again.
    pub fn upkeep(&mut self, ctx: &DriverCtx, store: &Store) {
        let now = Instant::now();
        let mut offline = Vec::new();
        let mut pings = Vec::new();
        for (ieee, record) in &store.devices {
            let Some(tracked) = self.tracked.get(ieee) else {
                continue;
            };
            if record.interviewed {
                let silence = now.duration_since(tracked.last_seen);
                if record.listening() {
                    // One read per silence, not one per look: a lamp cut off
                    // at the wall must not cost the mesh a search a minute.
                    let due = tracked
                        .ping_tried
                        .is_none_or(|tried| now.duration_since(tried) >= LISTENING_SILENCE);
                    if silence >= LISTENING_SILENCE && due && !record.left {
                        pings.push(ieee.clone());
                    }
                } else if silence >= SLEEPY_SILENCE {
                    offline.push(ieee.clone());
                }
            } else if record.nwk.is_some()
                && !self.queued(ieee)
                && tracked
                    .interview_tried
                    .is_none_or(|tried| now.duration_since(tried) >= INTERVIEW_RETRY)
            {
                self.interviews.push_back(ieee.clone());
            }
        }
        for ieee in offline {
            self.set_online(ctx, &ieee, false);
        }
        for ieee in pings {
            if let Some(tracked) = self.tracked.get_mut(&ieee) {
                tracked.ping_tried = Some(now);
            }
            self.chores.push_back(Chore::Ping(ieee));
        }
        self.next_interview(store);
    }
}

/// How an order the NCP took ends, within `budget`. An answer already there
/// counts even if the report of its delivery failed or came late: it beat it.
/// Acknowledged by the device without a word from its application, it has the
/// order: some never answer.
async fn order_outcome(delivery: Delivery, expected: Expected, budget: Duration) -> OrderOutcome {
    let started = Instant::now();
    match tokio::time::timeout(budget, delivery.wait()).await {
        Ok(Ok(())) => match expected
            .wait(budget.saturating_sub(started.elapsed()))
            .await
        {
            Ok(frame) => answer_outcome(&frame),
            Err(_) => OrderOutcome::Taken,
        },
        failed => match expected.wait(Duration::ZERO).await {
            Ok(frame) => answer_outcome(&frame),
            Err(_) => OrderOutcome::Lost(match failed {
                Ok(Err(error)) => error.to_string(),
                _ => "no acknowledgement in time".to_owned(),
            }),
        },
    }
}

/// What a device answered to an order.
fn answer_outcome(frame: &[u8]) -> OrderOutcome {
    let Ok((header, payload)) = zcl::Header::parse(frame) else {
        return OrderOutcome::Taken;
    };
    match (header.command, payload) {
        (zcl::DEFAULT_RESPONSE, [_, status, ..]) | (zcl::WRITE_RESPONSE, [status, ..])
            if *status != zcl::STATUS_SUCCESS =>
        {
            OrderOutcome::Refused(*status)
        }
        _ => OrderOutcome::Taken,
    }
}

/// Kept in memory when it fails: the next change saves it too.
fn save(store: &Store, path: &Path) {
    if let Err(error) = store.save(path) {
        tracing::error!(%error, "the zigbee store could not be saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_to_orders() {
        // Success, then refusals (UNSUPPORTED_CLUSTER_COMMAND, READ_ONLY).
        assert_eq!(
            answer_outcome(&[0x18, 1, zcl::DEFAULT_RESPONSE, 0x01, 0x00]),
            OrderOutcome::Taken
        );
        assert_eq!(
            answer_outcome(&[0x18, 1, zcl::DEFAULT_RESPONSE, 0x01, 0x81]),
            OrderOutcome::Refused(0x81)
        );
        assert_eq!(
            answer_outcome(&[0x18, 1, zcl::WRITE_RESPONSE, 0x88, 0x12, 0x00]),
            OrderOutcome::Refused(0x88)
        );
        assert_eq!(
            answer_outcome(&[0x18, 1, zcl::WRITE_RESPONSE, 0x00]),
            OrderOutcome::Taken
        );
        // What cannot be read says nothing against it.
        assert_eq!(answer_outcome(&[0x18]), OrderOutcome::Taken);
    }
}

//! Native Zigbee driver: talks EZSP to a Silicon Labs coordinator dongle
//! (serial or TCP), no Zigbee2MQTT and no broker.
//!
//! Dormant: nothing runs unless `[[driver]] kind = "zigbee"` is in `moli.toml`.

pub mod aps;
pub mod ash;
pub mod backup;
pub mod coordinator;
pub mod ezsp;
pub mod network;
#[cfg(test)]
mod sim;
pub mod store;
pub mod transport;
pub mod zdo;

use std::path::{Path, PathBuf};

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::{Deserialize, Deserializer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::aps::Aps;
use crate::coordinator::{Event, Init, Network};
use crate::ezsp::{Callback, Ezsp, EzspFailure, Version};
use crate::store::Store;

/// The wall clock, in unix seconds ([`backup::unix_now`]; fixed in tests):
/// when a frame counter is recorded, and how old it is at a restore.
type Clock = fn() -> u64;

/// How long a clean stop may take (joining closed, the counter recorded a
/// last time) before Moli goes on stopping without it.
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Serial device (`/dev/ttyUSB0`) or a network adapter (`tcp://host:6638`).
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default)]
    pub flow: Flow,
    /// Zigbee channel (11 to 26) of a new network; 15 if unset.
    #[serde(default, deserialize_with = "zigbee_channel")]
    pub channel: Option<u8>,
}

fn default_baud() -> u32 {
    115_200
}

/// A channel outside 11..=26 stops Moli at startup, naming it.
fn zigbee_channel<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u8>, D::Error> {
    match Option::<u8>::deserialize(deserializer)? {
        Some(channel) if !network::CHANNELS.contains(&channel) => Err(serde::de::Error::custom(
            format!("channel {channel} is not a Zigbee channel (11 to 26)"),
        )),
        channel => Ok(channel),
    }
}

/// Serial flow control.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Flow {
    /// XON/XOFF: what the Sonoff ZBDongle-E ships with.
    #[default]
    Software,
    Hardware,
    None,
}

#[derive(Debug)]
pub struct Zigbee {
    config: Config,
    /// Where the network state (devices, counters) is kept; never the key.
    data_dir: PathBuf,
}

impl Zigbee {
    #[must_use]
    pub fn new(config: Config, data_dir: &Path) -> Self {
        Self {
            config,
            data_dir: data_dir.to_path_buf(),
        }
    }
}

impl Driver for Zigbee {
    fn kind(&self) -> &'static str {
        "zigbee"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, &self.data_dir, ctx))
    }
}

/// Open the port, then [`run_on`] it.
async fn run(config: &Config, data_dir: &Path, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    tracing::debug!(
        port = %config.port,
        baud = config.baud,
        flow = ?config.flow,
        channel = ?config.channel,
        data_dir = %data_dir.display(),
        "zigbee driver started"
    );
    ctx.wait_for(moli_i18n::tr!("pilotes.zigbee.demarrage"));
    let io = refusing_orders(ctx, transport::open(config)).await?;
    run_on(config, data_dir, ctx, io, backup::unix_now).await
}

/// What the startup brings back.
struct Started {
    ezsp: Ezsp,
    version: Version,
    callbacks: mpsc::Receiver<Callback>,
    init: Init,
}

/// Everything after the port: connect, configure and init the NCP, publish
/// the coordinator, resume or form the network (or wait for a person), then
/// follow the link until it ends (an error: the supervisor restarts the
/// driver) or Moli stops.
///
/// Orders are refused until the coordinator is ready. A firmware too old
/// for EZSP 13, a missing vault or a network to restore wait for a person
/// (no restart loop: only that person helps).
async fn run_on<T>(
    config: &Config,
    data_dir: &Path,
    ctx: &mut DriverCtx,
    io: T,
    clock: Clock,
) -> anyhow::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let store_path = store::path(data_dir, ctx.instance().as_str());
    let mut store = Store::load(&store_path)?;
    let started = match refusing_orders(ctx, start(io)).await {
        Ok(started) => started,
        Err(error) => {
            if let Some(&EzspFailure::FirmwareTooOld { protocol }) = error.downcast_ref() {
                tracing::warn!(protocol, "NCP firmware too old");
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.zigbee.firmware_ancien",
                    version = protocol
                ));
                while let Some(order) = ctx.next_command().await {
                    refuse(order);
                }
                return Ok(());
            }
            return Err(error);
        }
    };
    let Started {
        ezsp,
        version,
        mut callbacks,
        init,
    } = started;
    let protocol = version.protocol;
    let coordinator = ctx.device_id("coordinator");
    ctx.upsert_device(coordinator_device(ctx, &coordinator));
    ctx.set_state(
        &coordinator,
        "firmware",
        Value::Text(version.display().into()),
    );
    ctx.set_state(&coordinator, "ezsp_version", Value::Int(protocol.into()));
    let outcome = if let Some(wait) = interrupted_import(&store, init) {
        wait
    } else {
        match init {
            Init::Resumed => {
                let has_key = ctx.secret(network::NETWORK_KEY).is_some();
                refusing_orders(
                    ctx,
                    resume(
                        &ezsp,
                        &mut callbacks,
                        protocol,
                        has_key,
                        &mut store,
                        &store_path,
                        clock,
                    ),
                )
                .await?
            }
            Init::NotJoined => {
                let dongle = Dongle {
                    ezsp: &ezsp,
                    protocol,
                    store_path: &store_path,
                    clock,
                };
                first_network(config, ctx, &dongle, &mut callbacks, &mut store).await?
            }
        }
    };
    let network = match outcome {
        Outcome::Ready(network) => network,
        Outcome::Wait { reason, network } => {
            ctx.set_state(&coordinator, "network", network_value(network));
            return park(ctx, &ezsp, &mut callbacks, reason).await;
        }
    };
    ctx.set_state(&coordinator, "network", network_value(network));
    ctx.set_state(&coordinator, "permit_join", Value::Bool(false));
    tracing::info!(
        firmware = %version.display(),
        protocol,
        network = network.as_str(),
        "zigbee coordinator ready"
    );
    ctx.ready();
    let live = Live {
        coordinator,
        protocol,
        network,
        // Up at startup: formed, restored, or resumed and checked to be
        // the store's own network. A resumed network that stayed down was
        // not checked yet: it is when it comes up.
        checked: network == Network::Up,
        check_due: false,
        permit_until: None,
        store: &mut store,
        store_path: &store_path,
        clock,
    };
    serve(ctx, &ezsp, &mut callbacks, live).await
}

/// The coordinator ready: follow callbacks and orders until the link ends
/// (an error: the supervisor restarts the driver) or Moli stops (cleanly:
/// [`Live::stop`]).
async fn serve(
    ctx: &mut DriverCtx,
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    mut live: Live<'_>,
) -> anyhow::Result<()> {
    let aps = Aps::new(ezsp.clone(), live.protocol);
    // The first record was made at startup: the next one an hour later.
    let mut record_at = tokio::time::interval_at(
        tokio::time::Instant::now() + backup::COUNTER_RECORD_PERIOD,
        backup::COUNTER_RECORD_PERIOD,
    );
    record_at.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let close_at = live.permit_until;
        tokio::select! {
            callback = callbacks.recv() => match callback {
                // Read continuously: the client drops what is not read in time.
                Some(callback) => live.on_callback(ctx, &aps, &callback),
                None => return Err(coordinator::link_error(ezsp).await),
            },
            order = ctx.next_command() => {
                let Some(order) = order else {
                    live.stop(ctx, &aps, callbacks).await;
                    return Ok(());
                };
                live.on_order(ctx, &aps, callbacks, order).await;
            }
            () = tokio::time::sleep_until(close_at.unwrap_or_else(tokio::time::Instant::now)),
                if close_at.is_some() => live.joining_closed(ctx),
            _ = record_at.tick() => {
                if live.checked {
                    live.record_counter(ctx, &aps, callbacks).await;
                } else {
                    // A check that failed to read: tried again.
                    live.check_due = live.network == Network::Up;
                }
            }
        }
        // The network came up after the startup: checked to be ours (the
        // startup rules) before its counter is recorded.
        if live.check_due
            && let Some(reason) = live.check_network(ctx, &aps, callbacks).await
        {
            return park(ctx, ezsp, callbacks, reason).await;
        }
    }
}

/// The running coordinator: what its callbacks and orders change.
struct Live<'a> {
    coordinator: DeviceId,
    protocol: u8,
    network: Network,
    /// Whether the dongle's network was checked to be the store's (at
    /// startup, or once it came up later): only then is its frame counter
    /// recorded, every [`backup::COUNTER_RECORD_PERIOD`].
    checked: bool,
    /// The network came up unchecked: check it before anything else.
    check_due: bool,
    /// When the joining opened from Moli closes by itself (the NCP and the
    /// routers close it on their own then: only the point is updated).
    permit_until: Option<tokio::time::Instant>,
    store: &'a mut Store,
    store_path: &'a Path,
    clock: Clock,
}

impl Live<'_> {
    /// One callback: a `messageSentHandler` goes to the unicast awaiting
    /// it, the stack status to the `network` point, an arrival to the store
    /// and the `joining` point; the rest is logged.
    fn on_callback(&mut self, ctx: &DriverCtx, aps: &Aps, callback: &Callback) {
        if aps.on_message_sent(callback) {
            return;
        }
        match coordinator::event(callback, self.protocol) {
            Some(Event::Network(network)) => {
                self.network = network;
                self.check_due = network == Network::Up && !self.checked;
                ctx.set_state(&self.coordinator, "network", network_value(network));
                if network != Network::Up && self.permit_until.is_some() {
                    self.joining_closed(ctx);
                }
            }
            Some(Event::Arrival { nwk, ieee }) => self.arrival(ctx, nwk, ieee),
            None => {}
        }
    }

    /// A device joined or announced itself: shown as the last arrival, and
    /// recorded (not interviewed yet; saved only when something changed)
    /// once the network is checked to be the store's: never into the store
    /// of another network.
    fn arrival(&mut self, ctx: &DriverCtx, nwk: u16, ieee: [u8; 8]) {
        let device = network::ieee(ieee);
        if !self.checked {
            tracing::info!(%device, "a device arrived on a network not checked yet: not recorded");
        } else if self.store.record_join(&device, nwk)
            && let Err(error) = self.store.save(self.store_path)
        {
            // Kept in memory: the next change saves it too.
            tracing::error!(%error, %device, "the zigbee store could not be saved");
        }
        ctx.set_state(&self.coordinator, "joining", Value::Text(device.into()));
    }

    fn joining_closed(&mut self, ctx: &DriverCtx) {
        self.permit_until = None;
        ctx.set_state(&self.coordinator, "permit_join", Value::Bool(false));
        tracing::info!("joining closed");
    }

    /// An order: `permit_join` is the coordinator's only writable point.
    /// Opening (254 s) or closing joining needs the network up and checked
    /// to be the store's (a network that came up late and could not be
    /// read yet is not); the callbacks keep being read meanwhile.
    async fn on_order(
        &mut self,
        ctx: &mut DriverCtx,
        aps: &Aps,
        callbacks: &mut mpsc::Receiver<Callback>,
        order: CommandRequest,
    ) {
        let open = match permit_order(&order.key, &order.value) {
            Ok(open) => open,
            Err(reason) => {
                order.reply(Err(reason));
                return;
            }
        };
        if self.network != Network::Up {
            order.reply(Err(moli_i18n::tr!("pilotes.zigbee.reseau_absent")));
            return;
        }
        if !self.checked {
            order.reply(Err(moli_i18n::tr!("pilotes.zigbee.reseau_non_verifie")));
            return;
        }
        if order.abandoned() {
            // Nobody waits for it any more: never run late.
            tracing::debug!(open, "permit join order abandoned");
            return;
        }
        let seconds = if open {
            coordinator::PERMIT_JOIN_SECONDS
        } else {
            0
        };
        let work = coordinator::permit_joining(aps, seconds);
        match self.draining(ctx, aps, callbacks, work).await {
            Ok(()) => {
                self.permit_until =
                    open.then(|| tokio::time::Instant::now() + coordinator::PERMIT_JOIN);
                ctx.set_state(&self.coordinator, "permit_join", Value::Bool(open));
                order.reply(Ok(()));
            }
            Err(error) => {
                tracing::warn!(%error, open, "permit join failed");
                order.reply(Err(moli_i18n::tr!(
                    "pilotes.zigbee.appairage_echec",
                    error = error
                )));
            }
        }
    }

    /// The network came up after the startup: the startup's checks
    /// ([`adopt`]), then its counter recorded at once and every hour. Why a
    /// person is needed when it is not ours (nothing changed). A failed read
    /// is logged and tried again at the next hour.
    async fn check_network(
        &mut self,
        ctx: &DriverCtx,
        aps: &Aps,
        callbacks: &mut mpsc::Receiver<Callback>,
    ) -> Option<String> {
        self.check_due = false;
        let read = network::read_network(aps.ezsp(), self.protocol);
        let dongle = match self.draining(ctx, aps, callbacks, read).await {
            Ok(dongle) => dongle,
            Err(error) => {
                tracing::warn!(%error, "the network that came up could not be read: tried again in an hour");
                return None;
            }
        };
        let has_key = ctx.secret(network::NETWORK_KEY).is_some();
        match adopt(dongle, has_key, self.store, self.store_path) {
            Ok(None) => {}
            Ok(Some(reason)) => return Some(reason),
            Err(error) => {
                tracing::warn!(%error, "the zigbee store could not be saved: tried again in an hour");
                return None;
            }
        }
        self.checked = true;
        self.record_counter(ctx, aps, callbacks).await;
        None
    }

    /// The hourly record of the dongle's NWK frame counter, while the
    /// network is up: read with the callbacks still handled meanwhile, then
    /// written to the store. A failed read is logged and tried again at the
    /// next hour; it never clears the recorded counter.
    async fn record_counter(
        &mut self,
        ctx: &DriverCtx,
        aps: &Aps,
        callbacks: &mut mpsc::Receiver<Callback>,
    ) {
        if self.network != Network::Up {
            return;
        }
        let read = network::network_key_info(aps.ezsp());
        let recorded = match self.draining(ctx, aps, callbacks, read).await {
            Ok(info) => backup::store_counter(self.store, self.store_path, info, (self.clock)()),
            Err(error) => Err(error),
        };
        if let Err(error) = recorded {
            tracing::warn!(%error, "the NWK frame counter could not be recorded: tried again in an hour");
        }
    }

    /// Moli stops: on a network up and checked to be ours, joining closed
    /// if Moli opened it, then the frame counter recorded a last time, so
    /// that an export right after (or a later restore) starts from it,
    /// stamped with the stop. Within [`STOP_TIMEOUT`]: whatever does not
    /// answer in time is left (the store keeps its last record), and Moli
    /// goes on stopping.
    async fn stop(&mut self, ctx: &DriverCtx, aps: &Aps, callbacks: &mut mpsc::Receiver<Callback>) {
        if !self.checked || self.network != Network::Up {
            return;
        }
        let stopping = async {
            if self.permit_until.is_some() {
                let close = coordinator::permit_joining(aps, 0);
                match self.draining(ctx, aps, callbacks, close).await {
                    Ok(()) => self.joining_closed(ctx),
                    Err(error) => tracing::warn!(%error, "joining could not be closed at stop"),
                }
            }
            self.record_counter(ctx, aps, callbacks).await;
        };
        if tokio::time::timeout(STOP_TIMEOUT, stopping).await.is_err() {
            tracing::warn!("the zigbee stop took too long: the last record is the one kept");
        }
    }

    /// Runs `work` while handling the callbacks that arrive meanwhile (a
    /// unicast's `messageSentHandler` among them). A link that ends fails
    /// the work; the run loop then reports it.
    async fn draining<F: std::future::Future>(
        &mut self,
        ctx: &DriverCtx,
        aps: &Aps,
        callbacks: &mut mpsc::Receiver<Callback>,
        work: F,
    ) -> F::Output {
        let mut work = std::pin::pin!(work);
        let mut linked = true;
        loop {
            tokio::select! {
                output = &mut work => return output,
                callback = callbacks.recv(), if linked => match callback {
                    Some(callback) => self.on_callback(ctx, aps, &callback),
                    None => linked = false,
                },
            }
        }
    }
}

/// Connect, configure, `networkInit`.
async fn start<T>(io: T) -> anyhow::Result<Started>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (ezsp, version, callbacks) = Ezsp::connect(io).await?;
    let init = coordinator::start_ncp(&ezsp, &version).await?;
    Ok(Started {
        ezsp,
        version,
        callbacks,
        init,
    })
}

/// An interrupted `moli-os zigbee import` (`import_pending`): the store and
/// the vault may not match, so nothing is touched, on the dongle (its
/// network, if any, is left « down », not waited for) or anywhere else,
/// until the same import runs again.
fn interrupted_import(store: &Store, init: Init) -> Option<Outcome> {
    if !store.import_pending {
        return None;
    }
    tracing::warn!("an interrupted `moli-os zigbee import`: waiting for it to be run again");
    Some(Outcome::Wait {
        reason: moli_i18n::tr!("pilotes.zigbee.import_interrompu"),
        network: match init {
            Init::Resumed => Network::Down,
            Init::NotJoined => Network::None,
        },
    })
}

/// How the network startup ended.
#[derive(Debug)]
enum Outcome {
    /// The coordinator is ready, its network in this state.
    Ready(Network),
    /// A person is needed (`reason`); nothing was changed, on the dongle
    /// or in the store.
    Wait { reason: String, network: Network },
}

/// No network on the dongle: restore the one the store knows (never form
/// over it: that would orphan every paired device), else form one; or why
/// a person is needed: no vault.
async fn first_network(
    config: &Config,
    ctx: &mut DriverCtx,
    dongle: &Dongle<'_>,
    callbacks: &mut mpsc::Receiver<Callback>,
    store: &mut Store,
) -> anyhow::Result<Outcome> {
    let &Dongle {
        ezsp,
        protocol,
        store_path,
        clock,
    } = dongle;
    if let Some(known) = store.network.clone() {
        return restore_known(ctx, dongle, callbacks, store, &known).await;
    }
    if !ctx.can_store_secrets() {
        tracing::warn!("no vault: the network is not formed");
        return Ok(Outcome::Wait {
            reason: moli_i18n::tr!(
                "pilotes.zigbee.coffre_indisponible",
                problem = ctx.secrets_problem().unwrap_or_default()
            ),
            network: Network::None,
        });
    }
    // In the vault before anything security-related reaches the NCP.
    let keys = network::keys(ctx).await?;
    let channel = config.channel.unwrap_or(network::DEFAULT_CHANNEL);
    let record = refusing_orders(
        ctx,
        network::form(ezsp, callbacks, protocol, channel, &keys),
    )
    .await?;
    store.network = Some(record);
    store.save(store_path)?;
    refusing_orders(ctx, record_counter(ezsp, store, store_path, clock())).await;
    Ok(Outcome::Ready(Network::Up))
}

/// The dongle's NWK frame counter recorded (at `now`) as soon as the
/// network runs ([`backup::record_counter`]); a failure is logged, the next
/// try is an hour later (the store keeps what it had).
async fn record_counter(ezsp: &Ezsp, store: &mut Store, store_path: &Path, now: u64) {
    if let Err(error) = backup::record_counter(ezsp, store, store_path, now).await {
        tracing::warn!(%error, "the NWK frame counter could not be recorded: tried again in an hour");
    }
}

/// The dongle has no network, the store knows one (an imported Zigbee2MQTT
/// backup, or a dongle wiped since): restore it with the vault's keys, on
/// that network's own dongle only, from a fresh frame counter only.
///
/// A person is needed, and nothing is sent to the dongle (but `getEui64`)
/// nor written (vault or store), without a vault, without the network key
/// in it, without a fresh counter (a Moli-formed network, or a counter a
/// dongle has run past since: the devices would drop every frame of a
/// coordinator counting below what they saw), with a counter recorded more
/// than [`backup::COUNTER_MAX_AGE`] ago (a dongle left powered meanwhile
/// may have run past the margin), with a counter at its end, or with
/// another dongle plugged in. A restored network's saved counter is no
/// longer fresh (it is never used twice); the restored dongle's own is
/// recorded instead.
async fn restore_known(
    ctx: &mut DriverCtx,
    dongle: &Dongle<'_>,
    callbacks: &mut mpsc::Receiver<Callback>,
    store: &mut Store,
    known: &store::NetworkRecord,
) -> anyhow::Result<Outcome> {
    let &Dongle {
        ezsp,
        protocol,
        store_path,
        clock,
    } = dongle;
    let wait = |reason: String| {
        Ok(Outcome::Wait {
            reason,
            network: Network::None,
        })
    };
    if !ctx.can_store_secrets() {
        tracing::warn!("no vault: the network is not restored");
        return wait(moli_i18n::tr!(
            "pilotes.zigbee.coffre_indisponible",
            problem = ctx.secrets_problem().unwrap_or_default()
        ));
    }
    if ctx.secret(network::NETWORK_KEY).is_none() {
        tracing::warn!(pan_id = %pan(known), "no network key in the vault: the network is not restored");
        return wait(moli_i18n::tr!(
            "pilotes.zigbee.cle_absente",
            pan = pan(known)
        ));
    }
    let (Some(saved), true) = (known.frame_counter, known.frame_counter_fresh) else {
        tracing::warn!(
            pan_id = %pan(known),
            "no fresh frame counter: the network is not restored"
        );
        return wait(moli_i18n::tr!(
            "pilotes.zigbee.compteur_inconnu",
            pan = pan(known)
        ));
    };
    if !backup::counter_recent(known, clock()) {
        tracing::warn!(
            pan_id = %pan(known),
            recorded_at = ?known.frame_counter_recorded_at,
            "the frame counter was recorded too long ago: the network is not restored"
        );
        return wait(moli_i18n::tr!(
            "pilotes.zigbee.compteur_ancien",
            pan = pan(known)
        ));
    }
    if backup::restore_counter(saved).is_none() {
        tracing::warn!(pan_id = %pan(known), saved, "the frame counter is at its end");
        return wait(moli_i18n::tr!(
            "pilotes.zigbee.compteur_epuise",
            pan = pan(known)
        ));
    }
    // Before anything is written, even to the vault.
    if let Some(dongle) = refusing_orders(ctx, backup::other_dongle(ezsp, known)).await? {
        return wait(moli_i18n::tr!(
            "pilotes.zigbee.autre_dongle",
            pan = pan(known),
            expected = known.coordinator_ieee.as_str(),
            dongle = dongle
        ));
    }
    let Some(keys) = network::held_keys(ctx).await? else {
        anyhow::bail!("the vault's network key went away during the restore");
    };
    let restored = refusing_orders(
        ctx,
        backup::restore(ezsp, callbacks, protocol, &keys, known, saved),
    )
    .await?;
    match restored {
        // Checked just above; the dongle cannot change under the link.
        backup::Restored::OtherDongle { dongle } => {
            anyhow::bail!("the dongle changed during the restore ({dongle})")
        }
        backup::Restored::Network(record) => {
            // The counter is spent: never restored from twice. The dongle's
            // own, recorded at once, takes over.
            store.network = Some(record.keeping_backup_of(known));
            store.save(store_path)?;
            refusing_orders(ctx, record_counter(ezsp, store, store_path, clock())).await;
            Ok(Outcome::Ready(Network::Up))
        }
    }
}

/// What forming or restoring a network works with, besides the store and
/// the callbacks: the link, the protocol, where the store is saved, the
/// clock its counters are recorded with.
struct Dongle<'a> {
    ezsp: &'a Ezsp,
    protocol: u8,
    store_path: &'a Path,
    clock: Clock,
}

/// A network's PAN ID as people read it (`0x1a62`).
fn pan(record: &store::NetworkRecord) -> String {
    format!("{:#06x}", record.pan_id)
}

/// A network the dongle kept: wait for it to come up, then check it is
/// ours before recording it. Not up within [`coordinator::NETWORK_UP_TIMEOUT`]:
/// the coordinator is still ready, its network « down » (no restart loop;
/// the stack status that follows moves the point, and the network is
/// checked when it comes up); meanwhile the store's counter is no longer
/// restorable ([`unvouched`]).
///
/// Never adopted (a person decides; nothing is changed: the dongle's
/// network is not left, the store not rewritten): a dongle that is not the
/// coordinator of its network; a network other than the store's (its keys
/// are not the vault's); a network an empty store does not know while the
/// vault holds no `network_key`. An empty store with a key in the vault is
/// a first formation interrupted before the store was written: adopted.
/// The same network with a new channel or update id updates the store
/// (keeping what an imported backup recorded: frame counter, key sequence
/// number); a backup imported for the network the dongle still runs is
/// thus taken over as it is. A network kept is then the dongle's to count:
/// its frame counter is recorded.
async fn resume(
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    protocol: u8,
    has_key: bool,
    store: &mut Store,
    store_path: &Path,
    clock: Clock,
) -> anyhow::Result<Outcome> {
    let network =
        coordinator::wait_network_up(ezsp, callbacks, protocol, coordinator::NETWORK_UP_TIMEOUT)
            .await?;
    if network != Network::Up {
        unvouched(ezsp, protocol, store, store_path).await?;
        return Ok(Outcome::Ready(network));
    }
    let dongle = network::read_network(ezsp, protocol).await?;
    if let Some(reason) = adopt(dongle, has_key, store, store_path)? {
        return Ok(Outcome::Wait { reason, network });
    }
    // Ours: its counter recorded from now on.
    record_counter(ezsp, store, store_path, clock()).await;
    Ok(Outcome::Ready(network))
}

/// A resumed network not up yet, so not checked: the store's counter for
/// it is no longer restorable (the dongle may be sending, and nothing
/// records it until the network is up and checked), its value kept, as
/// [`store::NetworkRecord::keeping_backup_of`] does. Left alone when the
/// dongle reads as another network (the store's counter is not this
/// dongle's); cleared when it cannot be read (conservative).
async fn unvouched(
    ezsp: &Ezsp,
    protocol: u8,
    store: &mut Store,
    store_path: &Path,
) -> anyhow::Result<()> {
    let Some(known) = store
        .network
        .as_mut()
        .filter(|known| known.frame_counter_fresh)
    else {
        return Ok(());
    };
    if let Ok(dongle) = network::read_network(ezsp, protocol).await
        && !known.same_network(&dongle.record)
    {
        return Ok(());
    }
    tracing::info!(
        pan_id = %pan(known),
        "the network is not up: its recorded frame counter is no longer restorable"
    );
    known.frame_counter_fresh = false;
    store.save(store_path)
}

/// Whether the network a dongle runs is Moli's (startup rules, see
/// [`resume`]): `None` when it is (the store then updated with it, saved if
/// it changed), or why a person is needed (nothing changed).
fn adopt(
    dongle: network::DongleNetwork,
    has_key: bool,
    store: &mut Store,
    store_path: &Path,
) -> anyhow::Result<Option<String>> {
    let record = dongle.record;
    if !dongle.coordinator {
        tracing::warn!(pan_id = %pan(&record), "the dongle is not its network's coordinator");
        return Ok(Some(moli_i18n::tr!(
            "pilotes.zigbee.pas_coordinateur",
            pan = pan(&record)
        )));
    }
    // What only an imported backup knows of the same network stays.
    let record = match &store.network {
        Some(known) if known.same_network(&record) => record.keeping_backup_of(known),
        _ => record,
    };
    match &store.network {
        Some(known) if !known.same_network(&record) => {
            tracing::warn!(
                known = %pan(known),
                dongle = %pan(&record),
                "the dongle runs another network than the one Moli knows"
            );
            return Ok(Some(moli_i18n::tr!(
                "pilotes.zigbee.autre_reseau",
                known = pan(known),
                dongle = pan(&record)
            )));
        }
        Some(_) => {}
        None if !has_key => {
            tracing::warn!(pan_id = %pan(&record), "the dongle runs a network Moli has no key for");
            return Ok(Some(moli_i18n::tr!(
                "pilotes.zigbee.reseau_inconnu",
                pan = pan(&record)
            )));
        }
        None => tracing::info!(
            pan_id = %pan(&record),
            "adopting the network of an interrupted first formation"
        ),
    }
    if store.network.as_ref() != Some(&record) {
        store.network = Some(record);
        store.save(store_path)?;
    }
    Ok(None)
}

/// Show `reason` and wait for a person: orders refused, callbacks drained,
/// until Moli stops (or the link ends: an error, the supervisor restarts).
async fn park(
    ctx: &mut DriverCtx,
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    reason: String,
) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    loop {
        tokio::select! {
            callback = callbacks.recv() => if callback.is_none() {
                return Err(coordinator::link_error(ezsp).await);
            },
            order = ctx.next_command() => match order {
                Some(order) => refuse(order),
                None => return Ok(()),
            },
        }
    }
}

/// Run `work`, refusing every order that arrives meanwhile.
async fn refusing_orders<F: std::future::Future>(ctx: &mut DriverCtx, work: F) -> F::Output {
    let mut work = std::pin::pin!(work);
    loop {
        tokio::select! {
            output = &mut work => return output,
            Some(order) = ctx.next_command() => refuse(order),
        }
    }
}

/// What a write to the coordinator asks: open (`true`) or close joining;
/// `permit_join` with a boolean is the only write it takes.
fn permit_order(key: &str, value: &Value) -> Result<bool, String> {
    match (key, value) {
        ("permit_join", &Value::Bool(open)) => Ok(open),
        _ => Err(moli_i18n::tr!("pilotes.zigbee.ecriture_refusee")),
    }
}

fn refuse(order: CommandRequest) {
    order.reply(Err(moli_i18n::tr!("pilotes.zigbee.pas_pret")));
}

fn network_value(network: Network) -> Value {
    Value::Text(network.as_str().into())
}

/// The coordinator as a Moli device: what the dongle runs, read-only; the
/// joining it opens (`permit_join`, writable) and the last device that
/// arrived (`joining`, its IEEE address).
fn coordinator_device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    let point = |key: &str, label: &str, kind: Kind| PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Config,
    };
    // Opening pairing is an order: an agent asking gets a human's
    // approval first.
    let permit_join = PointSpec {
        access: Access {
            read: true,
            write: true,
        },
        semantic: Semantic::Control,
        ..point(
            "permit_join",
            &moli_i18n::tr!("pilotes.zigbee.appairage"),
            Kind::Binary,
        )
    };
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.zigbee.coordinateur").into(),
        manufacturer: Some("Silicon Labs".into()),
        model: Some("EmberZNet (EZSP)".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            point(
                "firmware",
                &moli_i18n::tr!("pilotes.zigbee.firmware"),
                Kind::Text,
            ),
            point(
                "ezsp_version",
                &moli_i18n::tr!("pilotes.zigbee.version_ezsp"),
                Kind::Numeric {
                    min: None,
                    max: None,
                    step: Some(1.0),
                },
            ),
            point(
                "network",
                &moli_i18n::tr!("pilotes.zigbee.reseau"),
                Kind::Enum {
                    values: Network::VALUES.iter().map(|&v| v.into()).collect(),
                },
            ),
            permit_join,
            point(
                "joining",
                &moli_i18n::tr!("pilotes.zigbee.arrivee"),
                Kind::Text,
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use moli_core::{DriverStatus, InstanceId, Origin, PointId};
    use moli_runtime::{CommandError, Hub, HubOptions, MasterKey, spawn_driver};
    use tokio::io::DuplexStream;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::ezsp::ids;
    use crate::network::hex;
    use crate::network::tests::{EUI64, key_info, network_parameters, network_parameters_of};
    use crate::sim::FakeNcp;
    use crate::store::tests::{TempDir, record};

    const EMBER_13: Version = Version {
        protocol: 13,
        stack_type: 2,
        stack_version: 0x7440,
    };
    /// EmberZNet 6.10: EZSP 8, too old.
    const EMBER_OLD: Version = Version {
        protocol: 8,
        stack_type: 2,
        stack_version: 0x6a00,
    };
    /// Commands the NCP receives up to `networkInit`: `version` twice, the
    /// configuration, the endpoint, the trust center policies, `networkInit`.
    const STARTUP: usize =
        2 + coordinator::STACK_CONFIG.len() + 1 + coordinator::TRUST_CENTER_POLICIES.len() + 1;

    /// The driver on a simulated link, run by the real supervisor; with
    /// `probe`, it first publishes a writable point to send orders to.
    struct Simulated {
        config: Config,
        data_dir: PathBuf,
        io: Mutex<Option<DuplexStream>>,
        probe: bool,
    }

    impl Driver for Simulated {
        fn kind(&self) -> &'static str {
            "zigbee"
        }

        fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                if self.probe {
                    ctx.upsert_device(probe(ctx));
                }
                let io = self.io.lock().unwrap().take();
                let io = io.ok_or_else(|| anyhow::anyhow!("simulated link already used"))?;
                run_on(&self.config, &self.data_dir, ctx, io, test_clock).await
            })
        }
    }

    /// The wall clock of every simulated run (unix seconds): what the
    /// driver records its counters at.
    const NOW: u64 = 1_800_000_000;

    fn test_clock() -> u64 {
        NOW
    }

    fn probe(ctx: &DriverCtx) -> Device {
        Device {
            id: ctx.device_id("probe"),
            instance: ctx.instance().clone(),
            native_name: "probe".into(),
            manufacturer: None,
            model: None,
            description: None,
            native_room: None,
            members: Vec::new(),
            points: vec![PointSpec {
                key: "on".into(),
                label: "on".into(),
                kind: Kind::Binary,
                access: Access {
                    read: true,
                    write: true,
                },
                unit: None,
                semantic: Semantic::OnOff,
            }],
        }
    }

    /// How a test runs the driver.
    #[derive(Default)]
    struct Setup {
        probe: bool,
        /// A hub with a vault (master key set).
        vault: bool,
        /// A store already on disk.
        store: Option<Store>,
    }

    impl Setup {
        /// A store that knows the network the fake NCP reports (PAN 0x1a62).
        fn known() -> Self {
            Self {
                store: Some(known_store(record(0x1a62))),
                ..Self::default()
            }
        }
    }

    fn known_store(network: store::NetworkRecord) -> Store {
        Store {
            network: Some(network),
            ..Store::default()
        }
    }

    /// The hub and the data directory, before the driver starts.
    struct Prepared {
        hub: Hub,
        instance: InstanceId,
        dir: TempDir,
    }

    impl Prepared {
        fn new(setup: &Setup) -> Self {
            let dir = TempDir::new();
            let instance = InstanceId::from("zigbee");
            if let Some(store) = &setup.store {
                store.save(&store::path(&dir.0, instance.as_str())).unwrap();
            }
            let secrets = setup
                .vault
                .then(|| (dir.0.join("secrets.enc"), MasterKey::generate().unwrap()));
            let hub = Hub::new(HubOptions {
                secrets,
                ..HubOptions::default()
            })
            .unwrap();
            Self { hub, instance, dir }
        }
    }

    /// A driver running on a hub; stopped when dropped.
    struct Running {
        hub: Hub,
        instance: InstanceId,
        cancel: CancellationToken,
        dir: TempDir,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.cancel.cancel();
        }
    }

    impl Running {
        fn start(io: DuplexStream, probe: bool) -> Self {
            Self::with(
                io,
                &Setup {
                    probe,
                    ..Setup::default()
                },
            )
        }

        fn with(io: DuplexStream, setup: &Setup) -> Self {
            let probe = setup.probe;
            Self::spawn(Prepared::new(setup), io, probe)
        }

        fn spawn(prepared: Prepared, io: DuplexStream, probe: bool) -> Self {
            let Prepared { hub, instance, dir } = prepared;
            let cancel = CancellationToken::new();
            let driver = Simulated {
                config: config(r#"{"port":"/dev/ttyUSB0"}"#).unwrap(),
                data_dir: dir.0.clone(),
                io: Mutex::new(Some(io)),
                probe,
            };
            spawn_driver(&hub, instance.clone(), Arc::new(driver), cancel.clone());
            Self {
                hub,
                instance,
                cancel,
                dir,
            }
        }

        fn status(&self) -> Option<DriverStatus> {
            self.hub
                .snapshot()
                .drivers
                .into_iter()
                .find(|d| d.instance == self.instance)
                .map(|d| d.status)
        }

        /// The first status `pick` accepts, within 5 s.
        async fn wait_status<R>(&self, pick: impl Fn(&DriverStatus) -> Option<R>) -> R {
            for _ in 0..500 {
                if let Some(found) = self.status().as_ref().and_then(&pick) {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("status never reached: {:?}", self.status());
        }

        async fn wait_ready(&self) {
            self.wait_status(|status| (*status == DriverStatus::Running).then_some(()))
                .await;
        }

        async fn wait_waiting(&self) -> String {
            self.wait_status(|status| match status {
                DriverStatus::Waiting { reason } => Some(reason.clone()),
                _ => None,
            })
            .await
        }

        fn point(&self, key: &str) -> Option<Value> {
            let device = DeviceId::new(&self.instance, "coordinator");
            self.hub
                .state(&PointId::new(&device, key))
                .map(|sample| sample.value)
        }

        fn store(&self) -> Store {
            Store::load(&self.store_path()).unwrap()
        }

        fn store_path(&self) -> PathBuf {
            store::path(&self.dir.0, self.instance.as_str())
        }

        /// A person writes `value` to the coordinator's `key`.
        async fn set(&self, key: &str, value: Value) -> Result<(), CommandError> {
            self.set_from(Origin::Ui, key, value).await
        }

        async fn set_from(
            &self,
            origin: Origin,
            key: &str,
            value: Value,
        ) -> Result<(), CommandError> {
            let device = DeviceId::new(&self.instance, "coordinator");
            self.hub
                .command(&PointId::new(&device, key), value, origin, None)
                .await
        }

        /// Waits (5 s) for the coordinator's `key` to read `value`.
        async fn wait_point(&self, key: &str, value: &Value) {
            for _ in 0..500 {
                if self.point(key).as_ref() == Some(value) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("{key} still {:?}, not {value:?}", self.point(key));
        }

        async fn order(&self) -> Result<(), CommandError> {
            let device = DeviceId::new(&self.instance, "probe");
            self.hub
                .command(
                    &PointId::new(&device, "on"),
                    Value::Bool(true),
                    Origin::Api,
                    None,
                )
                .await
        }
    }

    fn text(s: &str) -> Value {
        Value::Text(s.into())
    }

    fn not_ready() -> Result<(), CommandError> {
        Err(CommandError::Driver(moli_i18n::tr!(
            "pilotes.zigbee.pas_pret"
        )))
    }

    /// An NCP that accepts the startup and answers `networkInit` with `init`.
    fn ncp_with_init(init: &[u8]) -> (FakeNcp, DuplexStream) {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        ncp.respond(ids::ADD_ENDPOINT, &[0x00]);
        ncp.respond(ids::SET_POLICY, &[0x00]);
        ncp.respond(ids::NETWORK_INIT, init);
        (ncp, io)
    }

    /// The outgoing NWK frame counter the fake NCP reports
    /// (`getNetworkKeyInfo`): above every counter a test restores from.
    const LIVE_COUNTER: u32 = 270_000;
    /// `getNetworkKeyInfo` refused (`SL_STATUS_FAIL`): no counter read.
    const KEY_INFO_REFUSED: [u8; 12] = [0x01, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];

    /// `network` once the driver recorded the dongle's counter: restorable.
    fn live(network: store::NetworkRecord) -> store::NetworkRecord {
        store::NetworkRecord {
            frame_counter: Some(LIVE_COUNTER),
            frame_counter_fresh: true,
            frame_counter_recorded_at: Some(NOW),
            key_sequence_number: Some(0),
            ..network
        }
    }

    /// … that also forms, and reports the network it runs and its key's
    /// frame counter ([`LIVE_COUNTER`]).
    fn forming_ncp(init: &[u8]) -> (FakeNcp, DuplexStream) {
        let (ncp, io) = ncp_with_init(init);
        for id in [
            ids::SET_INITIAL_SECURITY_STATE,
            ids::SET_VALUE,
            ids::CLEAR_KEY_TABLE,
            ids::FORM_NETWORK,
        ] {
            ncp.respond(id, &[0x00]);
        }
        ncp.respond(ids::GET_NETWORK_PARAMETERS, &network_parameters(0x1a62, 15));
        ncp.respond(ids::GET_EUI64, &EUI64);
        ncp.respond(ids::GET_NETWORK_KEY_INFO, &key_info(0, LIVE_COUNTER));
        (ncp, io)
    }

    fn sent(ncp: &FakeNcp, id: u16) -> Option<Vec<u8>> {
        ncp.received()
            .into_iter()
            .find(|(got, _)| *got == id)
            .map(|(_, params)| params)
    }

    #[tokio::test]
    async fn forms_when_empty() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                ..Setup::default()
            },
        );
        ncp.wait_command(ids::SET_INITIAL_SECURITY_STATE).await;
        // The key was in the vault before the NCP heard of it.
        let key = run
            .hub
            .secret(&run.instance, network::NETWORK_KEY)
            .expect("network_key in the vault");
        assert_eq!(key.len(), 32);
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        let state = sent(&ncp, ids::SET_INITIAL_SECURITY_STATE).unwrap();
        assert_eq!(hex(&state[18..34]), key, "the vault's key is the one sent");
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.point("network"), Some(text("up")));
        let formed = sent(&ncp, ids::FORM_NETWORK).unwrap();
        let formed = ezsp::NetworkParameters::decode(&formed).unwrap().0;
        assert_eq!(formed.radio_channel, 15);
        // The store has the network and its counter, never the key.
        let saved = run.store();
        assert_eq!(saved.network, Some(live(record(0x1a62))));
        let file = std::fs::read_to_string(run.store_path()).unwrap();
        assert!(!file.contains(&key), "{file}");
    }

    #[tokio::test]
    async fn a_key_the_vault_holds_is_reused() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let held = "01030507090b0d0f00020406080a0c0d";
        let prepared = Prepared::new(&Setup {
            vault: true,
            ..Setup::default()
        });
        // Left by a previous attempt that stopped before writing the store.
        prepared
            .hub
            .store_secret(&prepared.instance, network::NETWORK_KEY, held)
            .await
            .unwrap();
        let run = Running::spawn(prepared, io, false);
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        let state = sent(&ncp, ids::SET_INITIAL_SECURITY_STATE).unwrap();
        assert_eq!(hex(&state[18..34]), held);
        assert_eq!(
            run.hub
                .secret(&run.instance, network::NETWORK_KEY)
                .as_deref(),
            Some(held)
        );
    }

    #[tokio::test]
    async fn no_vault_no_network() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::start(io, true);
        let reason = run.wait_waiting().await;
        let problem = moli_i18n::tr!("serveur.coffre.cle_absente");
        assert_eq!(
            reason,
            moli_i18n::tr!("pilotes.zigbee.coffre_indisponible", problem = problem)
        );
        assert_eq!(run.order().await, not_ready());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let received: Vec<_> = ncp.received().into_iter().map(|(id, _)| id).collect();
        assert_eq!(received.len(), STARTUP, "{received:x?}");
        assert!(!received.contains(&ids::SET_INITIAL_SECURITY_STATE));
        assert!(!received.contains(&ids::FORM_NETWORK));
        assert_eq!(run.status(), Some(DriverStatus::Waiting { reason }));
        assert_eq!(run.point("network"), Some(text("none")));
        assert_eq!(run.store(), Store::default(), "nothing written");
    }

    /// The network an import just left in the store: a fresh frame counter
    /// (123456), imported just now.
    fn imported_store() -> Store {
        known_store(imported_record())
    }

    fn imported_record() -> store::NetworkRecord {
        store::NetworkRecord {
            frame_counter: Some(123_456),
            frame_counter_fresh: true,
            frame_counter_recorded_at: Some(NOW),
            key_sequence_number: Some(0),
            ..record(0x1a62)
        }
    }

    /// … once a dongle has run that network: its counter is spent.
    fn spent_store() -> Store {
        known_store(store::NetworkRecord {
            frame_counter_fresh: false,
            ..imported_record()
        })
    }

    const IMPORTED_KEY: &str = "01030507090b0d0f00020406080a0c0d";

    /// A hub with a vault and `store`; `key` in the vault if given.
    async fn imported(store: Store, key: Option<&str>) -> Prepared {
        let prepared = Prepared::new(&Setup {
            vault: true,
            store: Some(store),
            ..Setup::default()
        });
        if let Some(key) = key {
            prepared
                .hub
                .store_secret(&prepared.instance, network::NETWORK_KEY, key)
                .await
                .unwrap();
        }
        prepared
    }

    /// Restores the imported network on a wiped dongle; returns the run.
    /// The dongle's counter is recorded unless `ncp` refuses to give it.
    async fn restored(ncp: &FakeNcp, io: DuplexStream) -> Running {
        let run = Running::spawn(
            imported(imported_store(), Some(IMPORTED_KEY)).await,
            io,
            false,
        );
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        run
    }

    #[tokio::test]
    async fn a_known_network_is_restored() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = restored(&ncp, io).await;
        assert_eq!(run.point("network"), Some(text("up")));
        let after: Vec<_> = ncp.received()[STARTUP..]
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            after,
            [
                // The dongle checked before anything is written…
                ids::GET_EUI64,
                // … and again by the restore, which guards itself.
                ids::GET_EUI64,
                ids::SET_VALUE,
                ids::SET_INITIAL_SECURITY_STATE,
                ids::SET_VALUE,
                ids::FORM_NETWORK,
                ids::SET_VALUE,
                ids::GET_NETWORK_PARAMETERS,
                ids::GET_EUI64,
                // Its counter, recorded at once.
                ids::GET_NETWORK_KEY_INFO,
            ],
            "restored, never formed anew (no key table cleared)"
        );
        // The saved counter plus the margin (123456 + 65536); the vault's keys.
        let counter = sent(&ncp, ids::SET_VALUE).unwrap();
        assert_eq!(counter, [0x23, 4, 0x40, 0xE2, 0x02, 0x00]);
        let state = sent(&ncp, ids::SET_INITIAL_SECURITY_STATE).unwrap();
        assert_eq!(hex(&state[18..34]), IMPORTED_KEY);
        // No trust center link key in the vault: a new one, kept there.
        let tclk = run
            .hub
            .secret(&run.instance, network::TC_LINK_KEY)
            .expect("tc_link_key in the vault");
        assert_eq!(hex(&state[2..18]), tclk);
        let formed = sent(&ncp, ids::FORM_NETWORK).unwrap();
        let formed = ezsp::NetworkParameters::decode(&formed).unwrap().0;
        assert_eq!((formed.pan_id, formed.extended_pan_id), (0x1a62, [0xDD; 8]));
        // The backup's counter is spent; the dongle's own, recorded, is
        // what a later restore starts from.
        assert_eq!(run.store(), known_store(live(record(0x1a62))));
    }

    /// Waits for `reason`, then checks nothing went to the dongle, the
    /// vault or the store.
    async fn waits_untouched(ncp: &FakeNcp, run: &Running, store: &Store, reason: &str) {
        assert_eq!(run.wait_waiting().await, reason);
        assert_eq!(run.order().await, not_ready());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(ncp.received().len(), STARTUP, "nothing sent");
        assert_eq!(run.hub.secret(&run.instance, network::TC_LINK_KEY), None);
        assert_eq!(run.store(), *store, "store untouched");
        assert_eq!(run.point("network"), Some(text("none")));
    }

    #[tokio::test]
    async fn an_unknown_counter_never_restores() {
        let moli_formed = known_store(record(0x1a62));
        // A counter without its freshness: who knows what was sent since.
        let unmarked = known_store(store::NetworkRecord {
            frame_counter_fresh: false,
            ..imported_record()
        });
        for store in [moli_formed, unmarked] {
            let (ncp, io) = forming_ncp(&[0x93]);
            let run = Running::spawn(imported(store.clone(), Some(IMPORTED_KEY)).await, io, true);
            let reason = moli_i18n::tr!("pilotes.zigbee.compteur_inconnu", pan = "0x1a62");
            waits_untouched(&ncp, &run, &store, &reason).await;
        }
    }

    #[tokio::test]
    async fn a_used_counter_is_not_reused() {
        let (ncp, io) = forming_ncp(&[0x93]);
        // The dongle's counter could not be read: only the backup's is
        // known, and it is spent.
        ncp.respond(ids::GET_NETWORK_KEY_INFO, &KEY_INFO_REFUSED);
        let run = restored(&ncp, io).await;
        let after = run.store();
        assert_eq!(after, spent_store());
        drop(run);
        // The dongle wiped again later: that counter is long behind.
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::spawn(imported(after.clone(), Some(IMPORTED_KEY)).await, io, true);
        let reason = moli_i18n::tr!("pilotes.zigbee.compteur_inconnu", pan = "0x1a62");
        waits_untouched(&ncp, &run, &after, &reason).await;
    }

    #[tokio::test]
    async fn an_old_backup_is_not_reimported() {
        // Imported, restored (the counter spent, the dongle's own not
        // read), then the dongle reflashed.
        let (ncp, io) = forming_ncp(&[0x93]);
        ncp.respond(ids::GET_NETWORK_KEY_INFO, &KEY_INFO_REFUSED);
        let after = restored(&ncp, io).await.store();
        assert_eq!(after, spent_store());
        // The same backup again: refused, even forced, the store unchanged.
        let again = backup::Import {
            record: imported_record(),
            network_key: network::Key::from_hex(IMPORTED_KEY).unwrap(),
            tc_link_key: None,
            devices: Vec::new(),
        };
        for force in [false, true] {
            let mut store = after.clone();
            let error = again.apply(&mut store, force, NOW).unwrap_err().to_string();
            assert_eq!(
                error,
                moli_i18n::tr!(
                    "pilotes.zigbee.sauvegarde_pas_plus_recente",
                    backup = 123_456,
                    used = 123_456
                )
            );
            assert_eq!(store, after);
        }
        // So the driver still waits, touching nothing.
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::spawn(imported(after.clone(), Some(IMPORTED_KEY)).await, io, true);
        let reason = moli_i18n::tr!("pilotes.zigbee.compteur_inconnu", pan = "0x1a62");
        waits_untouched(&ncp, &run, &after, &reason).await;
        drop(run);
        // A newer backup is accepted, and restores from its counter.
        let newer = backup::Import {
            record: store::NetworkRecord {
                frame_counter: Some(200_000),
                ..imported_record()
            },
            ..again
        };
        let mut store = after.clone();
        newer.apply(&mut store, false, NOW).unwrap();
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::spawn(imported(store, Some(IMPORTED_KEY)).await, io, false);
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        // 200000 + 65536 = 265536 = 0x00040D40, little-endian.
        assert_eq!(
            sent(&ncp, ids::SET_VALUE).unwrap(),
            [0x23, 4, 0x40, 0x0D, 0x04, 0x00]
        );
    }

    #[tokio::test]
    async fn a_counter_at_its_end_never_restores() {
        let ended = known_store(store::NetworkRecord {
            frame_counter: Some(u32::MAX - 100),
            ..imported_record()
        });
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::spawn(imported(ended.clone(), Some(IMPORTED_KEY)).await, io, true);
        let reason = moli_i18n::tr!("pilotes.zigbee.compteur_epuise", pan = "0x1a62");
        waits_untouched(&ncp, &run, &ended, &reason).await;
    }

    #[tokio::test]
    async fn an_old_counter_never_restores() {
        // Recorded (here imported) more than a day ago, or at no known
        // time: a dongle left powered since may have sent past the margin.
        let day = backup::COUNTER_MAX_AGE.as_secs();
        for recorded_at in [Some(NOW - day - 1), None] {
            let old = known_store(store::NetworkRecord {
                frame_counter_recorded_at: recorded_at,
                ..imported_record()
            });
            let (ncp, io) = forming_ncp(&[0x93]);
            let run = Running::spawn(imported(old.clone(), Some(IMPORTED_KEY)).await, io, true);
            let reason = moli_i18n::tr!("pilotes.zigbee.compteur_ancien", pan = "0x1a62");
            waits_untouched(&ncp, &run, &old, &reason).await;
        }
        // A day old at most: restored.
        let (ncp, io) = forming_ncp(&[0x93]);
        let recent = known_store(store::NetworkRecord {
            frame_counter_recorded_at: Some(NOW - day),
            ..imported_record()
        });
        let run = Running::spawn(imported(recent, Some(IMPORTED_KEY)).await, io, false);
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.store(), known_store(live(record(0x1a62))));
    }

    #[tokio::test]
    async fn an_interrupted_import_waits() {
        // The store written, the vault maybe not: never trust either.
        let pending = Store {
            import_pending: true,
            ..imported_store()
        };
        for (init, network) in [(0x93, "none"), (0x00, "down")] {
            let (ncp, io) = forming_ncp(&[init]);
            let run = Running::spawn(imported(pending.clone(), None).await, io, true);
            let reason = run.wait_waiting().await;
            assert_eq!(reason, moli_i18n::tr!("pilotes.zigbee.import_interrompu"));
            assert_eq!(run.order().await, not_ready());
            ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(ncp.received().len(), STARTUP, "nothing sent");
            assert_eq!(run.store(), pending, "store untouched");
            assert_eq!(run.hub.secret(&run.instance, network::TC_LINK_KEY), None);
            assert_eq!(run.point("network"), Some(text(network)));
        }
    }

    #[tokio::test]
    async fn a_known_network_without_its_key_waits() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::spawn(imported(imported_store(), None).await, io, true);
        let reason = moli_i18n::tr!("pilotes.zigbee.cle_absente", pan = "0x1a62");
        waits_untouched(&ncp, &run, &imported_store(), &reason).await;
        assert_eq!(run.hub.secret(&run.instance, network::NETWORK_KEY), None);
    }

    #[tokio::test]
    async fn a_known_network_without_vault_waits() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::with(
            io,
            &Setup {
                store: Some(imported_store()),
                ..Setup::default()
            },
        );
        let reason = run.wait_waiting().await;
        let problem = moli_i18n::tr!("serveur.coffre.cle_absente");
        assert_eq!(
            reason,
            moli_i18n::tr!("pilotes.zigbee.coffre_indisponible", problem = problem)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(ncp.received().len(), STARTUP, "nothing sent");
    }

    #[tokio::test]
    async fn a_known_network_on_another_dongle_waits() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let elsewhere = known_store(store::NetworkRecord {
            coordinator_ieee: "0x00124b0000000009".into(),
            ..imported_record()
        });
        let run = Running::spawn(
            imported(elsewhere.clone(), Some(IMPORTED_KEY)).await,
            io,
            true,
        );
        let reason = run.wait_waiting().await;
        assert_eq!(
            reason,
            moli_i18n::tr!(
                "pilotes.zigbee.autre_dongle",
                pan = "0x1a62",
                expected = "0x00124b0000000009",
                dongle = "0x00124b0000000001"
            )
        );
        assert_eq!(run.order().await, not_ready());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after: Vec<_> = ncp.received()[STARTUP..]
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(after, [ids::GET_EUI64], "nothing written to the dongle");
        assert_eq!(run.point("network"), Some(text("none")));
        assert_eq!(run.store(), elsewhere, "store untouched");
        // Nor to the vault: no trust center link key made up.
        assert_eq!(run.hub.secret(&run.instance, network::TC_LINK_KEY), None);
        assert_eq!(
            run.hub
                .secret(&run.instance, network::NETWORK_KEY)
                .as_deref(),
            Some(IMPORTED_KEY)
        );
    }

    #[tokio::test]
    async fn a_resumed_network_records_the_dongle_counter() {
        // The dongle kept the imported network (Zigbee2MQTT's own dongle):
        // adopted, the backup's counter spent (the dongle sends again), the
        // dongle's own recorded instead; kept spent if it cannot be read.
        for (answer, after) in [
            (key_info(0, LIVE_COUNTER), known_store(live(record(0x1a62)))),
            (KEY_INFO_REFUSED.to_vec(), spent_store()),
        ] {
            let (ncp, io) = forming_ncp(&[0x00]);
            ncp.respond(ids::GET_NETWORK_KEY_INFO, &answer);
            let run = Running::spawn(
                imported(imported_store(), Some(IMPORTED_KEY)).await,
                io,
                false,
            );
            ncp.wait_received(STARTUP).await;
            ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
            run.wait_ready().await;
            assert_eq!(run.store(), after);
            assert_eq!(sent(&ncp, ids::FORM_NETWORK), None);
        }
    }

    #[tokio::test]
    async fn resumes_existing_network() {
        let (ncp, io) = forming_ncp(&[0x00]);
        // The same network (PAN, extended PAN), since moved to channel 15.
        let before = store::NetworkRecord {
            channel: 11,
            ..record(0x1a62)
        };
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                store: Some(known_store(before)),
                ..Setup::default()
            },
        );
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.point("network"), Some(text("up")));
        assert_eq!(run.store().network, Some(live(record(0x1a62))));
        assert_eq!(sent(&ncp, ids::FORM_NETWORK), None);
        assert_eq!(sent(&ncp, ids::SET_INITIAL_SECURITY_STATE), None);
    }

    /// Resumes a network, then waits for the reason the driver gives.
    async fn resumed_wait(ncp: &FakeNcp, run: &Running) -> String {
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        let reason = run.wait_waiting().await;
        // It stays there, refusing orders, changing nothing on the dongle.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            run.status(),
            Some(DriverStatus::Waiting {
                reason: reason.clone()
            })
        );
        let after: Vec<_> = ncp.received()[STARTUP..]
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(after, [ids::GET_NETWORK_PARAMETERS, ids::GET_EUI64]);
        assert_eq!(run.point("network"), Some(text("up")));
        reason
    }

    #[tokio::test]
    async fn a_different_network_is_not_adopted() {
        // Another extended PAN only: still another network.
        for known in [
            record(0x1234),
            store::NetworkRecord {
                extended_pan_id: [0xEE; 8],
                ..record(0x1a62)
            },
        ] {
            let (ncp, io) = forming_ncp(&[0x00]);
            let run = Running::with(
                io,
                &Setup {
                    probe: true,
                    vault: true,
                    store: Some(known_store(known.clone())),
                },
            );
            let reason = resumed_wait(&ncp, &run).await;
            assert_eq!(
                reason,
                moli_i18n::tr!(
                    "pilotes.zigbee.autre_reseau",
                    known = format!("{:#06x}", known.pan_id),
                    dongle = "0x1a62"
                )
            );
            assert_eq!(run.order().await, not_ready());
            assert_eq!(run.store(), known_store(known), "store untouched");
        }
    }

    #[tokio::test]
    async fn an_unknown_network_without_key_is_not_adopted() {
        // With a vault holding no key, and without a vault at all.
        for vault in [true, false] {
            let (ncp, io) = forming_ncp(&[0x00]);
            let run = Running::with(
                io,
                &Setup {
                    vault,
                    ..Setup::default()
                },
            );
            let reason = resumed_wait(&ncp, &run).await;
            assert_eq!(
                reason,
                moli_i18n::tr!("pilotes.zigbee.reseau_inconnu", pan = "0x1a62")
            );
            assert_eq!(run.store(), Store::default(), "nothing written");
            assert!(!run.store_path().exists());
        }
    }

    #[tokio::test]
    async fn a_non_coordinator_is_not_adopted() {
        // Known or not, a dongle that joined as a router is never adopted.
        for store in [Some(known_store(record(0x1a62))), None] {
            let (ncp, io) = forming_ncp(&[0x00]);
            ncp.respond(
                ids::GET_NETWORK_PARAMETERS,
                &network_parameters_of(2, 0x1a62, 15),
            );
            let prepared = Prepared::new(&Setup {
                vault: true,
                store: store.clone(),
                ..Setup::default()
            });
            prepared
                .hub
                .store_secret(&prepared.instance, network::NETWORK_KEY, &"0".repeat(32))
                .await
                .unwrap();
            let run = Running::spawn(prepared, io, false);
            let reason = resumed_wait(&ncp, &run).await;
            assert_eq!(
                reason,
                moli_i18n::tr!("pilotes.zigbee.pas_coordinateur", pan = "0x1a62")
            );
            assert_eq!(run.store(), store.unwrap_or_default(), "store untouched");
        }
    }

    #[tokio::test]
    async fn an_interrupted_formation_is_adopted() {
        // The key reached the vault, the network the dongle, not the store.
        let (ncp, io) = forming_ncp(&[0x00]);
        let prepared = Prepared::new(&Setup {
            vault: true,
            ..Setup::default()
        });
        prepared
            .hub
            .store_secret(&prepared.instance, network::NETWORK_KEY, &"0".repeat(32))
            .await
            .unwrap();
        let run = Running::spawn(prepared, io, false);
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.store(), known_store(live(record(0x1a62))));
        assert_eq!(sent(&ncp, ids::FORM_NETWORK), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resumed_network_that_stays_down_is_ready_and_down() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::start(io, false);
        ncp.wait_received(STARTUP).await;
        // No stack status at all: after 10 s the coordinator is ready anyway.
        tokio::time::sleep(coordinator::NETWORK_UP_TIMEOUT + Duration::from_secs(1)).await;
        run.wait_ready().await;
        assert_eq!(run.point("network"), Some(text("down")));
        assert_eq!(sent(&ncp, ids::GET_NETWORK_PARAMETERS), None);
        assert_eq!(run.store(), Store::default());
        // No restart: a later stack status moves the point; the network is
        // then checked as at startup (here unknown, without a key: a person
        // decides, nothing written).
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_point("network", &text("up")).await;
        let reason = run.wait_waiting().await;
        assert_eq!(
            reason,
            moli_i18n::tr!("pilotes.zigbee.reseau_inconnu", pan = "0x1a62")
        );
        assert_eq!(run.store(), Store::default());
        assert_eq!(counter_reads(&ncp), 0);
    }

    /// The store's network with a counter recorded live by an earlier run.
    fn recorded_store(network: store::NetworkRecord) -> Store {
        known_store(store::NetworkRecord {
            frame_counter: Some(100_000),
            frame_counter_fresh: true,
            frame_counter_recorded_at: Some(NOW),
            key_sequence_number: Some(0),
            ..network
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_network_up_late_is_checked_then_recorded() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                store: Some(recorded_store(record(0x1a62))),
                ..Setup::default()
            },
        );
        ncp.wait_received(STARTUP).await;
        tokio::time::sleep(coordinator::NETWORK_UP_TIMEOUT + Duration::from_secs(1)).await;
        run.wait_ready().await;
        assert_eq!(run.point("network"), Some(text("down")));
        // Down, so unchecked and unrecorded: the counter is kept but no
        // longer restorable (the dongle may be sending meanwhile).
        let mut stale = recorded_store(record(0x1a62));
        stale.network.as_mut().unwrap().frame_counter_fresh = false;
        assert_eq!(run.store(), stale);
        assert_eq!(counter_reads(&ncp), 0);
        // Up later: checked (the store's network), its counter recorded at
        // once, fresh again.
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        wait_counter(&run, LIVE_COUNTER).await;
        assert_eq!(run.store(), known_store(live(record(0x1a62))));
        assert_eq!(counter_reads(&ncp), 1);
        assert_eq!(run.status(), Some(DriverStatus::Running));
        // Then every hour.
        ncp.respond(
            ids::GET_NETWORK_KEY_INFO,
            &key_info(0, LIVE_COUNTER + 7_000),
        );
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD).await;
        wait_counter(&run, LIVE_COUNTER + 7_000).await;
        assert!(run.store().network.unwrap().frame_counter_fresh);
        assert_eq!(counter_reads(&ncp), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn another_network_up_late_is_not_adopted() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let known = recorded_store(record(0x1234));
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                store: Some(known.clone()),
                ..Setup::default()
            },
        );
        ncp.wait_received(STARTUP).await;
        tokio::time::sleep(coordinator::NETWORK_UP_TIMEOUT + Duration::from_secs(1)).await;
        run.wait_ready().await;
        // The dongle reads as another network: the store's counter is not
        // its own, left as it is.
        assert_eq!(run.store(), known);
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        let reason = run.wait_waiting().await;
        assert_eq!(
            reason,
            moli_i18n::tr!(
                "pilotes.zigbee.autre_reseau",
                known = "0x1234",
                dongle = "0x1a62"
            )
        );
        assert_eq!(run.store(), known, "store untouched");
        assert_eq!(counter_reads(&ncp), 0);
        assert_eq!(sent(&ncp, ids::FORM_NETWORK), None);
    }

    #[tokio::test]
    async fn driver_reaches_ready() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::with(io, &Setup::known());
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.point("firmware"), Some(text("7.4.4.0")));
        assert_eq!(run.point("ezsp_version"), Some(Value::Int(13)));
        assert_eq!(run.point("network"), Some(text("up")));
        let coordinator = DeviceId::new(&run.instance, "coordinator");
        let (device, _) = run
            .hub
            .describe(&coordinator)
            .expect("the coordinator is published");
        let points: Vec<_> = device
            .points
            .iter()
            .map(|p| (&*p.key, p.access.write, p.semantic))
            .collect();
        // Opening pairing is an order: an agent asking needs a human's
        // approval first.
        assert_eq!(
            points,
            [
                ("firmware", false, Semantic::Config),
                ("ezsp_version", false, Semantic::Config),
                ("network", false, Semantic::Config),
                ("permit_join", true, Semantic::Control),
                ("joining", false, Semantic::Config),
            ]
        );
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
        assert_eq!(run.point("joining"), None, "nobody arrived yet");
        let after: Vec<_> = ncp.received()[STARTUP - 1..]
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            after,
            [
                ids::NETWORK_INIT,
                ids::GET_NETWORK_PARAMETERS,
                ids::GET_EUI64,
                ids::GET_NETWORK_KEY_INFO,
            ]
        );
    }

    #[tokio::test]
    async fn a_resumed_network_is_up_and_followed() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::with(io, &Setup::known());
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.point("network"), Some(text("up")));
        // Later stack statuses move the point.
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
        for _ in 0..500 {
            if run.point("network") == Some(text("down")) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("network still {:?}", run.point("network"));
    }

    /// The NCP of a known network, up and ready, also accepting what
    /// joining needs.
    async fn running_network() -> (FakeNcp, Running) {
        let (ncp, io) = forming_ncp(&[0x00]);
        ncp.respond(ids::IMPORT_TRANSIENT_KEY, &[0, 0, 0, 0]);
        ncp.respond(ids::CLEAR_TRANSIENT_LINK_KEYS, &[]);
        ncp.respond(ids::PERMIT_JOINING, &[0x00]);
        ncp.respond(ids::SEND_BROADCAST, &[0x00, 0x10]);
        let run = Running::with(io, &Setup::known());
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        (ncp, run)
    }

    /// The commands the NCP received since the first `from`.
    fn since(ncp: &FakeNcp, from: usize) -> Vec<(u16, Vec<u8>)> {
        ncp.received()[from..].to_vec()
    }

    #[tokio::test(start_paused = true)]
    async fn permit_join_opens_and_closes() {
        let (ncp, run) = running_network().await;
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
        let before = ncp.received().len();
        // An agent asking: a human approves first, nothing is sent.
        for agent in [Origin::Api, Origin::Mcp] {
            let asked = run.set_from(agent, "permit_join", Value::Bool(true)).await;
            assert!(
                matches!(asked, Err(CommandError::NeedsApproval { .. })),
                "{asked:?}"
            );
        }
        assert_eq!(ncp.received().len(), before);
        run.set("permit_join", Value::Bool(true)).await.unwrap();
        assert_eq!(run.point("permit_join"), Some(Value::Bool(true)));
        let opened = since(&ncp, before);
        let ids: Vec<_> = opened.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [
                ids::IMPORT_TRANSIENT_KEY,
                ids::SET_POLICY,
                ids::PERMIT_JOINING,
                ids::SEND_BROADCAST
            ]
        );
        assert_eq!(opened[2].1, [254], "the coordinator for 254 s");
        // … and every router, by Mgmt_Permit_Joining_req to 0xFFFC.
        let broadcast = &opened[3].1;
        assert_eq!(broadcast[..2], [0xFC, 0xFF]);
        assert_eq!(broadcast[broadcast.len() - 2..], [254, 1]);
        // It closes by itself after 254 s, without a word to the NCP.
        let after_open = ncp.received().len();
        tokio::time::sleep(Duration::from_secs(253)).await;
        assert_eq!(run.point("permit_join"), Some(Value::Bool(true)));
        tokio::time::sleep(Duration::from_secs(2)).await;
        run.wait_point("permit_join", &Value::Bool(false)).await;
        assert_eq!(ncp.received().len(), after_open);
        // Opened again, then closed at once.
        run.set("permit_join", Value::Bool(true)).await.unwrap();
        let before = ncp.received().len();
        run.set("permit_join", Value::Bool(false)).await.unwrap();
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
        let closed = since(&ncp, before);
        let ids: Vec<_> = closed.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [
                ids::CLEAR_TRANSIENT_LINK_KEYS,
                ids::SET_POLICY,
                ids::PERMIT_JOINING,
                ids::SEND_BROADCAST
            ]
        );
        assert_eq!(closed[2].1, [0]);
        let broadcast = &closed[3].1;
        assert_eq!(broadcast[broadcast.len() - 2..], [0, 1]);
        // The closing deadline of the second opening is gone with it.
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
    }

    #[tokio::test]
    async fn permit_join_needs_the_network() {
        let (ncp, run) = running_network().await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
        run.wait_point("network", &text("down")).await;
        let before = ncp.received().len();
        for open in [true, false] {
            assert_eq!(
                run.set("permit_join", Value::Bool(open)).await,
                Err(CommandError::Driver(moli_i18n::tr!(
                    "pilotes.zigbee.reseau_absent"
                )))
            );
        }
        assert_eq!(ncp.received().len(), before, "nothing sent");
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchecked_network_neither_pairs_nor_records() {
        let (ncp, io) = forming_ncp(&[0x00]);
        ncp.respond(ids::IMPORT_TRANSIENT_KEY, &[0, 0, 0, 0]);
        ncp.respond(ids::PERMIT_JOINING, &[0x00]);
        ncp.respond(ids::SEND_BROADCAST, &[0x00, 0x10]);
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                store: Some(recorded_store(record(0x1a62))),
                ..Setup::default()
            },
        );
        ncp.wait_received(STARTUP).await;
        tokio::time::sleep(coordinator::NETWORK_UP_TIMEOUT + Duration::from_secs(1)).await;
        run.wait_ready().await;
        let before = run.store();
        // Up later, but its check cannot read the dongle: not known to be
        // the store's network yet.
        ncp.respond(ids::GET_NETWORK_PARAMETERS, &[0x01, 0x00]);
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_point("network", &text("up")).await;
        let sent_before = ncp.received().len();
        for open in [true, false] {
            assert_eq!(
                run.set("permit_join", Value::Bool(open)).await,
                Err(CommandError::Driver(moli_i18n::tr!(
                    "pilotes.zigbee.reseau_non_verifie"
                )))
            );
        }
        assert_eq!(sent(&ncp, ids::PERMIT_JOINING), None, "never opened");
        assert_eq!(ncp.received().len(), sent_before, "nothing sent");
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
        // A device announcing itself meanwhile is shown, not recorded.
        let ieee = [0x02, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
        ncp.callback(
            ids::TRUST_CENTER_JOIN_HANDLER,
            &trust_center_join(0x1234, ieee, 1),
        );
        run.wait_point("joining", &text("0x00124b0000000002")).await;
        assert_eq!(run.store(), before, "nothing recorded");
        assert_eq!(counter_reads(&ncp), 0);
        // The check succeeds an hour later: ours, so pairing opens and
        // arrivals are recorded.
        ncp.respond(ids::GET_NETWORK_PARAMETERS, &network_parameters(0x1a62, 15));
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD).await;
        wait_counter(&run, LIVE_COUNTER).await;
        run.set("permit_join", Value::Bool(true)).await.unwrap();
        assert_eq!(run.point("permit_join"), Some(Value::Bool(true)));
        ncp.callback(
            ids::TRUST_CENTER_JOIN_HANDLER,
            &trust_center_join(0x1234, ieee, 1),
        );
        for _ in 0..500 {
            if run.store().devices.contains_key("0x00124b0000000002") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the arrival was never recorded: {:?}", run.store().devices);
    }

    #[tokio::test]
    async fn a_refused_permit_join_is_reported() {
        let (ncp, run) = running_network().await;
        // INVALID_CALL
        ncp.respond(ids::PERMIT_JOINING, &[0x70]);
        let Err(CommandError::Driver(reason)) = run.set("permit_join", Value::Bool(true)).await
        else {
            panic!("accepted");
        };
        assert!(
            reason.starts_with(&moli_i18n::tr!(
                "pilotes.zigbee.appairage_echec",
                error = ""
            )),
            "{reason}"
        );
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
        assert_eq!(run.status(), Some(DriverStatus::Running), "still running");
    }

    #[tokio::test]
    async fn a_failed_open_leaves_joining_closed() {
        let (ncp, run) = running_network().await;
        // The broadcast refused (NETWORK_BUSY) once the coordinator opened.
        ncp.respond(ids::SEND_BROADCAST, &[0xA1, 0x00]);
        let before = ncp.received().len();
        let Err(CommandError::Driver(reason)) = run.set("permit_join", Value::Bool(true)).await
        else {
            panic!("accepted");
        };
        assert!(reason.contains("sendBroadcast"), "{reason}");
        let after = since(&ncp, before);
        let ids: Vec<_> = after.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [
                ids::IMPORT_TRANSIENT_KEY,
                ids::SET_POLICY,
                ids::PERMIT_JOINING,
                ids::SEND_BROADCAST,
                ids::CLEAR_TRANSIENT_LINK_KEYS,
                ids::SET_POLICY,
                ids::PERMIT_JOINING,
            ]
        );
        assert_eq!(after[5].1, [0x00, 0x02], "rejoins only again");
        assert_eq!(after[6].1, [0], "the coordinator closed again");
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
    }

    #[test]
    fn only_permit_join_is_written() {
        assert_eq!(permit_order("permit_join", &Value::Bool(true)), Ok(true));
        assert_eq!(permit_order("permit_join", &Value::Bool(false)), Ok(false));
        let unsupported = Err(moli_i18n::tr!("pilotes.zigbee.ecriture_refusee"));
        assert_eq!(permit_order("permit_join", &Value::Int(1)), unsupported);
        assert_eq!(permit_order("network", &text("up")), unsupported);
        assert_eq!(permit_order("joining", &Value::Bool(true)), unsupported);
        assert_ne!(
            moli_i18n::tr!("pilotes.zigbee.ecriture_refusee"),
            moli_i18n::tr!("pilotes.zigbee.pas_pret")
        );
    }

    /// `trustCenterJoinHandler` for `ieee` (as it travels) at `nwk`.
    fn trust_center_join(nwk: u16, ieee: [u8; 8], status: u8) -> Vec<u8> {
        let mut params = nwk.to_le_bytes().to_vec();
        params.extend_from_slice(&ieee);
        // status, decision (preconfigured key), parent
        params.extend_from_slice(&[status, 0x00, 0x00, 0x00]);
        params
    }

    /// A v13 `incomingMessageHandler` carrying a device announce.
    fn device_announce(nwk: u16, ieee: [u8; 8]) -> Vec<u8> {
        let mut params = vec![0x04]; // BROADCAST
        crate::zdo::frame(crate::zdo::DEVICE_ANNOUNCE).encode(&mut params);
        params.push(200); // lqi
        params.push(0xC4); // rssi
        params.extend_from_slice(&nwk.to_le_bytes());
        params.extend_from_slice(&[0xFF, 0xFF]);
        let mut zdo = vec![0x81];
        zdo.extend_from_slice(&nwk.to_le_bytes());
        zdo.extend_from_slice(&ieee);
        zdo.push(0x80);
        params.push(u8::try_from(zdo.len()).unwrap());
        params.extend_from_slice(&zdo);
        params
    }

    #[tokio::test]
    async fn join_creates_record() {
        let (ncp, run) = running_network().await;
        // 0x00124b0000000002
        let ieee = [0x02, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
        let name = "0x00124b0000000002";
        ncp.callback(
            ids::TRUST_CENTER_JOIN_HANDLER,
            &trust_center_join(0x1234, ieee, 1),
        );
        run.wait_point("joining", &text(name)).await;
        let device = store::DeviceRecord {
            ieee: name.into(),
            nwk: Some(0x1234),
            interviewed: false,
        };
        assert_eq!(run.store().devices.get(name), Some(&device));
        assert_eq!(
            run.store().network,
            Some(live(record(0x1a62))),
            "network kept"
        );
        // Its announce later, from a new address.
        ncp.callback(
            ids::INCOMING_MESSAGE_HANDLER,
            &device_announce(0x5678, ieee),
        );
        for _ in 0..500 {
            if run.store().devices[name].nwk == Some(0x5678) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(run.store().devices[name].nwk, Some(0x5678));
        // Another device; then the first one leaves: kept in the store (v1).
        let other = [0x03, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
        ncp.callback(
            ids::TRUST_CENTER_JOIN_HANDLER,
            &trust_center_join(0x2222, other, 1),
        );
        run.wait_point("joining", &text("0x00124b0000000003")).await;
        ncp.callback(
            ids::TRUST_CENTER_JOIN_HANDLER,
            &trust_center_join(0x5678, ieee, 2),
        );
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
        run.wait_point("network", &text("down")).await;
        let devices = run.store().devices;
        assert_eq!(devices.len(), 2);
        assert!(devices.contains_key(name));
        assert_eq!(run.point("joining"), Some(text("0x00124b0000000003")));
    }

    /// How many times the NCP was asked for its key's frame counter.
    fn counter_reads(ncp: &FakeNcp) -> usize {
        ncp.received()
            .iter()
            .filter(|(id, _)| *id == ids::GET_NETWORK_KEY_INFO)
            .count()
    }

    /// Waits (5 s) for the NCP to have been asked `count` times.
    async fn wait_reads(ncp: &FakeNcp, count: usize) {
        for _ in 0..500 {
            if counter_reads(ncp) >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{} counter reads, not {count}", counter_reads(ncp));
    }

    /// Waits (5 s) for the store's network to record `counter`.
    async fn wait_counter(run: &Running, counter: u32) {
        for _ in 0..500 {
            if run.store().network.and_then(|n| n.frame_counter) == Some(counter) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the store still records {:?}", run.store().network);
    }

    #[tokio::test]
    async fn counter_is_recorded_after_form() {
        let (ncp, io) = forming_ncp(&[0x93]);
        ncp.respond(ids::GET_NETWORK_KEY_INFO, &key_info(0, 42));
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                ..Setup::default()
            },
        );
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        // Read once the network is up, before the coordinator is ready.
        let last = ncp.received().last().map(|(id, _)| *id);
        assert_eq!(last, Some(ids::GET_NETWORK_KEY_INFO));
        assert_eq!(counter_reads(&ncp), 1);
        let network = run.store().network.unwrap();
        assert_eq!(network.frame_counter, Some(42));
        assert!(network.frame_counter_fresh, "restorable");
        assert_eq!(network.key_sequence_number, Some(0));
    }

    #[tokio::test(start_paused = true)]
    async fn counter_is_recorded_hourly() {
        let (ncp, run) = running_network().await;
        assert_eq!(run.store().network, Some(live(record(0x1a62))));
        assert_eq!(counter_reads(&ncp), 1, "read once at the start");
        ncp.respond(
            ids::GET_NETWORK_KEY_INFO,
            &key_info(0, LIVE_COUNTER + 5_000),
        );
        tokio::time::sleep(Duration::from_secs(3599)).await;
        assert_eq!(counter_reads(&ncp), 1, "not before the hour");
        tokio::time::sleep(Duration::from_secs(2)).await;
        wait_counter(&run, LIVE_COUNTER + 5_000).await;
        assert_eq!(counter_reads(&ncp), 2);
        assert!(run.store().network.unwrap().frame_counter_fresh);
        // A read the NCP does not answer: the callbacks are still handled
        // while it waits (a device arriving meanwhile is recorded); it
        // fails, the counter stays, and is read again an hour later.
        ncp.silence(ids::GET_NETWORK_KEY_INFO);
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD).await;
        wait_reads(&ncp, 3).await;
        let asked = tokio::time::Instant::now();
        let ieee = [0x02, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
        ncp.callback(
            ids::TRUST_CENTER_JOIN_HANDLER,
            &trust_center_join(0x1234, ieee, 1),
        );
        run.wait_point("joining", &text("0x00124b0000000002")).await;
        assert!(
            asked.elapsed() < Duration::from_secs(1),
            "handled while the read was pending"
        );
        assert!(run.store().devices.contains_key("0x00124b0000000002"));
        // Refused, too: kept.
        ncp.respond(ids::GET_NETWORK_KEY_INFO, &KEY_INFO_REFUSED);
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD).await;
        wait_reads(&ncp, 4).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let network = run.store().network.unwrap();
        assert_eq!(network.frame_counter, Some(LIVE_COUNTER + 5_000));
        assert!(network.frame_counter_fresh);
        assert_eq!(run.status(), Some(DriverStatus::Running), "still running");
        ncp.respond(
            ids::GET_NETWORK_KEY_INFO,
            &key_info(0, LIVE_COUNTER + 9_000),
        );
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD).await;
        wait_counter(&run, LIVE_COUNTER + 9_000).await;
        // The network down: nothing read.
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
        run.wait_point("network", &text("down")).await;
        let reads = counter_reads(&ncp);
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD * 2).await;
        assert_eq!(counter_reads(&ncp), reads);
    }

    /// Stops Moli (the supervisor's cancellation) and waits for the driver
    /// to be stopped; returns how long that took.
    async fn stop(run: &Running) -> Duration {
        let asked = tokio::time::Instant::now();
        run.cancel.cancel();
        run.wait_status(|status| (*status == DriverStatus::Stopped).then_some(()))
            .await;
        asked.elapsed()
    }

    #[tokio::test(start_paused = true)]
    async fn a_clean_stop_closes_joining_and_records_the_counter() {
        let (ncp, run) = running_network().await;
        run.set("permit_join", Value::Bool(true)).await.unwrap();
        ncp.respond(
            ids::GET_NETWORK_KEY_INFO,
            &key_info(0, LIVE_COUNTER + 3_000),
        );
        let before = ncp.received().len();
        stop(&run).await;
        // Joining closed on the dongle and the routers, then the counter
        // read a last time: an export right after starts from it.
        let after = since(&ncp, before);
        let ids: Vec<_> = after.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [
                ids::CLEAR_TRANSIENT_LINK_KEYS,
                ids::SET_POLICY,
                ids::PERMIT_JOINING,
                ids::SEND_BROADCAST,
                ids::GET_NETWORK_KEY_INFO,
            ]
        );
        assert_eq!(after[2].1, [0], "closed");
        let network = run.store().network.unwrap();
        assert_eq!(network.frame_counter, Some(LIVE_COUNTER + 3_000));
        assert!(network.frame_counter_fresh);
        assert_eq!(network.frame_counter_recorded_at, Some(NOW));
        assert_eq!(run.point("permit_join"), Some(Value::Bool(false)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_never_hangs_nor_touches_an_unchecked_network() {
        // An NCP that no longer answers: the stop gives up in time, the
        // recorded counter kept.
        let (ncp, run) = running_network().await;
        ncp.silence(ids::GET_NETWORK_KEY_INFO);
        let took = stop(&run).await;
        assert!(took <= STOP_TIMEOUT + Duration::from_secs(1), "{took:?}");
        assert_eq!(counter_reads(&ncp), 2, "tried once more");
        assert_eq!(run.store(), known_store(live(record(0x1a62))));
        // A network that is down (or not checked): nothing sent at all.
        let (ncp, run) = running_network().await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
        run.wait_point("network", &text("down")).await;
        let before = ncp.received().len();
        stop(&run).await;
        assert_eq!(ncp.received().len(), before, "nothing sent");
    }

    /// Fails if a file under `dir` holds any of `keys` (hex, either case).
    fn assert_no_key_under(dir: &Path, keys: &[&str]) {
        let mut seen = 0;
        let mut todo = vec![dir.to_path_buf()];
        while let Some(path) = todo.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    todo.push(entry.unwrap().path());
                }
                continue;
            }
            seen += 1;
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes).to_lowercase();
            for key in keys {
                assert_eq!(key.len(), 32);
                let raw: Vec<u8> = (0..16)
                    .map(|i| u8::from_str_radix(&key[2 * i..2 * i + 2], 16).unwrap())
                    .collect();
                assert!(
                    !text.contains(&key.to_lowercase()),
                    "{} holds a key",
                    path.display()
                );
                assert!(
                    !bytes.windows(16).any(|w| w == raw.as_slice()),
                    "{} holds a key",
                    path.display()
                );
            }
        }
        assert!(seen >= 2, "the store and the vault at least");
    }

    #[tokio::test(start_paused = true)]
    async fn no_key_at_rest() {
        // Restored from an import, then recorded live and hourly.
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = restored(&ncp, io).await;
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD + Duration::from_secs(1)).await;
        wait_reads(&ncp, 2).await;
        let tclk = run.hub.secret(&run.instance, network::TC_LINK_KEY).unwrap();
        assert!(run.dir.0.join("secrets.enc").exists());
        assert_no_key_under(&run.dir.0, &[IMPORTED_KEY, &tclk]);
        drop(run);
        // Formed, then recorded live and hourly.
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                ..Setup::default()
            },
        );
        ncp.wait_command(ids::FORM_NETWORK).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        tokio::time::sleep(backup::COUNTER_RECORD_PERIOD + Duration::from_secs(1)).await;
        wait_reads(&ncp, 2).await;
        let key = run.hub.secret(&run.instance, network::NETWORK_KEY).unwrap();
        let tclk = run.hub.secret(&run.instance, network::TC_LINK_KEY).unwrap();
        assert_eq!(run.store().network, Some(live(record(0x1a62))));
        assert_no_key_under(&run.dir.0, &[&key, &tclk]);
    }

    #[tokio::test]
    async fn old_firmware_waits_with_reason() {
        let (_ncp, io) = FakeNcp::spawn(EMBER_OLD);
        let run = Running::start(io, true);
        let reason = run
            .wait_status(|status| match status {
                DriverStatus::Waiting { reason } if reason.contains("EZSP") => Some(reason.clone()),
                _ => None,
            })
            .await;
        assert_eq!(
            reason,
            moli_i18n::tr!("pilotes.zigbee.firmware_ancien", version = 8)
        );
        assert!(reason.contains("EZSP 8"), "{reason}");
        // It stays there (no restart loop), refusing orders.
        assert_eq!(run.order().await, not_ready());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(run.status(), Some(DriverStatus::Waiting { reason }));
    }

    #[tokio::test]
    async fn commands_before_ready_are_refused() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        ncp.respond(ids::ADD_ENDPOINT, &[0x00]);
        ncp.respond(ids::SET_POLICY, &[0x00]);
        // networkInit never answered: the driver stays in its startup.
        let run = Running::start(io, true);
        ncp.wait_received(STARTUP).await;
        assert_ne!(run.status(), Some(DriverStatus::Running));
        assert_eq!(run.order().await, not_ready());
    }

    #[tokio::test]
    async fn a_lost_link_ends_the_run() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::with(io, &Setup::known());
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        drop(ncp);
        let error = run
            .wait_status(|status| match status {
                DriverStatus::Backoff { error, .. } => Some(error.clone()),
                _ => None,
            })
            .await;
        assert!(error.contains("link to the NCP lost"), "{error}");
    }

    #[tokio::test]
    async fn a_lost_link_while_waiting_ends_the_run() {
        let (ncp, io) = ncp_with_init(&[0x93]);
        let run = Running::start(io, false);
        run.wait_waiting().await;
        drop(ncp);
        let error = run
            .wait_status(|status| match status {
                DriverStatus::Backoff { error, .. } => Some(error.clone()),
                _ => None,
            })
            .await;
        assert!(error.contains("link to the NCP lost"), "{error}");
    }

    fn config(json: &str) -> Result<Config, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn config_defaults() {
        let c = config(r#"{"port":"/dev/ttyUSB0"}"#).unwrap();
        assert_eq!(c.port, "/dev/ttyUSB0");
        assert_eq!(c.baud, 115_200);
        assert_eq!(c.flow, Flow::Software);
        assert_eq!(c.channel, None);
    }

    #[test]
    fn config_rejects_unknown_fields() {
        assert!(config(r#"{"port":"x","bauds":1}"#).is_err());
    }

    #[test]
    fn tcp_port_is_accepted() {
        let c = config(r#"{"port":"tcp://192.168.1.x:6638"}"#).unwrap();
        assert_eq!(c.port, "tcp://192.168.1.x:6638");
    }

    #[test]
    fn flow_and_channel_are_read() {
        let c = config(r#"{"port":"x","flow":"hardware","channel":15,"baud":460800}"#).unwrap();
        assert_eq!(c.flow, Flow::Hardware);
        assert_eq!(c.channel, Some(15));
        assert_eq!(c.baud, 460_800);
        assert_eq!(
            config(r#"{"port":"x","flow":"none"}"#).unwrap().flow,
            Flow::None
        );
        assert!(config(r#"{"port":"x","flow":"rts"}"#).is_err());
    }

    #[test]
    fn a_channel_outside_zigbee_stops_the_start() {
        for channel in [11, 26] {
            let c = config(&format!(r#"{{"port":"x","channel":{channel}}}"#)).unwrap();
            assert_eq!(c.channel, Some(channel));
        }
        for channel in [0, 10, 27] {
            let error = config(&format!(r#"{{"port":"x","channel":{channel}}}"#))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!("channel {channel} is not a Zigbee channel")),
                "{error}"
            );
        }
    }

    #[test]
    fn kind_is_zigbee() {
        let z = Zigbee::new(config(r#"{"port":"x"}"#).unwrap(), Path::new("/tmp"));
        assert_eq!(z.kind(), "zigbee");
    }
}

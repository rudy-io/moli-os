//! Native Zigbee driver: talks EZSP to a Silicon Labs coordinator dongle
//! (serial or TCP), no Zigbee2MQTT and no broker.
//!
//! Dormant: nothing runs unless `[[driver]] kind = "zigbee"` is in `moli.toml`.

pub mod ash;
pub mod coordinator;
pub mod ezsp;
pub mod network;
#[cfg(test)]
mod sim;
pub mod store;
pub mod transport;

use std::path::{Path, PathBuf};

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::{Deserialize, Deserializer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::coordinator::{Init, Network};
use crate::ezsp::{Callback, Ezsp, EzspFailure, Version};
use crate::store::Store;

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
    run_on(config, data_dir, ctx, io).await
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
    let network = match init {
        Init::Resumed => {
            refusing_orders(
                ctx,
                resume(&ezsp, &mut callbacks, protocol, &mut store, &store_path),
            )
            .await?
        }
        Init::NotJoined => {
            match first_network(
                config,
                ctx,
                &ezsp,
                &mut callbacks,
                protocol,
                &mut store,
                &store_path,
            )
            .await?
            {
                Ok(network) => network,
                Err(reason) => {
                    ctx.set_state(&coordinator, "network", network_value(Network::None));
                    return park(ctx, &ezsp, &mut callbacks, reason).await;
                }
            }
        }
    };
    ctx.set_state(&coordinator, "network", network_value(network));
    tracing::info!(
        firmware = %version.display(),
        protocol,
        network = network.as_str(),
        "zigbee coordinator ready"
    );
    ctx.ready();
    loop {
        tokio::select! {
            callback = callbacks.recv() => match callback {
                // Read continuously: the client drops what is not read in time.
                Some(callback) => {
                    if let Some(network) = coordinator::network_change(&callback, protocol) {
                        ctx.set_state(&coordinator, "network", network_value(network));
                    }
                }
                None => return Err(coordinator::link_error(&ezsp).await),
            },
            order = ctx.next_command() => match order {
                // No writable point yet: devices arrive with the network.
                Some(order) => refuse(order),
                None => return Ok(()),
            },
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

/// No network on the dongle: form one (`Ok`, up), or why a person is
/// needed (`Err`): no vault, or a network the store knows (to restore,
/// never to form over: that would orphan every paired device).
async fn first_network(
    config: &Config,
    ctx: &mut DriverCtx,
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    protocol: u8,
    store: &mut Store,
    store_path: &Path,
) -> anyhow::Result<Result<Network, String>> {
    if let Some(known) = &store.network {
        tracing::warn!(
            pan_id = format_args!("{:#06x}", known.pan_id),
            "the dongle has no network but Moli knows one"
        );
        return Ok(Err(moli_i18n::tr!(
            "pilotes.zigbee.restauration_a_venir",
            pan = format!("{:#06x}", known.pan_id)
        )));
    }
    if !ctx.can_store_secrets() {
        tracing::warn!("no vault: the network is not formed");
        return Ok(Err(moli_i18n::tr!(
            "pilotes.zigbee.coffre_indisponible",
            problem = ctx.secrets_problem().unwrap_or_default()
        )));
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
    Ok(Ok(Network::Up))
}

/// A network the dongle kept: wait for it to come up, then record what it
/// is in the store. Not up within [`coordinator::NETWORK_UP_TIMEOUT`]: the
/// coordinator is still ready, its network « down » (no restart loop; the
/// stack status that follows moves the point).
async fn resume(
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    protocol: u8,
    store: &mut Store,
    store_path: &Path,
) -> anyhow::Result<Network> {
    let network =
        coordinator::wait_network_up(ezsp, callbacks, protocol, coordinator::NETWORK_UP_TIMEOUT)
            .await?;
    if network != Network::Up {
        return Ok(network);
    }
    let record = network::read_network(ezsp, protocol).await?;
    if store.network.as_ref() != Some(&record) {
        if let Some(known) = &store.network
            && (known.pan_id, known.extended_pan_id) != (record.pan_id, record.extended_pan_id)
        {
            tracing::warn!(
                known = format_args!("{:#06x}", known.pan_id),
                dongle = format_args!("{:#06x}", record.pan_id),
                "the dongle runs another network than the one Moli knew; following the dongle"
            );
        }
        store.network = Some(record);
        store.save(store_path)?;
    }
    Ok(network)
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

fn refuse(order: CommandRequest) {
    order.reply(Err(moli_i18n::tr!("pilotes.zigbee.pas_pret")));
}

fn network_value(network: Network) -> Value {
    Value::Text(network.as_str().into())
}

/// The coordinator as a Moli device: what the dongle runs, read-only.
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
    use crate::network::tests::{EUI64, network_parameters};
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
    /// configuration, the trust center policies, `networkInit`.
    const STARTUP: usize =
        2 + coordinator::STACK_CONFIG.len() + coordinator::TRUST_CENTER_POLICIES.len() + 1;

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
                run_on(&self.config, &self.data_dir, ctx, io).await
            })
        }
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
        ncp.respond(ids::SET_POLICY, &[0x00]);
        ncp.respond(ids::NETWORK_INIT, init);
        (ncp, io)
    }

    /// … that also forms, and reports the network it runs.
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
        // The store has the network, never the key.
        let saved = run.store();
        assert_eq!(saved.network, Some(record(0x1a62)));
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

    #[tokio::test]
    async fn a_known_network_is_not_formed_over() {
        let (ncp, io) = forming_ncp(&[0x93]);
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                store: Some(Store {
                    network: Some(record(0x1a62)),
                    ..Store::default()
                }),
                ..Setup::default()
            },
        );
        let reason = run.wait_waiting().await;
        assert_eq!(
            reason,
            moli_i18n::tr!("pilotes.zigbee.restauration_a_venir", pan = "0x1a62")
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(ncp.received().len(), STARTUP);
        assert_eq!(run.hub.secret(&run.instance, network::NETWORK_KEY), None);
    }

    #[tokio::test]
    async fn resumes_existing_network() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::with(
            io,
            &Setup {
                vault: true,
                store: Some(Store {
                    network: Some(record(0x1234)),
                    ..Store::default()
                }),
                ..Setup::default()
            },
        );
        ncp.wait_received(STARTUP).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        run.wait_ready().await;
        assert_eq!(run.point("network"), Some(text("up")));
        assert_eq!(run.store().network, Some(record(0x1a62)));
        assert_eq!(sent(&ncp, ids::FORM_NETWORK), None);
        assert_eq!(sent(&ncp, ids::SET_INITIAL_SECURITY_STATE), None);
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
        // No restart: a later stack status moves the point.
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        for _ in 0..500 {
            if run.point("network") == Some(text("up")) {
                assert_eq!(run.status(), Some(DriverStatus::Running));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("network still {:?}", run.point("network"));
    }

    #[tokio::test]
    async fn driver_reaches_ready() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::start(io, false);
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
            .map(|p| (&*p.key, p.access.write))
            .collect();
        assert_eq!(
            points,
            [
                ("firmware", false),
                ("ezsp_version", false),
                ("network", false)
            ]
        );
        let after: Vec<_> = ncp.received()[STARTUP - 1..]
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            after,
            [
                ids::NETWORK_INIT,
                ids::GET_NETWORK_PARAMETERS,
                ids::GET_EUI64
            ]
        );
    }

    #[tokio::test]
    async fn a_resumed_network_is_up_and_followed() {
        let (ncp, io) = forming_ncp(&[0x00]);
        let run = Running::start(io, false);
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
        let run = Running::start(io, false);
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

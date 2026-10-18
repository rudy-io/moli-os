//! Native Zigbee driver: talks EZSP to a Silicon Labs coordinator dongle
//! (serial or TCP), no Zigbee2MQTT and no broker.
//!
//! Dormant: nothing runs unless `[[driver]] kind = "zigbee"` is in `moli.toml`.

pub mod ash;
pub mod coordinator;
pub mod ezsp;
#[cfg(test)]
mod sim;
pub mod transport;

use std::path::{Path, PathBuf};

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::coordinator::Network;
use crate::ezsp::{Callback, Ezsp, EzspFailure, Version};

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
    /// Zigbee channel (11 to 26) of a new network; chosen by the driver if unset.
    #[serde(default)]
    pub channel: Option<u8>,
}

fn default_baud() -> u32 {
    115_200
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
    run_on(config, ctx, io).await
}

/// What the startup brings back.
struct Started {
    ezsp: Ezsp,
    version: Version,
    callbacks: mpsc::Receiver<Callback>,
    network: Network,
}

/// Everything after the port: connect, configure and init the NCP, publish
/// the coordinator, then follow the link until it ends (an error: the
/// supervisor restarts the driver) or Moli stops.
///
/// Orders are refused until the coordinator is ready. A firmware too old
/// for EZSP 13 waits for a person (no restart loop: only a new firmware
/// helps).
async fn run_on<T>(_config: &Config, ctx: &mut DriverCtx, io: T) -> anyhow::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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
        network,
    } = started;
    let coordinator = ctx.device_id("coordinator");
    ctx.upsert_device(coordinator_device(ctx, &coordinator));
    ctx.set_state(
        &coordinator,
        "firmware",
        Value::Text(version.display().into()),
    );
    ctx.set_state(
        &coordinator,
        "ezsp_version",
        Value::Int(version.protocol.into()),
    );
    ctx.set_state(&coordinator, "network", network_value(network));
    tracing::info!(
        firmware = %version.display(),
        protocol = version.protocol,
        network = network.as_str(),
        "zigbee coordinator ready"
    );
    ctx.ready();
    loop {
        tokio::select! {
            callback = callbacks.recv() => match callback {
                // Read continuously: the client drops what is not read in time.
                Some(callback) => {
                    if let Some(network) = coordinator::network_change(&callback, version.protocol) {
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

/// Connect, configure, `networkInit`, and the state of the network.
async fn start<T>(io: T) -> anyhow::Result<Started>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (ezsp, version, mut callbacks) = Ezsp::connect(io).await?;
    let network = match coordinator::start_ncp(&ezsp, &version).await? {
        // Phase 2 forms it.
        coordinator::Init::NotJoined => Network::None,
        coordinator::Init::Resumed => {
            coordinator::wait_network_up(
                &ezsp,
                &mut callbacks,
                version.protocol,
                coordinator::NETWORK_UP_TIMEOUT,
            )
            .await?
        }
    };
    Ok(Started {
        ezsp,
        version,
        callbacks,
        network,
    })
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
    use moli_runtime::{CommandError, Hub, HubOptions, spawn_driver};
    use tokio::io::DuplexStream;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::ezsp::ids;
    use crate::sim::FakeNcp;

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
    /// configuration, `networkInit`.
    const STARTUP: usize = 2 + coordinator::STACK_CONFIG.len() + 1;

    /// The driver on a simulated link, run by the real supervisor; with
    /// `probe`, it first publishes a writable point to send orders to.
    struct Simulated {
        config: Config,
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
                run_on(&self.config, ctx, io).await
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

    /// A driver running on a hub; stopped when dropped.
    struct Running {
        hub: Hub,
        instance: InstanceId,
        cancel: CancellationToken,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.cancel.cancel();
        }
    }

    impl Running {
        fn start(io: DuplexStream, probe: bool) -> Self {
            let hub = Hub::new(HubOptions::default()).unwrap();
            let instance = InstanceId::from("zigbee");
            let cancel = CancellationToken::new();
            let driver = Simulated {
                config: config(r#"{"port":"/dev/ttyUSB0"}"#).unwrap(),
                io: Mutex::new(Some(io)),
                probe,
            };
            spawn_driver(&hub, instance.clone(), Arc::new(driver), cancel.clone());
            Self {
                hub,
                instance,
                cancel,
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

        fn point(&self, key: &str) -> Option<Value> {
            let device = DeviceId::new(&self.instance, "coordinator");
            self.hub
                .state(&PointId::new(&device, key))
                .map(|sample| sample.value)
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

    #[tokio::test]
    async fn driver_reaches_ready() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        // NOT_JOINED: no network on the dongle yet.
        ncp.respond(ids::NETWORK_INIT, &[0x93]);
        let run = Running::start(io, false);
        run.wait_ready().await;
        assert_eq!(run.point("firmware"), Some(text("7.4.4.0")));
        assert_eq!(run.point("ezsp_version"), Some(Value::Int(13)));
        assert_eq!(run.point("network"), Some(text("none")));
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
        assert_eq!(ncp.received().len(), STARTUP);
        assert_eq!(ncp.received()[STARTUP - 1].0, ids::NETWORK_INIT);
    }

    #[tokio::test]
    async fn a_resumed_network_is_up_and_followed() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        ncp.respond(ids::NETWORK_INIT, &[0x00]);
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
        // networkInit never answered: the driver stays in its startup.
        let run = Running::start(io, true);
        ncp.wait_received(STARTUP).await;
        assert_ne!(run.status(), Some(DriverStatus::Running));
        assert_eq!(run.order().await, not_ready());
    }

    #[tokio::test]
    async fn a_lost_link_ends_the_run() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        ncp.respond(ids::NETWORK_INIT, &[0x93]);
        let run = Running::start(io, false);
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
    fn kind_is_zigbee() {
        let z = Zigbee::new(config(r#"{"port":"x"}"#).unwrap(), Path::new("/tmp"));
        assert_eq!(z.kind(), "zigbee");
    }
}

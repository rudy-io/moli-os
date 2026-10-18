//! Coordinator startup: the stack configuration a coordinator needs, then
//! `networkInit`, which resumes the network the NCP keeps in its tokens.
//!
//! The values and their order follow zigbee-herdsman (MIT),
//! `src/adapter/ember/adapter/emberAdapter.ts` (`initEzsp`, `initTrustCenter`).

use std::time::Duration;

use anyhow::bail;
use tokio::sync::mpsc;

use crate::ezsp::{
    Callback, Ezsp, EzspFailure, SlStatus, Version, ids, read_ezsp_status, read_status,
};

/// How long `networkInit` gets to bring the network up once it accepted:
/// `DEFAULT_NETWORK_REQUEST_TIMEOUT` of herdsman's ember adapter.
pub const NETWORK_UP_TIMEOUT: Duration = Duration::from_secs(10);

/// `EmberNetworkInitStruct.bitmask` herdsman passes to `networkInit`:
/// `PARENT_INFO_IN_TOKEN | END_DEVICE_REJOIN_ON_REBOOT` (`enums.ts`,
/// `EmberNetworkInitBitmask`).
const NETWORK_INIT_BITMASK: u16 = 0x0001 | 0x0002;

/// One `setConfigurationValue`: the `EzspConfigId`, its name for the logs,
/// and the value (`uint16_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigValue {
    pub id: u8,
    pub name: &'static str,
    pub value: u16,
}

/// What herdsman's ember adapter sets on a coordinator before anything else
/// (`initEzsp`, in this order), with its default values (`DEFAULT_STACK_CONFIG`
/// and the constants of `emberAdapter.ts` / `consts.ts`) and the ids of
/// `EzspConfigId` (`src/adapter/ember/ezsp/enums.ts`).
///
/// Table sizes (addresses, keys, source routes, packet buffers) are not
/// among them: herdsman keeps the ones the firmware was built with, since
/// they can only change right after a reset and cost RAM the build planned.
pub const STACK_CONFIG: [ConfigValue; 9] = [
    // `TRUST_CENTER_ADDRESS_CACHE_SIZE` const: the trust center needs the
    // address cache for source routing.
    ConfigValue {
        id: 0x19,
        name: "TRUST_CENTER_ADDRESS_CACHE_SIZE",
        value: 2,
    },
    // MAC indirect timeout of 7.68 s (Zigbee PRO stack profile).
    ConfigValue {
        id: 0x12,
        name: "INDIRECT_TRANSMISSION_TIMEOUT",
        value: 7680,
    },
    // 2 * nwkMaxDepth, which is 15 for Zigbee PRO.
    ConfigValue {
        id: 0x10,
        name: "MAX_HOPS",
        value: 30,
    },
    // A single network on the NCP.
    ConfigValue {
        id: 0x2d,
        name: "SUPPORTED_NETWORKS",
        value: 1,
    },
    // `STACK_PROFILE_ZIGBEE_PRO` (`consts.ts`).
    ConfigValue {
        id: 0x0c,
        name: "STACK_PROFILE",
        value: 2,
    },
    // `SECURITY_LEVEL_Z3` (`consts.ts`).
    ConfigValue {
        id: 0x0d,
        name: "SECURITY_LEVEL",
        value: 5,
    },
    // `DEFAULT_STACK_CONFIG.MAX_END_DEVICE_CHILDREN`.
    ConfigValue {
        id: 0x11,
        name: "MAX_END_DEVICE_CHILDREN",
        value: 32,
    },
    // `DEFAULT_STACK_CONFIG.END_DEVICE_POLL_TIMEOUT`.
    ConfigValue {
        id: 0x13,
        name: "END_DEVICE_POLL_TIMEOUT",
        value: 8,
    },
    // `DEFAULT_STACK_CONFIG.TRANSIENT_KEY_TIMEOUT_S`.
    ConfigValue {
        id: 0x36,
        name: "TRANSIENT_KEY_TIMEOUT_S",
        value: 300,
    },
];

/// How `networkInit` answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Init {
    /// The NCP had a network and is bringing it up (`stackStatusHandler`
    /// follows).
    Resumed,
    /// The NCP has no network yet: one must be formed.
    NotJoined,
}

/// The coordinator's `network` point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Up,
    Down,
    /// No network on the dongle yet.
    None,
}

impl Network {
    /// The point's values, in this order.
    pub const VALUES: [&'static str; 3] = ["up", "down", "none"];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => Self::VALUES[0],
            Self::Down => Self::VALUES[1],
            Self::None => Self::VALUES[2],
        }
    }
}

/// Configure the stack ([`STACK_CONFIG`]; a value the NCP refuses is
/// logged and its own kept), then `networkInit`.
///
/// # Errors
///
/// The link failing or a command timing out, an unreadable answer, or
/// `networkInit` failing otherwise than « not joined ».
pub async fn start_ncp(ezsp: &Ezsp, version: &Version) -> anyhow::Result<Init> {
    let protocol = version.protocol;
    for config in STACK_CONFIG {
        let mut params = vec![config.id];
        params.extend_from_slice(&config.value.to_le_bytes());
        let answer = ezsp.call(ids::SET_CONFIGURATION_VALUE, &params).await?;
        let (status, _) = read_ezsp_status(&answer, protocol)?;
        if status == SlStatus::Ok {
            tracing::debug!(
                config = config.name,
                value = config.value,
                "EZSP configuration set"
            );
        } else {
            tracing::warn!(
                config = config.name,
                value = config.value,
                ?status,
                "the NCP refused a configuration value, its own is kept"
            );
        }
    }
    let answer = ezsp
        .call(ids::NETWORK_INIT, &NETWORK_INIT_BITMASK.to_le_bytes())
        .await?;
    match read_status(&answer, protocol)?.0 {
        SlStatus::Ok => Ok(Init::Resumed),
        SlStatus::NotJoined => Ok(Init::NotJoined),
        status => bail!("networkInit failed: {status:?}"),
    }
}

/// What a callback says of the network: `Some` for a `stackStatusHandler`
/// reporting it up or down; anything else is only logged.
#[must_use]
pub fn network_change(callback: &Callback, protocol: u8) -> Option<Network> {
    if callback.id != ids::STACK_STATUS_HANDLER {
        tracing::debug!(
            id = format_args!("{:#06x}", callback.id),
            len = callback.params.len(),
            "EZSP callback"
        );
        return None;
    }
    match read_status(&callback.params, protocol) {
        Ok((SlStatus::NetworkUp, _)) => Some(Network::Up),
        Ok((SlStatus::NetworkDown, _)) => Some(Network::Down),
        Ok((status, _)) => {
            tracing::debug!(?status, "stack status");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "unreadable stack status");
            None
        }
    }
}

/// After a `networkInit` that resumed: wait (reading every callback) for
/// the stack to report the network up. `Down` after [`NETWORK_UP_TIMEOUT`]
/// without it, or if it reports the network down.
///
/// # Errors
///
/// The link ended (the failure that ended it).
pub async fn wait_network_up(
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    protocol: u8,
    timeout: Duration,
) -> anyhow::Result<Network> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, callbacks.recv()).await {
            Err(_) => {
                tracing::warn!(?timeout, "the network did not come up after networkInit");
                return Ok(Network::Down);
            }
            Ok(None) => return Err(link_error(ezsp).await),
            Ok(Some(callback)) => {
                if let Some(network) = network_change(&callback, protocol) {
                    return Ok(network);
                }
            }
        }
    }
}

/// Why the link ended, once its callback channel closed: the client fails
/// every later command at once with that reason.
pub async fn link_error(ezsp: &Ezsp) -> anyhow::Error {
    match ezsp.call(ids::NOP, &[]).await {
        Err(error) => error,
        Ok(_) => EzspFailure::LinkLost.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::FakeNcp;

    const EMBER_13: Version = Version {
        protocol: 13,
        stack_type: 2,
        stack_version: 0x7440,
    };
    const EMBER_14: Version = Version {
        protocol: 14,
        stack_type: 2,
        stack_version: 0x8000,
    };

    fn set_config(config: ConfigValue) -> (u16, Vec<u8>) {
        let [low, high] = config.value.to_le_bytes();
        (ids::SET_CONFIGURATION_VALUE, vec![config.id, low, high])
    }

    #[test]
    fn stack_config_is_herdsmans() {
        let values: Vec<_> = STACK_CONFIG.iter().map(|c| (c.id, c.value)).collect();
        assert_eq!(
            values,
            [
                (0x19, 2),
                (0x12, 7680),
                (0x10, 30),
                (0x2d, 1),
                (0x0c, 2),
                (0x0d, 5),
                (0x11, 32),
                (0x13, 8),
                (0x36, 300),
            ]
        );
    }

    #[tokio::test]
    async fn configures_then_inits_the_network() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        ncp.respond(ids::NETWORK_INIT, &[0x93]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(start_ncp(&ezsp, &version).await.unwrap(), Init::NotJoined);
        let mut expected: Vec<_> = STACK_CONFIG.into_iter().map(set_config).collect();
        expected.push((ids::NETWORK_INIT, vec![0x03, 0x00]));
        // After the two `version` commands.
        assert_eq!(ncp.received()[2..], expected);
    }

    #[tokio::test]
    async fn a_refused_value_does_not_stop_the_start() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        // EZSP_ERROR_INVALID_ID for every value.
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x36]);
        ncp.respond(ids::NETWORK_INIT, &[0x00]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(start_ncp(&ezsp, &version).await.unwrap(), Init::Resumed);
        assert_eq!(ncp.received().len(), 2 + STACK_CONFIG.len() + 1);
    }

    #[tokio::test]
    async fn statuses_are_read_at_the_ncp_width() {
        let (ncp, io) = FakeNcp::spawn(EMBER_14);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0, 0, 0, 0]);
        ncp.respond(ids::NETWORK_INIT, &[0x17, 0, 0, 0]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(start_ncp(&ezsp, &version).await.unwrap(), Init::NotJoined);
    }

    #[tokio::test]
    async fn a_failed_network_init_is_an_error() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x00]);
        // INVALID_CALL
        ncp.respond(ids::NETWORK_INIT, &[0x70]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        let err = start_ncp(&ezsp, &version).await.unwrap_err();
        assert!(err.to_string().contains("networkInit"), "{err}");
    }

    #[tokio::test]
    async fn the_network_comes_up_with_the_stack_status() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        ncp.respond(ids::NOP, &[]);
        let (ezsp, _, mut callbacks) = Ezsp::connect(io).await.unwrap();
        // A message first: logged, not taken for the status.
        ncp.callback(ids::INCOMING_MESSAGE_HANDLER, &[0x00]);
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        let network = wait_network_up(&ezsp, &mut callbacks, 13, NETWORK_UP_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(network, Network::Up);
    }

    #[tokio::test]
    async fn without_stack_status_the_network_is_down() {
        let (_ncp, io) = FakeNcp::spawn(EMBER_13);
        let (ezsp, _, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let network = wait_network_up(&ezsp, &mut callbacks, 13, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(network, Network::Down);
    }

    #[tokio::test]
    async fn a_lost_link_ends_the_wait_with_its_failure() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        let (ezsp, _, mut callbacks) = Ezsp::connect(io).await.unwrap();
        drop(ncp);
        let err = wait_network_up(&ezsp, &mut callbacks, 13, NETWORK_UP_TIMEOUT)
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<EzspFailure>(),
            Some(&EzspFailure::LinkLost)
        );
    }

    #[test]
    fn stack_statuses() {
        let status = |params: &[u8], protocol| {
            network_change(
                &Callback {
                    id: ids::STACK_STATUS_HANDLER,
                    params: params.to_vec(),
                },
                protocol,
            )
        };
        assert_eq!(status(&[0x90], 13), Some(Network::Up));
        assert_eq!(status(&[0x91], 13), Some(Network::Down));
        assert_eq!(status(&[0x15, 0, 0, 0], 14), Some(Network::Up));
        assert_eq!(status(&[0x16, 0, 0, 0], 14), Some(Network::Down));
        // Network opened for joining: no change.
        assert_eq!(status(&[0x9c], 13), None);
        assert_eq!(status(&[], 13), None);
        let other = Callback {
            id: ids::MESSAGE_SENT_HANDLER,
            params: vec![0x90],
        };
        assert_eq!(network_change(&other, 13), None);
    }

    #[test]
    fn network_values() {
        assert_eq!(
            [Network::Up, Network::Down, Network::None].map(Network::as_str),
            Network::VALUES
        );
    }
}

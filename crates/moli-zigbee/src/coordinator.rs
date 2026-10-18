//! Coordinator startup: the stack configuration a coordinator needs, its
//! application endpoint, the trust center policies, then `networkInit`,
//! which resumes the network the NCP keeps in its tokens. Then what the
//! running coordinator does: open or close joining, and tell callbacks apart.
//!
//! The values and their order follow zigbee-herdsman (MIT),
//! `src/adapter/ember/adapter/emberAdapter.ts` (`initEzsp`,
//! `registerFixedEndpoints`, `initTrustCenter`, `permitJoin`,
//! `onTrustCenterJoin`).

use std::time::Duration;

use anyhow::{bail, ensure};
use tokio::sync::mpsc;

use crate::aps::{Aps, Incoming};
use crate::ezsp::{
    Callback, Ezsp, EzspFailure, SL_STATUS_VERSION, SlStatus, Version, ids, read_ezsp_status,
    read_status,
};
use crate::{network, zdo};

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

/// One `setPolicy`: the `EzspPolicyId`, its name for the logs, and the
/// `EzspDecisionId` (or `EzspDecisionBitmask`) chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub id: u8,
    pub name: &'static str,
    pub decision: u8,
}

/// The trust center policies herdsman's ember adapter sets right before
/// `networkInit` (`initTrustCenter`, in this order), whether the network is
/// then resumed or formed: policies live in the NCP's RAM, not its tokens.
/// Ids and decisions from `src/adapter/ember/ezsp/enums.ts`.
pub const TRUST_CENTER_POLICIES: [Policy; 3] = [
    // `ALLOW_TC_KEY_REQUESTS_AND_SEND_CURRENT_KEY`: a joined device asking
    // for its trust center link key gets it.
    Policy {
        id: 0x05,
        name: "TC_KEY_REQUEST_POLICY",
        decision: 0x51,
    },
    // `DENY_APP_KEY_REQUESTS` (herdsman's `ALLOW_APP_KEY_REQUESTS = false`).
    Policy {
        id: 0x06,
        name: "APP_KEY_REQUEST_POLICY",
        decision: 0x60,
    },
    // `emberSetJoinPolicy(USE_PRECONFIGURED_KEY)`:
    // `ALLOW_JOINS | ALLOW_UNSECURED_REJOINS` (`EzspDecisionBitmask`).
    Policy {
        id: 0x00,
        name: "TRUST_CENTER_POLICY",
        decision: 0x01 | 0x02,
    },
];

/// An application endpoint the NCP answers on (`FixedEndpointInfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub endpoint: u8,
    pub profile: u16,
    pub device_id: u16,
    pub device_version: u8,
    /// Server clusters.
    pub in_clusters: &'static [u16],
    /// Client clusters.
    pub out_clusters: &'static [u16],
}

/// The coordinator's endpoint 1: herdsman's `FIXED_ENDPOINTS[0]`
/// (`src/adapter/ember/adapter/endpoints.ts`), Home Automation profile,
/// with its device id and cluster lists. Its green power endpoint and
/// multicast entries are not registered.
pub const ENDPOINT: Endpoint = Endpoint {
    endpoint: 1,
    profile: 0x0104,
    device_id: 0x0065,
    device_version: 1,
    in_clusters: &[
        0x0000, // Basic
        0x0003, // Identify
        0x0006, // On/Off
        0x0008, // Level Control
        0x000A, // Time
        0x0019, // OTA Upgrade
        0x0300, // Color Control
    ],
    out_clusters: &[
        0x0000, // Basic
        0x0003, // Identify
        0x0004, // Groups
        0x0005, // Scenes
        0x0006, // On/Off
        0x0008, // Level Control
        0x0020, // Poll Control
        0x0300, // Color Control
        0x0400, // Illuminance Measurement
        0x0402, // Temperature Measurement
        0x0405, // Relative Humidity Measurement
        0x0406, // Occupancy Sensing
        0x0500, // IAS Zone
        0x0702, // Metering
        0x0B01, // Meter Identification
        0x0B03, // Appliance Statistics
        0x0B04, // Electrical Measurement
        0x1000, // Touchlink
    ],
};

impl Endpoint {
    /// `addEndpoint`'s parameters (`ezspAddEndpoint`): endpoint, profile,
    /// device id, version, both list lengths, then both lists.
    #[must_use]
    pub fn add_endpoint_params(&self) -> Vec<u8> {
        let mut params = vec![self.endpoint];
        params.extend_from_slice(&self.profile.to_le_bytes());
        params.extend_from_slice(&self.device_id.to_le_bytes());
        params.push(self.device_version);
        for list in [self.in_clusters, self.out_clusters] {
            params.push(u8::try_from(list.len()).unwrap_or(u8::MAX));
        }
        for cluster in self.in_clusters.iter().chain(self.out_clusters) {
            params.extend_from_slice(&cluster.to_le_bytes());
        }
        params
    }
}

/// How long joining stays open: herdsman's (and Zigbee2MQTT's) longest,
/// 255 meaning « forever » to the stack.
pub const PERMIT_JOIN_SECONDS: u8 = 254;
pub const PERMIT_JOIN: Duration = Duration::from_secs(254);

/// `ZIGBEE_PROFILE_INTEROPERABILITY_LINK_KEY` (`src/adapter/ember/consts.ts`):
/// the well-known « ZigBeeAlliance09 » key new devices join with, imported
/// as a transient key while joining is open (public, not a secret).
const INTEROPERABILITY_LINK_KEY: [u8; 16] = *b"ZigBeeAlliance09";

/// `BLANK_EUI64` (`src/zspec/consts.ts`): the transient key is for any device.
const BLANK_EUI64: [u8; 8] = [0xFF; 8];

/// `EzspPolicyId.TRUST_CENTER_POLICY` and the `EzspDecisionBitmask`s of
/// `emberSetJoinPolicy`.
const TRUST_CENTER_POLICY: u8 = 0x00;
const ALLOW_JOINS: u8 = 0x01;
const ALLOW_UNSECURED_REJOINS: u8 = 0x02;

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
/// logged and its own kept), register [`ENDPOINT`], set the
/// [`TRUST_CENTER_POLICIES`], then `networkInit`.
///
/// The endpoint is added unconditionally: the link starts with a reset,
/// and the NCP forgets its endpoints at each one (herdsman first asks
/// `getEndpointFlags` because it may init without a reset).
///
/// # Errors
///
/// The link failing or a command timing out, an unreadable answer, the
/// endpoint or a trust center policy refused (fatal in herdsman too), or
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
        .call(ids::ADD_ENDPOINT, &ENDPOINT.add_endpoint_params())
        .await?;
    match read_ezsp_status(&answer, protocol)?.0 {
        SlStatus::Ok => tracing::debug!(endpoint = ENDPOINT.endpoint, "endpoint registered"),
        status => bail!("the NCP refused endpoint {}: {status:?}", ENDPOINT.endpoint),
    }
    for policy in TRUST_CENTER_POLICIES {
        let answer = ezsp
            .call(ids::SET_POLICY, &[policy.id, policy.decision])
            .await?;
        match read_ezsp_status(&answer, protocol)?.0 {
            SlStatus::Ok => tracing::debug!(
                policy = policy.name,
                decision = policy.decision,
                "EZSP policy set"
            ),
            status => bail!("the NCP refused the policy {}: {status:?}", policy.name),
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

/// After a `networkInit` that resumed, or a `formNetwork`: wait (reading
/// every callback) for the stack to report the network up. A « down » on
/// the way is ignored, as herdsman waits for `NETWORK_UP` only; `Down`
/// after `timeout` without it.
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
            Ok(Some(callback)) => match network_change(&callback, protocol) {
                Some(Network::Up) => return Ok(Network::Up),
                Some(other) => tracing::debug!(?other, "ignored while waiting for the network"),
                None => {}
            },
        }
    }
}

/// Open joining for `seconds` (0 closes it), as herdsman's `permitJoin`
/// does for the whole network: on the coordinator first (the transient
/// « ZigBeeAlliance09 » key and the join policy, then `permitJoining`),
/// then a Mgmt_Permit_Joining_req broadcast to every router. Closing
/// clears the transient keys and lets only rejoins in.
///
/// # Errors
///
/// The link failing, or the NCP refusing a step.
pub async fn permit_joining(aps: &Aps, seconds: u8) -> anyhow::Result<()> {
    let ezsp = aps.ezsp();
    let protocol = aps.protocol();
    let policy = if seconds > 0 {
        let mut params = BLANK_EUI64.to_vec();
        params.extend_from_slice(&INTEROPERABILITY_LINK_KEY);
        if protocol < SL_STATUS_VERSION {
            // `SecManFlag.NONE`, v13 only.
            params.push(0);
        }
        let answer = ezsp.call(ids::IMPORT_TRANSIENT_KEY, &params).await?;
        // An `SlStatus` whatever the version (`ezspImportTransientKey`).
        let status = answer
            .first_chunk::<4>()
            .map(|code| SlStatus::from_u32(u32::from_le_bytes(*code)));
        ensure!(
            status == Some(SlStatus::Ok),
            "importTransientKey failed: {status:?}"
        );
        ALLOW_JOINS | ALLOW_UNSECURED_REJOINS
    } else {
        ezsp.call(ids::CLEAR_TRANSIENT_LINK_KEYS, &[]).await?;
        ALLOW_UNSECURED_REJOINS
    };
    let answer = ezsp
        .call(ids::SET_POLICY, &[TRUST_CENTER_POLICY, policy])
        .await?;
    let status = read_ezsp_status(&answer, protocol)?.0;
    ensure!(status == SlStatus::Ok, "join policy refused: {status:?}");
    let answer = ezsp.call(ids::PERMIT_JOINING, &[seconds]).await?;
    let status = read_status(&answer, protocol)?.0;
    ensure!(status == SlStatus::Ok, "permitJoining failed: {status:?}");
    let tag = aps.next_tag();
    aps.send_broadcast(
        zdo::BROADCAST_ROUTERS,
        &zdo::frame(zdo::MGMT_PERMIT_JOINING_REQUEST),
        zdo::REQUEST_RADIUS,
        tag,
        &zdo::mgmt_permit_joining_request(tag, seconds),
    )
    .await?;
    tracing::info!(seconds, "joining permitted for {seconds} s");
    Ok(())
}

/// What a callback means to the running coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The stack reported the network up or down.
    Network(Network),
    /// A device joined or rejoined (`trustCenterJoinHandler`, or its
    /// device announce).
    Arrival { nwk: u16, ieee: [u8; 8] },
}

/// Classify a callback (a `messageSentHandler` is the APS layer's): the
/// rest is logged, a device leaving included (kept in the store in v1).
#[must_use]
pub fn event(callback: &Callback, protocol: u8) -> Option<Event> {
    match callback.id {
        ids::TRUST_CENTER_JOIN_HANDLER => trust_center_join(&callback.params),
        ids::INCOMING_MESSAGE_HANDLER => match Incoming::decode(&callback.params, protocol) {
            Ok(message) => incoming(&message),
            Err(error) => {
                tracing::warn!(%error, "unreadable incoming message");
                None
            }
        },
        // The stack status; anything else is logged there.
        _ => network_change(callback, protocol).map(Event::Network),
    }
}

/// `EmberDeviceUpdate.DEVICE_LEFT`, `EmberJoinDecision.DENY_JOIN` (`enums.ts`).
const DEVICE_LEFT: u8 = 2;
const DENY_JOIN: u8 = 2;

/// A `trustCenterJoinHandler`: new node id, its EUI64, `EmberDeviceUpdate`,
/// `EmberJoinDecision`, parent (the same in every version).
fn trust_center_join(params: &[u8]) -> Option<Event> {
    let Some(raw) = params.first_chunk::<14>() else {
        tracing::warn!(len = params.len(), "unreadable trustCenterJoinHandler");
        return None;
    };
    let nwk = u16::from_le_bytes([raw[0], raw[1]]);
    let mut ieee = [0; 8];
    ieee.copy_from_slice(&raw[2..10]);
    let (status, decision) = (raw[10], raw[11]);
    let parent = u16::from_le_bytes([raw[12], raw[13]]);
    let device = network::ieee(ieee);
    if status == DEVICE_LEFT {
        // Kept in the store (v1): it may come back.
        tracing::info!(%device, nwk = format_args!("{nwk:#06x}"), "a device left the network");
        return None;
    }
    if decision == DENY_JOIN {
        tracing::warn!(
            %device,
            nwk = format_args!("{nwk:#06x}"),
            parent = format_args!("{parent:#06x}"),
            "a device was denied joining"
        );
        return None;
    }
    tracing::info!(
        %device,
        nwk = format_args!("{nwk:#06x}"),
        status,
        "a device joined"
    );
    Some(Event::Arrival { nwk, ieee })
}

/// An incoming message: a device announce is an arrival; the rest is only
/// logged until the interview and ZCL read them.
fn incoming(message: &Incoming) -> Option<Event> {
    let frame = &message.frame;
    if frame.profile == zdo::PROFILE && frame.cluster == zdo::DEVICE_ANNOUNCE {
        return match zdo::DeviceAnnounce::parse(&message.payload) {
            Ok(announce) => {
                tracing::debug!(
                    device = %network::ieee(announce.ieee),
                    nwk = format_args!("{:#06x}", announce.nwk),
                    capability = announce.capability,
                    "device announce"
                );
                Some(Event::Arrival {
                    nwk: announce.nwk,
                    ieee: announce.ieee,
                })
            }
            Err(error) => {
                tracing::warn!(%error, "unreadable device announce");
                None
            }
        };
    }
    tracing::debug!(
        from = format_args!("{:#06x}", message.nwk),
        profile = format_args!("{:#06x}", frame.profile),
        cluster = format_args!("{:#06x}", frame.cluster),
        endpoint = frame.src_ep,
        lqi = message.lqi,
        len = message.payload.len(),
        "incoming message"
    );
    None
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

    fn set_policy(policy: Policy) -> (u16, Vec<u8>) {
        (ids::SET_POLICY, vec![policy.id, policy.decision])
    }

    /// Every startup command accepted, `networkInit` answering `init`.
    fn accepting(ncp: &FakeNcp, ok: &[u8], init: &[u8]) {
        ncp.respond(ids::SET_CONFIGURATION_VALUE, ok);
        ncp.respond(ids::ADD_ENDPOINT, ok);
        ncp.respond(ids::SET_POLICY, ok);
        ncp.respond(ids::NETWORK_INIT, init);
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

    #[test]
    fn trust_center_policies_are_herdsmans() {
        let values: Vec<_> = TRUST_CENTER_POLICIES
            .iter()
            .map(|p| (p.id, p.decision))
            .collect();
        assert_eq!(values, [(0x05, 0x51), (0x06, 0x60), (0x00, 0x03)]);
    }

    #[tokio::test]
    async fn configures_then_inits_the_network() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        accepting(&ncp, &[0x00], &[0x93]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(start_ncp(&ezsp, &version).await.unwrap(), Init::NotJoined);
        let mut expected: Vec<_> = STACK_CONFIG.into_iter().map(set_config).collect();
        // The endpoint before the policies, as herdsman registers it.
        expected.push((ids::ADD_ENDPOINT, ENDPOINT.add_endpoint_params()));
        expected.extend(TRUST_CENTER_POLICIES.into_iter().map(set_policy));
        expected.push((ids::NETWORK_INIT, vec![0x03, 0x00]));
        // After the two `version` commands.
        assert_eq!(ncp.received()[2..], expected);
    }

    #[tokio::test]
    async fn a_refused_value_does_not_stop_the_start() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        // EZSP_ERROR_INVALID_ID for every value.
        ncp.respond(ids::SET_CONFIGURATION_VALUE, &[0x36]);
        ncp.respond(ids::ADD_ENDPOINT, &[0x00]);
        ncp.respond(ids::SET_POLICY, &[0x00]);
        ncp.respond(ids::NETWORK_INIT, &[0x00]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(start_ncp(&ezsp, &version).await.unwrap(), Init::Resumed);
        assert_eq!(
            ncp.received().len(),
            2 + STACK_CONFIG.len() + 1 + TRUST_CENTER_POLICIES.len() + 1
        );
    }

    #[tokio::test]
    async fn a_refused_policy_stops_the_start() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        accepting(&ncp, &[0x00], &[0x00]);
        // EZSP_ERROR_INVALID_VALUE
        ncp.respond(ids::SET_POLICY, &[0x35]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        let err = start_ncp(&ezsp, &version).await.unwrap_err();
        assert!(err.to_string().contains("TC_KEY_REQUEST_POLICY"), "{err}");
        assert!(
            !ncp.received()
                .iter()
                .any(|(id, _)| *id == ids::NETWORK_INIT),
            "no networkInit"
        );
    }

    #[tokio::test]
    async fn statuses_are_read_at_the_ncp_width() {
        let (ncp, io) = FakeNcp::spawn(EMBER_14);
        accepting(&ncp, &[0, 0, 0, 0], &[0x17, 0, 0, 0]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(start_ncp(&ezsp, &version).await.unwrap(), Init::NotJoined);
    }

    #[tokio::test]
    async fn a_failed_network_init_is_an_error() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        // INVALID_CALL
        accepting(&ncp, &[0x00], &[0x70]);
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
    async fn a_down_on_the_way_is_ignored() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        let (ezsp, _, mut callbacks) = Ezsp::connect(io).await.unwrap();
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x91]);
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
    fn endpoint_is_herdsmans() {
        let params = ENDPOINT.add_endpoint_params();
        assert_eq!(params.len(), 8 + 2 * (7 + 18));
        assert_eq!(
            params[..10],
            [
                0x01, // endpoint
                0x04, 0x01, // Home Automation
                0x65, 0x00, // device id
                0x01, // version
                7, 18, // list lengths
                0x00, 0x00, // Basic first
            ]
        );
        // Color Control ends the server list, Touchlink the client one.
        assert_eq!(params[20..22], [0x00, 0x03]);
        assert_eq!(params[params.len() - 2..], [0x00, 0x10]);
        assert_eq!(
            PERMIT_JOIN,
            Duration::from_secs(u64::from(PERMIT_JOIN_SECONDS))
        );
    }

    #[tokio::test]
    async fn a_refused_endpoint_stops_the_start() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        accepting(&ncp, &[0x00], &[0x00]);
        // EZSP_ERROR_INVALID_CALL: endpoints can no longer be added.
        ncp.respond(ids::ADD_ENDPOINT, &[0x37]);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        let err = start_ncp(&ezsp, &version).await.unwrap_err();
        assert!(err.to_string().contains("endpoint 1"), "{err}");
        assert!(
            !ncp.received().iter().any(|(id, _)| *id == ids::SET_POLICY),
            "stopped there"
        );
    }

    /// An NCP accepting what joining needs, answered at `version`'s width.
    async fn joining_ncp(version: Version) -> (FakeNcp, Aps) {
        let (ncp, io) = FakeNcp::spawn(version);
        let wide = version.protocol >= 14;
        let ok: &[u8] = if wide { &[0, 0, 0, 0] } else { &[0] };
        ncp.respond(ids::IMPORT_TRANSIENT_KEY, &[0, 0, 0, 0]);
        ncp.respond(ids::CLEAR_TRANSIENT_LINK_KEYS, &[]);
        ncp.respond(ids::SET_POLICY, ok);
        ncp.respond(ids::PERMIT_JOINING, ok);
        let sent: &[u8] = if wide {
            &[0, 0, 0, 0, 0x10]
        } else {
            &[0, 0x10]
        };
        ncp.respond(ids::SEND_BROADCAST, sent);
        let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        (ncp, Aps::new(ezsp, version.protocol))
    }

    /// The ZDO payload (transaction number first) of the `sendBroadcast`
    /// in `params`, and its destination.
    fn broadcast(params: &[u8], protocol: u8) -> (u16, Vec<u8>) {
        let (destination, frame_at) = if protocol >= 14 {
            (u16::from_le_bytes([params[2], params[3]]), 5)
        } else {
            (u16::from_le_bytes([params[0], params[1]]), 2)
        };
        let (frame, rest) = crate::aps::ApsFrame::decode(&params[frame_at..]).unwrap();
        assert_eq!(frame, zdo::frame(zdo::MGMT_PERMIT_JOINING_REQUEST));
        assert_eq!(rest[0], zdo::REQUEST_RADIUS);
        let tag_len = if protocol >= 14 { 2 } else { 1 };
        let payload = &rest[1 + tag_len..];
        assert_eq!(usize::from(payload[0]), payload.len() - 1);
        assert_eq!(payload[1], rest[1], "the ZDO transaction is the tag");
        (destination, payload[1..].to_vec())
    }

    #[tokio::test]
    async fn permit_joining_like_herdsman() {
        for version in [EMBER_13, EMBER_14] {
            let protocol = version.protocol;
            let (ncp, aps) = joining_ncp(version).await;
            permit_joining(&aps, PERMIT_JOIN_SECONDS).await.unwrap();
            let received = ncp.received()[2..].to_vec();
            let ids: Vec<_> = received.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids,
                [
                    ids::IMPORT_TRANSIENT_KEY,
                    ids::SET_POLICY,
                    ids::PERMIT_JOINING,
                    ids::SEND_BROADCAST
                ],
                "v{protocol}"
            );
            let mut key = vec![0xFF; 8];
            key.extend_from_slice(b"ZigBeeAlliance09");
            if protocol < 14 {
                key.push(0); // flags, v13 only
            }
            assert_eq!(received[0].1, key, "v{protocol}");
            assert_eq!(received[1].1, [0x00, 0x03], "joins and rejoins");
            assert_eq!(received[2].1, [254]);
            let (destination, zdo) = broadcast(&received[3].1, protocol);
            assert_eq!(destination, 0xFFFC);
            assert_eq!(zdo[1..], [254, 1], "v{protocol}");

            permit_joining(&aps, 0).await.unwrap();
            let received = ncp.received()[6..].to_vec();
            let ids: Vec<_> = received.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids,
                [
                    ids::CLEAR_TRANSIENT_LINK_KEYS,
                    ids::SET_POLICY,
                    ids::PERMIT_JOINING,
                    ids::SEND_BROADCAST
                ],
                "v{protocol}"
            );
            assert_eq!(received[0].1, Vec::<u8>::new());
            assert_eq!(received[1].1, [0x00, 0x02], "rejoins only");
            assert_eq!(received[2].1, [0]);
            let (destination, zdo) = broadcast(&received[3].1, protocol);
            assert_eq!(destination, 0xFFFC);
            assert_eq!(zdo[1..], [0, 1]);
        }
    }

    #[tokio::test]
    async fn a_refused_permit_joining_is_an_error() {
        let (ncp, aps) = joining_ncp(EMBER_13).await;
        // INVALID_CALL: no network.
        ncp.respond(ids::PERMIT_JOINING, &[0x70]);
        let err = permit_joining(&aps, PERMIT_JOIN_SECONDS).await.unwrap_err();
        assert!(err.to_string().contains("permitJoining"), "{err}");
        assert!(
            !ncp.received()
                .iter()
                .any(|(id, _)| *id == ids::SEND_BROADCAST),
            "nothing broadcast"
        );
    }

    /// `0x00124b0000000001` as it travels.
    const IEEE: [u8; 8] = [0x01, 0, 0, 0, 0, 0x4b, 0x12, 0x00];

    fn trust_center_join(status: u8, decision: u8) -> Callback {
        let mut params = vec![0x34, 0x12];
        params.extend_from_slice(&IEEE);
        params.extend_from_slice(&[status, decision, 0x00, 0x00]);
        Callback {
            id: ids::TRUST_CENTER_JOIN_HANDLER,
            params,
        }
    }

    /// An incoming ZDO message (v13 layout) on `cluster`.
    fn incoming_zdo(cluster: u16, payload: &[u8]) -> Callback {
        let mut params = vec![0x00];
        zdo::frame(cluster).encode(&mut params);
        params.extend_from_slice(&[200, 0xC4, 0x34, 0x12, 0xFF, 0xFF]);
        params.push(u8::try_from(payload.len()).unwrap());
        params.extend_from_slice(payload);
        Callback {
            id: ids::INCOMING_MESSAGE_HANDLER,
            params,
        }
    }

    #[test]
    fn events() {
        let arrival = Some(Event::Arrival {
            nwk: 0x1234,
            ieee: IEEE,
        });
        // Unsecured join, secured rejoin: arrivals.
        assert_eq!(event(&trust_center_join(1, 0), 13), arrival);
        assert_eq!(event(&trust_center_join(0, 4), 14), arrival);
        // A device left: logged only; a join denied: no arrival.
        assert_eq!(event(&trust_center_join(2, 3), 13), None);
        assert_eq!(event(&trust_center_join(1, 2), 13), None);
        // Too short: logged.
        let mut cut = trust_center_join(1, 0);
        cut.params.pop();
        assert_eq!(event(&cut, 13), None);
        // A device announce.
        let mut announce = vec![0x81, 0x34, 0x12];
        announce.extend_from_slice(&IEEE);
        announce.push(0x8E);
        assert_eq!(
            event(&incoming_zdo(zdo::DEVICE_ANNOUNCE, &announce), 13),
            arrival
        );
        // Other messages: logged (phase 3 reads them).
        assert_eq!(event(&incoming_zdo(zdo::BIND_RESPONSE, &[1, 0]), 13), None);
        // The stack status, as before.
        let up = Callback {
            id: ids::STACK_STATUS_HANDLER,
            params: vec![0x90],
        };
        assert_eq!(event(&up, 13), Some(Event::Network(Network::Up)));
    }

    #[test]
    fn network_values() {
        assert_eq!(
            [Network::Up, Network::Down, Network::None].map(Network::as_str),
            Network::VALUES
        );
    }
}

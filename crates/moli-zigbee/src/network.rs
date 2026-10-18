//! Forming a new network, and reading the one the coordinator runs.
//!
//! The sequence and its values follow zigbee-herdsman (MIT),
//! `src/adapter/ember/adapter/emberAdapter.ts`, `formNetwork` (the « from
//! config » path of `initTrustCenter`), with the encodings of
//! `src/adapter/ember/ezsp/ezsp.ts` and `ezsp/buffalo.ts` and the values of
//! `src/adapter/ember/enums.ts`, `ezsp/enums.ts` and `src/zspec/consts.ts`.
//!
//! The keys only ever exist in the vault and in the commands that hand them
//! to the NCP: never in a file, a log or a `Debug` output.

use std::fmt;

use anyhow::{Context, bail};
use moli_runtime::DriverCtx;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::mpsc;

use crate::coordinator::{self, Network};
use crate::ezsp::{
    Callback, Ezsp, EzspError, NetworkParameters, SlStatus, ids, read_ezsp_status, read_status,
};
use crate::store::NetworkRecord;

/// Vault entry of the network key (32 lowercase hex digits).
pub const NETWORK_KEY: &str = "network_key";
/// Vault entry of the trust center link key, the root the NCP hashes each
/// device's link key from (herdsman: a random key, kept in its backup as
/// `hashed_tclk`; needed to restore the network on another dongle).
pub const TC_LINK_KEY: &str = "tc_link_key";

/// Channel of a new network when the configuration sets none (no energy
/// scan yet): one of the four that avoid the busiest Wi-Fi channels.
pub const DEFAULT_CHANNEL: u8 = 15;
/// The 2.4 GHz Zigbee channels.
pub const CHANNELS: std::ops::RangeInclusive<u8> = 11..=26;

/// `EmberInitialSecurityBitmask` of `formNetwork`:
/// `TRUST_CENTER_GLOBAL_LINK_KEY | HAVE_PRECONFIGURED_KEY | HAVE_NETWORK_KEY
/// | TRUST_CENTER_USES_HASHED_LINK_KEY | REQUIRE_ENCRYPTED_KEY`.
const INITIAL_SECURITY_BITMASK: u16 = 0x0004 | 0x0100 | 0x0200 | 0x0084 | 0x0800;
/// `EmberExtendedSecurityBitmask` of `formNetwork`:
/// `JOINER_GLOBAL_LINK_KEY | NWK_LEAVE_REQUEST_NOT_ALLOWED`.
const EXTENDED_SECURITY_BITMASK: u16 = 0x0010 | 0x0100;
/// `EzspValueId.EXTENDED_SECURITY_BITMASK` (`ezspSetExtendedSecurityBitmask`).
const VALUE_EXTENDED_SECURITY_BITMASK: u8 = 0x0a;
/// `EzspValueId.STACK_TOKEN_WRITING` (`ezspStartWritingStackTokens`).
const VALUE_STACK_TOKEN_WRITING: u8 = 0x07;
/// `preconfiguredTrustCenterEui64`: `ZSpec.BLANK_EUI64`.
const BLANK_EUI64: [u8; 8] = [0xff; 8];
/// `radioTxPower` when none is configured (`transmitPower || 5`), dBm.
const RADIO_TX_POWER: i8 = 5;
/// `EmberJoinMethod.MAC_ASSOCIATION`.
const JOIN_METHOD_MAC_ASSOCIATION: u8 = 0;
/// `ZSpec.COORDINATOR_ADDRESS`, the network manager.
const COORDINATOR_ADDRESS: u16 = 0x0000;
/// `ZSpec.ALL_802_15_4_CHANNELS_MASK`: channels 11 to 26.
const ALL_CHANNELS_MASK: u32 = 0x07ff_f800;
/// `EmberNodeType.COORDINATOR`.
const NODE_TYPE_COORDINATOR: u8 = 1;

/// A 128-bit key; its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; 16]);

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(<redacted>)")
    }
}

impl Key {
    fn random(rng: &SystemRandom) -> anyhow::Result<Self> {
        let mut bytes = [0u8; 16];
        rng.fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("no randomness for a key"))?;
        Ok(Self(bytes))
    }

    /// As the vault keeps it: 32 lowercase hex digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Reads 32 hex digits (either case); `None` otherwise.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() != 32 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
            let pair = std::str::from_utf8(pair).ok()?;
            *byte = u8::from_str_radix(pair, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

/// The two keys of a new network, taken from the vault (or put there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keys {
    pub network: Key,
    pub tc_link: Key,
}

/// The keys of the network to form: those the vault already holds (a
/// previous attempt that stopped before the store was written), or new
/// random ones, written to the vault **before** anything goes to the NCP.
///
/// # Errors
///
/// No vault, a vault entry that is not a key, or the vault write failing.
pub async fn keys(ctx: &DriverCtx) -> anyhow::Result<Keys> {
    if !ctx.can_store_secrets() {
        bail!("no secret vault: a network is never formed without one");
    }
    let rng = SystemRandom::new();
    Ok(Keys {
        network: vault_key(ctx, NETWORK_KEY, &rng).await?,
        tc_link: vault_key(ctx, TC_LINK_KEY, &rng).await?,
    })
}

async fn vault_key(ctx: &DriverCtx, name: &str, rng: &SystemRandom) -> anyhow::Result<Key> {
    if let Some(held) = ctx.secret(name) {
        let Some(key) = Key::from_hex(&held) else {
            bail!("the vault's {name} is not a 128-bit key (32 hex digits)");
        };
        tracing::info!(secret = name, "reusing the key the vault already holds");
        return Ok(key);
    }
    let key = Key::random(rng)?;
    ctx.store_secret(name, &key.to_hex())
        .await
        .with_context(|| format!("storing {name} in the vault"))?;
    tracing::info!(secret = name, "new key stored in the vault");
    Ok(key)
}

/// `EmberInitialSecurityState` (`writeEmberInitialSecurityState`): bitmask
/// (`uint16_t`), preconfigured (trust center link) key, network key, key
/// sequence number, preconfigured trust center EUI64.
fn initial_security_state(keys: &Keys) -> Vec<u8> {
    let mut state = Vec::with_capacity(2 + 16 + 16 + 1 + 8);
    state.extend_from_slice(&INITIAL_SECURITY_BITMASK.to_le_bytes());
    state.extend_from_slice(&keys.tc_link.0);
    state.extend_from_slice(&keys.network.0);
    state.push(0);
    state.extend_from_slice(&BLANK_EUI64);
    state
}

/// The parameters `formNetwork` gets: random PAN and extended PAN on
/// `channel`.
fn new_parameters(channel: u8) -> anyhow::Result<NetworkParameters> {
    let rng = SystemRandom::new();
    let random = |bytes: &mut [u8]| {
        rng.fill(bytes)
            .map_err(|_| anyhow::anyhow!("no randomness for the network identifiers"))
    };
    // 0x0000 and 0xFFFF are not PAN IDs; an all-zero or all-0xFF extended
    // PAN ID means « any » to the stack.
    let pan_id = loop {
        let mut bytes = [0u8; 2];
        random(&mut bytes)?;
        let pan_id = u16::from_le_bytes(bytes);
        if pan_id != 0x0000 && pan_id != 0xFFFF {
            break pan_id;
        }
    };
    let extended_pan_id = loop {
        let mut bytes = [0u8; 8];
        random(&mut bytes)?;
        if bytes != [0x00; 8] && bytes != [0xFF; 8] {
            break bytes;
        }
    };
    Ok(NetworkParameters {
        extended_pan_id,
        pan_id,
        radio_tx_power: RADIO_TX_POWER,
        radio_channel: channel,
        join_method: JOIN_METHOD_MAC_ASSOCIATION,
        nwk_manager_id: COORDINATOR_ADDRESS,
        nwk_update_id: 0,
        channels: ALL_CHANNELS_MASK,
    })
}

/// Forms a new network on `channel` with `keys` (herdsman's `formNetwork`,
/// not from a backup): initial security state, extended security bitmask,
/// key table cleared, `formNetwork`, then `stackStatusHandler` reporting it
/// up within [`coordinator::NETWORK_UP_TIMEOUT`], then stack tokens written.
/// Returns what the NCP then reports of it.
///
/// # Errors
///
/// The link failing, the NCP refusing a step, or the network not coming up.
pub async fn form(
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    protocol: u8,
    channel: u8,
    keys: &Keys,
) -> anyhow::Result<NetworkRecord> {
    let answer = ezsp
        .call(
            ids::SET_INITIAL_SECURITY_STATE,
            &initial_security_state(keys),
        )
        .await?;
    match read_status(&answer, protocol)?.0 {
        SlStatus::Ok => {}
        status => bail!("setInitialSecurityState failed: {status:?}"),
    }
    let [low, high] = EXTENDED_SECURITY_BITMASK.to_le_bytes();
    match set_value(
        ezsp,
        protocol,
        VALUE_EXTENDED_SECURITY_BITMASK,
        &[low, high],
    )
    .await?
    {
        SlStatus::Ok => {}
        status => bail!("setting the extended security bitmask failed: {status:?}"),
    }
    let answer = ezsp.call(ids::CLEAR_KEY_TABLE, &[]).await?;
    match read_status(&answer, protocol)?.0 {
        SlStatus::Ok => {}
        // herdsman logs it and goes on.
        status => tracing::warn!(?status, "clearKeyTable failed"),
    }
    let parameters = new_parameters(channel)?;
    tracing::info!(
        channel,
        pan_id = format_args!("{:#06x}", parameters.pan_id),
        "forming a new Zigbee network"
    );
    let mut params = Vec::with_capacity(NetworkParameters::LEN);
    parameters.encode(&mut params);
    let answer = ezsp.call(ids::FORM_NETWORK, &params).await?;
    match read_status(&answer, protocol)?.0 {
        SlStatus::Ok => {}
        status => bail!("formNetwork failed: {status:?}"),
    }
    let network =
        coordinator::wait_network_up(ezsp, callbacks, protocol, coordinator::NETWORK_UP_TIMEOUT)
            .await?;
    if network != Network::Up {
        bail!(
            "the new network did not come up within {:?}",
            coordinator::NETWORK_UP_TIMEOUT
        );
    }
    let status = set_value(ezsp, protocol, VALUE_STACK_TOKEN_WRITING, &[1]).await?;
    tracing::debug!(?status, "start writing stack tokens");
    let dongle = read_network(ezsp, protocol).await?;
    if !dongle.coordinator {
        bail!("the formed network's node is not a coordinator");
    }
    let record = dongle.record;
    tracing::info!(
        channel = record.channel,
        pan_id = format_args!("{:#06x}", record.pan_id),
        coordinator = %record.coordinator_ieee,
        "new Zigbee network formed"
    );
    Ok(record)
}

/// `setValue`: value id, length, bytes; its status is an `EzspStatus` in v13.
async fn set_value(ezsp: &Ezsp, protocol: u8, id: u8, value: &[u8]) -> anyhow::Result<SlStatus> {
    let mut params = vec![id, u8::try_from(value.len())?];
    params.extend_from_slice(value);
    let answer = ezsp.call(ids::SET_VALUE, &params).await?;
    Ok(read_ezsp_status(&answer, protocol)?.0)
}

/// The network a dongle runs, as it reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DongleNetwork {
    /// Whether the dongle is that network's coordinator (`EmberNodeType`);
    /// a dongle that joined someone else's network as a router is not.
    pub coordinator: bool,
    pub record: NetworkRecord,
}

/// The network the dongle runs (`getNetworkParameters`: node type and
/// parameters) and its IEEE address (`getEui64`), as herdsman reads them
/// once the network is up.
///
/// # Errors
///
/// The link failing, an unreadable answer, or the NCP refusing.
pub async fn read_network(ezsp: &Ezsp, protocol: u8) -> anyhow::Result<DongleNetwork> {
    let answer = ezsp.call(ids::GET_NETWORK_PARAMETERS, &[]).await?;
    let (status, rest) = read_status(&answer, protocol)?;
    if status != SlStatus::Ok {
        bail!("getNetworkParameters failed: {status:?}");
    }
    let (&node_type, rest) = rest.split_first().ok_or(EzspError::TooShort)?;
    let (parameters, _) = NetworkParameters::decode(rest)?;
    let answer = ezsp.call(ids::GET_EUI64, &[]).await?;
    let (eui64, _) = answer.split_first_chunk::<8>().ok_or(EzspError::TooShort)?;
    Ok(DongleNetwork {
        coordinator: node_type == NODE_TYPE_COORDINATOR,
        record: NetworkRecord {
            pan_id: parameters.pan_id,
            extended_pan_id: parameters.extended_pan_id,
            channel: parameters.radio_channel,
            coordinator_ieee: ieee(*eui64),
            nwk_update_id: parameters.nwk_update_id,
        },
    })
}

/// An EUI64 as it travels (little-endian) to its usual form, `0x` and the
/// bytes most significant first (herdsman's `eui64LEBufferToHex`).
#[must_use]
pub fn ieee(mut eui64: [u8; 8]) -> String {
    eui64.reverse();
    format!("0x{}", hex(&eui64))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ezsp::Version;
    use crate::sim::FakeNcp;

    const EMBER_13: Version = Version {
        protocol: 13,
        stack_type: 2,
        stack_version: 0x7440,
    };

    /// The network key of the docs.
    const DOC_KEY: &str = "01030507090B0D0F00020406080A0C0D";

    fn doc_keys() -> Keys {
        Keys {
            network: Key::from_hex(DOC_KEY).unwrap(),
            tc_link: Key([0x5A; 16]),
        }
    }

    /// `getNetworkParameters` answered by a v13 coordinator on PAN `pan_id`.
    pub(crate) fn network_parameters(pan_id: u16, channel: u8) -> Vec<u8> {
        network_parameters_of(NODE_TYPE_COORDINATOR, pan_id, channel)
    }

    /// … by a node of `node_type` (`EmberNodeType`: 2 is a router).
    pub(crate) fn network_parameters_of(node_type: u8, pan_id: u16, channel: u8) -> Vec<u8> {
        let mut answer = vec![0x00, node_type];
        NetworkParameters {
            extended_pan_id: [0xDD; 8],
            pan_id,
            radio_tx_power: 5,
            radio_channel: channel,
            join_method: 0,
            nwk_manager_id: 0,
            nwk_update_id: 0,
            channels: ALL_CHANNELS_MASK,
        }
        .encode(&mut answer);
        answer
    }

    /// `getEui64`: `0x00124b0000000001`, little-endian.
    pub(crate) const EUI64: [u8; 8] = [0x01, 0, 0, 0, 0, 0x4b, 0x12, 0x00];

    #[test]
    fn keys_are_hex_and_never_shown() {
        let key = Key::from_hex(DOC_KEY).unwrap();
        assert_eq!(key.to_hex(), DOC_KEY.to_lowercase());
        assert_eq!(Key::from_hex(&key.to_hex()), Some(key.clone()));
        assert_eq!(Key::from_hex("0103"), None);
        assert_eq!(Key::from_hex(&"zz".repeat(16)), None);
        assert_eq!(Key::from_hex(&"é".repeat(16)), None);
        let shown = format!("{:?} {:?}", key, doc_keys());
        assert!(!shown.to_lowercase().contains("0103"), "{shown}");
        assert!(!shown.contains("5a"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
        let random = Key::random(&SystemRandom::new()).unwrap();
        assert_ne!(random, Key::random(&SystemRandom::new()).unwrap());
    }

    #[test]
    fn initial_security_state_is_herdsmans() {
        let state = initial_security_state(&doc_keys());
        assert_eq!(state.len(), 43);
        // 0x0B84, little-endian.
        assert_eq!(state[..2], [0x84, 0x0B]);
        assert_eq!(state[2..18], [0x5A; 16], "trust center link key");
        assert_eq!(hex(&state[18..34]), DOC_KEY.to_lowercase(), "network key");
        assert_eq!(state[34], 0, "key sequence number");
        assert_eq!(state[35..], [0xFF; 8]);
    }

    #[test]
    fn new_parameters_are_random_on_the_channel() {
        let a = new_parameters(15).unwrap();
        let b = new_parameters(15).unwrap();
        assert_eq!(a.radio_channel, 15);
        assert_eq!(a.radio_tx_power, 5);
        assert_eq!(a.channels, 0x07ff_f800);
        assert_eq!(
            (a.join_method, a.nwk_manager_id, a.nwk_update_id),
            (0, 0, 0)
        );
        assert_ne!((a.pan_id, a.extended_pan_id), (b.pan_id, b.extended_pan_id));
        assert!(a.pan_id != 0 && a.pan_id != 0xFFFF);
    }

    #[test]
    fn ieee_is_most_significant_first() {
        assert_eq!(ieee(EUI64), "0x00124b0000000001");
    }

    #[tokio::test]
    async fn forms_with_herdsmans_sequence() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        for id in [
            ids::SET_INITIAL_SECURITY_STATE,
            ids::SET_VALUE,
            ids::CLEAR_KEY_TABLE,
            ids::FORM_NETWORK,
        ] {
            ncp.respond(id, &[0x00]);
        }
        ncp.respond(ids::GET_NETWORK_PARAMETERS, &network_parameters(0x1a62, 20));
        ncp.respond(ids::GET_EUI64, &EUI64);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let forming = async {
            ncp.wait_command(ids::FORM_NETWORK).await;
            ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        };
        let keys = doc_keys();
        let (record, ()) = tokio::join!(
            form(&ezsp, &mut callbacks, version.protocol, 20, &keys),
            forming
        );
        assert_eq!(
            record.unwrap(),
            NetworkRecord {
                pan_id: 0x1a62,
                extended_pan_id: [0xDD; 8],
                channel: 20,
                coordinator_ieee: "0x00124b0000000001".into(),
                nwk_update_id: 0,
            }
        );
        let received = ncp.received();
        let sent: Vec<_> = received[2..].iter().map(|(id, _)| *id).collect();
        assert_eq!(
            sent,
            [
                ids::SET_INITIAL_SECURITY_STATE,
                ids::SET_VALUE,
                ids::CLEAR_KEY_TABLE,
                ids::FORM_NETWORK,
                ids::SET_VALUE,
                ids::GET_NETWORK_PARAMETERS,
                ids::GET_EUI64,
            ]
        );
        assert_eq!(received[2].1, initial_security_state(&doc_keys()));
        assert_eq!(received[3].1, [0x0a, 2, 0x10, 0x01]);
        let formed = NetworkParameters::decode(&received[5].1).unwrap().0;
        assert_eq!(formed.radio_channel, 20);
        assert_eq!(received[6].1, [0x07, 1, 1]);
    }

    #[tokio::test]
    async fn a_refused_formation_is_an_error() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        // SECURITY_STATE_NOT_SET
        ncp.respond(ids::SET_INITIAL_SECURITY_STATE, &[0xA8]);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let error = form(&ezsp, &mut callbacks, version.protocol, 15, &doc_keys())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("setInitialSecurityState"), "{error}");
        assert!(
            !ncp.received()
                .iter()
                .any(|(id, _)| *id == ids::FORM_NETWORK),
            "nothing formed"
        );
    }
}

//! Taking over a Zigbee2MQTT network: its « Open Coordinator Backup », and
//! the restore of that network on the dongle, without re-pairing devices.
//!
//! The format is zigpy's open-coordinator-backup v1 as zigbee-herdsman (MIT)
//! writes and reads it: `src/models/backup-storage-unified.ts` (fields),
//! `src/utils/backup.ts` (`toUnifiedBackup` / `fromUnifiedBackup`), and the
//! ember adapter's `getStoredBackup` and `backup`
//! (`src/adapter/ember/adapter/emberAdapter.ts`) for what an EmberZNet backup
//! holds and in which byte order. The restore follows that adapter's
//! `initTrustCenter` « form from backup » path and its `formNetwork(fromBackup
//! = true)`.
//!
//! The keys in a backup go to the vault only: never to the store, a log or
//! a `Debug` output.

use std::fmt;

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::ezsp::{Callback, Ezsp, EzspError, ids};
use crate::network::{self, FromBackup, Key, Keys};
use crate::store::{DeviceRecord, NetworkRecord, Store};

/// `metadata.format` of an Open Coordinator Backup.
pub const FORMAT: &str = "zigpy/open-coordinator-backup";
/// `metadata.version` this reader knows.
pub const FORMAT_VERSION: u32 = 1;
/// Added to the saved NWK frame counter on restore: the network kept
/// sending after the backup was written, and a device drops a frame whose
/// counter it already saw.
pub const FRAME_COUNTER_MARGIN: u32 = 1024;
/// herdsman's `BACKUP_OLDEST_SUPPORTED_EZSP_VERSION`: older EmberZNet
/// backups are not restored.
const OLDEST_EZSP_VERSION: u64 = 12;
/// herdsman warns past this saved counter: the `uint32_t` must not wrap,
/// the network will need a new key soon.
const FRAME_COUNTER_NEAR_END: u32 = 0xfeee_eeee;

/// A hex string that is a key: kept as written, never shown by `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretHex(String);

impl fmt::Debug for SecretHex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// An Open Coordinator Backup (v1), field for field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCoordinatorBackup {
    pub metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_specific: Option<StackSpecific>,
    /// zigbee-herdsman's ember adapter writes the EUI64 little-endian.
    pub coordinator_ieee: String,
    /// Hex, without `0x` (`1a62`).
    pub pan_id: String,
    /// Hex, the bytes as the stack stores them.
    pub extended_pan_id: String,
    pub nwk_update_id: u8,
    pub security_level: u8,
    pub channel: u8,
    pub channel_mask: Vec<u8>,
    pub network_key: NetworkKey,
    #[serde(default)]
    pub devices: Vec<BackupDevice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The writer's own data (herdsman: `date`, `ezspVersion`).
    #[serde(default)]
    pub internal: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackSpecific {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ezsp: Option<EzspSpecific>,
    /// Other stacks (`zstack`), kept as written.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EzspSpecific {
    /// The trust center link key the NCP hashes each device's key from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashed_tclk: Option<SecretHex>,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkKey {
    pub key: SecretHex,
    pub sequence_number: u8,
    pub frame_counter: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDevice {
    /// Hex, without `0x`; `null` when the writer did not know it.
    #[serde(default)]
    pub nwk_address: Option<String>,
    pub ieee_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_child: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_key: Option<BackupLinkKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupLinkKey {
    pub key: SecretHex,
    pub rx_counter: u32,
    pub tx_counter: u32,
}

/// What `moli-os zigbee import` files from a backup: the record for the
/// store, the keys for the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// With the frame counter and key sequence number of the backup.
    pub record: NetworkRecord,
    pub network_key: Key,
    /// `None`: the backup has no `hashed_tclk`.
    pub tc_link_key: Option<Key>,
    pub devices: Vec<DeviceRecord>,
}

impl OpenCoordinatorBackup {
    /// Reads a backup file.
    ///
    /// # Errors
    ///
    /// Not JSON, not an Open Coordinator Backup, or another version than 1.
    /// The message never quotes the file: serde's would (an `invalid type:
    /// string "…"` may be a key), only its position is given.
    pub fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        let backup: Self = serde_json::from_slice(bytes).map_err(|error| {
            let what = match error.classify() {
                serde_json::error::Category::Data => "a field is missing or of the wrong type",
                serde_json::error::Category::Eof => "the file ends too early",
                serde_json::error::Category::Syntax | serde_json::error::Category::Io => {
                    "it is not JSON"
                }
            };
            anyhow!(
                "not a readable Open Coordinator Backup: {what} (line {}, column {})",
                error.line(),
                error.column()
            )
        })?;
        if backup.metadata.format != FORMAT {
            bail!("not an Open Coordinator Backup: metadata.format is not {FORMAT:?}");
        }
        if backup.metadata.version != FORMAT_VERSION {
            bail!(
                "an Open Coordinator Backup version {}: this Moli reads version {FORMAT_VERSION}",
                backup.metadata.version
            );
        }
        Ok(backup)
    }

    /// The backup as JSON, indented as zigbee-herdsman writes it
    /// (`JSON.stringify(backup, null, 2)`).
    #[must_use]
    pub fn to_json(&self) -> String {
        // Strings, numbers and maps with string keys: always serializable.
        serde_json::to_string_pretty(self).expect("a backup always serializes")
    }

    /// What to file from this backup.
    ///
    /// # Errors
    ///
    /// Not an EmberZNet backup written by zigbee-herdsman, or a value that
    /// is not what it should be (key, PAN, channel, IEEE address).
    pub fn import(&self) -> anyhow::Result<Import> {
        // The byte order of the EUI64s and the extended PAN ID depends on
        // the writer: only zigbee-herdsman's ember adapter is read.
        let source = self.metadata.source.as_deref().unwrap_or_default();
        if !source.starts_with("zigbee-herdsman") {
            bail!(
                "this backup was written by {source:?}: only Zigbee2MQTT's (zigbee-herdsman) are read"
            );
        }
        // herdsman's `getStoredBackup`.
        let Some(ezsp) = self.stack_specific.as_ref().and_then(|s| s.ezsp.as_ref()) else {
            bail!(
                "not an EmberZNet backup (no stack_specific.ezsp): the dongle must be the one Zigbee2MQTT ran"
            );
        };
        match self
            .metadata
            .internal
            .get("ezspVersion")
            .and_then(serde_json::Value::as_u64)
        {
            None => bail!("not an EmberZNet backup (no metadata.internal.ezspVersion)"),
            Some(version) if version < OLDEST_EZSP_VERSION => bail!(
                "a backup of EZSP {version}: none older than EZSP {OLDEST_EZSP_VERSION} is restored"
            ),
            Some(_) => {}
        }
        let network_key = Key::from_hex(self.network_key.key.0.as_str())
            .ok_or_else(|| anyhow!("network_key.key is not a 128-bit key (32 hex digits)"))?;
        let tc_link_key = match &ezsp.hashed_tclk {
            Some(key) => Some(Key::from_hex(key.0.as_str()).ok_or_else(|| {
                anyhow!("stack_specific.ezsp.hashed_tclk is not a 128-bit key (32 hex digits)")
            })?),
            None => None,
        };
        let pan_id = u16::from_str_radix(&self.pan_id, 16)
            .ok()
            .filter(|pan| *pan != 0x0000 && *pan != 0xFFFF)
            .ok_or_else(|| anyhow!("pan_id {:?} is not a PAN ID", self.pan_id))?;
        let extended_pan_id = hex_bytes::<8>(&self.extended_pan_id).ok_or_else(|| {
            anyhow!(
                "extended_pan_id {:?} is not 16 hex digits",
                self.extended_pan_id
            )
        })?;
        if !network::CHANNELS.contains(&self.channel) {
            bail!(
                "channel {} is not a Zigbee channel (11 to 26)",
                self.channel
            );
        }
        let coordinator_ieee = herdsman_eui64(&self.coordinator_ieee).ok_or_else(|| {
            anyhow!(
                "coordinator_ieee {:?} is not an EUI64 (16 hex digits)",
                self.coordinator_ieee
            )
        })?;
        let mut devices = Vec::with_capacity(self.devices.len());
        for (n, device) in self.devices.iter().enumerate() {
            let ieee = herdsman_eui64(&device.ieee_address).ok_or_else(|| {
                anyhow!(
                    "devices[{n}].ieee_address {:?} is not an EUI64 (16 hex digits)",
                    device.ieee_address
                )
            })?;
            let nwk = device
                .nwk_address
                .as_deref()
                .map(|nwk| u16::from_str_radix(nwk, 16))
                .transpose()
                .with_context(|| format!("devices[{n}].nwk_address is not a short address"))?;
            if ieee != coordinator_ieee {
                devices.push(DeviceRecord {
                    ieee,
                    nwk,
                    interviewed: false,
                });
            }
        }
        Ok(Import {
            record: NetworkRecord {
                pan_id,
                extended_pan_id,
                channel: self.channel,
                coordinator_ieee,
                nwk_update_id: self.nwk_update_id,
                frame_counter: Some(self.network_key.frame_counter),
                // Fresh: Zigbee2MQTT is stopped, no dongle has sent since.
                frame_counter_fresh: true,
                key_sequence_number: Some(self.network_key.sequence_number),
            },
            network_key,
            tc_link_key,
            devices,
        })
    }
}

/// An EUI64 as zigbee-herdsman's ember adapter writes it in a backup: the
/// bytes little-endian (`Buffer.from(eui64.substring(2), "hex").reverse()`
/// in its `backup`; its tests read `coordinator_ieee` with
/// `eui64LEBufferToHex`), to the usual `0x…` form.
fn herdsman_eui64(text: &str) -> Option<String> {
    hex_bytes::<8>(text).map(network::ieee)
}

/// Exactly `2 * N` hex digits, either case.
fn hex_bytes<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; N];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

fn devices(count: usize) -> String {
    if count == 1 {
        "1 device".to_owned()
    } else {
        format!("{count} devices")
    }
}

impl Import {
    /// Puts this network in `store`. Over another network (another PAN or
    /// extended PAN), refused unless `force`; then its devices go too.
    ///
    /// # Errors
    ///
    /// The store records another network and `force` is not set.
    pub fn apply(&self, store: &mut Store, force: bool) -> anyhow::Result<()> {
        if let Some(known) = &store.network
            && !known.same_network(&self.record)
        {
            if !force {
                bail!(
                    "this instance already records another network (PAN {:#06x}, extended PAN {}, {}): \
                     importing replaces it and its devices. Run again with --force to replace it",
                    known.pan_id,
                    network::hex(&known.extended_pan_id),
                    devices(store.devices.len())
                );
            }
            store.devices.clear();
        }
        store.network = Some(self.record.clone());
        for device in &self.devices {
            store
                .devices
                .entry(device.ieee.clone())
                .or_insert_with(|| device.clone());
        }
        Ok(())
    }

    /// How many devices it brings, as people read it (`2 devices`).
    #[must_use]
    pub fn devices_text(&self) -> String {
        devices(self.devices.len())
    }
}

/// The outgoing NWK frame counter a restore starts from: the saved one
/// plus [`FRAME_COUNTER_MARGIN`]; `None` when that leaves no room below
/// the `uint32_t` limit (the stack would stop sending; the network needs a
/// new key, never a saturated counter).
#[must_use]
pub fn restore_counter(saved: u32) -> Option<u32> {
    saved
        .checked_add(FRAME_COUNTER_MARGIN)
        .filter(|start| *start < u32::MAX)
}

/// The dongle's IEEE address when it is not `record`'s coordinator
/// (`getEui64`, nothing written): `None` when it is.
///
/// # Errors
///
/// The link failing or an unreadable answer.
pub async fn other_dongle(ezsp: &Ezsp, record: &NetworkRecord) -> anyhow::Result<Option<String>> {
    let answer = ezsp.call(ids::GET_EUI64, &[]).await?;
    let (eui64, _) = answer.split_first_chunk::<8>().ok_or(EzspError::TooShort)?;
    let dongle = network::ieee(*eui64);
    if dongle.eq_ignore_ascii_case(&record.coordinator_ieee) {
        return Ok(None);
    }
    tracing::warn!(
        %dongle,
        expected = %record.coordinator_ieee,
        "the dongle is not the coordinator of the network to restore"
    );
    Ok(Some(dongle))
}

/// How a restore ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restored {
    /// The network runs again; as the dongle now reports it.
    Network(NetworkRecord),
    /// The dongle is not the backup's coordinator (its IEEE address,
    /// `dongle`, is another): nothing was sent to it.
    OtherDongle { dongle: String },
}

/// Restores `record`'s network on a dongle that has none, with `keys`
/// (the vault's), its outgoing NWK frame counter starting at the saved
/// `frame_counter` plus [`FRAME_COUNTER_MARGIN`].
///
/// Only on the backup's own dongle (its IEEE address is
/// `record.coordinator_ieee`): the devices know the trust center by that
/// address, and rewriting a dongle's address (a token that cannot be
/// undone) is not done; another dongle is [`Restored::OtherDongle`],
/// nothing sent to it.
///
/// Then herdsman's `formNetwork(fromBackup = true)`
/// ([`network::form_network`]): `setValue(NWK_FRAME_COUNTER)`
/// (`ezspSetNWKFrameCounter`), the initial security state with
/// `NO_FRAME_COUNTER_RESET` and the backup's key sequence number, the
/// extended security bitmask, no key table cleared (herdsman imports the
/// backup's link keys there instead; Zigbee2MQTT writes none), `formNetwork`
/// with the saved PAN, extended PAN, channel and update id, the network up,
/// stack tokens written.
///
/// # Errors
///
/// A counter without room for the margin ([`restore_counter`]), the link
/// failing, the NCP refusing a step, or the network not coming up.
pub async fn restore(
    ezsp: &Ezsp,
    callbacks: &mut mpsc::Receiver<Callback>,
    protocol: u8,
    keys: &Keys,
    record: &NetworkRecord,
    frame_counter: u32,
) -> anyhow::Result<Restored> {
    let Some(start) = restore_counter(frame_counter) else {
        bail!(
            "the saved frame counter {frame_counter} leaves no room for the restore margin: the network needs a new key"
        );
    };
    if let Some(dongle) = other_dongle(ezsp, record).await? {
        return Ok(Restored::OtherDongle { dongle });
    }
    if frame_counter > FRAME_COUNTER_NEAR_END {
        tracing::warn!(
            frame_counter,
            "the network key frame counter is reaching its limit: a new network key will be needed soon"
        );
    }
    tracing::info!(
        pan_id = format_args!("{:#06x}", record.pan_id),
        channel = record.channel,
        frame_counter = start,
        "restoring the Zigbee network"
    );
    let restored = network::form_network(
        ezsp,
        callbacks,
        protocol,
        keys,
        &network::saved_parameters(record),
        Some(FromBackup {
            key_sequence_number: record.key_sequence_number.unwrap_or(0),
            frame_counter: start,
        }),
    )
    .await?;
    tracing::info!(
        pan_id = format_args!("{:#06x}", restored.pan_id),
        channel = restored.channel,
        "Zigbee network restored"
    );
    Ok(Restored::Network(restored))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ezsp::{NetworkParameters, Version, ids};
    use crate::network::tests::{EUI64, network_parameters_on};
    use crate::network::{Key, hex};
    use crate::sim::FakeNcp;
    use crate::store::tests::record;

    const EMBER_13: Version = Version {
        protocol: 13,
        stack_type: 2,
        stack_version: 0x7440,
    };

    /// Fictitious, with the values of the docs: PAN `0x1a62`, key `01 03 05
    /// … 0D`, frame counter 123456.
    const FIXTURE: &[u8] =
        include_bytes!("../../../integrations/zigbee/fixtures/coordinator_backup.json");
    const DOC_KEY: &str = "01030507090b0d0f00020406080a0c0d";
    const TCLK: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

    fn fixture() -> OpenCoordinatorBackup {
        OpenCoordinatorBackup::parse(FIXTURE).unwrap()
    }

    /// The fixture with `change` applied to its JSON.
    fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut json: serde_json::Value = serde_json::from_slice(FIXTURE).unwrap();
        change(&mut json);
        serde_json::to_vec(&json).unwrap()
    }

    /// The fixture's extended PAN ID: asymmetric, its byte order is pinned
    /// (the stack's order, copied as herdsman writes it).
    const EXTENDED_PAN_ID: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

    /// The fixture's network, as the store keeps it.
    fn on_fixture_pan(pan_id: u16) -> NetworkRecord {
        NetworkRecord {
            extended_pan_id: EXTENDED_PAN_ID,
            ..record(pan_id)
        }
    }

    fn imported() -> NetworkRecord {
        NetworkRecord {
            frame_counter: Some(123_456),
            frame_counter_fresh: true,
            key_sequence_number: Some(0),
            ..on_fixture_pan(0x1a62)
        }
    }

    fn doc_keys() -> Keys {
        Keys {
            network: Key::from_hex(DOC_KEY).unwrap(),
            tc_link: Key::from_hex(TCLK).unwrap(),
        }
    }

    #[test]
    fn parses_fixture() {
        let backup = fixture();
        assert_eq!(backup.metadata.format, FORMAT);
        assert_eq!(backup.metadata.version, 1);
        assert_eq!(backup.pan_id, "1a62");
        assert_eq!(backup.channel, 15);
        assert_eq!(backup.channel_mask, (11..=26).collect::<Vec<u8>>());
        assert_eq!(backup.network_key.frame_counter, 123_456);
        assert_eq!(backup.devices.len(), 2);
        let import = backup.import().unwrap();
        assert_eq!(import.record, imported());
        assert_eq!(import.network_key.to_hex(), DOC_KEY);
        assert_eq!(
            import.tc_link_key.map(|key| key.to_hex()).as_deref(),
            Some(TCLK)
        );
        assert_eq!(
            import.devices,
            [
                DeviceRecord {
                    ieee: "0x00124b0000000002".into(),
                    nwk: Some(0x4f2a),
                    interviewed: false,
                },
                DeviceRecord {
                    ieee: "0x00124b0000000003".into(),
                    nwk: None,
                    interviewed: false,
                },
            ]
        );
        // Without `hashed_tclk`: no trust center link key.
        let bare = changed(|json| json["stack_specific"]["ezsp"] = serde_json::json!({}));
        let import = OpenCoordinatorBackup::parse(&bare)
            .unwrap()
            .import()
            .unwrap();
        assert_eq!(import.tc_link_key, None);
    }

    #[test]
    fn round_trips_backup() {
        let backup = fixture();
        let json = backup.to_json();
        assert_eq!(
            OpenCoordinatorBackup::parse(json.as_bytes()).unwrap(),
            backup
        );
        // Field for field, the writer's own data and the key's case included.
        let written: serde_json::Value = serde_json::from_str(&json).unwrap();
        let read: serde_json::Value = serde_json::from_slice(FIXTURE).unwrap();
        assert_eq!(written, read);
        assert!(json.starts_with("{\n  \"metadata\": {"), "{json}");
    }

    #[test]
    fn keys_of_a_backup_are_never_shown() {
        let backup = fixture();
        let shown = format!("{backup:?} {:?}", backup.import().unwrap()).to_lowercase();
        assert!(!shown.contains("0103"), "{shown}");
        assert!(!shown.contains("5a5a"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }

    #[test]
    fn what_cannot_be_read_is_refused() {
        let parse = |change: fn(&mut serde_json::Value)| {
            OpenCoordinatorBackup::parse(&changed(change))
                .map(|_| ())
                .unwrap_err()
                .to_string()
        };
        let import = |change: fn(&mut serde_json::Value)| {
            OpenCoordinatorBackup::parse(&changed(change))
                .and_then(|backup| backup.import())
                .map(|_| ())
                .unwrap_err()
                .to_string()
        };
        assert!(OpenCoordinatorBackup::parse(b"{").is_err());
        let error = parse(|json| json["metadata"]["format"] = "other".into());
        assert!(error.contains(FORMAT), "{error}");
        let error = parse(|json| json["metadata"]["version"] = 2.into());
        assert!(error.contains("version 2"), "{error}");
        // Not zigbee-herdsman's ember adapter: another byte order.
        let error = import(|json| json["metadata"]["source"] = "bellows@0.40.0".into());
        assert!(error.contains("zigbee-herdsman"), "{error}");
        let error = import(|json| json["stack_specific"] = serde_json::json!({"zstack": {}}));
        assert!(error.contains("EmberZNet"), "{error}");
        let error = import(|json| json["metadata"]["internal"]["ezspVersion"] = 11.into());
        assert!(error.contains("EZSP 11"), "{error}");
        let error = import(|json| json["network_key"]["key"] = "0103".into());
        assert!(error.contains("network_key"), "{error}");
        assert!(!error.contains("0103"), "{error}");
        let error = import(|json| json["stack_specific"]["ezsp"]["hashed_tclk"] = "zz".into());
        assert!(error.contains("hashed_tclk"), "{error}");
        let error = import(|json| json["channel"] = 27.into());
        assert!(error.contains("27"), "{error}");
        let error = import(|json| json["pan_id"] = "ffff".into());
        assert!(error.contains("pan_id"), "{error}");
        let error = import(|json| json["extended_pan_id"] = "dd".into());
        assert!(error.contains("extended_pan_id"), "{error}");
        let error = import(|json| json["coordinator_ieee"] = "0x00".into());
        assert!(error.contains("coordinator_ieee"), "{error}");
        let error = import(|json| json["devices"][0]["ieee_address"] = "xyz".into());
        assert!(error.contains("ieee_address"), "{error}");
    }

    #[test]
    fn a_malformed_backup_never_shows_its_keys() {
        // A key where an object belongs: serde's own message would quote it.
        let bare = changed(|json| json["network_key"] = DOC_KEY.to_uppercase().into());
        let error = OpenCoordinatorBackup::parse(&bare).unwrap_err();
        let shown = format!("{error:#} {error:?}").to_lowercase();
        assert!(!shown.contains("0103"), "{shown}");
        assert!(shown.contains("wrong type"), "{shown}");
        assert!(shown.contains("line"), "{shown}");
        // A device link key where a string belongs: same fixed message.
        let bare = changed(|json| {
            json["devices"][0]["link_key"] =
                serde_json::json!({"key": 5, "rx_counter": 0, "tx_counter": 0});
        });
        let shown = format!("{:#}", OpenCoordinatorBackup::parse(&bare).unwrap_err());
        assert!(!shown.contains("integer"), "{shown}");
        assert!(shown.contains("wrong type"), "{shown}");
        // Not JSON at all: no excerpt either.
        let shown = format!(
            "{:#}",
            OpenCoordinatorBackup::parse(b"01030507090B0D0F00020406080A0C0D").unwrap_err()
        );
        assert!(!shown.contains("0103"), "{shown}");
    }

    #[test]
    fn another_network_is_replaced_only_with_force() {
        let import = fixture().import().unwrap();
        let mut other = Store {
            network: Some(record(0x1234)),
            ..Store::default()
        };
        other.devices.insert(
            "0x00124b0000000009".into(),
            DeviceRecord {
                ieee: "0x00124b0000000009".into(),
                nwk: Some(0x1111),
                interviewed: true,
            },
        );
        let before = other.clone();
        let error = import.apply(&mut other, false).unwrap_err().to_string();
        assert!(
            error.contains("0x1234") && error.contains("--force"),
            "{error}"
        );
        assert!(error.contains("1 device"), "{error}");
        assert_eq!(other, before, "untouched");
        import.apply(&mut other, true).unwrap();
        assert_eq!(other.network, Some(imported()));
        let ieees: Vec<_> = other.devices.keys().map(String::as_str).collect();
        assert_eq!(ieees, ["0x00124b0000000002", "0x00124b0000000003"]);
        // An empty store, or the same network: no force needed; what the
        // store knows of a device stays.
        let mut empty = Store::default();
        import.apply(&mut empty, false).unwrap();
        assert_eq!(empty.network, Some(imported()));
        let mut same = Store {
            network: Some(NetworkRecord {
                channel: 20,
                ..on_fixture_pan(0x1a62)
            }),
            ..Store::default()
        };
        let known = DeviceRecord {
            ieee: "0x00124b0000000002".into(),
            nwk: Some(0x2222),
            interviewed: true,
        };
        same.devices.insert(known.ieee.clone(), known.clone());
        import.apply(&mut same, false).unwrap();
        assert_eq!(same.network, Some(imported()));
        assert_eq!(same.devices["0x00124b0000000002"], known);
        assert_eq!(same.devices.len(), 2);
    }

    /// An NCP that accepts a restore and then reports the network.
    fn restoring_ncp(eui64: [u8; 8]) -> (FakeNcp, tokio::io::DuplexStream) {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        for id in [
            ids::SET_VALUE,
            ids::SET_INITIAL_SECURITY_STATE,
            ids::FORM_NETWORK,
        ] {
            ncp.respond(id, &[0x00]);
        }
        ncp.respond(
            ids::GET_NETWORK_PARAMETERS,
            &network_parameters_on(EXTENDED_PAN_ID, 0x1a62, 20),
        );
        ncp.respond(ids::GET_EUI64, &eui64);
        (ncp, io)
    }

    #[tokio::test]
    async fn restore_bumps_frame_counter() {
        let (ncp, io) = restoring_ncp(EUI64);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let saved = NetworkRecord {
            channel: 20,
            nwk_update_id: 3,
            key_sequence_number: Some(2),
            ..imported()
        };
        let forming = async {
            ncp.wait_command(ids::FORM_NETWORK).await;
            ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        };
        let keys = doc_keys();
        let (restored, ()) = tokio::join!(
            restore(
                &ezsp,
                &mut callbacks,
                version.protocol,
                &keys,
                &saved,
                123_456
            ),
            forming
        );
        assert_eq!(
            restored.unwrap(),
            Restored::Network(NetworkRecord {
                channel: 20,
                ..on_fixture_pan(0x1a62)
            })
        );
        let received = ncp.received();
        let sent: Vec<_> = received[2..].iter().map(|(id, _)| *id).collect();
        // herdsman's `formNetwork(fromBackup)`: no key table cleared.
        assert_eq!(
            sent,
            [
                ids::GET_EUI64,
                ids::SET_VALUE,
                ids::SET_INITIAL_SECURITY_STATE,
                ids::SET_VALUE,
                ids::FORM_NETWORK,
                ids::SET_VALUE,
                ids::GET_NETWORK_PARAMETERS,
                ids::GET_EUI64,
            ]
        );
        // `NWK_FRAME_COUNTER` (0x23), 4 bytes: 123456 + 1024, little-endian.
        assert_eq!(123_456 + FRAME_COUNTER_MARGIN, 0x0001_E640);
        assert_eq!(received[3].1, [0x23, 4, 0x40, 0xE6, 0x01, 0x00]);
        let state = &received[4].1;
        // ... | NO_FRAME_COUNTER_RESET (0x1000): 0x1B84.
        assert_eq!(state[..2], [0x84, 0x1B]);
        assert_eq!(hex(&state[2..18]), TCLK, "trust center link key");
        assert_eq!(hex(&state[18..34]), DOC_KEY, "network key");
        assert_eq!(state[34], 2, "key sequence number");
        assert_eq!(state[35..], [0xFF; 8]);
        assert_eq!(received[5].1, [0x0a, 2, 0x10, 0x01]);
        let formed = NetworkParameters::decode(&received[6].1).unwrap().0;
        assert_eq!(
            formed,
            NetworkParameters {
                // The fixture's `0123456789abcdef`, byte for byte.
                extended_pan_id: EXTENDED_PAN_ID,
                pan_id: 0x1a62,
                radio_tx_power: 5,
                radio_channel: 20,
                join_method: 0,
                nwk_manager_id: 0,
                nwk_update_id: 3,
                channels: 0x07ff_f800,
            }
        );
        assert_eq!(received[7].1, [0x07, 1, 1]);
    }

    #[test]
    fn the_restore_counter_never_saturates() {
        assert_eq!(restore_counter(123_456), Some(124_480));
        assert_eq!(restore_counter(0), Some(1024));
        assert_eq!(restore_counter(u32::MAX - 1025), Some(u32::MAX - 1));
        for full in [u32::MAX - 1024, u32::MAX - 10, u32::MAX] {
            assert_eq!(restore_counter(full), None, "{full:#x}");
        }
    }

    #[tokio::test]
    async fn a_counter_without_room_is_refused() {
        let (ncp, io) = restoring_ncp(EUI64);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let error = restore(
            &ezsp,
            &mut callbacks,
            version.protocol,
            &doc_keys(),
            &imported(),
            u32::MAX - 10,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("no room"), "{error}");
        assert_eq!(ncp.received().len(), 2, "nothing sent after the handshake");
    }

    #[tokio::test]
    async fn other_dongle_is_refused() {
        let other = [0x09, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
        let (ncp, io) = restoring_ncp(other);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let restored = restore(
            &ezsp,
            &mut callbacks,
            version.protocol,
            &doc_keys(),
            &imported(),
            123_456,
        )
        .await
        .unwrap();
        assert_eq!(
            restored,
            Restored::OtherDongle {
                dongle: "0x00124b0000000009".into()
            }
        );
        let sent: Vec<_> = ncp.received()[2..].iter().map(|(id, _)| *id).collect();
        assert_eq!(sent, [ids::GET_EUI64], "nothing written to the dongle");
    }

    #[tokio::test]
    async fn a_refused_frame_counter_stops_the_restore() {
        let (ncp, io) = restoring_ncp(EUI64);
        ncp.respond(ids::SET_VALUE, &[0x01]);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let error = restore(
            &ezsp,
            &mut callbacks,
            version.protocol,
            &doc_keys(),
            &imported(),
            123_456,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("frame counter"), "{error}");
        assert!(
            !ncp.received().iter().any(|(id, _)| {
                *id == ids::SET_INITIAL_SECURITY_STATE || *id == ids::FORM_NETWORK
            }),
            "nothing formed"
        );
    }
}

//! APS messages through the NCP: the `EmberApsFrame` every message carries,
//! `sendUnicast` / `sendBroadcast`, and the callbacks that answer them
//! (`messageSentHandler`) or bring messages in (`incomingMessageHandler`).
//!
//! Layouts from zigbee-herdsman (MIT), `src/adapter/ember/ezsp/buffalo.ts`
//! (`writeEmberApsFrame`, `readEmberRxPacketInfo`, `writePayload`) and
//! `src/adapter/ember/ezsp/ezsp.ts` (`ezspSendUnicast`, `ezspSendBroadcast`,
//! the `MESSAGE_SENT_HANDLER` and `INCOMING_MESSAGE_HANDLER` cases of the
//! callback dispatch); integers little-endian.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{bail, ensure};
use tokio::sync::oneshot;

use crate::ezsp::{Callback, Ezsp, EzspError, SL_STATUS_VERSION, SlStatus, ids, read_status};

/// How long a unicast gets for its `messageSentHandler`.
pub const SENT_TIMEOUT: Duration = Duration::from_secs(10);

/// `EmberApsOption`s herdsman puts on every request (`DEFAULT_APS_OPTIONS`
/// of `emberAdapter.ts`): `RETRY | ENABLE_ROUTE_DISCOVERY |
/// ENABLE_ADDRESS_DISCOVERY` (`enums.ts`, `EmberApsOption`).
pub const DEFAULT_OPTIONS: u16 = 0x0040 | 0x0100 | 0x1000;

/// `EmberOutgoingMessageType.DIRECT` (`enums.ts`).
const OUTGOING_DIRECT: u8 = 0;

/// `NULL_NODE_ID` (`src/zspec/consts.ts`): no alias for a broadcast.
const NULL_NODE_ID: u16 = 0xFFFF;

/// Message tags (and ZDO transaction numbers, herdsman uses one for both)
/// stay in 1..=0x7F: herdsman's `APPLICATION_ZDO_SEQUENCE_MASK`, the stack
/// numbering its own requests with the high bit set.
const TAG_MASK: u8 = 0x7F;

/// `EmberApsFrame`, as `writeEmberApsFrame` lays it out (11 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApsFrame {
    pub profile: u16,
    pub cluster: u16,
    pub src_ep: u8,
    pub dst_ep: u8,
    /// `EmberApsOption` bits.
    pub options: u16,
    pub group: u16,
    /// APS counter; set by the stack on the way out.
    pub seq: u8,
}

impl ApsFrame {
    /// Bytes on the wire.
    pub const LEN: usize = 11;

    /// Appends the frame to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.profile.to_le_bytes());
        out.extend_from_slice(&self.cluster.to_le_bytes());
        out.push(self.src_ep);
        out.push(self.dst_ep);
        out.extend_from_slice(&self.options.to_le_bytes());
        out.extend_from_slice(&self.group.to_le_bytes());
        out.push(self.seq);
    }

    /// Reads the frame at the start of `bytes`; returns what follows.
    pub fn decode(bytes: &[u8]) -> Result<(Self, &[u8]), EzspError> {
        let (raw, rest) = bytes
            .split_first_chunk::<{ Self::LEN }>()
            .ok_or(EzspError::TooShort)?;
        let [p0, p1, c0, c1, src_ep, dst_ep, o0, o1, g0, g1, seq] = *raw;
        let frame = Self {
            profile: u16::from_le_bytes([p0, p1]),
            cluster: u16::from_le_bytes([c0, c1]),
            src_ep,
            dst_ep,
            options: u16::from_le_bytes([o0, o1]),
            group: u16::from_le_bytes([g0, g1]),
            seq,
        };
        Ok((frame, rest))
    }
}

/// A message the NCP received for the coordinator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    /// Short address of the sender.
    pub nwk: u16,
    pub frame: ApsFrame,
    /// Link quality of the last hop.
    pub lqi: u8,
    pub payload: Vec<u8>,
}

impl Incoming {
    /// Reads an `incomingMessageHandler`: before v14 the last hop's LQI and
    /// RSSI come first, then the sender and the binding and address
    /// indexes; from v14 on an `EmberRxPacketInfo` (sender, its EUI64, the
    /// indexes, LQI, RSSI, timestamp). Then the payload, its length first.
    pub fn decode(params: &[u8], protocol: u8) -> Result<Self, EzspError> {
        let (_kind, rest) = params.split_first().ok_or(EzspError::TooShort)?;
        let (frame, rest) = ApsFrame::decode(rest)?;
        let (nwk, lqi, rest) = if protocol < SL_STATUS_VERSION {
            // lqi, rssi, sender (2), binding index, address index
            let (raw, rest) = rest.split_first_chunk::<6>().ok_or(EzspError::TooShort)?;
            (u16::from_le_bytes([raw[2], raw[3]]), raw[0], rest)
        } else {
            // sender (2), EUI64 (8), binding index, address index, lqi,
            // rssi, timestamp (4)
            let (raw, rest) = rest.split_first_chunk::<18>().ok_or(EzspError::TooShort)?;
            (u16::from_le_bytes([raw[0], raw[1]]), raw[12], rest)
        };
        let payload = read_payload(rest)?;
        Ok(Self {
            nwk,
            frame,
            lqi,
            payload: payload.to_vec(),
        })
    }
}

/// A `writePayload`d buffer: its length (one byte), then the bytes.
fn read_payload(bytes: &[u8]) -> Result<&[u8], EzspError> {
    let (&len, rest) = bytes.split_first().ok_or(EzspError::TooShort)?;
    rest.get(..usize::from(len)).ok_or(EzspError::TooShort)
}

/// A `messageSentHandler`: how a message the host sent ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageSent {
    pub status: SlStatus,
    /// `EmberOutgoingMessageType`.
    pub kind: u8,
    /// Destination (or index) the message was sent to.
    pub destination: u16,
    pub frame: ApsFrame,
    pub tag: u16,
}

impl MessageSent {
    /// Before v14: type, destination, APS frame, tag (one byte), then the
    /// status (one byte); from v14 on the status (32 bits) comes first and
    /// the tag is 16 bits. The message contents that may follow are not read.
    pub fn decode(params: &[u8], protocol: u8) -> Result<Self, EzspError> {
        let legacy = protocol < SL_STATUS_VERSION;
        let (status, rest) = if legacy {
            (SlStatus::Ok, params)
        } else {
            read_status(params, protocol)?
        };
        let (&kind, rest) = rest.split_first().ok_or(EzspError::TooShort)?;
        let (destination, rest) = rest.split_first_chunk::<2>().ok_or(EzspError::TooShort)?;
        let (frame, rest) = ApsFrame::decode(rest)?;
        let (tag, status) = if legacy {
            let (&tag, rest) = rest.split_first().ok_or(EzspError::TooShort)?;
            (u16::from(tag), read_status(rest, protocol)?.0)
        } else {
            let (tag, _) = rest.split_first_chunk::<2>().ok_or(EzspError::TooShort)?;
            (u16::from_le_bytes(*tag), status)
        };
        Ok(Self {
            status,
            kind,
            destination: u16::from_le_bytes(*destination),
            frame,
            tag,
        })
    }
}

/// Sends APS messages and matches their `messageSentHandler` by tag; clones
/// share the tags. Whoever reads the callbacks hands every
/// `messageSentHandler` to [`Aps::on_message_sent`]: a unicast never waits
/// on the loop that reads them.
#[derive(Clone, Debug)]
pub struct Aps {
    ezsp: Ezsp,
    protocol: u8,
    tags: Arc<Mutex<Tags>>,
}

#[derive(Debug, Default)]
struct Tags {
    last: u8,
    /// Unicasts awaiting their `messageSentHandler`, by tag.
    waiting: HashMap<u16, oneshot::Sender<SlStatus>>,
}

impl Tags {
    /// The tag after `last` in 1..=0x7F, skipping those awaited (if all
    /// are, the next one anyway: its older waiter is then replaced).
    fn next(&mut self) -> u8 {
        for _ in 0..TAG_MASK {
            self.last = (self.last & TAG_MASK) % TAG_MASK + 1;
            if !self.waiting.contains_key(&u16::from(self.last)) {
                break;
            }
        }
        self.last
    }
}

impl Aps {
    #[must_use]
    pub fn new(ezsp: Ezsp, protocol: u8) -> Self {
        Self {
            ezsp,
            protocol,
            tags: Arc::new(Mutex::new(Tags::default())),
        }
    }

    #[must_use]
    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    #[must_use]
    pub fn ezsp(&self) -> &Ezsp {
        &self.ezsp
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tags> {
        self.tags.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The next tag (1..=0x7F), skipping those still awaited; also the ZDO
    /// transaction number of a ZDO request (herdsman's `sendZdo`).
    #[must_use]
    pub fn next_tag(&self) -> u8 {
        self.lock().next()
    }

    /// Sends `payload` to `nwk` (`sendUnicast`, `DIRECT`) with `frame`, then
    /// waits up to [`SENT_TIMEOUT`] for the `messageSentHandler` of its tag.
    /// `payload` starts with whatever transaction number its protocol needs.
    ///
    /// # Errors
    ///
    /// The link failing, the NCP refusing the message, no `messageSentHandler`
    /// in time, or one reporting a failed delivery.
    pub async fn send_unicast(
        &self,
        nwk: u16,
        frame: &ApsFrame,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        self.send_unicast_tagged(nwk, frame, self.next_tag(), payload)
            .await
    }

    /// [`Aps::send_unicast`] with a chosen `tag` (a ZDO request's transaction
    /// number).
    ///
    /// # Errors
    ///
    /// As [`Aps::send_unicast`].
    pub async fn send_unicast_tagged(
        &self,
        nwk: u16,
        frame: &ApsFrame,
        tag: u8,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let len = u8::try_from(payload.len())
            .map_err(|_| anyhow::anyhow!("APS payload of {} bytes", payload.len()))?;
        let mut params = vec![OUTGOING_DIRECT];
        params.extend_from_slice(&nwk.to_le_bytes());
        frame.encode(&mut params);
        self.push_tag(&mut params, tag);
        params.push(len);
        params.extend_from_slice(payload);
        // Before sending: the callback may come before the response.
        let (_waiting, receiver) = self.wait_for(tag);
        let answer = self.ezsp.call(ids::SEND_UNICAST, &params).await?;
        let (status, _) = read_status(&answer, self.protocol)?;
        ensure!(
            status == SlStatus::Ok,
            "sendUnicast to {nwk:#06x} refused: {status:?}"
        );
        match tokio::time::timeout(SENT_TIMEOUT, receiver).await {
            Ok(Ok(SlStatus::Ok)) => Ok(()),
            Ok(Ok(status)) => bail!("message to {nwk:#06x} not delivered: {status:?}"),
            Ok(Err(_)) => bail!("message to {nwk:#06x}: its tag was taken back"),
            Err(_) => bail!("message to {nwk:#06x}: no messageSentHandler within {SENT_TIMEOUT:?}"),
        }
    }

    /// Sends `payload` to a broadcast address (`sendBroadcast`, no alias)
    /// with `frame` and `tag`; its `messageSentHandler` is not awaited, as
    /// herdsman does for a broadcast.
    ///
    /// # Errors
    ///
    /// The link failing, or the NCP refusing the message.
    pub async fn send_broadcast(
        &self,
        destination: u16,
        frame: &ApsFrame,
        radius: u8,
        tag: u8,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let len = u8::try_from(payload.len())
            .map_err(|_| anyhow::anyhow!("APS payload of {} bytes", payload.len()))?;
        let mut params = Vec::with_capacity(20 + payload.len());
        if self.protocol >= SL_STATUS_VERSION {
            params.extend_from_slice(&NULL_NODE_ID.to_le_bytes());
            params.extend_from_slice(&destination.to_le_bytes());
            // NWK sequence of the alias: unused without one.
            params.push(0);
        } else {
            params.extend_from_slice(&destination.to_le_bytes());
        }
        frame.encode(&mut params);
        params.push(radius);
        self.push_tag(&mut params, tag);
        params.push(len);
        params.extend_from_slice(payload);
        let answer = self.ezsp.call(ids::SEND_BROADCAST, &params).await?;
        let (status, _) = read_status(&answer, self.protocol)?;
        ensure!(
            status == SlStatus::Ok,
            "sendBroadcast to {destination:#06x} refused: {status:?}"
        );
        Ok(())
    }

    /// A tag is one byte before v14, two from v14 on.
    fn push_tag(&self, params: &mut Vec<u8>, tag: u8) {
        if self.protocol >= SL_STATUS_VERSION {
            params.extend_from_slice(&u16::from(tag).to_le_bytes());
        } else {
            params.push(tag);
        }
    }

    /// Registers `tag` as awaited until the guard is dropped.
    fn wait_for(&self, tag: u8) -> (Waiting<'_>, oneshot::Receiver<SlStatus>) {
        let (sender, receiver) = oneshot::channel();
        self.lock().waiting.insert(u16::from(tag), sender);
        let waiting = Waiting {
            aps: self,
            tag: u16::from(tag),
        };
        (waiting, receiver)
    }

    /// Takes a `messageSentHandler` (`true`), handing its status to the
    /// unicast awaiting its tag; other callbacks are left (`false`).
    pub fn on_message_sent(&self, callback: &Callback) -> bool {
        if callback.id != ids::MESSAGE_SENT_HANDLER {
            return false;
        }
        match MessageSent::decode(&callback.params, self.protocol) {
            Ok(sent) => {
                let waiter = self.lock().waiting.remove(&sent.tag);
                if let Some(waiter) = waiter {
                    let _ = waiter.send(sent.status);
                } else {
                    // A broadcast, or a unicast whose sender gave up.
                    tracing::debug!(
                        tag = sent.tag,
                        destination = format_args!("{:#06x}", sent.destination),
                        status = ?sent.status,
                        "message sent"
                    );
                }
            }
            Err(error) => tracing::warn!(%error, "unreadable messageSentHandler"),
        }
        true
    }
}

/// A tag awaited; released however the wait ends (even a dropped future).
struct Waiting<'a> {
    aps: &'a Aps,
    tag: u16,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.aps.lock().waiting.remove(&self.tag);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ezsp::Version;
    use crate::sim::FakeNcp;

    pub(crate) const EMBER_13: Version = Version {
        protocol: 13,
        stack_type: 2,
        stack_version: 0x7440,
    };
    const EMBER_14: Version = Version {
        protocol: 14,
        stack_type: 2,
        stack_version: 0x8000,
    };

    /// A ZCL On/Off frame from endpoint 1 to endpoint 1.
    fn on_off() -> ApsFrame {
        ApsFrame {
            profile: 0x0104,
            cluster: 0x0006,
            src_ep: 1,
            dst_ep: 1,
            options: DEFAULT_OPTIONS,
            group: 0,
            seq: 0x2A,
        }
    }

    const ON_OFF_BYTES: [u8; ApsFrame::LEN] = [
        0x04, 0x01, // profile
        0x06, 0x00, // cluster
        0x01, // source endpoint
        0x01, // destination endpoint
        0x40, 0x11, // options
        0x00, 0x00, // group
        0x2A, // sequence
    ];

    #[test]
    fn aps_frame_round_trip() {
        let mut out = vec![0xAA];
        on_off().encode(&mut out);
        assert_eq!(out[0], 0xAA, "what was already there stays");
        assert_eq!(out[1..], ON_OFF_BYTES);
        let mut tail = out[1..].to_vec();
        tail.push(0x42);
        let (decoded, rest) = ApsFrame::decode(&tail).unwrap();
        assert_eq!(decoded, on_off());
        assert_eq!(rest, [0x42]);
        assert_eq!(
            ApsFrame::decode(&ON_OFF_BYTES[..10]),
            Err(EzspError::TooShort)
        );
    }

    #[test]
    fn default_options_are_herdsmans() {
        assert_eq!(DEFAULT_OPTIONS, 0x1140);
    }

    /// The same incoming message (an attribute report from 0x1234, LQI 200,
    /// RSSI -60) as v13 and v14 NCPs lay it out.
    fn incoming(protocol: u8) -> Vec<u8> {
        let mut params = vec![0x00]; // UNICAST
        params.extend_from_slice(&ON_OFF_BYTES);
        if protocol < 14 {
            params.extend_from_slice(&[200, 0xC4, 0x34, 0x12, 0xFF, 0xFF]);
        } else {
            params.extend_from_slice(&[0x34, 0x12]);
            params.extend_from_slice(&[1, 0, 0, 0, 0, 0x4b, 0x12, 0]); // EUI64
            params.extend_from_slice(&[0xFF, 0xFF, 200, 0xC4]);
            params.extend_from_slice(&[0x10, 0x20, 0x30, 0x40]); // timestamp
        }
        params.extend_from_slice(&[3, 0x18, 0x01, 0x0A]);
        params
    }

    #[test]
    fn incoming_message_v13_and_v14() {
        for protocol in [13, 14] {
            let message = Incoming::decode(&incoming(protocol), protocol).unwrap();
            assert_eq!(
                message,
                Incoming {
                    nwk: 0x1234,
                    frame: on_off(),
                    lqi: 200,
                    payload: vec![0x18, 0x01, 0x0A],
                },
                "v{protocol}"
            );
        }
        // Read with the other version's layout, a frame is too short or wrong.
        assert_ne!(
            Incoming::decode(&incoming(14), 13).ok().map(|m| m.nwk),
            Some(0x1234)
        );
        assert_eq!(
            Incoming::decode(&incoming(13), 14),
            Err(EzspError::TooShort)
        );
        // A payload longer than what arrived.
        let mut cut = incoming(13);
        cut.pop();
        assert_eq!(Incoming::decode(&cut, 13), Err(EzspError::TooShort));
    }

    /// A `messageSentHandler` for `tag`, as v13 and v14 NCPs lay it out.
    fn message_sent(protocol: u8, tag: u16, delivered: bool) -> Vec<u8> {
        let mut params = Vec::new();
        if protocol >= 14 {
            let status: u32 = if delivered { 0 } else { 0x0c02 };
            params.extend_from_slice(&status.to_le_bytes());
        }
        params.push(0); // DIRECT
        params.extend_from_slice(&0x1234u16.to_le_bytes());
        params.extend_from_slice(&ON_OFF_BYTES);
        if protocol >= 14 {
            params.extend_from_slice(&tag.to_le_bytes());
        } else {
            params.push(u8::try_from(tag).unwrap());
            params.push(if delivered { 0x00 } else { 0x66 });
        }
        params.push(0); // contents: none
        params
    }

    #[test]
    fn message_sent_v13_and_v14() {
        for protocol in [13, 14] {
            let sent = MessageSent::decode(&message_sent(protocol, 7, true), protocol).unwrap();
            assert_eq!(
                sent,
                MessageSent {
                    status: SlStatus::Ok,
                    kind: 0,
                    destination: 0x1234,
                    frame: on_off(),
                    tag: 7,
                },
                "v{protocol}"
            );
            let failed = MessageSent::decode(&message_sent(protocol, 7, false), protocol).unwrap();
            assert_eq!(failed.status, SlStatus::Other(0x0c02), "v{protocol}");
        }
    }

    /// An NCP accepting unicasts, its callbacks routed as the driver does.
    async fn linked(version: Version, accepted: &[u8]) -> (FakeNcp, Aps) {
        let (ncp, io) = FakeNcp::spawn(version);
        ncp.respond(ids::SEND_UNICAST, accepted);
        let (ezsp, version, mut callbacks) = Ezsp::connect(io).await.unwrap();
        let aps = Aps::new(ezsp, version.protocol);
        let router = aps.clone();
        tokio::spawn(async move {
            while let Some(callback) = callbacks.recv().await {
                router.on_message_sent(&callback);
            }
        });
        (ncp, aps)
    }

    /// The tag of the last `sendUnicast` the NCP received.
    async fn sent_tag(ncp: &FakeNcp, protocol: u8) -> u16 {
        ncp.wait_command(ids::SEND_UNICAST).await;
        let (_, params) = ncp
            .received()
            .into_iter()
            .rev()
            .find(|(id, _)| *id == ids::SEND_UNICAST)
            .unwrap();
        let at = 3 + ApsFrame::LEN;
        if protocol >= 14 {
            u16::from_le_bytes([params[at], params[at + 1]])
        } else {
            u16::from(params[at])
        }
    }

    #[tokio::test]
    async fn send_unicast_waits_for_its_message_sent() {
        for (version, accepted) in [
            (EMBER_13, &[0x00, 0x2A][..]),
            (EMBER_14, &[0, 0, 0, 0, 0x2A]),
        ] {
            let protocol = version.protocol;
            let (ncp, aps) = linked(version, accepted).await;
            let sender = aps.clone();
            let send = tokio::spawn(async move {
                sender
                    .send_unicast(0x1234, &on_off(), &[0x01, 0x02, 0x00])
                    .await
            });
            let tag = sent_tag(&ncp, protocol).await;
            // Another message's report first: not this one's.
            ncp.callback(
                ids::MESSAGE_SENT_HANDLER,
                &message_sent(protocol, tag + 1, false),
            );
            ncp.callback(
                ids::MESSAGE_SENT_HANDLER,
                &message_sent(protocol, tag, true),
            );
            send.await.unwrap().unwrap();
            let (_, params) = ncp
                .received()
                .into_iter()
                .find(|(id, _)| *id == ids::SEND_UNICAST)
                .unwrap();
            let mut expected = vec![0x00, 0x34, 0x12];
            expected.extend_from_slice(&ON_OFF_BYTES);
            if protocol >= 14 {
                expected.extend_from_slice(&tag.to_le_bytes());
            } else {
                expected.push(u8::try_from(tag).unwrap());
            }
            expected.extend_from_slice(&[3, 0x01, 0x02, 0x00]);
            assert_eq!(params, expected, "v{protocol}");
            assert!(aps.lock().waiting.is_empty(), "the tag is released");
        }
    }

    #[tokio::test]
    async fn a_failed_delivery_is_an_error() {
        let (ncp, aps) = linked(EMBER_13, &[0x00, 0x2A]).await;
        let sender = aps.clone();
        let send =
            tokio::spawn(async move { sender.send_unicast(0x1234, &on_off(), &[0x01]).await });
        let tag = sent_tag(&ncp, 13).await;
        ncp.callback(ids::MESSAGE_SENT_HANDLER, &message_sent(13, tag, false));
        let error = send.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("not delivered"), "{error}");
    }

    #[tokio::test]
    async fn a_refused_unicast_is_an_error_at_once() {
        // EMBER_NETWORK_DOWN
        let (_ncp, aps) = linked(EMBER_13, &[0x91, 0x00]).await;
        let error = aps
            .send_unicast(0x1234, &on_off(), &[0x01])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("refused"), "{error}");
        assert!(aps.lock().waiting.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn no_message_sent_times_out() {
        let (_ncp, aps) = linked(EMBER_13, &[0x00, 0x2A]).await;
        let error = aps
            .send_unicast(0x1234, &on_off(), &[0x01])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no messageSentHandler"), "{error}");
        assert!(aps.lock().waiting.is_empty(), "the tag is released");
    }

    #[tokio::test]
    async fn broadcast_layout_by_version() {
        for version in [EMBER_13, EMBER_14] {
            let protocol = version.protocol;
            let (ncp, io) = FakeNcp::spawn(version);
            let ok: &[u8] = if protocol >= 14 {
                &[0, 0, 0, 0, 0]
            } else {
                &[0, 0]
            };
            ncp.respond(ids::SEND_BROADCAST, ok);
            let (ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
            let aps = Aps::new(ezsp, version.protocol);
            aps.send_broadcast(0xFFFC, &on_off(), 0xFF, 5, &[5, 254, 1])
                .await
                .unwrap();
            let (_, params) = ncp
                .received()
                .into_iter()
                .find(|(id, _)| *id == ids::SEND_BROADCAST)
                .unwrap();
            let mut expected = Vec::new();
            if protocol >= 14 {
                expected.extend_from_slice(&[0xFF, 0xFF, 0xFC, 0xFF, 0x00]);
            } else {
                expected.extend_from_slice(&[0xFC, 0xFF]);
            }
            expected.extend_from_slice(&ON_OFF_BYTES);
            expected.push(0xFF);
            if protocol >= 14 {
                expected.extend_from_slice(&[5, 0]);
            } else {
                expected.push(5);
            }
            expected.extend_from_slice(&[3, 5, 254, 1]);
            assert_eq!(params, expected, "v{protocol}");
        }
    }

    #[test]
    fn tags_skip_those_awaited_and_stay_below_0x80() {
        let mut tags = Tags::default();
        assert_eq!(tags.next(), 1);
        tags.last = 0x7E;
        let (sender, _receiver) = oneshot::channel();
        tags.waiting.insert(0x7F, sender);
        assert_eq!(tags.next(), 1, "0x7F is awaited, 0x80 is the stack's");
        assert_eq!(tags.next(), 2);
    }
}

//! ZDO (Zigbee Device Objects, profile 0, endpoint 0): the requests the
//! coordinator sends and the frames it reads, each payload starting with
//! its transaction number.
//!
//! Cluster ids from zigbee-herdsman (MIT), `src/zspec/zdo/definition/
//! clusters.ts`; layouts from `src/zspec/zdo/buffaloZdo.ts` (`build…`,
//! `read…`); integers little-endian, IEEE addresses as they travel (least
//! significant byte first).

use std::fmt;

use crate::aps::{ApsFrame, DEFAULT_OPTIONS};

/// `ZDO_PROFILE_ID`, `ZDO_ENDPOINT` (`definition/consts.ts`).
pub const PROFILE: u16 = 0x0000;
pub const ENDPOINT: u8 = 0;

pub const NODE_DESCRIPTOR_REQUEST: u16 = 0x0002;
pub const NODE_DESCRIPTOR_RESPONSE: u16 = 0x8002;
pub const SIMPLE_DESCRIPTOR_REQUEST: u16 = 0x0004;
pub const SIMPLE_DESCRIPTOR_RESPONSE: u16 = 0x8004;
pub const ACTIVE_ENDPOINTS_REQUEST: u16 = 0x0005;
pub const ACTIVE_ENDPOINTS_RESPONSE: u16 = 0x8005;
/// `END_DEVICE_ANNOUNCE`: a device that joined (or rejoined) says who it is.
pub const DEVICE_ANNOUNCE: u16 = 0x0013;
pub const BIND_REQUEST: u16 = 0x0021;
pub const BIND_RESPONSE: u16 = 0x8021;
/// `LEAVE_REQUEST` (Mgmt_Leave_req).
pub const MGMT_LEAVE_REQUEST: u16 = 0x0034;
/// `PERMIT_JOINING_REQUEST` (Mgmt_Permit_Joining_req).
pub const MGMT_PERMIT_JOINING_REQUEST: u16 = 0x0036;

/// `BroadcastAddress.DEFAULT` (`src/zspec/enums.ts`): all routers and the
/// coordinator.
pub const BROADCAST_ROUTERS: u16 = 0xFFFC;
/// `ZDO_REQUEST_RADIUS` of herdsman's ember adapter.
pub const REQUEST_RADIUS: u8 = 0xFF;

/// `UNICAST_BINDING`, `MULTICAST_BINDING` (`definition/consts.ts`).
const UNICAST_BINDING: u8 = 0x03;
const MULTICAST_BINDING: u8 = 0x01;

/// `Status.SUCCESS` of a ZDO response.
const SUCCESS: u8 = 0x00;

/// The APS frame of a ZDO request (herdsman's `sendZdo`).
#[must_use]
pub fn frame(cluster: u16) -> ApsFrame {
    ApsFrame {
        profile: PROFILE,
        cluster,
        src_ep: ENDPOINT,
        dst_ep: ENDPOINT,
        options: DEFAULT_OPTIONS,
        group: 0,
        seq: 0,
    }
}

/// Why a ZDO frame could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZdoError {
    TooShort,
    /// The device answered with this ZDO status (not `SUCCESS`).
    Status(u8),
}

impl fmt::Display for ZdoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => f.write_str("ZDO frame too short"),
            Self::Status(status) => write!(f, "ZDO status {status:#04x}"),
        }
    }
}

impl std::error::Error for ZdoError {}

/// The transaction number a ZDO frame starts with.
#[must_use]
pub fn transaction(payload: &[u8]) -> Option<u8> {
    payload.first().copied()
}

/// Node_Desc_req: the node descriptor of `nwk`.
#[must_use]
pub fn node_descriptor_request(seq: u8, nwk: u16) -> Vec<u8> {
    let [low, high] = nwk.to_le_bytes();
    vec![seq, low, high]
}

/// Active_EP_req: the endpoints of `nwk`.
#[must_use]
pub fn active_endpoints_request(seq: u8, nwk: u16) -> Vec<u8> {
    let [low, high] = nwk.to_le_bytes();
    vec![seq, low, high]
}

/// Simple_Desc_req: what `endpoint` of `nwk` is.
#[must_use]
pub fn simple_descriptor_request(seq: u8, nwk: u16, endpoint: u8) -> Vec<u8> {
    let [low, high] = nwk.to_le_bytes();
    vec![seq, low, high, endpoint]
}

/// Where a binding sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindTarget {
    /// A device's endpoint (usually the coordinator's endpoint 1).
    Unicast {
        ieee: [u8; 8],
        endpoint: u8,
    },
    Group(u16),
}

/// Bind_req, sent to the source device: bind `cluster` of `endpoint` of
/// `source` to `target`.
#[must_use]
pub fn bind_request(
    seq: u8,
    source: [u8; 8],
    endpoint: u8,
    cluster: u16,
    target: BindTarget,
) -> Vec<u8> {
    let mut payload = vec![seq];
    payload.extend_from_slice(&source);
    payload.push(endpoint);
    payload.extend_from_slice(&cluster.to_le_bytes());
    match target {
        BindTarget::Unicast { ieee, endpoint } => {
            payload.push(UNICAST_BINDING);
            payload.extend_from_slice(&ieee);
            payload.push(endpoint);
        }
        BindTarget::Group(group) => {
            payload.push(MULTICAST_BINDING);
            payload.extend_from_slice(&group.to_le_bytes());
        }
    }
    payload
}

/// Mgmt_Leave_req: `ieee` leaves the network (and rejoins, with `rejoin`:
/// `LeaveRequestFlags.AND_REJOIN`).
#[must_use]
pub fn mgmt_leave_request(seq: u8, ieee: [u8; 8], rejoin: bool) -> Vec<u8> {
    let mut payload = vec![seq];
    payload.extend_from_slice(&ieee);
    payload.push(if rejoin { 0x80 } else { 0x00 });
    payload
}

/// Mgmt_Permit_Joining_req: joining open for `seconds` (0 closes it); TC
/// significance always 1, as the specification and herdsman want.
#[must_use]
pub fn mgmt_permit_joining_request(seq: u8, seconds: u8) -> Vec<u8> {
    vec![seq, seconds, 1]
}

/// Device_annce: a device that joined says its addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceAnnounce {
    pub nwk: u16,
    /// As it travels (least significant byte first).
    pub ieee: [u8; 8],
    /// MAC capability flags.
    pub capability: u8,
}

impl DeviceAnnounce {
    /// # Errors
    ///
    /// The frame is too short.
    pub fn parse(payload: &[u8]) -> Result<Self, ZdoError> {
        let body = body(payload)?;
        let (raw, _) = body.split_first_chunk::<11>().ok_or(ZdoError::TooShort)?;
        let mut ieee = [0; 8];
        ieee.copy_from_slice(&raw[2..10]);
        Ok(Self {
            nwk: u16::from_le_bytes([raw[0], raw[1]]),
            ieee,
            capability: raw[10],
        })
    }
}

/// Node_Desc_rsp, the fields the interview needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeDescriptor {
    pub nwk: u16,
    /// 0 coordinator, 1 router, 2 end device.
    pub logical_type: u8,
    /// MAC capability flags.
    pub capability: u8,
    pub manufacturer_code: u16,
}

impl NodeDescriptor {
    /// # Errors
    ///
    /// The frame is too short, or reports a failure.
    pub fn parse(payload: &[u8]) -> Result<Self, ZdoError> {
        let body = success(payload)?;
        // nwk (2), byte 1, byte 2, MAC capabilities, manufacturer (2),
        // max buffer, max incoming (2), server mask (2), max outgoing (2),
        // descriptor capabilities.
        let (raw, _) = body.split_first_chunk::<15>().ok_or(ZdoError::TooShort)?;
        Ok(Self {
            nwk: u16::from_le_bytes([raw[0], raw[1]]),
            logical_type: raw[2] & 0x07,
            capability: raw[4],
            manufacturer_code: u16::from_le_bytes([raw[5], raw[6]]),
        })
    }
}

/// Active_EP_rsp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveEndpoints {
    pub nwk: u16,
    pub endpoints: Vec<u8>,
}

impl ActiveEndpoints {
    /// # Errors
    ///
    /// The frame is too short, or reports a failure.
    pub fn parse(payload: &[u8]) -> Result<Self, ZdoError> {
        let body = success(payload)?;
        let (&[n0, n1, count], rest) = body.split_first_chunk::<3>().ok_or(ZdoError::TooShort)?;
        let endpoints = rest.get(..usize::from(count)).ok_or(ZdoError::TooShort)?;
        Ok(Self {
            nwk: u16::from_le_bytes([n0, n1]),
            endpoints: endpoints.to_vec(),
        })
    }
}

/// Simple_Desc_rsp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleDescriptor {
    pub nwk: u16,
    pub endpoint: u8,
    pub profile: u16,
    pub device_id: u16,
    pub device_version: u8,
    pub in_clusters: Vec<u16>,
    pub out_clusters: Vec<u16>,
}

impl SimpleDescriptor {
    /// # Errors
    ///
    /// The frame is too short, or reports a failure.
    pub fn parse(payload: &[u8]) -> Result<Self, ZdoError> {
        let body = success(payload)?;
        // nwk (2), length, endpoint, profile (2), device (2), version.
        let (raw, rest) = body.split_first_chunk::<9>().ok_or(ZdoError::TooShort)?;
        let (in_clusters, rest) = clusters(rest)?;
        let (out_clusters, _) = clusters(rest)?;
        Ok(Self {
            nwk: u16::from_le_bytes([raw[0], raw[1]]),
            endpoint: raw[3],
            profile: u16::from_le_bytes([raw[4], raw[5]]),
            device_id: u16::from_le_bytes([raw[6], raw[7]]),
            device_version: raw[8],
            in_clusters,
            out_clusters,
        })
    }
}

/// A cluster list: its count, then the ids.
fn clusters(bytes: &[u8]) -> Result<(Vec<u16>, &[u8]), ZdoError> {
    let (&count, rest) = bytes.split_first().ok_or(ZdoError::TooShort)?;
    let len = usize::from(count) * 2;
    let list = rest.get(..len).ok_or(ZdoError::TooShort)?;
    let ids = list
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&id| u16::from_le_bytes(id))
        .collect();
    Ok((ids, &rest[len..]))
}

/// Bind_rsp: `Ok` once the device bound.
///
/// # Errors
///
/// The frame is too short, or reports a failure.
pub fn bind_response(payload: &[u8]) -> Result<(), ZdoError> {
    success(payload).map(|_| ())
}

/// What follows the transaction number.
fn body(payload: &[u8]) -> Result<&[u8], ZdoError> {
    payload.get(1..).ok_or(ZdoError::TooShort)
}

/// What follows the transaction number and a `SUCCESS` status.
fn success(payload: &[u8]) -> Result<&[u8], ZdoError> {
    let (&status, rest) = body(payload)?.split_first().ok_or(ZdoError::TooShort)?;
    if status == SUCCESS {
        Ok(rest)
    } else {
        Err(ZdoError::Status(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x00124b0000000001` as it travels.
    const IEEE: [u8; 8] = [0x01, 0, 0, 0, 0, 0x4b, 0x12, 0x00];

    #[test]
    fn device_announce_parse() {
        let payload = [0x81, 0x34, 0x12, 0x01, 0, 0, 0, 0, 0x4b, 0x12, 0x00, 0x8E];
        let announce = DeviceAnnounce::parse(&payload).unwrap();
        assert_eq!(
            announce,
            DeviceAnnounce {
                nwk: 0x1234,
                ieee: IEEE,
                capability: 0x8E,
            }
        );
        assert_eq!(crate::network::ieee(announce.ieee), "0x00124b0000000001");
        assert_eq!(transaction(&payload), Some(0x81));
        assert_eq!(
            DeviceAnnounce::parse(&payload[..11]),
            Err(ZdoError::TooShort)
        );
    }

    #[test]
    fn requests() {
        assert_eq!(node_descriptor_request(3, 0x1234), [3, 0x34, 0x12]);
        assert_eq!(active_endpoints_request(4, 0x1234), [4, 0x34, 0x12]);
        assert_eq!(simple_descriptor_request(5, 0x1234, 1), [5, 0x34, 0x12, 1]);
        assert_eq!(mgmt_permit_joining_request(6, 254), [6, 254, 1]);
        assert_eq!(mgmt_permit_joining_request(7, 0), [7, 0, 1]);
        let mut leave = vec![8];
        leave.extend_from_slice(&IEEE);
        leave.push(0x00);
        assert_eq!(mgmt_leave_request(8, IEEE, false), leave);
        *leave.last_mut().unwrap() = 0x80;
        assert_eq!(mgmt_leave_request(8, IEEE, true), leave);
    }

    #[test]
    fn bind_requests() {
        let coordinator = [0x02, 0, 0, 0, 0, 0x4b, 0x12, 0x00];
        let unicast = bind_request(
            9,
            IEEE,
            1,
            0x0006,
            BindTarget::Unicast {
                ieee: coordinator,
                endpoint: 1,
            },
        );
        let mut expected = vec![9];
        expected.extend_from_slice(&IEEE);
        expected.extend_from_slice(&[1, 0x06, 0x00, 0x03]);
        expected.extend_from_slice(&coordinator);
        expected.push(1);
        assert_eq!(unicast, expected);
        let group = bind_request(9, IEEE, 1, 0x0006, BindTarget::Group(901));
        let mut expected = vec![9];
        expected.extend_from_slice(&IEEE);
        expected.extend_from_slice(&[1, 0x06, 0x00, 0x01, 0x85, 0x03]);
        assert_eq!(group, expected);
    }

    #[test]
    fn zdo_frame() {
        assert_eq!(
            frame(MGMT_PERMIT_JOINING_REQUEST),
            ApsFrame {
                profile: 0,
                cluster: 0x0036,
                src_ep: 0,
                dst_ep: 0,
                options: 0x1140,
                group: 0,
                seq: 0,
            }
        );
    }

    #[test]
    fn node_descriptor_response() {
        // A router (logical type 1), mains-powered, manufacturer 0x115F.
        let payload = [
            0x03, 0x00, // seq, SUCCESS
            0x34, 0x12, // nwk
            0x01, 0x40, 0x8E, // byte 1, byte 2, MAC capabilities
            0x5F, 0x11, // manufacturer
            0x52, 0x80, 0x00, 0x00, 0x2C, 0x80, 0x00, 0x00, // buffer, sizes, server mask
        ];
        assert_eq!(
            NodeDescriptor::parse(&payload),
            Ok(NodeDescriptor {
                nwk: 0x1234,
                logical_type: 1,
                capability: 0x8E,
                manufacturer_code: 0x115F,
            })
        );
        assert_eq!(
            NodeDescriptor::parse(&payload[..10]),
            Err(ZdoError::TooShort)
        );
        // DEVICE_NOT_FOUND
        assert_eq!(
            NodeDescriptor::parse(&[0x03, 0x81, 0x34, 0x12]),
            Err(ZdoError::Status(0x81))
        );
    }

    #[test]
    fn active_endpoints_response() {
        let payload = [0x04, 0x00, 0x34, 0x12, 2, 1, 242];
        assert_eq!(
            ActiveEndpoints::parse(&payload),
            Ok(ActiveEndpoints {
                nwk: 0x1234,
                endpoints: vec![1, 242],
            })
        );
        assert_eq!(
            ActiveEndpoints::parse(&payload[..6]),
            Err(ZdoError::TooShort)
        );
    }

    #[test]
    fn simple_descriptor_response() {
        let payload = [
            0x05, 0x00, 0x34, 0x12, // seq, SUCCESS, nwk
            14,   // length
            1,    // endpoint
            0x04, 0x01, // profile
            0x00, 0x01, // device id
            0x01, // version
            2, 0x00, 0x00, 0x06, 0x00, // in: Basic, On/Off
            1, 0x19, 0x00, // out: OTA
        ];
        assert_eq!(
            SimpleDescriptor::parse(&payload),
            Ok(SimpleDescriptor {
                nwk: 0x1234,
                endpoint: 1,
                profile: 0x0104,
                device_id: 0x0100,
                device_version: 1,
                in_clusters: vec![0x0000, 0x0006],
                out_clusters: vec![0x0019],
            })
        );
        assert_eq!(
            SimpleDescriptor::parse(&payload[..payload.len() - 1]),
            Err(ZdoError::TooShort)
        );
    }

    #[test]
    fn bind_responses() {
        assert_eq!(bind_response(&[0x09, 0x00]), Ok(()));
        // NOT_SUPPORTED
        assert_eq!(bind_response(&[0x09, 0x84]), Err(ZdoError::Status(0x84)));
        assert_eq!(bind_response(&[0x09]), Err(ZdoError::TooShort));
    }
}

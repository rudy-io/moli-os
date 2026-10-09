//! The ESPHome API messages Moli uses (identifiers and field numbers of
//! `api.proto`, ESPHome 2026.3). Plain functions: a message is built or read
//! where it is needed.

use moli_net::proto::{Writer, parse};

pub const HELLO_REQUEST: u16 = 1;
pub const HELLO_RESPONSE: u16 = 2;
pub const DISCONNECT_REQUEST: u16 = 5;
pub const DISCONNECT_RESPONSE: u16 = 6;
pub const PING_REQUEST: u16 = 7;
pub const PING_RESPONSE: u16 = 8;
pub const DEVICE_INFO_REQUEST: u16 = 9;
pub const DEVICE_INFO_RESPONSE: u16 = 10;
pub const LIST_ENTITIES_REQUEST: u16 = 11;
pub const LIST_ENTITIES_DONE: u16 = 19;
pub const SUBSCRIBE_STATES: u16 = 20;
pub const LIST_MEDIA_PLAYER: u16 = 63;
pub const MEDIA_PLAYER_STATE: u16 = 64;
pub const MEDIA_PLAYER_COMMAND: u16 = 65;
pub const LIST_EVENT: u16 = 107;
pub const EVENT: u16 = 108;

/// The API version Moli speaks (the device warns below 1.14).
const API_MAJOR: u64 = 1;
const API_MINOR: u64 = 14;

#[must_use]
pub fn hello() -> Vec<u8> {
    Writer::new()
        .str(1, "Moli OS")
        .uint(2, API_MAJOR)
        .uint(3, API_MINOR)
        .finish()
}

/// What the device says of itself (`DeviceInfoResponse`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub friendly_name: String,
    pub mac: String,
    pub esphome_version: String,
    pub model: String,
    pub manufacturer: String,
    pub project: String,
    pub project_version: String,
    pub voice_flags: u32,
}

#[must_use]
pub fn device_info(payload: &[u8]) -> Option<DeviceInfo> {
    let m = parse(payload)?;
    let text = |n| m.str(n).unwrap_or_default().to_owned();
    Some(DeviceInfo {
        name: text(2),
        mac: text(3),
        esphome_version: text(4),
        model: text(6),
        project: text(8),
        project_version: text(9),
        manufacturer: text(12),
        friendly_name: text(13),
        voice_flags: u32::try_from(m.uint(17).unwrap_or(0)).unwrap_or(0),
    })
}

/// An entity of interest: its key (the handle of every later message) and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    pub key: u32,
    pub name: String,
}

/// `ListEntitiesMediaPlayerResponse` or `ListEntitiesEventResponse`: key 2, name 3.
#[must_use]
pub fn entity(payload: &[u8]) -> Option<Entity> {
    let m = parse(payload)?;
    Some(Entity {
        key: m.fixed32(2)?,
        name: m.str(3).unwrap_or_default().to_owned(),
    })
}

/// The media player's state: 0 none, 1 idle, 2 playing, 3 paused, 4 announcing, 5 off, 6 on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaState {
    pub key: u32,
    pub state: u64,
    pub volume: f32,
    pub muted: bool,
}

#[must_use]
pub fn media_state(payload: &[u8]) -> Option<MediaState> {
    let m = parse(payload)?;
    Some(MediaState {
        key: m.fixed32(1)?,
        state: m.uint(2).unwrap_or(0),
        volume: m.float(3).unwrap_or(0.0),
        muted: m.bool(4),
    })
}

/// `MediaPlayerCommandRequest`: the volume, 0 to 1.
#[must_use]
pub fn set_volume(key: u32, volume: f32) -> Vec<u8> {
    Writer::new()
        .fixed32(1, key)
        .bool(4, true)
        .float(5, volume.clamp(0.0, 1.0))
        .finish()
}

/// `MediaPlayerCommandRequest`: mute (3) or unmute (4).
#[must_use]
pub fn set_muted(key: u32, muted: bool) -> Vec<u8> {
    Writer::new()
        .fixed32(1, key)
        .bool(2, true)
        .uint(3, if muted { 3 } else { 4 })
        .finish()
}

/// `EventResponse`: which entity, which event (`double_press`…).
#[must_use]
pub fn event(payload: &[u8]) -> Option<(u32, String)> {
    let m = parse(payload)?;
    Some((m.fixed32(1)?, m.str(2).unwrap_or_default().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_announces_moli_and_api_1_14() {
        let bytes = hello();
        let m = parse(&bytes).unwrap();
        assert_eq!(m.str(1), Some("Moli OS"));
        assert_eq!((m.uint(2), m.uint(3)), (Some(1), Some(14)));
    }

    #[test]
    fn device_info_is_read() {
        let payload = Writer::new()
            .str(2, "home-assistant-voice-0a8ccb")
            .str(3, "AA:BB:CC:DD:EE:FF")
            .str(4, "2026.3.2")
            .str(6, "Home Assistant Voice PE")
            .str(13, "Home Assistant Voice 0a8ccb")
            .uint(17, 61)
            .finish();
        let info = device_info(&payload).unwrap();
        assert_eq!(info.friendly_name, "Home Assistant Voice 0a8ccb");
        assert_eq!(info.voice_flags, 61);
        assert_eq!(info.esphome_version, "2026.3.2");
    }

    #[test]
    fn media_player_messages() {
        let listed = Writer::new()
            .fixed32(2, 0x1234_5678)
            .str(3, "Media Player")
            .finish();
        assert_eq!(
            entity(&listed),
            Some(Entity {
                key: 0x1234_5678,
                name: "Media Player".into()
            })
        );
        let state = Writer::new()
            .fixed32(1, 7)
            .uint(2, 1)
            .float(3, 0.5)
            .bool(4, false)
            .finish();
        assert_eq!(
            media_state(&state),
            Some(MediaState {
                key: 7,
                state: 1,
                volume: 0.5,
                muted: false
            })
        );
        let bytes = set_volume(7, 1.7);
        let command = parse(&bytes).unwrap();
        assert_eq!(command.fixed32(1), Some(7));
        assert!(command.bool(4));
        assert_eq!(command.float(5), Some(1.0), "clamped");
        let bytes = set_muted(7, true);
        let mute = parse(&bytes).unwrap();
        assert_eq!((mute.bool(2), mute.uint(3)), (true, Some(3)));
    }
}

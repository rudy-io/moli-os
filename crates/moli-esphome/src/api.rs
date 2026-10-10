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
pub const SUBSCRIBE_VOICE_ASSISTANT: u16 = 89;
pub const VOICE_REQUEST: u16 = 90;
pub const VOICE_RESPONSE: u16 = 91;
pub const VOICE_EVENT: u16 = 92;
pub const VOICE_AUDIO: u16 = 106;
pub const VOICE_ANNOUNCE_FINISHED: u16 = 120;
pub const VOICE_CONFIGURATION_REQUEST: u16 = 121;
pub const VOICE_CONFIGURATION_RESPONSE: u16 = 122;
pub const VOICE_SET_CONFIGURATION: u16 = 123;
pub const NOISE_SET_KEY_REQUEST: u16 = 124;
pub const NOISE_SET_KEY_RESPONSE: u16 = 125;

/// The voice pipeline's events, as the device reacts to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceEvent {
    Error = 0,
    RunStart = 1,
    RunEnd = 2,
    SttStart = 3,
    SttEnd = 4,
    IntentStart = 5,
    IntentEnd = 6,
    TtsStart = 7,
    TtsEnd = 8,
    SttVadStart = 11,
    SttVadEnd = 12,
}

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
    /// It can take an encryption key from a client (pi: encryption: without a key).
    pub encryption_supported: bool,
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
        encryption_supported: m.bool(19),
    })
}

/// NoiseEncryptionSetKeyRequest: the key in base64 (the device decodes it).
#[must_use]
pub fn set_key(key_base64: &str) -> Vec<u8> {
    Writer::new().str(1, key_base64).finish()
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

/// `MediaPlayerCommandRequest`: play `url` as an announcement (over what
/// plays, which comes back afterwards).
#[must_use]
pub fn announce(key: u32, url: &str) -> Vec<u8> {
    Writer::new()
        .fixed32(1, key)
        .bool(6, true)
        .str(7, url)
        .bool(8, true)
        .bool(9, true)
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

/// `SubscribeVoiceAssistantRequest`: Moli is the device's voice assistant.
#[must_use]
pub fn subscribe_voice() -> Vec<u8> {
    Writer::new().bool(1, true).finish()
}

/// The device starts (a wake word, its button, a continued conversation) or stops.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoiceRequest {
    pub start: bool,
    pub conversation_id: String,
    pub flags: u64,
    pub wake_word: String,
}

#[must_use]
pub fn voice_request(payload: &[u8]) -> Option<VoiceRequest> {
    let m = parse(payload)?;
    Some(VoiceRequest {
        start: m.bool(1),
        conversation_id: m.str(2).unwrap_or_default().to_owned(),
        flags: m.uint(3).unwrap_or(0),
        wake_word: m.str(5).unwrap_or_default().to_owned(),
    })
}

/// `VoiceAssistantResponse`: port 0 = the audio comes through the API; or an error.
#[must_use]
pub fn voice_response(error: bool) -> Vec<u8> {
    if error {
        Writer::new().bool(2, true).finish()
    } else {
        Writer::new().uint(1, 0).finish()
    }
}

/// `VoiceAssistantEventResponse` with its data (always strings).
#[must_use]
pub fn voice_event(event: VoiceEvent, data: &[(&str, &str)]) -> Vec<u8> {
    let mut w = Writer::new().uint(1, event as u64);
    for (name, value) in data {
        w = w.msg(2, Writer::new().str(1, name).str(2, value));
    }
    w.finish()
}

/// The microphone's audio (16 kHz, 16-bit, mono, little-endian).
#[must_use]
pub fn voice_audio(payload: &[u8]) -> Option<Vec<u8>> {
    parse(payload)?.bytes(1).map(<[u8]>::to_vec)
}

/// The wake words the device has, the active ones, and how many may be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WakeWords {
    pub available: Vec<(String, String)>,
    pub active: Vec<String>,
    pub max_active: u64,
}

#[must_use]
pub fn wake_words(payload: &[u8]) -> Option<WakeWords> {
    let m = parse(payload)?;
    Some(WakeWords {
        available: m
            .all_msgs(1)
            .map(|w| {
                (
                    w.str(1).unwrap_or_default().to_owned(),
                    w.str(2).unwrap_or_default().to_owned(),
                )
            })
            .collect(),
        active: m
            .all_bytes(2)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect(),
        max_active: m.uint(3).unwrap_or(0),
    })
}

/// `VoiceAssistantSetConfiguration`: these wake words, and only these, active.
#[must_use]
pub fn set_wake_words(ids: &[&str]) -> Vec<u8> {
    ids.iter()
        .fold(Writer::new(), |w, id| w.str(1, id))
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_messages() {
        let request = Writer::new()
            .bool(1, true)
            .str(2, "conv-1")
            .uint(3, 1)
            .str(5, "Okay Nabu")
            .finish();
        assert_eq!(
            voice_request(&request),
            Some(VoiceRequest {
                start: true,
                conversation_id: "conv-1".into(),
                flags: 1,
                wake_word: "Okay Nabu".into()
            })
        );
        let ok = voice_response(false);
        let ok = parse(&ok).unwrap();
        assert_eq!((ok.uint(1), ok.bool(2)), (Some(0), false));
        let event = voice_event(
            VoiceEvent::IntentEnd,
            &[("conversation_id", "c"), ("continue_conversation", "1")],
        );
        let event = parse(&event).unwrap();
        assert_eq!(event.uint(1), Some(6));
        let data: Vec<_> = event
            .all_msgs(2)
            .map(|d| (d.str(1).unwrap().to_owned(), d.str(2).unwrap().to_owned()))
            .collect();
        assert_eq!(data[1], ("continue_conversation".into(), "1".into()));
        let audio = Writer::new().bytes(1, &[1, 2, 3, 4]).finish();
        assert_eq!(voice_audio(&audio), Some(vec![1, 2, 3, 4]));
        let config = Writer::new()
            .msg(1, Writer::new().str(1, "okay_nabu").str(2, "Okay Nabu"))
            .msg(1, Writer::new().str(1, "hey_jarvis").str(2, "Hey Jarvis"))
            .str(2, "okay_nabu")
            .uint(3, 2)
            .finish();
        let words = wake_words(&config).unwrap();
        assert_eq!(words.available.len(), 2);
        assert_eq!(words.active, ["okay_nabu"]);
        assert_eq!(words.max_active, 2);
        let set = set_wake_words(&["hey_moli", "dis_moli"]);
        let set = parse(&set).unwrap();
        assert_eq!(set.all_bytes(1).count(), 2);
    }

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
    fn an_announcement_is_a_url_marked_as_one() {
        let message = announce(7, "http://moli/api/voice/a.wav");
        let m = parse(&message).unwrap();
        assert_eq!(m.fixed32(1), Some(7));
        assert_eq!(m.str(7), Some("http://moli/api/voice/a.wav"));
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

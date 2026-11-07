//! Moli for the voice satellites ([`VoiceBrain`]): hearing, one spoken turn
//! with the conversation so far, the answer's voice as a stream the
//! satellite's player fetches, and the words that close a conversation.

use std::fmt::Write as _;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use moli_core::{Origin, PointId, Value};
use moli_runtime::voice::{Answer, AtDoor, Pcm, Said, Speaker, VoiceBrain};
use moli_runtime::{BoxFuture, DeviceView};
use ring::rand::{SecureRandom as _, SystemRandom};

use crate::house::{Layout, norm};
use crate::{Assistant, AssistantError, Message, Reply, Role, Speech, Turn, voice, word_list};

/// A prepared voice stays fetchable this long (the satellite asks at once).
const PREPARED_TTL: Duration = Duration::from_secs(60);
const MAX_PREPARED: usize = 64;
/// The provider's raw voice: 24 kHz, 16-bit, mono.
const VOICE_RATE: u32 = 24_000;
/// The provider's voice comes about -25 LUFS, far below a speaker's usual
/// level (and below the closing chime): +9 dB for the satellites.
const SATELLITE_GAIN: f32 = 2.8;
/// Above this, peaks are rounded off instead of clipped.
const KNEE: f32 = 0.7;
/// A speaker has this long to start saying an answer; past it, the satellite
/// says it itself.
const SPEAKER_STARTS_WITHIN: Duration = Duration::from_secs(5);
/// The longest answer a speaker says (500 characters at most).
const SPEAKER_LONGEST: Duration = Duration::from_secs(45);
/// How often the speaker's `speaking` point is read meanwhile.
const SPEAKER_POLL: Duration = Duration::from_millis(50);

/// The tools that only read: a turn that called nothing else changed nothing.
const READ_ONLY_TOOLS: [&str; 8] = [
    "show",
    "get_device",
    "energy",
    "history",
    "forecast",
    "web_search",
    "delegation_status",
    "end_conversation",
];

/// A screen plays in the room of `satellite`: a TV that says what it plays, or
/// the speaker that carries its sound. A device that is offline says nothing.
fn screen_plays(devices: &[DeviceView], layout: &Layout, satellite: &str) -> bool {
    let room_of = |view: &DeviceView| {
        view.label
            .room
            .as_deref()
            .or(view.device.native_room.as_deref())
            .map(|room| norm(&layout.room(room)))
    };
    let Some(room) = devices
        .iter()
        .find(|d| d.device.id.as_str() == satellite)
        .and_then(room_of)
    else {
        return false;
    };
    devices
        .iter()
        .filter(|d| d.online != Some(false) && room_of(d).as_ref() == Some(&room))
        .any(|d| {
            let text = |key: &str| match d.state.get(key).map(|s| &s.value) {
                Some(Value::Text(text)) => Some(&**text),
                _ => None,
            };
            let carries_tv = text("source") == Some("tv")
                && matches!(
                    d.state.get("playing").map(|s| &s.value),
                    Some(Value::Bool(true))
                );
            text("media_state") == Some("playing") || carries_tv
        })
}

/// What a turn answers while a screen plays: words only for what it did, and
/// the conversation ends (the film would be taken for the next question).
pub(crate) fn hush(reply: &mut Reply) {
    let acted = !reply.actions.is_empty()
        || !reply.proposals.is_empty()
        || reply
            .tools
            .iter()
            .any(|tool| !READ_ONLY_TOOLS.contains(&tool.as_str()));
    if !acted {
        reply.reply.clear();
    }
    reply.end = true;
}

impl Assistant {
    /// Says `text` on a speaker (`point`, its `announce` point) and waits until
    /// it has said it: its `speaking` point holds the words while it says them.
    /// `Err` when it is silenced, refuses, or has not started within `starts`.
    pub(crate) async fn say_on_speaker(
        &self,
        point: &PointId,
        text: &str,
        starts: Duration,
    ) -> Result<(), String> {
        let (device, _) = point.split().ok_or("not a point")?;
        let at = |key: &str| PointId::from(format!("{}/{key}", device.as_str()));
        let hub = &self.0.hub;
        let read = |key: &str| hub.state(&at(key)).map(|s| s.value);
        if read("quiet") == Some(Value::Bool(true)) {
            return Err("the speaker is silenced".into());
        }
        let saying = || matches!(read("speaking"), Some(Value::Text(said)) if &*said == text);
        hub.command(
            point,
            Value::Text(text.into()),
            Origin::Assistant,
            Some("Moli".into()),
        )
        .await
        .map_err(|e| e.to_string())?;
        let asked = Instant::now();
        while !saying() {
            if asked.elapsed() > starts {
                return Err("the speaker did not start".into());
            }
            tokio::time::sleep(SPEAKER_POLL).await;
        }
        while saying() && asked.elapsed() < SPEAKER_LONGEST {
            tokio::time::sleep(SPEAKER_POLL).await;
        }
        Ok(())
    }

    /// This turn comes from a satellite that should keep quiet (`quiet_during_tv`).
    pub(crate) async fn screen_plays_for(&self, turn: &Turn) -> bool {
        if !self.0.config.quiet_during_tv || turn.surface.as_deref() != Some("satellite") {
            return false;
        }
        let Some(satellite) = turn
            .asker
            .as_deref()
            .and_then(|asker| asker.strip_prefix("satellite:"))
        else {
            return false;
        };
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        screen_plays(&self.0.hub.snapshot().devices, &layout, satellite)
    }

    /// The voice prepared under `name`, streamed while the cloud voice makes
    /// it, louder for a satellite's small speaker: `<id>.wav` as WAV,
    /// `<id>.flac` as FLAC (a media player's announcement). Whole from the
    /// house's voice otherwise.
    pub async fn satellite_voice(&self, name: &str) -> Result<Speech, AssistantError> {
        let (id, flac) = match name.strip_suffix(".flac") {
            Some(id) => (id, true),
            None => (
                name.strip_suffix(".wav")
                    .or_else(|| name.strip_suffix(".mp3"))
                    .unwrap_or(name),
                false,
            ),
        };
        let text = self
            .prepared_text(id)
            .ok_or_else(|| AssistantError::Invalid("unknown or expired voice".into()))?;
        let speech = self
            .voice_for(&text, None, Some(voice::Stream::Pcm))
            .await?;
        Ok(match (speech, flac) {
            (Speech::Stream(stream), false) => Speech::Stream(louder_wav(stream)),
            (Speech::Stream(stream), true) => Speech::Stream(louder_flac(stream)),
            (Speech::Whole(spoken), true) => Speech::Whole(as_flac(spoken)),
            (whole @ Speech::Whole(_), false) => whole,
        })
    }

    /// Files `text` under a new unguessable id.
    fn prepare(&self, text: &str) -> Option<String> {
        let mut raw = [0u8; 16];
        SystemRandom::new().fill(&mut raw).ok()?;
        let id = raw.iter().fold(String::with_capacity(32), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        let now = Instant::now();
        let mut prepared = self
            .0
            .prepared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        prepared.retain(|_, (until, _)| *until > now);
        if prepared.len() >= MAX_PREPARED {
            return None;
        }
        prepared.insert(id.clone(), (now + PREPARED_TTL, text.to_owned()));
        Some(id)
    }

    fn prepared_text(&self, id: &str) -> Option<String> {
        let prepared = self
            .0
            .prepared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        prepared
            .get(id)
            .filter(|(until, _)| *until > Instant::now())
            .map(|(_, text)| text.clone())
    }
}

impl VoiceBrain for Assistant {
    fn hear(&self, wav: Vec<u8>) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            let audio = self.record(&wav);
            let heard = self.listen(&wav, "audio/wav").await;
            // Its recording goes with the turn that follows.
            self.note_heard(voice::seconds_of(&wav, "audio/wav"), audio);
            heard.map(|heard| heard.text).map_err(|e| e.to_string())
        })
    }

    fn answer(
        &self,
        satellite: &moli_core::DeviceId,
        conversation: Vec<Said>,
    ) -> BoxFuture<'_, Result<Answer, String>> {
        let asker = format!("satellite:{satellite}");
        Box::pin(async move {
            let messages = conversation
                .into_iter()
                .map(|line| Message {
                    role: match line.who {
                        Speaker::Person => Role::User,
                        Speaker::Moli => Role::Assistant,
                    },
                    content: line.text,
                })
                .collect();
            let turn = Turn {
                messages,
                surface: Some("satellite".into()),
                spoken: true,
                id: None,
                asker: Some(asker),
            };
            self.turn(turn)
                .await
                .map(|reply| Answer {
                    text: reply.reply,
                    closes: reply.end,
                })
                .map_err(|e| e.to_string())
        })
    }

    fn voice_url(&self, base: &str, text: &str) -> Option<String> {
        let id = self.prepare(text.trim())?;
        Some(format!("{}/api/voice/{id}.wav", base.trim_end_matches('/')))
    }

    fn announcement_url(&self, base: &str, text: &str) -> Option<String> {
        let id = self.prepare(text.trim())?;
        Some(format!(
            "{}/api/voice/{id}.flac",
            base.trim_end_matches('/')
        ))
    }

    fn voice_pcm(&self, text: &str) -> BoxFuture<'_, Result<Pcm, String>> {
        let text = text.trim().to_owned();
        Box::pin(async move {
            let speech = self
                .voice_for(&text, None, Some(voice::Stream::Pcm))
                .await
                .map_err(|e| e.to_string())?;
            match speech {
                Speech::Stream(mut stream) => {
                    let mut raw = Vec::new();
                    while let Some(chunk) = stream.chunks.recv().await {
                        raw.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
                    }
                    Ok(Pcm {
                        samples: samples_of(&louder(&raw, SATELLITE_GAIN)),
                        rate: VOICE_RATE,
                    })
                }
                Speech::Whole(spoken) => voice::pcm_of(&spoken.audio)
                    .map(|(rate, pcm)| Pcm {
                        samples: samples_of(pcm),
                        rate,
                    })
                    .ok_or_else(|| format!("a voice in {}", spoken.mime)),
            }
        })
    }

    fn at_door(
        &self,
        door: String,
        wav: Option<Vec<u8>>,
        rang: bool,
    ) -> BoxFuture<'_, Result<AtDoor, String>> {
        Box::pin(async move { self.door(&door, wav, rang).await.map_err(|e| e.to_string()) })
    }

    fn is_goodbye(&self, text: &str) -> bool {
        goodbye(text, &word_list("assistant.voice.goodbye"))
    }

    fn heard_nothing(&self, wav: Option<Vec<u8>>) {
        self.heard_nothing_on("satellite", wav.as_deref());
    }

    fn say_on(&self, point: &str, text: &str) -> BoxFuture<'_, Result<(), String>> {
        let point = PointId::from(point);
        let text = text.trim().to_owned();
        Box::pin(async move {
            self.say_on_speaker(&point, &text, SPEAKER_STARTS_WITHIN)
                .await
        })
    }
}

/// Lowercase, the typographic apostrophe made plain, punctuation removed,
/// spaces collapsed: « Merci, Moli ! » → « merci moli ».
fn normalize(text: &str) -> String {
    let plain: String = text
        .to_lowercase()
        .replace('’', "'")
        .chars()
        .map(|c| {
            if matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | '«' | '»' | '"') {
                ' '
            } else {
                c
            }
        })
        .collect();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The whole utterance is one of the closing phrases (a sentence that only
/// starts with « merci » still asks something).
fn goodbye(text: &str, phrases: &[String]) -> bool {
    let said = normalize(text);
    !said.is_empty() && phrases.iter().any(|p| normalize(p) == said)
}

/// The raw voice as a WAV stream, louder: the header first (its sizes left
/// open, the stream ends with the sentence), then each chunk amplified.
fn louder_wav(stream: voice::SpeechStream) -> voice::SpeechStream {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let mut chunks = stream.chunks;
    tokio::spawn(async move {
        if tx
            .send(Ok(open_wav_header(VOICE_RATE).into()))
            .await
            .is_err()
        {
            return;
        }
        // A sample may straddle two chunks.
        let mut carry: Option<u8> = None;
        while let Some(chunk) = chunks.recv().await {
            let out = chunk.map(|bytes| {
                let mut raw = Vec::with_capacity(bytes.len() + 1);
                raw.extend(carry.take());
                raw.extend_from_slice(&bytes);
                if raw.len() % 2 == 1 {
                    carry = raw.pop();
                }
                louder(&raw, SATELLITE_GAIN).into()
            });
            if tx.send(out).await.is_err() {
                return;
            }
        }
    });
    voice::SpeechStream {
        mime: "audio/wav",
        engine: stream.engine,
        chunks: rx,
    }
}

/// The raw voice as a FLAC stream, louder: its header first, then the
/// frames each chunk completes, the last one when the sentence ends.
fn louder_flac(stream: voice::SpeechStream) -> voice::SpeechStream {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let mut chunks = stream.chunks;
    tokio::spawn(async move {
        let mut flac = moli_audio::flac::Writer::new(VOICE_RATE);
        if tx.send(Ok(flac.header().into())).await.is_err() {
            return;
        }
        let mut carry: Option<u8> = None;
        while let Some(chunk) = chunks.recv().await {
            let out = chunk.map(|bytes| {
                let mut raw = Vec::with_capacity(bytes.len() + 1);
                raw.extend(carry.take());
                raw.extend_from_slice(&bytes);
                if raw.len() % 2 == 1 {
                    carry = raw.pop();
                }
                flac.push(&samples_of(&louder(&raw, SATELLITE_GAIN))).into()
            });
            if tx.send(out).await.is_err() {
                return;
            }
        }
        let _ = tx.send(Ok(flac.finish().into())).await;
    });
    voice::SpeechStream {
        mime: "audio/flac",
        engine: stream.engine,
        chunks: rx,
    }
}

/// A whole WAV voice as FLAC (another format is kept as it is).
fn as_flac(spoken: voice::Spoken) -> voice::Spoken {
    match voice::pcm_of(&spoken.audio) {
        Some((rate, pcm)) => voice::Spoken {
            audio: moli_audio::flac::encode(&samples_of(pcm), rate),
            mime: "audio/flac",
            engine: spoken.engine,
        },
        None => spoken,
    }
}

/// 16-bit little-endian bytes as samples (an odd last byte dropped).
fn samples_of(pcm: &[u8]) -> Vec<i16> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// A 16-bit mono WAV header whose sizes say « until the end ».
fn open_wav_header(rate: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&u32::MAX.to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&1u16.to_le_bytes()); // mono
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * 2).to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(u32::MAX - 36).to_le_bytes());
    h
}

/// 16-bit little-endian samples times `gain`, the peaks above the knee
/// rounded off (never a hard clip).
fn louder(pcm: &[u8], gain: f32) -> Vec<u8> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .flat_map(|b| {
            let x = f32::from(i16::from_le_bytes(*b)) / 32768.0 * gain;
            let m = x.abs();
            let y = if m <= KNEE {
                x
            } else {
                x.signum() * (KNEE + (1.0 - KNEE) * ((m - KNEE) / (1.0 - KNEE)).tanh())
            };
            #[allow(clippy::cast_possible_truncation)]
            let s = (y * 32767.0).round() as i16;
            s.to_le_bytes()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use moli_core::Sample;

    use super::*;

    fn view(id: &str, room: Option<&str>, state: &[(&str, Value)]) -> DeviceView {
        DeviceView {
            device: std::sync::Arc::new(moli_core::Device {
                id: id.into(),
                instance: id.split(':').next().unwrap().into(),
                native_name: id.into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: room.map(Into::into),
                members: Vec::new(),
                points: Vec::new(),
            }),
            label: moli_core::Label::default(),
            online: Some(true),
            state: state
                .iter()
                .map(|(key, value)| {
                    (
                        (*key).into(),
                        Sample {
                            value: value.clone(),
                            ts: 0,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            camera: false,
            printer: false,
            live: false,
        }
    }

    fn text(value: &str) -> Value {
        Value::Text(value.into())
    }

    /// A speaker that says what it is told, or not (`says`), and tells it
    /// through `speaking` like the Sonos does.
    #[derive(Debug)]
    struct Speaker {
        says: bool,
    }

    impl moli_runtime::Driver for Speaker {
        fn kind(&self) -> &'static str {
            "speaker"
        }

        fn run<'a>(
            &'a self,
            ctx: &'a mut moli_runtime::DriverCtx,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                let id = ctx.device_id("one");
                let point = |key: &str, write: bool| moli_core::PointSpec {
                    key: key.into(),
                    label: key.into(),
                    kind: moli_core::Kind::Text,
                    access: moli_core::Access {
                        read: !write,
                        write,
                    },
                    unit: None,
                    semantic: moli_core::Semantic::Other,
                };
                ctx.upsert_device(moli_core::Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: "speaker".into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    members: Vec::new(),
                    points: vec![point("announce", true), point("speaking", false)],
                });
                ctx.set_state(&id, "speaking", Value::Null);
                ctx.ready();
                while let Some(command) = ctx.next_command().await {
                    let said = command.value.clone();
                    command.reply(Ok(()));
                    if self.says {
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        ctx.set_state(&id, "speaking", said);
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        ctx.set_state(&id, "speaking", Value::Null);
                    }
                }
                Ok(())
            })
        }
    }

    async fn moli_with_speaker(says: bool) -> Assistant {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        moli_runtime::spawn_driver(
            &hub,
            moli_core::InstanceId::from("speaker"),
            std::sync::Arc::new(Speaker { says }),
            tokio_util::sync::CancellationToken::new(),
        );
        for _ in 0..200 {
            if hub.stats().drivers_running == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let config = serde_json::from_value(serde_json::json!({})).unwrap();
        Assistant::new(hub, None, None, None, config).unwrap()
    }

    /// The answer is said on the speaker, and only then is it over.
    #[tokio::test]
    async fn an_answer_said_on_a_speaker_ends_when_it_has_been_said() {
        let moli = moli_with_speaker(true).await;
        let point = PointId::from("speaker:one/announce");
        let asked = Instant::now();
        moli.say_on_speaker(&point, "Il fait onze degrés.", Duration::from_secs(2))
            .await
            .unwrap();
        assert!(
            asked.elapsed() >= Duration::from_millis(200),
            "{:?}",
            asked.elapsed()
        );
        let speaking = moli.0.hub.state(&PointId::from("speaker:one/speaking"));
        assert_eq!(speaking.map(|s| s.value), Some(Value::Null));
    }

    /// A speaker that never starts: the satellite says it itself.
    #[tokio::test]
    async fn a_speaker_that_does_not_start_lets_the_satellite_speak() {
        let moli = moli_with_speaker(false).await;
        let point = PointId::from("speaker:one/announce");
        let err = moli
            .say_on_speaker(&point, "Il fait onze degrés.", Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.contains("did not start"), "{err}");
    }

    #[test]
    fn a_screen_plays_only_in_the_satellites_room() {
        let layout = Layout::default();
        let satellite = || view("voice:1", Some("Living room"), &[]);
        let plays = |other: DeviceView| screen_plays(&[satellite(), other], &layout, "voice:1");
        let tv = |state: &str| view("tv:1", Some("living ROOM"), &[("media_state", text(state))]);
        assert!(plays(tv("playing")));
        assert!(!plays(tv("paused")), "people can talk over a pause");
        assert!(!plays(view(
            "tv:1",
            Some("Kitchen"),
            &[("media_state", text("playing"))]
        )));
        let mut asleep = tv("playing");
        asleep.online = Some(false);
        assert!(!plays(asleep), "the last state of a device that left");
        // The speaker that carries a TV's sound, not one that plays music.
        let speaker = |source: &str, playing: bool| {
            view(
                "speaker:1",
                Some("Living room"),
                &[("source", text(source)), ("playing", Value::Bool(playing))],
            )
        };
        assert!(plays(speaker("tv", true)));
        assert!(!plays(speaker("tv", false)));
        assert!(!plays(speaker("music", true)));
        // A satellite with no room, or unknown: nothing to go by.
        let nowhere = view("voice:1", None, &[]);
        assert!(!screen_plays(&[nowhere, tv("playing")], &layout, "voice:1"));
        assert!(!screen_plays(&[tv("playing")], &layout, "voice:1"));
    }

    /// What it heard is the film: no words, and no listening again.
    #[tokio::test]
    async fn a_turn_that_did_nothing_keeps_quiet_while_a_screen_plays() {
        let moli = crate::tests::moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant","content":"Vous n'etes pas invisible."}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )
        .await;
        let mut reply = moli
            .turn(crate::tests::spoken("Je suis invisible ?"))
            .await
            .unwrap();
        assert_eq!(reply.reply, "Vous n'etes pas invisible.");
        assert!(!reply.end);
        hush(&mut reply);
        assert_eq!(reply.reply, "");
        assert!(reply.end);
    }

    /// What it did is said, then the conversation ends all the same.
    #[tokio::test]
    async fn a_turn_that_acted_says_so_while_a_screen_plays() {
        let moli = crate::tests::moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:lamp/on\",\"value\":true,\"say\":\"C'est allume.\"}"}}]}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )
        .await;
        let mut reply = moli
            .turn(crate::tests::spoken("Allume la lampe"))
            .await
            .unwrap();
        assert!(!reply.actions.is_empty());
        hush(&mut reply);
        assert_eq!(reply.reply, "C'est allume.");
        assert!(reply.end);
    }

    #[test]
    fn the_satellite_voice_is_louder_without_clipping() {
        let quiet = (3277i16).to_le_bytes(); // 0.1
        let loud = (16384i16).to_le_bytes(); // 0.5: 1.4 after the gain
        let negative = (-16384i16).to_le_bytes();
        let pcm = [quiet, loud, negative].concat();
        let out = samples_of(&louder(&pcm, SATELLITE_GAIN));
        assert!((i32::from(out[0]) - 9175).abs() < 3, "{out:?}"); // 0.28
        assert!(out[1] > 29_000 && out[1] < i16::MAX, "{out:?}");
        assert_eq!(out[2], -out[1]);
        let header = open_wav_header(VOICE_RATE);
        assert_eq!(header.len(), 44);
        assert_eq!(&header[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            24_000
        );
    }

    #[test]
    fn closing_words_close_and_questions_do_not() {
        let phrases = word_list("assistant.voice.goodbye");
        for said in [
            "Merci.",
            "Merci, Moli !",
            "C’est tout.",
            "c'est tout merci",
            "Stop",
            "Au revoir Moli",
            "Bonne nuit.",
        ] {
            assert!(goodbye(said, &phrases), "{said}");
        }
        for said in [
            "Merci, et la température dehors ?",
            "Stop la musique",
            "",
            "Allume le salon.",
        ] {
            assert!(!goodbye(said, &phrases), "{said}");
        }
    }

    #[tokio::test]
    async fn a_prepared_voice_is_found_then_expires() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let config = serde_json::from_value(serde_json::json!({})).unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        let url = moli
            .voice_url("http://192.168.1.x:8790/", "Il fait vingt degrés.")
            .unwrap();
        let name = url.rsplit('/').next().unwrap();
        assert!(
            url.starts_with("http://192.168.1.x:8790/api/voice/"),
            "{url}"
        );
        assert!(
            name.strip_suffix(".wav").is_some_and(|id| id.len() == 32),
            "{name}"
        );
        let id = name.strip_suffix(".wav").unwrap();
        assert_eq!(
            moli.prepared_text(id).as_deref(),
            Some("Il fait vingt degrés.")
        );
        assert!(moli.prepared_text("0000").is_none());
        moli.0.prepared.lock().unwrap().get_mut(id).unwrap().0 = Instant::now();
        assert!(moli.prepared_text(id).is_none(), "expired");
    }
}

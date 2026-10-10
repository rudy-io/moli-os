//! Moli for the voice satellites ([`VoiceBrain`]): hearing, one spoken turn
//! with the conversation so far, the answer's voice as a stream the
//! satellite's player fetches, and the words that close a conversation.

use std::fmt::Write as _;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use moli_runtime::BoxFuture;
use moli_runtime::voice::{Answer, Said, Speaker, VoiceBrain};
use ring::rand::{SecureRandom as _, SystemRandom};

use crate::{Assistant, AssistantError, Message, Role, Speech, Turn, voice, word_list};

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

impl Assistant {
    /// The voice prepared under `name` (`<id>.wav`), streamed as WAV while
    /// the cloud voice makes it, louder for a satellite's small speaker;
    /// whole (WAV) from the house's voice otherwise.
    pub async fn satellite_voice(&self, name: &str) -> Result<Speech, AssistantError> {
        let id = name
            .strip_suffix(".wav")
            .or_else(|| name.strip_suffix(".mp3"))
            .unwrap_or(name);
        let text = self
            .prepared_text(id)
            .ok_or_else(|| AssistantError::Invalid("unknown or expired voice".into()))?;
        Ok(
            match self
                .voice_for(&text, None, Some(voice::Stream::Pcm))
                .await?
            {
                Speech::Stream(stream) => Speech::Stream(louder_wav(stream)),
                whole @ Speech::Whole(_) => whole,
            },
        )
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

    fn answer(&self, conversation: Vec<Said>) -> BoxFuture<'_, Result<Answer, String>> {
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

    fn is_goodbye(&self, text: &str) -> bool {
        goodbye(text, &word_list("assistant.voice.goodbye"))
    }

    fn heard_nothing(&self, wav: Option<Vec<u8>>) {
        self.heard_nothing_on("satellite", wav.as_deref());
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
    use super::*;

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    #[test]
    fn the_satellite_voice_is_louder_without_clipping() {
        let quiet = (3277i16).to_le_bytes(); // 0.1
        let loud = (16384i16).to_le_bytes(); // 0.5: 1.4 after the gain
        let negative = (-16384i16).to_le_bytes();
        let pcm = [quiet, loud, negative].concat();
        let out = samples(&louder(&pcm, SATELLITE_GAIN));
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

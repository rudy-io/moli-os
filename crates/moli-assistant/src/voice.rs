//! Moli's voice: answers made speakable, the cloud voice first and the
//! house's own (Piper) when the cloud fails, and the PCM inside the WAV
//! files the dashboard records (for the local Whisper).

use std::time::Duration;

use http_body_util::BodyExt as _;
use moli_net::wyoming;
use tokio::sync::mpsc;

use crate::AssistantError;
use crate::llm::Endpoint;

const LOCAL_SPEAK_LIMIT: Duration = Duration::from_secs(15);
pub(crate) const LOCAL_LISTEN_LIMIT: Duration = Duration::from_secs(30);

/// Audio ready to play.
#[derive(Debug)]
pub struct Spoken {
    pub audio: Vec<u8>,
    pub mime: &'static str,
    /// `cloud` or `local`.
    pub engine: &'static str,
}

/// A voice as it is made: 16-bit PCM (`mime` says the rate), chunk by chunk.
#[derive(Debug)]
pub struct SpeechStream {
    pub mime: &'static str,
    /// `cloud`.
    pub engine: &'static str,
    /// Ends with the sentence; an `Err` cuts it (the provider stalled).
    pub chunks: mpsc::Receiver<Result<moli_net::Body, std::io::Error>>,
}

/// What a sentence becomes: audio whole, or a voice streamed while it is made.
#[derive(Debug)]
pub enum Speech {
    Whole(Spoken),
    Stream(SpeechStream),
}

pub(crate) const PCM_MIME: &str = "audio/pcm;rate=24000";

/// How a streamed voice is encoded: raw PCM, played piece by piece by the
/// dashboard, and turned into a louder WAV stream for a satellite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Pcm,
}

impl Stream {
    fn format(self) -> &'static str {
        match self {
            Self::Pcm => "pcm",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Pcm => PCM_MIME,
        }
    }
}
/// A stream that says nothing for this long is cut.
const STREAM_IDLE: Duration = Duration::from_secs(10);
/// 300 characters are under half a minute: 24 000 samples of 2 bytes a second.
const STREAM_MAX: usize = 4 * 1024 * 1024;

/// What was heard, and by whom (`cloud` or `local`).
#[derive(Debug)]
pub struct Heard {
    pub text: String,
    pub engine: &'static str,
}

pub(crate) struct Cloud<'a> {
    pub endpoint: &'a Endpoint,
    pub key: Option<&'a str>,
    pub model: &'a str,
    pub voice: &'a str,
    pub style: &'a str,
    pub speed: f64,
}

/// The voices available for one sentence, in order of preference.
pub(crate) struct Voices<'a> {
    pub cloud: Option<Cloud<'a>>,
    pub local: Option<(&'a str, u16)>,
    pub local_voice: Option<&'a str>,
}

impl Voices<'_> {
    pub(crate) async fn say(&self, text: &str) -> Result<Spoken, AssistantError> {
        let mut failure = None;
        if let Some(c) = &self.cloud {
            match c
                .endpoint
                .speech(c.key, c.model, c.voice, c.style, c.speed, text)
                .await
            {
                Ok(audio) => {
                    return Ok(Spoken {
                        audio,
                        mime: "audio/mpeg",
                        engine: "cloud",
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cloud voice failed, trying the local one");
                    failure = Some(e.to_string());
                }
            }
        }
        if let Some((host, port)) = self.local {
            match wyoming::synthesize(host, port, self.local_voice, text, LOCAL_SPEAK_LIMIT).await {
                Ok(audio) => {
                    return Ok(Spoken {
                        audio,
                        mime: "audio/wav",
                        engine: "local",
                    });
                }
                Err(e) => failure = Some(e.to_string()),
            }
        }
        Err(failure.map_or_else(
            || {
                AssistantError::NotConfigured(
                    "no voice: speech_model and local_speech are empty".into(),
                )
            },
            AssistantError::Upstream,
        ))
    }
}

impl Voices<'_> {
    /// The cloud voice as it is made; the house's (Piper, whole) when the
    /// cloud fails before saying anything.
    pub(crate) async fn say_streamed(
        &self,
        text: &str,
        stream: Stream,
    ) -> Result<Speech, AssistantError> {
        if let Some(c) = &self.cloud {
            match c
                .endpoint
                .speech_stream(
                    c.key,
                    c.model,
                    c.voice,
                    c.style,
                    c.speed,
                    stream.format(),
                    text,
                )
                .await
            {
                Ok(audio) => {
                    return Ok(Speech::Stream(SpeechStream {
                        mime: stream.mime(),
                        engine: "cloud",
                        chunks: relay(audio, text.chars().count()),
                    }));
                }
                Err(e) => tracing::warn!(error = %e, "cloud voice failed, trying the local one"),
            }
        }
        let local = Voices {
            cloud: None,
            local: self.local,
            local_voice: self.local_voice,
        };
        local.say(text).await.map(Speech::Whole)
    }
}

/// Hands the provider's audio over as it comes, within bounds (silence,
/// size); stops when nobody listens any more.
fn relay(
    mut audio: moli_net::Incoming,
    chars: usize,
) -> mpsc::Receiver<Result<moli_net::Body, std::io::Error>> {
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let mut bytes = 0usize;
        loop {
            let chunk = match tokio::time::timeout(STREAM_IDLE, audio.frame()).await {
                Ok(None) => break,
                Ok(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => data,
                    Err(_) => continue,
                },
                Ok(Some(Err(e))) => {
                    let _ = tx.send(Err(std::io::Error::other(e))).await;
                    break;
                }
                Err(_) => {
                    let _ = tx
                        .send(Err(std::io::Error::other("the voice stalled")))
                        .await;
                    break;
                }
            };
            bytes += chunk.len();
            if bytes > STREAM_MAX {
                let _ = tx
                    .send(Err(std::io::Error::other("the voice is too long")))
                    .await;
                break;
            }
            if tx.send(Ok(chunk)).await.is_err() {
                // The listener left (interrupted): the provider's stream goes.
                break;
            }
        }
        tracing::info!(
            engine = "cloud",
            chars,
            bytes,
            streamed = true,
            "voice spoken"
        );
    });
    rx
}

/// `host:port`, or nothing for an empty or malformed setting.
pub(crate) fn host_port(s: &str) -> Option<(&str, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    Some((host, port.parse().ok()?))
}

/// The 16-bit mono PCM of a WAV file, and its rate.
/// How long a recording lasts: exactly for a WAV, about for a compressed
/// one (some 4 kB a second).
#[allow(clippy::cast_precision_loss)]
pub(crate) fn seconds_of(audio: &[u8], mime: &str) -> f64 {
    match pcm_of(audio) {
        Some((rate, pcm)) if rate > 0 => pcm.len() as f64 / 2.0 / f64::from(rate),
        _ if mime.contains("wav") => 0.0,
        _ => audio.len() as f64 / 4_000.0,
    }
}

pub(crate) fn pcm_of(wav: &[u8]) -> Option<(u32, &[u8])> {
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut rate = None;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let len = usize::try_from(u32::from_le_bytes(wav[at + 4..at + 8].try_into().ok()?)).ok()?;
        let end = (at + 8).checked_add(len)?.min(wav.len());
        let body = &wav[at + 8..end];
        if id == b"fmt " {
            let field = |i: usize| body.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
            if field(0)? != 1 || field(2)? != 1 || field(14)? != 16 {
                return None;
            }
            rate = Some(u32::from_le_bytes(body.get(4..8)?.try_into().ok()?));
        } else if id == b"data" {
            return Some((rate?, body));
        }
        at = at.checked_add(8 + len + (len & 1))?;
    }
    None
}

/// An answer as it should be said: the house's decimal mark, units in words.
pub(crate) fn speakable(text: &str) -> String {
    let decimal = moli_i18n::tr!("assistant.say.decimal");
    let chars: Vec<char> = text.chars().collect();
    let mut spaced = String::with_capacity(text.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let after_digit = prev.is_some_and(|p| p.is_ascii_digit());
        if c == '.' && after_digit && next.is_some_and(|n| n.is_ascii_digit()) {
            spaced.push_str(&decimal);
            continue;
        }
        if matches!(c, '°' | '€' | '%') && after_digit {
            spaced.push(' ');
        }
        spaced.push(c);
    }
    let mut words = Vec::new();
    let mut after_number = false;
    for word in spaced.split(' ') {
        let core = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
        let tail = &word[core.len()..];
        let said = after_number
            .then(|| unit(core))
            .flatten()
            .or_else(|| hour(core));
        words.push(said.map_or_else(|| word.to_owned(), |s| format!("{s}{tail}")));
        after_number = core.chars().last().is_some_and(|c| c.is_ascii_digit());
    }
    words.join(" ")
}

fn unit(word: &str) -> Option<String> {
    let key = match word {
        "°C" | "°" => "assistant.say.unit.celsius",
        "kWh" => "assistant.say.unit.kwh",
        "Wh" => "assistant.say.unit.wh",
        "kW" => "assistant.say.unit.kw",
        "W" => "assistant.say.unit.w",
        "€" => "assistant.say.unit.eur",
        "%" => "assistant.say.unit.percent",
        "km/h" => "assistant.say.unit.kmh",
        "min" => "assistant.say.unit.min",
        _ => return None,
    };
    Some(moli_i18n::tr(key))
}

/// French `21h05` → `21 heures 05`, `7h` → `7 heures` (English: `21 hours 05`, `7 hours`).
fn hour(word: &str) -> Option<String> {
    let (h, m) = word.split_once('h')?;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !digits(h) || h.len() > 2 || !(m.is_empty() || (m.len() == 2 && digits(m))) {
        return None;
    }
    Some(if m.is_empty() || m == "00" {
        moli_i18n::tr!("assistant.say.hours", h = h)
    } else {
        moli_i18n::tr!("assistant.say.hours_minutes", h = h, m = m)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_speakable() {
        for (written, said) in [
            (
                "Il fait 22.9 °C dans le salon.",
                "Il fait 22,9 degrés dans le salon.",
            ),
            ("23°C dehors", "23 degrés dehors"),
            ("940 W en ce moment.", "940 watts en ce moment."),
            ("61 kWh pour 9,59 €.", "61 kilowattheures pour 9,59 euros."),
            ("Batterie à 78%.", "Batterie à 78 pour cent."),
            ("On a sonné à 21h05.", "On a sonné à 21 heures 05."),
            ("Réveil à 7h.", "Réveil à 7 heures."),
            ("Le W du mur reste.", "Le W du mur reste."),
            ("3.5 kW au compteur", "3,5 kilowatts au compteur"),
            ("Fin. Point.", "Fin. Point."),
        ] {
            assert_eq!(speakable(written), said, "{written}");
        }
    }

    fn wav16(rate: u32, channels: u16, bits: u16, pcm: &[u8]) -> Vec<u8> {
        let len = u32::try_from(pcm.len()).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(pcm);
        out
    }

    #[test]
    fn mono_16_bit_wav_gives_its_pcm() {
        let wav = wav16(16_000, 1, 16, &[1, 2, 3, 4]);
        assert_eq!(pcm_of(&wav), Some((16_000, &[1u8, 2, 3, 4][..])));
        assert_eq!(pcm_of(&wav16(16_000, 2, 16, &[1, 2, 3, 4])), None);
        assert_eq!(pcm_of(&wav16(16_000, 1, 8, &[1, 2])), None);
        assert_eq!(pcm_of(b"OggS....."), None);
    }

    #[test]
    fn host_and_port() {
        assert_eq!(host_port("127.0.0.1:10200"), Some(("127.0.0.1", 10200)));
        assert_eq!(host_port(""), None);
        assert_eq!(host_port("piper"), None);
    }

    /// A Piper stand-in answering any synthesis with 2 bytes of PCM.
    async fn fake_piper() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut line = String::new();
            tokio::io::BufReader::new(read)
                .read_line(&mut line)
                .await
                .unwrap();
            let fmt = r#"{"rate":22050,"width":2,"channels":1}"#;
            for (kind, payload) in [
                ("audio-start", &b""[..]),
                ("audio-chunk", &[7, 7][..]),
                ("audio-stop", &b""[..]),
            ] {
                let head = format!(
                    "{{\"type\":\"{kind}\",\"data_length\":{},\"payload_length\":{}}}\n",
                    fmt.len(),
                    payload.len()
                );
                write.write_all(head.as_bytes()).await.unwrap();
                write.write_all(fmt.as_bytes()).await.unwrap();
                write.write_all(payload).await.unwrap();
            }
        });
        port
    }

    #[tokio::test]
    async fn the_cloud_voice_comes_first() {
        let (url, _t) = crate::llm::tests::fake_http(200, b"ID3mp3").await;
        let endpoint = Endpoint::parse(&url).unwrap();
        let voices = Voices {
            cloud: Some(Cloud {
                endpoint: &endpoint,
                key: None,
                model: "m",
                voice: "v",
                style: "",
                speed: 1.0,
            }),
            local: Some(("127.0.0.1", 1)),
            local_voice: None,
        };
        let spoken = voices.say("Bonsoir").await.unwrap();
        assert_eq!((spoken.mime, spoken.engine), ("audio/mpeg", "cloud"));
        assert_eq!(spoken.audio, b"ID3mp3");
    }

    #[tokio::test]
    async fn piper_takes_over_when_the_cloud_fails() {
        let (url, _t) = crate::llm::tests::fake_http(500, br#"{"error":{"message":"down"}}"#).await;
        let endpoint = Endpoint::parse(&url).unwrap();
        let port = fake_piper().await;
        let voices = Voices {
            cloud: Some(Cloud {
                endpoint: &endpoint,
                key: None,
                model: "m",
                voice: "v",
                style: "",
                speed: 1.0,
            }),
            local: Some(("127.0.0.1", port)),
            local_voice: Some("fr_FR-siwis-medium"),
        };
        let spoken = voices.say("Bonsoir").await.unwrap();
        assert_eq!((spoken.mime, spoken.engine), ("audio/wav", "local"));
        assert_eq!(&spoken.audio[..4], b"RIFF");
    }

    #[tokio::test]
    async fn the_cloud_voice_streams_as_it_is_made() {
        let (url, task) = crate::llm::tests::fake_http(200, b"\x01\x00\x02\x00\x03\x00").await;
        let endpoint = Endpoint::parse(&url).unwrap();
        let voices = Voices {
            cloud: Some(Cloud {
                endpoint: &endpoint,
                key: None,
                model: "m",
                voice: "v",
                style: "",
                speed: 1.2,
            }),
            local: None,
            local_voice: None,
        };
        let Speech::Stream(mut stream) = voices.say_streamed("Bonsoir", Stream::Pcm).await.unwrap()
        else {
            panic!("a stream");
        };
        assert_eq!((stream.mime, stream.engine), (PCM_MIME, "cloud"));
        let mut heard = Vec::new();
        while let Some(chunk) = stream.chunks.recv().await {
            heard.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(heard, b"\x01\x00\x02\x00\x03\x00");
        let request = task.await.unwrap();
        assert!(request.contains("\"response_format\":\"pcm\""), "{request}");
        assert!(request.contains("\"speed\":1.2"), "{request}");
    }

    #[tokio::test]
    async fn a_failed_stream_lets_piper_speak() {
        let (url, _t) = crate::llm::tests::fake_http(500, br#"{"error":{"message":"down"}}"#).await;
        let endpoint = Endpoint::parse(&url).unwrap();
        let port = fake_piper().await;
        let voices = Voices {
            cloud: Some(Cloud {
                endpoint: &endpoint,
                key: None,
                model: "m",
                voice: "v",
                style: "",
                speed: 1.0,
            }),
            local: Some(("127.0.0.1", port)),
            local_voice: None,
        };
        let Speech::Whole(spoken) = voices.say_streamed("Bonsoir", Stream::Pcm).await.unwrap()
        else {
            panic!("Piper, whole");
        };
        assert_eq!((spoken.mime, spoken.engine), ("audio/wav", "local"));
    }

    #[tokio::test]
    async fn no_voice_at_all_says_so() {
        let voices = Voices {
            cloud: None,
            local: None,
            local_voice: None,
        };
        assert!(matches!(
            voices.say("x").await,
            Err(AssistantError::NotConfigured(_))
        ));
    }
}

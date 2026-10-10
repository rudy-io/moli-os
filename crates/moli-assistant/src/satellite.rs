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

impl Assistant {
    /// The voice prepared under `name` (`<id>.mp3`), streamed as MP3 while
    /// the cloud voice makes it; whole (WAV) from the house's voice otherwise.
    pub async fn satellite_voice(&self, name: &str) -> Result<Speech, AssistantError> {
        let id = name.strip_suffix(".mp3").unwrap_or(name);
        let text = self
            .prepared_text(id)
            .ok_or_else(|| AssistantError::Invalid("unknown or expired voice".into()))?;
        self.voice_for(&text, None, Some(voice::Stream::Mp3)).await
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
            self.listen(&wav, "audio/wav")
                .await
                .map(|heard| heard.text)
                .map_err(|e| e.to_string())
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
        Some(format!("{}/api/voice/{id}.mp3", base.trim_end_matches('/')))
    }

    fn is_goodbye(&self, text: &str) -> bool {
        goodbye(text, &word_list("assistant.voice.goodbye"))
    }

    fn heard_nothing(&self) {
        self.heard_nothing_on("satellite");
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

#[cfg(test)]
mod tests {
    use super::*;

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
            name.strip_suffix(".mp3").is_some_and(|id| id.len() == 32),
            "{name}"
        );
        let id = name.strip_suffix(".mp3").unwrap();
        assert_eq!(
            moli.prepared_text(id).as_deref(),
            Some("Il fait vingt degrés.")
        );
        assert!(moli.prepared_text("0000").is_none());
        moli.0.prepared.lock().unwrap().get_mut(id).unwrap().0 = Instant::now();
        assert!(moli.prepared_text(id).is_none(), "expired");
    }
}

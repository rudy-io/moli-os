//! What is said at a door (a doorbell's interphone), passed on to the
//! household: on Telegram, and said on the speakers of
//! `[assistant] door_speakers`. A door is outside and anyone can talk to it:
//! nothing said there reaches the model, nothing acts on the house, and
//! nothing tells who is home. Unasked (nobody rang), only a sentence that
//! calls Moli by name is a message; anything else is dropped, kept nowhere.

use std::time::Instant;

use moli_core::{Origin, PointId, Value};
use moli_runtime::voice::AtDoor;

use crate::exchanges::{self, Exchange};
use crate::{Assistant, AssistantError, house, pricing, voice};

impl Assistant {
    pub(crate) async fn door(
        &self,
        door: &str,
        wav: Option<Vec<u8>>,
        rang: bool,
    ) -> Result<AtDoor, AssistantError> {
        let started = Instant::now();
        let (said, seconds) = match &wav {
            Some(wav) => (
                self.listen(wav, "audio/wav").await?.text.trim().to_owned(),
                voice::seconds_of(wav, "audio/wav"),
            ),
            None => (String::new(), 0.0),
        };
        let message = if rang {
            said
        } else {
            match after_name(&said) {
                None => return Ok(AtDoor::Nothing),
                Some(rest) if rest.is_empty() => return Ok(AtDoor::Called),
                Some(rest) => rest,
            }
        };
        let place = self.door_name(door);
        let telegram = if message.is_empty() {
            moli_i18n::tr!("assistant.door.sonne", door = place)
        } else {
            moli_i18n::tr!("assistant.door.message", door = place, message = message)
        };
        self.tell_household(
            &telegram,
            (!message.is_empty())
                .then(|| moli_i18n::tr!("assistant.door.annonce", door = place, message = message)),
        )
        .await;
        self.0.exchanges.add(Exchange {
            at: jiff::Timestamp::now().as_millisecond(),
            kind: exchanges::Kind::Door,
            surface: "door".into(),
            spoken: true,
            question: message.clone(),
            reply: String::new(),
            tools: Vec::new(),
            orders: Vec::new(),
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            audio: wav.as_deref().and_then(|w| self.record(w)),
            cost: exchanges::Cost::new(
                0.0,
                pricing::hear(&self.0.config.transcribe_model, seconds),
                0.0,
                0.0,
            ),
        });
        tracing::info!(
            door,
            words = message.split_whitespace().count(),
            "message from the door passed on"
        );
        Ok(AtDoor::Relayed(message))
    }

    /// The door as the household calls it (its label in the house).
    fn door_name(&self, door: &str) -> String {
        self.0
            .hub
            .snapshot()
            .devices
            .iter()
            .find(|d| d.device.id.as_str() == door)
            .map_or_else(
                || moli_i18n::tr!("assistant.door.porte"),
                |d| house::name_of(d).to_owned(),
            )
    }

    /// The house's Telegram, if it has one.
    pub(crate) fn telegram(&self) -> Option<PointId> {
        self.0.hub.snapshot().devices.iter().find_map(|d| {
            (d.device.manufacturer.as_deref() == Some("Telegram")
                && d.device.points.iter().any(|p| &*p.key == "notify"))
            .then(|| PointId::new(&d.device.id, "notify"))
        })
    }

    /// `text` on Telegram, and `said` on the door speakers.
    async fn tell_household(&self, text: &str, said: Option<String>) {
        let mut orders: Vec<(PointId, String)> = Vec::new();
        if let Some(point) = self.telegram() {
            orders.push((point, text.to_owned()));
        }
        if let Some(said) = said {
            for speaker in &self.0.config.door_speakers {
                orders.push((PointId::from(speaker.as_str()), said.clone()));
            }
        }
        for (point, words) in orders {
            if let Err(e) = self
                .0
                .hub
                .command(
                    &point,
                    Value::Text(words.into()),
                    Origin::Assistant,
                    Some("Moli".into()),
                )
                .await
            {
                tracing::warn!(point = %point, error = %e, "a message from the door was not passed on");
            }
        }
    }
}

/// What follows Moli's name, when the sentence calls it in its first words
/// (« Hey Moli, préviens la maison… », « Dis Moli : … », « Émoli ? »); `None` when
/// it does not.
fn after_name(said: &str) -> Option<String> {
    let words: Vec<&str> = said.split_whitespace().collect();
    let at = words.iter().take(3).position(|w| is_name(w))?;
    let rest = words[at + 1..].join(" ");
    Some(
        rest.trim_start_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
            .trim()
            .to_owned(),
    )
}

/// « Moli » as a transcription writes it, alone or run into the word before
/// (« Émoli », « Dimoli »).
fn is_name(word: &str) -> bool {
    let plain: String = word
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic())
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ï' | 'î' => 'i',
            'ÿ' => 'y',
            other => other,
        })
        .collect();
    ["moli", "molly", "mollie", "moly", "molie"]
        .iter()
        .any(|name| plain == *name || (plain.ends_with(name) && plain.len() <= name.len() + 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_sentence_calling_moli_is_a_message() {
        assert_eq!(
            after_name("Hey Moli, dis à la maison que le colis est là.").as_deref(),
            Some("dis à la maison que le colis est là.")
        );
        assert_eq!(
            after_name("Émoli, je suis devant la porte").as_deref(),
            Some("je suis devant la porte")
        );
        assert_eq!(after_name("Dis Molly ?").as_deref(), Some(""));
        assert_eq!(after_name("Dimoli tu m'ouvres"), Some("tu m'ouvres".into()));
        assert_eq!(after_name("On se retrouve chez Moli demain"), None);
        assert_eq!(after_name("Il fait beau aujourd'hui"), None);
        assert_eq!(after_name(""), None);
        assert!(!is_name("immobilier"));
        assert!(!is_name("mol"));
    }
}

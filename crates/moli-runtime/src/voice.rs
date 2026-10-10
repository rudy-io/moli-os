//! What a voice satellite asks of the home's assistant. The drivers start
//! before the assistant exists: the binary hands it to the hub afterwards
//! ([`crate::Hub::set_voice_brain`]), and a satellite finds it at each request.

use crate::BoxFuture;

/// Who said a line of a spoken conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    Person,
    Moli,
}

/// One line of a spoken conversation, kept by the satellite while it lasts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub who: Speaker,
    pub text: String,
}

/// Moli's answer to a spoken turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// What to say; may be empty when the turn only closes the conversation.
    pub text: String,
    /// The person asked to end the conversation (« c'est bon, j'ai fini »).
    pub closes: bool,
}

/// What Moli made of a sentence said at a door.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtDoor {
    /// Nothing for the household: no words, or words not meant for Moli.
    Nothing,
    /// Moli was called by name and nothing more (« Hey Moli ? »): the door
    /// asks for the message.
    Called,
    /// Passed on to the household (empty: a ring without a word).
    Relayed(String),
}

/// Sound as samples: 16-bit, mono, `rate` per second.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pcm {
    pub samples: Vec<i16>,
    pub rate: u32,
}

pub trait VoiceBrain: Send + Sync + std::fmt::Debug {
    /// What was said (a 16 kHz mono 16-bit WAV), in the house's language;
    /// empty when nothing was understood.
    fn hear(&self, wav: Vec<u8>) -> BoxFuture<'_, Result<String, String>>;

    /// One spoken turn: Moli may act (under the guard, like any spoken
    /// order) and answers in one short sentence, or closes the conversation
    /// when asked. The last line is the person's.
    fn answer(&self, conversation: Vec<Said>) -> BoxFuture<'_, Result<Answer, String>>;

    /// An address under `base` the satellite plays: `text` in Moli's voice,
    /// streamed while it is made. `None` when no voice can say it.
    fn voice_url(&self, base: &str, text: &str) -> Option<String>;

    /// `text` in Moli's voice, whole: 16-bit mono samples and their rate,
    /// for a speaker Moli feeds itself (a doorbell's talk-back).
    fn voice_pcm(&self, _text: &str) -> BoxFuture<'_, Result<Pcm, String>> {
        Box::pin(async { Err("no voice".to_owned()) })
    }

    /// What was said at a door (`door`: its device id) — a 16 kHz WAV,
    /// `None` when the visitor rang and said nothing — passed on to the
    /// household. `rang`: the visitor pressed the button, all they say is a
    /// message; otherwise only a sentence that calls Moli by name is one,
    /// and anything else is dropped, kept nowhere. A door is outside, anyone
    /// talks to it: nothing said there reaches the model or acts on the
    /// house.
    fn at_door(
        &self,
        _door: String,
        _wav: Option<Vec<u8>>,
        _rang: bool,
    ) -> BoxFuture<'_, Result<AtDoor, String>> {
        Box::pin(async { Ok(AtDoor::Nothing) })
    }

    /// Whether `text` only closes the conversation (« merci », « c'est tout »…).
    fn is_goodbye(&self, text: &str) -> bool;

    /// The satellite woke and heard nothing it understood (a wake word said
    /// for nothing, or a false activation): for the history, with what it
    /// heard when no transcription was asked (`wav`).
    fn heard_nothing(&self, _wav: Option<Vec<u8>>) {}
}

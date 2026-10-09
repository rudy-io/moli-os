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

pub trait VoiceBrain: Send + Sync + std::fmt::Debug {
    /// What was said (a 16 kHz mono 16-bit WAV), in the house's language;
    /// empty when nothing was understood.
    fn hear(&self, wav: Vec<u8>) -> BoxFuture<'_, Result<String, String>>;

    /// One spoken turn: Moli may act (under the guard, like any spoken
    /// order) and answers in one short sentence. The last line is the person's.
    fn answer(&self, conversation: Vec<Said>) -> BoxFuture<'_, Result<String, String>>;

    /// An address under `base` the satellite plays: `text` in Moli's voice,
    /// streamed while it is made. `None` when no voice can say it.
    fn voice_url(&self, base: &str, text: &str) -> Option<String>;

    /// Whether `text` only closes the conversation (« merci », « c'est tout »…).
    fn is_goodbye(&self, text: &str) -> bool;
}

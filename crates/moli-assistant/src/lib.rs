//! Moli, the home's own assistant.
//!
//! A conversation (typed or spoken) with an OpenAI-compatible model that
//! acts through the hub, as `Origin::Assistant`: the guard applies exactly
//! as to any agent (protected rooms, quiet hours), and a held order comes
//! back to the dashboard to be approved with one gesture. The model also
//! decides what the screen shows (`show`): the answer is a few words and
//! the right controls, not a wall of text.
//!
//! The model never sees camera images, and never sees a secret: the API key
//! lives in the encrypted store (`moli-os secrets set assistant api_key`).

// The automation draft schema is one big `json!`.
#![recursion_limit = "256"]

mod auto;
mod contract;
mod directory;
mod door;
mod exchanges;
mod house;
mod house_model;
mod house_tools;
mod import;
mod layout_edit;
mod llm;
mod outside;
mod places;
mod pricing;
mod proposals;
mod providers;
mod recap;
mod receipt;
mod recordings;
mod satellite;
mod settings;
mod tier;
mod voice;

pub use auto::Draft;
pub use contract::{TOOLS_JSON as MODEL_TOOLS_JSON, render_prompt as render_model_prompt};
pub use directory::{Directory, Done as DirectoryDone};
pub use exchanges::{Cost, Exchange, Kind as ExchangeKind, Order};
pub use import::HaAutomation;
pub use layout_edit::saved_place;
pub use places::{OpenMeteoPlaces, Place, Places};
pub use providers::{ProviderConfig, Routes};
pub use receipt::ReceiptReader;
pub use tier::Tier;
pub use voice::{Heard, Speech, SpeechStream, Spoken};

#[cfg(test)]
mod before;

use std::collections::{HashMap, VecDeque};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use moli_core::{DeviceId, InstanceId, Origin, PointId, Value};
use moli_runtime::{CommandError, Hub};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use tokio_util::sync::CancellationToken;

use crate::house::{Layout, name_of};
pub use crate::house_model::ProposalError;
use crate::llm::Endpoint;
pub use crate::proposals::Proposal;
use crate::proposals::Proposals;

/// `[assistant]` in moli.toml.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// OpenAI-compatible API (`http://` for a model on the local network).
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// For drafting automations: rare, so a stronger model is affordable.
    #[serde(default = "default_builder")]
    pub builder_model: String,
    /// For reading receipts: a model that sees images. Empty: `model`. Only
    /// the single provider of `base_url` uses it; with named providers,
    /// receipts are read by the model of the provider that serves them
    /// (`[assistant.routes] receipts`, else the chat provider).
    #[serde(default)]
    pub receipts_model: String,
    /// Speech to text; empty to disable voice.
    #[serde(default = "default_transcribe")]
    pub transcribe_model: String,
    /// Text to speech in the cloud; empty = the local voice only.
    #[serde(default = "default_speech_model")]
    pub speech_model: String,
    #[serde(default = "default_speech_voice")]
    pub speech_voice: String,
    /// How the cloud voice should sound (models that take instructions).
    #[serde(default = "default_speech_style")]
    pub speech_style: String,
    /// How fast the cloud voice speaks, 0.25 to 4 (1 = the model's own pace).
    #[serde(default = "default_speech_speed")]
    pub speech_speed: f64,
    /// Piper (Wyoming) `host:port`, when the cloud fails; empty = none.
    #[serde(default = "default_local_speech")]
    pub local_speech: String,
    #[serde(default = "default_local_voice")]
    pub local_voice: String,
    /// Whisper (Wyoming) `host:port`, when the cloud fails; empty = none.
    #[serde(default = "default_local_listen")]
    pub local_listen: String,
    /// For the date and time the model is told.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Questions about the world outside, answered from the web (the
    /// provider's own search); empty to disable it.
    #[serde(default = "default_model")]
    pub search_model: String,
    /// When the day's exchanges are sent on Telegram (`HH:MM`, the house's
    /// time); empty for no recap.
    #[serde(default = "default_recap_at")]
    pub recap_at: String,
    /// Points that say the messages left at a door (`sonos:…/announce`,
    /// `alexa:…/say`); none: Telegram only.
    #[serde(default)]
    pub door_speakers: Vec<String>,
    /// Where the house is, for the forecast; by default `[automations]`'s.
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Named providers (`[[assistant.provider]]`): where each kind of request
    /// may go, and the most sensitive tier each one may receive. Empty: the
    /// single provider of `base_url` and `model`, as before.
    #[serde(default)]
    pub provider: Vec<ProviderConfig>,
    /// Which provider serves which task (`[assistant.routes]`).
    #[serde(default)]
    pub routes: Routes,
}

impl Default for Config {
    /// No `[assistant]` in moli.toml: Moli is there anyway, waiting for a key.
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: default_model(),
            builder_model: default_builder(),
            receipts_model: String::new(),
            transcribe_model: default_transcribe(),
            speech_model: default_speech_model(),
            speech_voice: default_speech_voice(),
            speech_style: default_speech_style(),
            speech_speed: default_speech_speed(),
            local_speech: default_local_speech(),
            local_voice: default_local_voice(),
            local_listen: default_local_listen(),
            timezone: default_timezone(),
            search_model: default_model(),
            recap_at: default_recap_at(),
            door_speakers: Vec::new(),
            latitude: None,
            longitude: None,
            provider: Vec::new(),
            routes: Routes::default(),
        }
    }
}

fn default_base_url() -> String {
    "https://api.openai.com/v1".into()
}

fn default_model() -> String {
    "gpt-4.1-mini".into()
}

fn default_transcribe() -> String {
    "gpt-4o-mini-transcribe".into()
}

fn default_speech_model() -> String {
    "gpt-4o-mini-tts".into()
}

fn default_speech_voice() -> String {
    "coral".into()
}

fn default_speech_style() -> String {
    "Ton enjoué et souriant, plein d'énergie, comme un ami content de rendre service. Intonation vivante, débit rapide, sans traîner, français naturel de France.".into()
}

/// A conversation, not a reading: a little brisker than the model's pace.
fn default_speech_speed() -> f64 {
    1.2
}

fn default_local_speech() -> String {
    "127.0.0.1:10200".into()
}

fn default_local_voice() -> String {
    "fr_FR-siwis-medium".into()
}

fn default_local_listen() -> String {
    "127.0.0.1:10300".into()
}

fn default_builder() -> String {
    "gpt-4.1".into()
}

fn default_timezone() -> String {
    "Europe/Paris".into()
}

fn default_recap_at() -> String {
    "21:00".into()
}

/// Model round trips per turn (tool calls included).
const MAX_STEPS: usize = 6;
/// Messages of the conversation sent back (the model has no memory).
const MAX_MESSAGES: usize = 14;
const MAX_MESSAGE_CHARS: usize = 2_000;
/// What the model may write in one step when it only has to talk: a short
/// answer, a call or two.
const WORDS: usize = 700;
/// The tools that take the person's request whole (`automation` hands it to
/// the builder model): offered one, the model may have to write the request
/// out again, inside the call's JSON.
const COPIES_THE_REQUEST: &[&str] = &["automation"];
/// Turns per window, all clients together: a runaway page cannot run up a bill.
const MAX_TURNS: usize = 60;
const TURN_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Paid calls one turn may make, whatever the model asks: each web search
/// is billed, each draft is one or two calls to the builder model.
const MAX_SEARCHES: usize = 2;
const MAX_DRAFTS: usize = 1;
const MAX_CARDS: usize = 8;
/// Sentences spoken per window, all clients together (each costs a little).
const MAX_SPOKEN: usize = 150;
const MAX_SPOKEN_CHARS: usize = 300;
/// OpenAI's voices, the only ones a caller may pick.
const CLOUD_VOICES: [&str; 13] = [
    "alloy", "ash", "ballad", "cedar", "coral", "echo", "fable", "marin", "nova", "onyx", "sage",
    "shimmer", "verse",
];

#[derive(Clone, Debug)]
pub struct Assistant(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    hub: Hub,
    energy: Option<moli_energy::Energy>,
    history: Option<moli_history::History>,
    layout_path: Option<PathBuf>,
    config: Config,
    endpoint: Endpoint,
    /// The named providers and routes (see `providers.rs`).
    providers: providers::Providers,
    /// What the conversation proposed and nobody decided yet.
    proposals: Proposals,
    /// The house model reads its prompt again after a confirmed change (off in tests that script the provider).
    warm_after_change: std::sync::atomic::AtomicBool,
    /// At most one prompt read at a time after changes (Ollama would queue them).
    warm: house_model::WarmGate,
    /// One confirmed change at a time: working out the inverse, applying and remembering go together.
    changing: tokio::sync::Mutex<()>,
    tz: jiff::tz::TimeZone,
    running: tokio::sync::Semaphore,
    recent: Mutex<VecDeque<Instant>>,
    /// The voice has its own budget: three calls per exchange would eat
    /// the conversation's.
    voice_running: tokio::sync::Semaphore,
    voice_recent: Mutex<VecDeque<Instant>>,
    /// The voice chosen in the dashboard, and where it is kept.
    prefs: std::sync::RwLock<Option<settings::VoicePrefs>>,
    prefs_path: std::sync::RwLock<Option<PathBuf>>,
    /// For drafting automations from the conversation.
    automations: std::sync::RwLock<Option<moli_automation::Automations>>,
    /// The weather the dashboard shows (« actualise la météo » forgets what it kept).
    weather: std::sync::RwLock<Option<moli_weather::Weather>>,
    /// Where the house is (latitude, longitude): `moli.toml`'s, or the one a person chose.
    location: std::sync::RwLock<Option<(f64, f64)>>,
    /// Who finds a town by its name, and who keeps the household's people.
    places: std::sync::RwLock<std::sync::Arc<dyn Places>>,
    directory: std::sync::RwLock<Option<std::sync::Arc<dyn Directory>>>,
    /// When the model last made the weather forget what it kept (a floor: see `refresh_weather`).
    weather_refreshed: Mutex<Option<Instant>>,
    /// Orders and reminders for later (in memory: a restart forgets them).
    planned: std::sync::Mutex<Vec<house_tools::Planned>>,
    /// The dashboard's turns being thought out, by the id the dashboard
    /// gave them, so the person who asked can stop theirs (and only theirs).
    /// Each turn adds its entry once admitted and removes it itself.
    stoppable: Mutex<HashMap<String, CancellationToken>>,
    /// A Home Assistant import is running.
    importing: std::sync::atomic::AtomicBool,
    /// SHA-256 of the HA automations translated since start: not paid for
    /// twice, even after their draft was renamed.
    imported: Mutex<std::collections::HashSet<String>>,
    /// Voices prepared for a satellite: id → (until when, text).
    prepared: Mutex<std::collections::HashMap<String, (Instant, String)>>,
    /// What was asked and answered, for the history and the recap.
    exchanges: exchanges::Exchanges,
    /// What the satellites heard, while someone tests the microphone.
    recordings: recordings::Recordings,
    /// The last thing heard, for the turn that follows (its cost, its audio).
    last_heard: Mutex<Option<LastHeard>>,
}

/// What a transcription leaves for the turn that follows it.
#[derive(Debug)]
struct LastHeard {
    at: Instant,
    seconds: f64,
    audio: Option<String>,
}

/// A transcription older than this belongs to no turn.
const HEARD_FRESH: Duration = Duration::from_secs(60);

/// One turn, as the dashboard sends it: the conversation so far, the last
/// message being the user's.
#[derive(Debug, Deserialize)]
pub struct Turn {
    pub messages: Vec<Message>,
    /// `bubble` (a quick question from any page) or `page` (Moli's own
    /// page, room for more).
    #[serde(default)]
    pub surface: Option<String>,
    /// The question was spoken and the answer will be heard.
    #[serde(default)]
    pub spoken: bool,
    /// Names the turn so the person who asked can stop it
    /// (`POST /api/assistant/stop`): 1 to 64 letters, digits, `-` or `_`.
    /// The dashboard sends one; a satellite's turn is never stopped from
    /// a screen.
    #[serde(default)]
    pub id: Option<String>,
    /// Who asked, as the server knows it (a dashboard's client, a satellite):
    /// never read from the request. A proposal belongs to it, and « annule »
    /// only takes back its own.
    #[serde(skip)]
    pub asker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Serialize)]
pub struct Reply {
    pub reply: String,
    /// What the screen should show, in order (see the `show` tool).
    pub cards: Vec<Json>,
    /// Orders given during the turn and what became of them.
    pub actions: Vec<Action>,
    pub model: String,
    pub usage: Usage,
    /// The person asked to end the spoken conversation (`end_conversation`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub end: bool,
    /// The person stopped the turn before it answered: no words, but the
    /// orders already given are in `actions`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Changes of configuration proposed, for a person to confirm or cancel.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<Proposal>,
    /// The tools called, for the history.
    #[serde(skip)]
    pub tools: Vec<String>,
    /// What the web searches cost (dollars), for the history.
    #[serde(skip)]
    pub search_cost: f64,
    /// What drafting an automation cost (dollars, the builder model's
    /// thinking), for the history.
    #[serde(skip)]
    pub draft_cost: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    /// Of the prompt tokens, those the provider read from its cache (cheaper).
    pub cached_tokens: u64,
    pub completion_tokens: u64,
    pub steps: usize,
}

#[derive(Debug, Serialize)]
pub struct Action {
    pub point: PointId,
    pub device: String,
    pub value: Json,
    /// `done`, `held` (a human approves request `request`) or `failed`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum AssistantError {
    /// Not set up (no key…): the message says what to do.
    NotConfigured(String),
    /// Too many turns at once or in the last minutes.
    Busy,
    /// The request itself is wrong.
    Invalid(String),
    /// The model's provider failed.
    Upstream(String),
}

impl std::fmt::Display for AssistantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured(why) => write!(f, "assistant not configured: {why}"),
            Self::Busy => f.write_str("assistant busy, try again in a moment"),
            Self::Invalid(why) => write!(f, "invalid request: {why}"),
            Self::Upstream(why) => write!(f, "model unavailable: {why}"),
        }
    }
}

impl std::error::Error for AssistantError {}

impl Assistant {
    pub fn new(
        hub: Hub,
        energy: Option<moli_energy::Energy>,
        history: Option<moli_history::History>,
        layout_path: Option<PathBuf>,
        config: Config,
    ) -> anyhow::Result<Self> {
        let (config, providers) = config.resolved()?;
        if providers.any_contract() {
            // Its prompt is French and frozen: caught here, not at the first turn.
            contract::ensure_french()?;
        }
        let key_present = providers
            .key_name(providers::Route::Chat)
            .is_some_and(|name| hub.secret(&InstanceId::from("assistant"), &name).is_some());
        if let Some(warning) = providers.warning(key_present) {
            tracing::warn!("{warning}");
        }
        if let Some((id, declared, known)) = providers.refused_chat() {
            tracing::warn!(
                "assistant: provider {id:?} declares contract {declared:?}, this Moli speaks {known}: the conversation is off until the model or the configuration is updated"
            );
        }
        let endpoint = Endpoint::parse(&config.base_url)?;
        let tz = jiff::tz::TimeZone::get(&config.timezone)?;
        anyhow::ensure!(
            (0.25..=4.0).contains(&config.speech_speed),
            "speech_speed must be between 0.25 and 4"
        );
        // A position a person chose wins over the file's.
        let location = layout_path
            .as_deref()
            .and_then(layout_edit::saved_place)
            .map(|p| (p.latitude, p.longitude))
            .or(config.latitude.zip(config.longitude));
        Ok(Self(Arc::new(Inner {
            hub,
            energy,
            history,
            layout_path,
            config,
            endpoint,
            providers,
            proposals: Proposals::default(),
            warm_after_change: std::sync::atomic::AtomicBool::new(true),
            warm: house_model::WarmGate::default(),
            changing: tokio::sync::Mutex::new(()),
            tz,
            running: tokio::sync::Semaphore::new(2),
            stoppable: Mutex::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
            voice_running: tokio::sync::Semaphore::new(3),
            voice_recent: Mutex::new(VecDeque::new()),
            prefs: std::sync::RwLock::new(None),
            prefs_path: std::sync::RwLock::new(None),
            automations: std::sync::RwLock::new(None),
            weather: std::sync::RwLock::new(None),
            location: std::sync::RwLock::new(location),
            places: std::sync::RwLock::new(std::sync::Arc::new(OpenMeteoPlaces)),
            directory: std::sync::RwLock::new(None),
            weather_refreshed: Mutex::new(None),
            planned: std::sync::Mutex::default(),
            importing: std::sync::atomic::AtomicBool::new(false),
            imported: Mutex::default(),
            prepared: Mutex::default(),
            exchanges: exchanges::Exchanges::default(),
            recordings: recordings::Recordings::default(),
            last_heard: Mutex::new(None),
        })))
    }

    /// The weather of the house, for « actualise la météo ».
    pub fn set_weather(&self, weather: moli_weather::Weather) {
        *self
            .0
            .weather
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(weather);
    }

    /// Who finds a town by its name (the geocoding service; a fake in tests).
    pub fn set_places(&self, places: Arc<dyn Places>) {
        *self
            .0
            .places
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = places;
    }

    /// The household's people, for « ajoute Léa au foyer ».
    pub fn set_directory(&self, directory: Arc<dyn Directory>) {
        *self
            .0
            .directory
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(directory);
    }

    /// Where the house is, when known (latitude, longitude).
    pub(crate) fn position(&self) -> Option<(f64, f64)> {
        *self
            .0
            .location
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The secret holding the chat provider's API key (`api_key` for an old
    /// config, `api_key.<id>` for a named provider).
    fn key_name(&self) -> String {
        self.0
            .providers
            .key_name(providers::Route::Chat)
            .unwrap_or_else(|| "api_key".to_owned())
    }

    /// Why the conversation is off: a contract this Moli does not speak.
    fn refused(&self) -> Option<String> {
        let (provider, declared, known) = self.0.providers.refused_chat()?;
        Some(moli_i18n::tr!(
            "assistant.contract.refused",
            provider = provider,
            declared = declared,
            known = known
        ))
    }

    fn key(&self) -> Result<Option<String>, AssistantError> {
        if let Some(why) = self.refused() {
            return Err(AssistantError::NotConfigured(why));
        }
        let name = self.key_name();
        let key = self.0.hub.secret(&InstanceId::from("assistant"), &name);
        if key.is_none() && !self.0.endpoint.is_local() {
            return Err(AssistantError::NotConfigured(format!(
                "no API key: pipe it into `moli-os secrets set assistant {name}`"
            )));
        }
        Ok(key)
    }

    /// What the dashboard needs to know before offering a conversation.
    pub fn status(&self) -> Json {
        let ready = self.key().is_ok();
        let c = &self.0.config;
        let cloud_listen = ready && !c.transcribe_model.is_empty();
        let local_listen = voice::host_port(&c.local_listen).is_some();
        let cloud_voice = ready && !c.speech_model.is_empty();
        let local_voice = voice::host_port(&c.local_speech).is_some();
        json!({
            "ready": ready,
            "model": c.model,
            "listen": ready && (cloud_listen || local_listen),
            "speak": ready && (cloud_voice || local_voice),
            "voices": { "cloud": cloud_voice, "local": local_voice },
            "privacy": self.0.providers.summary(),
            "refused": self.refused(),
        })
    }

    fn admit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, AssistantError> {
        self.admit_leaving(0)
    }

    /// The same admission, leaving `spare` turns of the window to others:
    /// background work (an import) must not silence the conversation.
    fn admit_leaving(
        &self,
        spare: usize,
    ) -> Result<tokio::sync::SemaphorePermit<'_>, AssistantError> {
        admit_window(
            &self.0.running,
            &self.0.recent,
            MAX_TURNS,
            TURN_WINDOW,
            spare,
        )
    }

    /// Speech to text: the cloud first, the local Whisper (WAV only) when
    /// it fails or is off.
    pub async fn listen(&self, audio: &[u8], mime: &str) -> Result<Heard, AssistantError> {
        let c = &self.0.config;
        let local = voice::host_port(&c.local_listen);
        if c.transcribe_model.is_empty() && local.is_none() {
            return Err(AssistantError::NotConfigured("voice is disabled".into()));
        }
        let _permit = self.admit()?;
        let mime = mime.split(';').next().unwrap_or_default().trim();
        if !mime.starts_with("audio/") {
            return Err(AssistantError::Invalid(
                "expected an audio recording".into(),
            ));
        }
        if audio.len() < 200 {
            return Err(AssistantError::Invalid("recording too short".into()));
        }
        let mut failure = None;
        if !c.transcribe_model.is_empty() {
            match self.key() {
                Ok(key) => match self
                    .0
                    .endpoint
                    .transcribe(key.as_deref(), &c.transcribe_model, audio, mime)
                    .await
                {
                    Ok(text) => {
                        tracing::info!(engine = "cloud", bytes = audio.len(), "voice heard");
                        self.note_heard(voice::seconds_of(audio, mime), None);
                        return Ok(Heard {
                            text,
                            engine: "cloud",
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cloud transcription failed, trying the local one");
                        failure = Some(AssistantError::Upstream(e.to_string()));
                    }
                },
                Err(e) => failure = Some(e),
            }
        }
        if let (Some((host, port)), Some((rate, pcm))) = (local, voice::pcm_of(audio)) {
            return moli_net::wyoming::transcribe(host, port, rate, pcm, voice::LOCAL_LISTEN_LIMIT)
                .await
                .map(|text| {
                    tracing::info!(engine = "local", bytes = audio.len(), "voice heard");
                    Heard {
                        text,
                        engine: "local",
                    }
                })
                .map_err(|e| AssistantError::Upstream(e.to_string()));
        }
        Err(failure.unwrap_or_else(|| {
            AssistantError::Invalid("the local voice needs a WAV recording".into())
        }))
    }

    /// One sentence of an answer, spoken (at most 300 characters), in the
    /// configured cloud voice or `voice` (one of OpenAI's, to try them).
    pub async fn speak(&self, text: &str, voice: Option<&str>) -> Result<Spoken, AssistantError> {
        match self.voice_for(text, voice, None).await? {
            Speech::Whole(spoken) => Ok(spoken),
            Speech::Stream(_) => Err(AssistantError::Upstream("a streamed voice".into())),
        }
    }

    /// The same, streamed while the cloud voice makes it (a second sooner
    /// in a conversation); whole from the house's voice when the cloud fails.
    pub async fn speak_streamed(
        &self,
        text: &str,
        voice: Option<&str>,
    ) -> Result<Speech, AssistantError> {
        self.voice_for(text, voice, Some(voice::Stream::Pcm)).await
    }

    async fn voice_for(
        &self,
        text: &str,
        voice: Option<&str>,
        stream: Option<voice::Stream>,
    ) -> Result<Speech, AssistantError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_SPOKEN_CHARS {
            return Err(AssistantError::Invalid(format!(
                "a sentence of 1 to {MAX_SPOKEN_CHARS} characters"
            )));
        }
        if voice.is_some_and(|v| !CLOUD_VOICES.contains(&v)) {
            return Err(AssistantError::Invalid(format!(
                "unknown voice; one of {}",
                CLOUD_VOICES.join(", ")
            )));
        }
        let _permit = admit_window(
            &self.0.voice_running,
            &self.0.voice_recent,
            MAX_SPOKEN,
            TURN_WINDOW,
            0,
        )?;
        let c = &self.0.config;
        let key = self.key().ok();
        let (chosen, style) = self.voice_and_style();
        let voices = voice::Voices {
            cloud: match &key {
                Some(key) if !c.speech_model.is_empty() => Some(voice::Cloud {
                    endpoint: &self.0.endpoint,
                    key: key.as_deref(),
                    model: &c.speech_model,
                    voice: voice.unwrap_or(&chosen),
                    style: &style,
                    speed: self.speed(),
                }),
                _ => None,
            },
            local: voice::host_port(&c.local_speech),
            local_voice: Some(c.local_voice.as_str()).filter(|v| !v.is_empty()),
        };
        let said = voice::speakable(text);
        let speech = match stream {
            Some(stream) => voices.say_streamed(&said, stream).await?,
            None => Speech::Whole(voices.say(&said).await?),
        };
        // What a conversation costs: characters in, seconds of audio out (a
        // stream says it at its end).
        if let Speech::Whole(spoken) = &speech {
            tracing::info!(
                engine = spoken.engine,
                chars = text.chars().count(),
                bytes = spoken.audio.len(),
                "voice spoken"
            );
        }
        Ok(speech)
    }

    /// Stops the dashboard's turn named `id`. Says whether there was one:
    /// « stop » pressed on nothing should be told so. Only the wait for the
    /// model is cut: a tool, an order or a draft already started goes to
    /// its end, and what the turn already did stays done.
    #[must_use]
    pub fn stop(&self, id: &str) -> bool {
        let token = self
            .0
            .stoppable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned();
        token.inspect(CancellationToken::cancel).is_some()
    }

    /// Admits a turn, THEN lists it as stoppable: in that order, a turn
    /// refused as busy never shows up in the table.
    fn admit_turn(
        &self,
        id: Option<&str>,
        stop: &CancellationToken,
    ) -> Result<(tokio::sync::SemaphorePermit<'_>, Option<Stoppable<'_>>), AssistantError> {
        // A malformed id is refused before it takes a place in the window.
        if let Some(id) = id {
            valid_turn_id(id)?;
        }
        let permit = self.admit()?;
        Ok((permit, self.stoppable(id, stop)?))
    }

    /// Lists an admitted turn under its `id` for as long as it runs. A turn
    /// refused (busy, invalid) never shows up, and an id in use is refused:
    /// one entry, one owner.
    fn stoppable(
        &self,
        id: Option<&str>,
        stop: &CancellationToken,
    ) -> Result<Option<Stoppable<'_>>, AssistantError> {
        let Some(id) = id else {
            return Ok(None);
        };
        valid_turn_id(id)?;
        let mut table = self
            .0
            .stoppable
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if table.contains_key(id) {
            return Err(AssistantError::Invalid("turn id already in use".into()));
        }
        table.insert(id.to_owned(), stop.clone());
        Ok(Some(Stoppable {
            table: &self.0.stoppable,
            id: id.to_owned(),
        }))
    }

    /// The model's answer, or `None` when the person stopped the turn while
    /// it was awaited. Only this wait is cut: what the answer asks for runs
    /// outside it, and always to its end.
    pub(crate) async fn ask_model(
        &self,
        key: Option<&str>,
        body: &Json,
        stop: &CancellationToken,
    ) -> Result<Option<Json>, AssistantError> {
        tokio::select! {
            // Stopped already: not even asked (nor paid for).
            biased;
            () = stop.cancelled() => Ok(None),
            answer = self.0.endpoint.chat(key, body) => answer
                .map(Some)
                .map_err(|e| AssistantError::Upstream(e.to_string())),
        }
    }

    /// One turn of the conversation: the model reads the house, may act and
    /// pick cards, then answers.
    /// One turn, kept in the history (question, answer, orders, time taken).
    pub async fn turn(&self, turn: Turn) -> Result<Reply, AssistantError> {
        let started = Instant::now();
        let question = turn
            .messages
            .last()
            .filter(|m| m.role == Role::User)
            .map(|m| m.content.clone());
        let surface = turn.surface.clone().unwrap_or_default();
        let spoken = turn.spoken;
        let stop = CancellationToken::new();
        let result = self.converse(turn, &stop).await;
        if let Some(question) = question {
            let heard = self.take_heard();
            let c = &self.0.config;
            let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let hear = heard
                .as_ref()
                .map_or(0.0, |h| pricing::hear(&c.transcribe_model, h.seconds));
            let (kind, reply, tools, orders, cost) = match &result {
                Ok(r) => (
                    if r.interrupted {
                        exchanges::Kind::Interrupted
                    } else {
                        exchanges::Kind::Turn
                    },
                    r.reply.clone(),
                    r.tools.clone(),
                    r.actions
                        .iter()
                        .map(|a| Order {
                            device: a.device.clone(),
                            status: a.status.to_owned(),
                        })
                        .collect(),
                    exchanges::Cost::new(
                        pricing::think(
                            &c.model,
                            r.usage.prompt_tokens,
                            r.usage.cached_tokens,
                            r.usage.completion_tokens,
                        ) + r.draft_cost,
                        hear,
                        if spoken {
                            pricing::speak(&c.speech_model, r.reply.chars().count())
                        } else {
                            0.0
                        },
                        r.search_cost,
                    ),
                ),
                Err(e) => (
                    exchanges::Kind::Failed,
                    e.to_string(),
                    Vec::new(),
                    Vec::new(),
                    exchanges::Cost::new(0.0, hear, 0.0, 0.0),
                ),
            };
            self.0.exchanges.add(Exchange {
                at: jiff::Timestamp::now().as_millisecond(),
                kind,
                surface,
                spoken,
                question,
                reply,
                tools,
                orders,
                ms,
                audio: heard.and_then(|h| h.audio),
                cost,
            });
        }
        result
    }

    /// A transcription's length (and, from a satellite, its recording),
    /// for the turn that follows.
    pub(crate) fn note_heard(&self, seconds: f64, audio: Option<String>) {
        *self
            .0
            .last_heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(LastHeard {
            at: Instant::now(),
            seconds,
            audio,
        });
    }

    /// What was heard just before, if it was just before.
    fn take_heard(&self) -> Option<LastHeard> {
        self.0
            .last_heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .filter(|h| h.at.elapsed() < HEARD_FRESH)
    }

    /// Keeps what a satellite heard, while recording is on; its name.
    pub(crate) fn record(&self, wav: &[u8]) -> Option<String> {
        self.0
            .recordings
            .save(wav, jiff::Timestamp::now().as_millisecond())
    }

    /// Where the satellites' recordings go (the data directory). The week
    /// runs out while Moli runs too: the old ones go every hour, recording
    /// or not.
    pub fn open_recordings(&self, dir: PathBuf) {
        self.0
            .recordings
            .open(dir, jiff::Timestamp::now().as_millisecond());
        let weak = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(recordings::EXPIRE_EVERY);
            loop {
                every.tick().await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                inner
                    .recordings
                    .expire(jiff::Timestamp::now().as_millisecond());
            }
        });
    }

    /// Until when the satellites' recordings are kept (none: not recording).
    #[must_use]
    pub fn recording_until(&self) -> Option<i64> {
        self.0
            .recordings
            .until(jiff::Timestamp::now().as_millisecond())
    }

    /// Recording on for `days` (a week at most), or off with 0.
    pub fn set_recording(&self, days: u32) -> Result<Option<i64>, AssistantError> {
        let until = self
            .0
            .recordings
            .switch(days, jiff::Timestamp::now().as_millisecond())
            .map_err(|e| AssistantError::Upstream(format!("cannot save the switch: {e}")))?;
        tracing::info!(?until, "satellite recordings switched from the dashboard");
        Ok(until)
    }

    /// The file of recording `name`, if it is one (and less than a week old).
    #[must_use]
    pub fn recording(&self, name: &str) -> Option<PathBuf> {
        self.0
            .recordings
            .path(name, jiff::Timestamp::now().as_millisecond())
    }

    /// Where the history is kept (the data directory), read back at start.
    pub fn open_exchanges(&self, path: PathBuf) {
        self.0
            .exchanges
            .open(path, jiff::Timestamp::now().as_millisecond());
    }

    /// The exchanges of the last `hours`, newest first.
    #[must_use]
    pub fn exchanges(&self, hours: u32) -> Vec<Exchange> {
        let from = jiff::Timestamp::now().as_millisecond() - i64::from(hours) * 3_600_000;
        let mut all = self.0.exchanges.since(from);
        all.reverse();
        all
    }

    /// A satellite woke and heard nothing it understood: counted in the
    /// history (false activations show up there).
    pub(crate) fn heard_nothing_on(&self, surface: &str, wav: Option<&[u8]>) {
        let heard = self.take_heard();
        let hear = heard.as_ref().map_or(0.0, |h| {
            pricing::hear(&self.0.config.transcribe_model, h.seconds)
        });
        let audio = match wav {
            Some(wav) => self.record(wav),
            None => heard.and_then(|h| h.audio),
        };
        self.0.exchanges.add(Exchange {
            at: jiff::Timestamp::now().as_millisecond(),
            kind: exchanges::Kind::Silence,
            surface: surface.into(),
            spoken: true,
            question: String::new(),
            reply: String::new(),
            tools: Vec::new(),
            orders: Vec::new(),
            ms: 0,
            audio,
            cost: exchanges::Cost::new(0.0, hear, 0.0, 0.0),
        });
    }

    /// Every evening at `recap_at`, the day's exchanges on Telegram.
    pub fn start_recap(&self) {
        let Some(at) = recap::parse_time(&self.0.config.recap_at) else {
            if !self.0.config.recap_at.trim().is_empty() {
                tracing::warn!(recap_at = %self.0.config.recap_at, "recap_at is not HH:MM: no recap");
            }
            return;
        };
        let moli = self.clone();
        tokio::spawn(async move {
            loop {
                let now = jiff::Timestamp::now().to_zoned(moli.0.tz.clone());
                let Some(next) = recap::next_after(&now, at) else {
                    return;
                };
                let wait = next.timestamp().as_millisecond() - now.timestamp().as_millisecond();
                tokio::time::sleep(Duration::from_millis(u64::try_from(wait).unwrap_or(0))).await;
                moli.send_recap().await;
            }
        });
    }

    /// The last 24 hours on the house's Telegram (nothing when nothing
    /// happened). Returns whether a message went out.
    pub async fn send_recap(&self) -> bool {
        let now = jiff::Timestamp::now().to_zoned(self.0.tz.clone());
        let day = recap_day(&now);
        let text = recap::compose(
            &self
                .0
                .exchanges
                .since(now.timestamp().as_millisecond() - 24 * 3_600_000),
            &self.0.tz,
            &day,
        );
        let Some(text) = text else {
            return false;
        };
        let Some(point) = self.telegram() else {
            tracing::info!("no Telegram device: the recap is only in the dashboard");
            return false;
        };
        match self
            .0
            .hub
            .command(
                &point,
                Value::Text(text.into()),
                Origin::Assistant,
                Some("Moli".into()),
            )
            .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "the recap was not sent");
                false
            }
        }
    }

    async fn converse(
        &self,
        turn: Turn,
        stop: &CancellationToken,
    ) -> Result<Reply, AssistantError> {
        if turn.messages.last().is_none_or(|m| m.role != Role::User) {
            return Err(AssistantError::Invalid(
                "the last message must be the user's".into(),
            ));
        }
        let key = self.key()?;
        let _admitted = self.admit_turn(turn.id.as_deref(), stop)?;
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        let page = turn.surface.as_deref() == Some("page");
        let question = turn
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();

        if self.speaks_contract() {
            return self
                .converse_house(&turn, &layout, key.as_deref(), stop)
                .await;
        }

        let power = self.power_now().await;
        let mut messages = vec![
            json!({ "role": "system", "content": with_voice(self.system_prompt(&layout, page, &power) + &self.web_line(), turn.spoken) }),
        ];
        messages.extend(recent(&turn.messages));

        // Spoken, nobody waits for cards: no `show`, one round trip less
        // (the screen still gets the cards the question calls for).
        let mut offered = tools();
        if turn.spoken
            && let Some(list) = offered.as_array_mut()
        {
            list.retain(|t| t["function"]["name"] != "show");
            list.push(end_tool());
        }
        if let Some(list) = offered.as_array_mut() {
            if self.position().is_some() {
                list.push(forecast_tool());
            }
            if self.searches() {
                list.push(search_tool());
            }
        }
        let mut run = Run::default();
        for step in 0..MAX_STEPS {
            let mut body = json!({
                "model": self.0.config.model,
                "messages": messages,
                "tools": offered,
            });
            // Reasoning models (gpt-5…, o…) take neither a temperature nor
            // `max_tokens`; they think briefly here: answers are short.
            if is_reasoning(&self.0.config.model) {
                body["max_completion_tokens"] = json!(4_000);
                body["reasoning_effort"] = json!("low");
            } else {
                if takes_temperature(&self.0.config.model) {
                    body["temperature"] = json!(0.3);
                }
                body["max_tokens"] = json!(output_room(&offered, &question));
            }
            if step + 1 == MAX_STEPS {
                body["tool_choice"] = json!("none");
            }
            let Some(answer) = self.ask_model(key.as_deref(), &body, stop).await? else {
                return Ok(self.interrupted(run));
            };
            run.usage.steps = step + 1;
            run.usage.prompt_tokens += answer["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
            run.usage.completion_tokens +=
                answer["usage"]["completion_tokens"].as_u64().unwrap_or(0);
            run.usage.cached_tokens += answer["usage"]["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0);
            if answer["choices"][0]["finish_reason"] == "length" {
                tracing::warn!(step, "the model was cut: no room left to answer");
                run.cut = true;
            }
            let message = &answer["choices"][0]["message"];
            let calls = message["tool_calls"].as_array().filter(|c| !c.is_empty());
            if let Some(calls) = calls {
                if let Some(said) = self
                    .act(message, calls, &layout, &mut run, &mut messages)
                    .await
                {
                    return Ok(self.finish(&said, run, &layout, &question));
                }
                continue;
            }
            let text = message["content"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_owned();
            return Ok(self.finish(&text, run, &layout, &question));
        }
        Ok(self.finish("", run, &layout, &question))
    }

    /// Runs an answer's tool calls, their results added to `messages`. Cards,
    /// orders and words in the same answer: the screen gives nothing back
    /// worth another round trip, nor an order the house carried out, and the
    /// words end the turn (a second or two sooner, which a spoken
    /// conversation hears). The words are the answer's text or, since models
    /// leave it empty beside a call, each order's `say`. An order held or
    /// failed goes back to the model, which must say so.
    async fn act(
        &self,
        message: &Json,
        calls: &[Json],
        layout: &Layout,
        run: &mut Run,
        messages: &mut Vec<Json>,
    ) -> Option<String> {
        let mut said = message["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let mut carried_out = calls.iter().all(|c| {
            matches!(
                c["function"]["name"].as_str(),
                Some("show" | "set" | "end_conversation")
            )
        });
        let mut says: Vec<String> = Vec::new();
        messages.push(json!({
            "role": "assistant",
            "content": message["content"],
            "tool_calls": calls,
        }));
        for call in calls {
            let name = call["function"]["name"].as_str().unwrap_or_default();
            if run.tools.len() < 16 {
                run.tools.push(name.chars().take(32).collect());
            }
            let args: Json = call["function"]["arguments"]
                .as_str()
                .and_then(|a| serde_json::from_str(a).ok())
                .unwrap_or_else(|| json!({}));
            let result = self.tool(name, &args, layout, run).await;
            if name == "set" {
                carried_out &= result["status"] == "done";
            }
            if (name == "set" || name == "end_conversation")
                && let Some(say) = args["say"].as_str().map(str::trim)
                && !say.is_empty()
                && !says.iter().any(|s| s == say)
            {
                says.push(say.to_owned());
            }
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call["id"],
                "content": result.to_string(),
            }));
        }
        if said.is_empty() {
            said = says.join(" ");
        }
        // Closing needs no words: the satellite's chime says it.
        (carried_out && (!said.is_empty() || run.end)).then_some(said)
    }

    /// What a stopped turn leaves: no words, but the orders already given,
    /// and the steps already taken, whose cost stays counted.
    fn interrupted(&self, run: Run) -> Reply {
        tracing::info!(
            steps = run.usage.steps,
            actions = run.actions.len(),
            "assistant turn stopped"
        );
        Reply {
            reply: String::new(),
            cards: Vec::new(),
            actions: run.actions,
            model: self.0.config.model.clone(),
            usage: run.usage,
            end: false,
            interrupted: true,
            // Proposed before the stop: still a person's to confirm.
            proposals: run.proposals,
            tools: run.tools,
            search_cost: run.search_cost,
            draft_cost: run.draft_cost,
        }
    }

    fn finish(&self, text: &str, mut run: Run, layout: &Layout, question: &str) -> Reply {
        // The model sometimes answers in words only: the screen still shows
        // what the question was obviously about.
        if run.cards.is_empty() && run.actions.is_empty() {
            run.cards = guess_cards(question);
        }
        // Whatever was acted on shows up, even if the model forgot to say so.
        for action in &run.actions {
            if let Some((device, _)) = action.point.split() {
                let shown = run.cards.iter().any(|c| c["id"] == device.as_str());
                if !shown && !layout.hides(device.as_str()) && run.cards.len() < MAX_CARDS {
                    run.cards.push(json!({ "kind": "device", "id": device }));
                }
            }
        }
        let whole = once(text);
        let filtered = trim_filler(&whole, !run.cards.is_empty());
        let reply = spoken_reply(&whole, filtered, &run);
        tracing::info!(
            steps = run.usage.steps,
            prompt_tokens = run.usage.prompt_tokens,
            completion_tokens = run.usage.completion_tokens,
            actions = run.actions.len(),
            cards = run.cards.len(),
            tools = run.tools.join(","),
            "assistant turn"
        );
        Reply {
            reply,
            cards: run.cards,
            actions: run.actions,
            model: self.0.config.model.clone(),
            usage: run.usage,
            end: run.end,
            interrupted: false,
            proposals: run.proposals,
            tools: run.tools,
            search_cost: run.search_cost,
            draft_cost: run.draft_cost,
        }
    }

    /// `forecast`: the days and hours to come where the house is.
    async fn forecast(&self, args: &Json) -> Json {
        let Some((latitude, longitude)) = self.position() else {
            return json!({ "error": "the house's location is unknown" });
        };
        let days = args["days"].as_u64().unwrap_or(3);
        match outside::forecast(latitude, longitude, days).await {
            Ok(forecast) => forecast,
            Err(e) => {
                tracing::warn!(error = %e, "forecast unavailable");
                json!({ "error": "the forecast is unavailable right now" })
            }
        }
    }

    /// The web is searched only through the provider (never a local model).
    fn searches(&self) -> bool {
        !self.0.config.search_model.is_empty() && !self.0.endpoint.is_local()
    }

    /// Models believe they cannot read the news: told otherwise when they can.
    fn web_line(&self) -> String {
        if self.searches() {
            moli_i18n::tr!("assistant.prompt.web")
        } else {
            String::new()
        }
    }

    /// `web_search`: a question about the world, answered from the web.
    async fn web_search(&self, args: &Json, run: &mut Run) -> Json {
        let Some(query) = args["query"]
            .as_str()
            .map(str::trim)
            .filter(|q| !q.is_empty())
        else {
            return json!({ "error": "query is required" });
        };
        let key = match self.key() {
            Ok(key) => key,
            Err(e) => return json!({ "error": e.to_string() }),
        };
        if run.searches == MAX_SEARCHES {
            return over_budget("web_search", MAX_SEARCHES);
        }
        run.searches += 1;
        let c = &self.0.config;
        match outside::search(
            &self.0.endpoint,
            key.as_deref(),
            &c.search_model,
            &c.timezone,
            query,
        )
        .await
        {
            Ok((found, usage)) => {
                run.search_cost += pricing::SEARCH
                    + pricing::think(
                        &c.search_model,
                        usage["input_tokens"].as_u64().unwrap_or(0),
                        usage["input_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                        usage["output_tokens"].as_u64().unwrap_or(0),
                    );
                found
            }
            Err(e) => {
                run.search_cost += pricing::SEARCH;
                tracing::warn!(error = %e, "web search failed");
                json!({ "error": "the web search failed" })
            }
        }
    }

    /// Live power, circuit by circuit, and today's figures: the most asked
    /// questions answered without a round trip.
    async fn power_now(&self) -> String {
        let Some(energy) = &self.0.energy else {
            return String::new();
        };
        let Ok(s) = energy.summary().await else {
            return String::new();
        };
        power_line(&s)
    }

    fn system_prompt(&self, layout: &Layout, page: bool, power: &str) -> String {
        let snapshot = self.0.hub.snapshot();
        let guard = &snapshot.guard;
        let now = jiff::Timestamp::now().to_zoned(self.0.tz.clone());
        let protected = if guard.protected_rooms.is_empty() {
            moli_i18n::tr!("assistant.prompt.none")
        } else {
            guard
                .protected_rooms
                .iter()
                .map(|r| layout.room(r))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let quiet = guard.quiet_hours.as_ref().map_or_else(
            || moli_i18n::tr!("assistant.prompt.none"),
            |q| quiet_text(&q.from, &q.to, q.active),
        );
        let rooms: Vec<&str> = layout.rooms.iter().map(|r| r.name.as_str()).collect();
        ChatPrompt {
            title: layout.title.as_deref(),
            page,
            protected: &protected,
            quiet: &quiet,
            date: &house_date(&now),
            power,
            rooms: &rooms.join(", "),
            inventory: &house::inventory(
                &snapshot.devices,
                layout,
                &guard.protected_rooms,
                house::Reader::Chat,
            ),
        }
        .render()
    }

    async fn tool(&self, name: &str, args: &Json, layout: &Layout, run: &mut Run) -> Json {
        match name {
            "get_device" => self.get_device(args, layout),
            "set" => self.set(args, run).await,
            "ambiance" => self.ambiance(args, run).await,
            "show" => show(&self.0.hub, args, layout, run),
            "end_conversation" => {
                run.end = true;
                json!({ "status": "done" })
            }
            "energy" => self.energy(args).await,
            "history" => self.history(args).await,
            "forecast" => self.forecast(args).await,
            "web_search" => self.web_search(args, run).await,
            "automation" => self.automation_tool(args, run).await,
            other => json!({ "error": format!("unknown tool {other}") }),
        }
    }

    fn get_device(&self, args: &Json, layout: &Layout) -> Json {
        let Some(id) = args["device"].as_str() else {
            return json!({ "error": "device is required" });
        };
        let Some(view) = self.0.hub.device(&DeviceId::from(id)) else {
            return json!({ "error": "unknown device" });
        };
        let points: Vec<Json> = view
            .device
            .points
            .iter()
            .filter(|p| !house::private(&p.key))
            .map(|p| {
                json!({
                    "key": p.key,
                    "label": p.label,
                    "kind": p.kind,
                    "unit": p.unit,
                    "writable": p.access.write,
                    "value": view.state.get(&p.key).map(|s| s.value.to_json()),
                })
            })
            .collect();
        json!({
            "id": view.device.id,
            "name": name_of(&view),
            "room": layout.room_of(&view),
            "online": view.online,
            "camera": view.camera,
            "points": points,
        })
    }

    async fn set(&self, args: &Json, run: &mut Run) -> Json {
        let (Some(point), Some(value)) = (args["point"].as_str(), args.get("value")) else {
            return json!({ "error": "point and value are required" });
        };
        let point = house::resolve_point(point, &self.0.hub.snapshot().devices);
        let device = point
            .split()
            .and_then(|(d, _)| self.0.hub.device(&d))
            .map_or_else(|| "?".to_owned(), |v| name_of(&v).to_owned());
        let result = self
            .0
            .hub
            .command(
                &point,
                Value::from_json(value),
                Origin::Assistant,
                Some("Moli".into()),
            )
            .await;
        let mut action = Action {
            point: point.clone(),
            device,
            value: value.clone(),
            status: "done",
            request: None,
            reason: None,
            error: None,
        };
        let answer = match result {
            Ok(()) => json!({ "status": "done" }),
            Err(CommandError::NeedsApproval { id, reason }) => {
                action.status = "held";
                action.request = Some(id);
                action.reason = Some(reason.clone());
                json!({
                    "status": "awaiting_approval",
                    "reason": reason,
                    "note": moli_i18n::tr!("assistant.tools.held_note"),
                })
            }
            Err(e) => {
                action.status = "failed";
                action.error = Some(e.to_string());
                json!({ "status": "failed", "error": e.to_string() })
            }
        };
        run.actions.push(action);
        answer
    }

    /// A look for several lights at once (a room): an ambiance, a colour or
    /// a white. Each lamp's orders go through the guard like `set`.
    async fn ambiance(&self, args: &Json, run: &mut Run) -> Json {
        let lights: Vec<DeviceId> = args["lights"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .take(32)
            .map(DeviceId::from)
            .collect();
        if lights.is_empty() {
            return json!({ "error": "lights is required" });
        }
        let (look, what) = if let Some(id) = args["ambiance"].as_str() {
            match moli_ambiance::find(id) {
                Some(a) => (moli_ambiance::Look::Ambiance(a), a.label()),
                None => return json!({ "error": format!("unknown ambiance {id}") }),
            }
        } else if let Some(hex) = args["color"]
            .as_str()
            .filter(|h| moli_core::color::hex_to_xy(h).is_some())
        {
            (moli_ambiance::Look::Color(hex), hex.to_owned())
        } else if let Some(k) = args["white"]
            .as_u64()
            .and_then(|k| u32::try_from(k).ok())
            .filter(|k| (1500..=10_000).contains(k))
        {
            (
                moli_ambiance::Look::White(k),
                moli_i18n::tr!("assistant.tools.white", kelvin = k),
            )
        } else {
            return json!({ "error": "one of ambiance, color (#rrggbb) or white (kelvins)" });
        };
        let report = moli_ambiance::apply(
            &self.0.hub,
            look,
            &lights,
            Origin::Assistant,
            Some("Moli".into()),
        )
        .await;
        let name = |id: &DeviceId| {
            self.0
                .hub
                .device(id)
                .map_or_else(|| id.to_string(), |v| name_of(&v).to_owned())
        };
        for id in &report.done {
            run.actions.push(Action {
                point: PointId::new(id, "ambiance"),
                device: name(id),
                value: json!(what),
                status: "done",
                request: None,
                reason: None,
                error: None,
            });
        }
        for held in &report.held {
            run.actions.push(Action {
                point: held.point.clone(),
                device: name(&held.device),
                value: json!(what),
                status: "held",
                request: Some(held.approval),
                reason: Some(held.reason.clone()),
                error: None,
            });
        }
        json!({
            "done": report.done.len(),
            "held": report.held.len(),
            "failed": report.failed.iter().map(|f| json!({ "device": name(&f.device), "error": f.error })).collect::<Vec<_>>(),
            "skipped": report.skipped.len(),
            "note": if report.held.is_empty() { String::new() } else { moli_i18n::tr!("assistant.tools.ambiance_held_note") },
        })
    }

    async fn energy(&self, args: &Json) -> Json {
        let Some(energy) = &self.0.energy else {
            return json!({ "error": "no energy meters" });
        };
        let result = match args["step"].as_str() {
            None | Some("") => energy
                .summary()
                .await
                .map(|s| serde_json::to_value(s).unwrap_or_default()),
            Some(step) => {
                let (step, count) = match step {
                    "hour" => (moli_energy::Step::Hour, 24),
                    "day" => (moli_energy::Step::Day, 30),
                    "month" => (moli_energy::Step::Month, 12),
                    other => {
                        return json!({ "error": format!("step {other:?}: hour, day or month") });
                    }
                };
                energy
                    .recent(step, count)
                    .await
                    .map(|r| serde_json::to_value(r).unwrap_or_default())
            }
        };
        result.unwrap_or_else(|e| json!({ "error": e.to_string() }))
    }

    async fn history(&self, args: &Json) -> Json {
        let Some(history) = &self.0.history else {
            return json!({ "error": "history is disabled" });
        };
        let Some(point) = args["point"].as_str() else {
            return json!({ "error": "point is required" });
        };
        // Where someone was is as private as where they are.
        if PointId::from(point)
            .split()
            .is_some_and(|(_, key)| house::private(key))
        {
            return json!({ "error": moli_i18n::tr!("assistant.tools.private") });
        }
        let hours = args["hours"]
            .as_f64()
            .unwrap_or(24.0)
            .clamp(0.1, 24.0 * 31.0);
        let to = moli_core::now_ms();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let from = to.saturating_sub((hours * 3_600_000.0) as u64);
        match history.series(point, from, to, 48).await {
            Ok(series) => serde_json::to_value(series).unwrap_or_default(),
            Err(e) => json!({ "error": e.to_string() }),
        }
    }
}

#[derive(Debug, Default)]
struct Run {
    cards: Vec<Json>,
    actions: Vec<Action>,
    /// Changes of configuration proposed in the turn.
    proposals: Vec<Proposal>,
    /// Who asked (see `Turn::asker`), and whether it is a satellite: it has no
    /// screen to confirm on, so it proposes nothing.
    asker: Option<String>,
    satellite: bool,
    usage: Usage,
    /// The tools called, in order, for the log: where a slow turn went.
    tools: Vec<String>,
    /// `end_conversation` was called.
    end: bool,
    /// The model ran out of room mid-answer: what came back is incomplete,
    /// and a cut-off tool call is simply lost. Not the same as having
    /// nothing to say, so it must not be reported as such.
    cut: bool,
    /// What the web searches cost (dollars).
    search_cost: f64,
    /// Web searches and automation drafts made (`MAX_SEARCHES`, `MAX_DRAFTS`).
    searches: usize,
    drafts: usize,
    /// What the drafts cost (dollars).
    draft_cost: f64,
}

/// A stoppable turn's entry, removed by the turn itself when it ends,
/// however it ends.
struct Stoppable<'a> {
    table: &'a Mutex<HashMap<String, CancellationToken>>,
    id: String,
}

impl Drop for Stoppable<'_> {
    fn drop(&mut self) {
        self.table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// A paid tool called once too often in a turn: the model is told, the turn
/// goes on with what it has.
/// A turn's id, as the dashboard names it: 1 to 64 letters, digits, `-` or `_`.
fn valid_turn_id(id: &str) -> Result<(), AssistantError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(AssistantError::Invalid(
            "turn id: 1 to 64 letters, digits, - or _".into(),
        ));
    }
    Ok(())
}

fn over_budget(tool: &str, max: usize) -> Json {
    json!({ "error": moli_i18n::tr!("assistant.tools.limit", tool = tool, max = max) })
}

/// The same answer said twice in a row (seen once from a small model)
/// is said once, and a tool call written out instead of made
/// (`show({cards:[…]})`) is not read to the family.
fn once(text: &str) -> String {
    let stripped = strip_calls(text);
    let t = stripped.trim();
    for (i, _) in t.match_indices(['.', '!', '?']) {
        let (a, b) = t.split_at(i + 1);
        if a.trim() == b.trim() {
            return a.trim().to_owned();
        }
    }
    t.to_owned()
}

/// Room for the model's output in one step: its own words, and, when an
/// offered tool copies the request, the request itself (as many tokens as
/// characters: French and its JSON escaping fit). Without it a long request
/// is cut mid-call, the call is lost and the turn ends with nothing. It
/// depends on the tools offered, not on who answers: any tool that carries a
/// long text meets the same limit.
fn output_room(offered: &Json, question: &str) -> usize {
    let copies = offered.as_array().is_some_and(|list| {
        list.iter().any(|tool| {
            tool["function"]["name"]
                .as_str()
                .is_some_and(|name| COPIES_THE_REQUEST.contains(&name))
        })
    });
    if copies {
        WORDS + question.chars().count().min(MAX_MESSAGE_CHARS)
    } else {
        WORDS
    }
}

/// The conversation sent to the model: the last `MAX_MESSAGES` at most, each
/// one no longer than `MAX_MESSAGE_CHARS`. A message trimmed here is one the
/// model never sees the end of, so the trimming is said out loud — silence
/// about it buys a wrong answer that nothing afterwards can explain.
fn recent(messages: &[Message]) -> Vec<Json> {
    let skip = messages.len().saturating_sub(MAX_MESSAGES);
    messages
        .iter()
        .skip(skip)
        .map(|m| {
            let whole = m.content.chars().count();
            if whole > MAX_MESSAGE_CHARS {
                tracing::warn!(
                    role = ?m.role,
                    kept = MAX_MESSAGE_CHARS,
                    was = whole,
                    "message cut short before the model saw it"
                );
            }
            let content: String = m.content.chars().take(MAX_MESSAGE_CHARS).collect();
            json!({ "role": m.role, "content": content })
        })
        .collect()
}

/// What the family is told. `trim_filler` is cosmetic, so when it empties a
/// real answer and nothing was shown or done either, the model's own words
/// stand: saying « I did not understand » would hide them. Only silence is
/// unknown, and an answer cut short says so. Measured on 22 Oct. 2026: a lone
/// « Veux-tu... ? » was erased and reported as a failure to understand.
fn spoken_reply(whole: &str, filtered: String, run: &Run) -> String {
    if !filtered.is_empty() || run.end {
        return filtered;
    }
    if !run.cards.is_empty() || !run.actions.is_empty() {
        return moli_i18n::tr!("assistant.reply.done");
    }
    if !whole.trim().is_empty() {
        return whole.to_owned();
    }
    if run.cut {
        return moli_i18n::tr!("assistant.reply.too_long");
    }
    moli_i18n::tr!("assistant.reply.unknown")
}

/// Sentences that only point at the screen (« Voici… ») when cards are
/// there, and offers to do more (« Tu veux… ? »): the family reads less.
fn trim_filler(text: &str, cards: bool) -> String {
    trim_filler_with(text, cards, &word_list)
}

/// `trim_filler` with the lists of words from `list` (the house's language).
fn trim_filler_with(text: &str, cards: bool, list: &dyn Fn(&str) -> Vec<String>) -> String {
    // How a sentence starts when it is an offer or points at the screen.
    let offers = list("assistant.filter.offers");
    let pointers = list("assistant.filter.pointers");
    let mut kept = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        // A sentence ends on . ! ? followed by a space or the end: « 22.9 »
        // is a number, not two sentences.
        let mut chars = rest.char_indices().peekable();
        let mut end = rest.len();
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|(_, n)| *n);
            if matches!(c, '.' | '!' | '?') && next.is_none_or(char::is_whitespace) {
                end = i + c.len_utf8();
                break;
            }
        }
        let sentence = rest[..end].trim();
        rest = rest[end..].trim_start();
        let lower = sentence.to_lowercase();
        let offer = sentence.ends_with('?') && offers.iter().any(|o| lower.starts_with(o.as_str()));
        let pointer = cards && pointers.iter().any(|p| lower.starts_with(p.as_str()));
        if !offer && !pointer && !sentence.is_empty() {
            kept.push(sentence);
        }
    }
    kept.join(" ")
}

/// Reasoning models take other sampling settings.
fn is_reasoning(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model).to_lowercase();
    m.starts_with("gpt-5")
        || (m.starts_with('o') && m[1..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Anthropic's OpenAI-compatible endpoint rejects `temperature` on its recent
/// models — HTTP 400, "`temperature` is deprecated for this model" — while it
/// still requires `max_tokens`. Measured on 21 Oct 2026 against
/// `https://api.anthropic.com/v1` with `claude-haiku-5-5`.
fn takes_temperature(model: &str) -> bool {
    !model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_lowercase()
        .starts_with("claude")
}

fn strip_calls(text: &str) -> String {
    let mut out = text.to_owned();
    for name in ["show", "set", "get_device", "energy", "history"] {
        let mut from = 0;
        while let Some(found) = out[from..].find(name) {
            let start = from + found;
            // `show({…})`, `show [ … ]`, `show {…}`: the whole word, maybe a
            // space, then an opening bracket.
            let tail = &out[start + name.len()..];
            let open = start + name.len() + (tail.len() - tail.trim_start().len());
            let whole_word = !out[..start].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            if !whole_word || !out[open..].starts_with(['(', '[', '{']) {
                from = start + name.len();
                continue;
            }
            let mut depth = 0usize;
            let mut end = out.len();
            for (i, c) in out[open..].char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = open + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.replace_range(start..end, "");
            from = start;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cards a question obviously calls for, when the model picked none.
fn guess_cards(question: &str) -> Vec<Json> {
    guess_cards_with(question, &word_list)
}

/// `guess_cards` with the fragments from `list` (the house's language).
fn guess_cards_with(question: &str, list: &dyn Fn(&str) -> Vec<String>) -> Vec<Json> {
    let q = house::norm(question);
    // Fragments of the house's language, accents removed like `q`.
    let any = |key: &str| list(key).iter().any(|w| q.contains(w.as_str()));
    let mut cards = Vec::new();
    if any("assistant.guess.energy") {
        cards.push(json!({ "kind": "energy" }));
    }
    if any("assistant.guess.weather") {
        cards.push(json!({ "kind": "weather" }));
    }
    if any("assistant.guess.cameras") {
        cards.push(json!({ "kind": "cameras" }));
    }
    cards
}

const CARD_KINDS: [&str; 7] = [
    "device", "room", "energy", "weather", "remote", "cameras", "history",
];

/// Checks the cards the model picked: unknown devices, hidden ones and
/// made-up rooms are dropped (and the model is told).
fn show(hub: &Hub, args: &Json, layout: &Layout, run: &mut Run) -> Json {
    let mut ignored = Vec::new();
    for card in args["cards"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let kind = card["kind"].as_str().unwrap_or_default();
        if !CARD_KINDS.contains(&kind) {
            ignored.push(format!("kind {kind:?}"));
            continue;
        }
        let mut clean = json!({ "kind": kind });
        match kind {
            "device" | "remote" => {
                let id = card["id"].as_str().unwrap_or_default();
                if hub.device(&DeviceId::from(id)).is_none() || layout.hides(id) {
                    ignored.push(format!("device {id:?}"));
                    continue;
                }
                clean["id"] = json!(id);
            }
            "room" => {
                let room = card["room"].as_str().unwrap_or_default();
                let known = hub
                    .snapshot()
                    .devices
                    .iter()
                    .any(|d| layout.same_room(&layout.room_of(d), room));
                if !known {
                    ignored.push(format!("room {room:?}"));
                    continue;
                }
                clean["room"] = json!(layout.room(room));
            }
            "history" => {
                let point = card["point"].as_str().unwrap_or_default();
                let exists = PointId::from(point)
                    .split()
                    .and_then(|(d, key)| {
                        hub.device(&d)
                            .map(|v| v.device.points.iter().any(|p| &*p.key == key))
                    })
                    .unwrap_or(false);
                if !exists {
                    ignored.push(format!("point {point:?}"));
                    continue;
                }
                clean["point"] = json!(point);
                if let Some(hours) = card["hours"].as_f64() {
                    clean["hours"] = json!(hours.clamp(1.0, 24.0 * 31.0));
                }
            }
            _ => {}
        }
        if let Some(title) = card["title"].as_str() {
            clean["title"] = json!(title.chars().take(60).collect::<String>());
        }
        if run.cards.len() < MAX_CARDS && !run.cards.contains(&clean) {
            run.cards.push(clean);
        }
    }
    if ignored.is_empty() {
        json!({ "shown": run.cards.len() })
    } else {
        json!({ "shown": run.cards.len(), "ignored": ignored })
    }
}

/// The `ambiance` tool: its choices are Moli's ambiances.
fn ambiance_tool() -> Json {
    let ids: Vec<&str> = moli_ambiance::AMBIANCES.iter().map(|a| a.id).collect();
    json!({
        "type": "function",
        "function": {
            "name": "ambiance",
            "description": moli_i18n::tr!("assistant.tools.ambiance.description", ambiances = ids.join(", ")),
            "parameters": {
                "type": "object",
                "properties": {
                    "lights": { "type": "array", "items": { "type": "string" }, "description": moli_i18n::tr!("assistant.tools.ambiance.lights") },
                    "ambiance": { "type": "string", "enum": ids },
                    "color": { "type": "string", "description": moli_i18n::tr!("assistant.tools.ambiance.color") },
                    "white": { "type": "integer", "description": moli_i18n::tr!("assistant.tools.ambiance.white") }
                },
                "required": ["lights"]
            }
        }
    })
}

/// The weather to come: offered when the house's location is known.
fn forecast_tool() -> Json {
    json!({
        "type": "function",
        "function": {
            "name": "forecast",
            "description": moli_i18n::tr!("assistant.tools.forecast.description"),
            "parameters": {
                "type": "object",
                "properties": {
                    "days": { "type": "integer", "minimum": 1, "maximum": 7, "description": moli_i18n::tr!("assistant.tools.forecast.days") }
                },
                "required": []
            }
        }
    })
}

/// The world outside the house, from the web.
fn search_tool() -> Json {
    json!({
        "type": "function",
        "function": {
            "name": "web_search",
            "description": moli_i18n::tr!("assistant.tools.search.description"),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": moli_i18n::tr!("assistant.tools.search.query") }
                },
                "required": ["query"]
            }
        }
    })
}

/// Offered in spoken turns only: the person may end the conversation in
/// any words (« c'est bon, j'ai fini », « tu peux arrêter »).
fn end_tool() -> Json {
    json!({
        "type": "function",
        "function": {
            "name": "end_conversation",
            "description": moli_i18n::tr!("assistant.tools.end.description"),
            "parameters": {
                "type": "object",
                "properties": {
                    "say": { "type": "string", "description": moli_i18n::tr!("assistant.tools.end.say") }
                },
                "required": []
            }
        }
    })
}

fn tools() -> Json {
    json!([
        {
            "type": "function",
            "function": {
                "name": "automation",
                "description": moli_i18n::tr!("assistant.tools.automation.description"),
                "parameters": {
                    "type": "object",
                    "properties": { "request": { "type": "string" } },
                    "required": ["request"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "show",
                "description": moli_i18n::tr!("assistant.tools.show.description"),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cards": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "kind": { "type": "string", "enum": CARD_KINDS },
                                    "id": { "type": "string", "description": moli_i18n::tr!("assistant.tools.show.id") },
                                    "room": { "type": "string", "description": moli_i18n::tr!("assistant.tools.show.room") },
                                    "point": { "type": "string", "description": moli_i18n::tr!("assistant.tools.show.point") },
                                    "hours": { "type": "number", "description": moli_i18n::tr!("assistant.tools.show.hours") },
                                    "title": { "type": "string", "description": moli_i18n::tr!("assistant.tools.show.title") }
                                },
                                "required": ["kind"]
                            }
                        }
                    },
                    "required": ["cards"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "set",
                "description": moli_i18n::tr!("assistant.tools.set.description"),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "point": { "type": "string", "description": moli_i18n::tr!("assistant.tools.set.point") },
                        "value": { "anyOf": [{ "type": "boolean" }, { "type": "number" }, { "type": "string" }] },
                        "say": { "type": "string", "description": moli_i18n::tr!("assistant.tools.set.say") }
                    },
                    "required": ["point", "value", "say"]
                }
            }
        },
        ambiance_tool(),
        {
            "type": "function",
            "function": {
                "name": "get_device",
                "description": moli_i18n::tr!("assistant.tools.get_device.description"),
                "parameters": {
                    "type": "object",
                    "properties": { "device": { "type": "string" } },
                    "required": ["device"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "energy",
                "description": moli_i18n::tr!("assistant.tools.energy.description"),
                "parameters": {
                    "type": "object",
                    "properties": { "step": { "type": "string", "enum": ["hour", "day", "month"] } }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "history",
                "description": moli_i18n::tr!("assistant.tools.history.description"),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "point": { "type": "string", "description": moli_i18n::tr!("assistant.tools.history.point") },
                        "hours": { "type": "number" }
                    },
                    "required": ["point"]
                }
            }
        }
    ])
}

/// The date the model is told, in the house's language.
fn house_date(now: &jiff::Zoned) -> String {
    date_in(&moli_i18n::language(), now)
}

/// A date and time as people say them in `language` (French when the
/// language has no date of its own yet, like the words themselves).
fn date_in(language: &str, now: &jiff::Zoned) -> String {
    match language {
        "en" => english_date(now),
        _ => french_date(now),
    }
}

fn english_date(now: &jiff::Zoned) -> String {
    const DAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let day = DAYS[usize::try_from(now.weekday().to_monday_zero_offset()).unwrap_or(0)];
    let month = MONTHS[usize::try_from(now.month() - 1).unwrap_or(0)];
    format!(
        "{day} {} {month} {}, {:02}:{:02}",
        now.day(),
        now.year(),
        now.hour(),
        now.minute()
    )
}

/// The recap's day: « samedi 10 octobre » in French, the date otherwise.
fn recap_day(now: &jiff::Zoned) -> String {
    if moli_i18n::language().starts_with("fr") {
        french_date(now)
            .split(',')
            .next()
            .unwrap_or_default()
            .trim_end_matches(char::is_numeric)
            .trim()
            .to_owned()
    } else {
        now.strftime("%A %-d %B").to_string()
    }
}

fn french_date(now: &jiff::Zoned) -> String {
    const DAYS: [&str; 7] = [
        "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
    ];
    const MONTHS: [&str; 12] = [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ];
    let day = DAYS[usize::try_from(now.weekday().to_monday_zero_offset()).unwrap_or(0)];
    let month = MONTHS[usize::try_from(now.month() - 1).unwrap_or(0)];
    format!(
        "{day} {} {month} {}, {:02}h{:02}",
        now.day(),
        now.year(),
        now.hour(),
        now.minute()
    )
}

/// One slot of `sem`, and one of `max` uses per `window` (keeping `spare`).
fn admit_window<'a>(
    sem: &'a tokio::sync::Semaphore,
    recent: &Mutex<VecDeque<Instant>>,
    max: usize,
    window: Duration,
    spare: usize,
) -> Result<tokio::sync::SemaphorePermit<'a>, AssistantError> {
    let permit = sem.try_acquire().map_err(|_| AssistantError::Busy)?;
    let mut recent = recent.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    while recent
        .front()
        .is_some_and(|t| now.duration_since(*t) > window)
    {
        recent.pop_front();
    }
    if recent.len() + spare >= max {
        return Err(AssistantError::Busy);
    }
    recent.push_back(now);
    Ok(permit)
}

/// The system prompt, with the spoken style when the question was spoken.
fn with_voice(prompt: String, spoken: bool) -> String {
    if spoken {
        prompt + &moli_i18n::tr!("assistant.prompt.spoken")
    } else {
        prompt
    }
}

/// A catalogue entry that is a list of words, `|` between them (the words
/// the safety nets look for in the house's language).
fn word_list(key: &str) -> Vec<String> {
    moli_i18n::tr(key)
        .split('|')
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The guard's quiet hours, as the prompt says them.
fn quiet_text(from: &str, to: &str, active: bool) -> String {
    if active {
        moli_i18n::tr!("assistant.prompt.quiet_now", from = from, to = to)
    } else {
        moli_i18n::tr!("assistant.prompt.quiet", from = from, to = to)
    }
}

/// What the conversation's system prompt is made of.
struct ChatPrompt<'a> {
    title: Option<&'a str>,
    page: bool,
    protected: &'a str,
    quiet: &'a str,
    date: &'a str,
    power: &'a str,
    rooms: &'a str,
    inventory: &'a str,
}

impl ChatPrompt<'_> {
    /// The words to fill in, the house's own text (its rooms, its inventory)
    /// last: a device named like a placeholder never gets filled in by the
    /// words after it.
    fn words(&self) -> Vec<(&'static str, String)> {
        let title = self
            .title
            .map(|t| moli_i18n::tr!("assistant.prompt.title", title = t))
            .unwrap_or_default();
        let cards = moli_i18n::tr(if self.page {
            "assistant.prompt.cards_page"
        } else {
            "assistant.prompt.cards_bubble"
        });
        vec![
            ("cards", cards),
            ("quiet", self.quiet.to_owned()),
            ("date", self.date.to_owned()),
            ("title", title),
            ("protected", self.protected.to_owned()),
            ("power", self.power.to_owned()),
            ("rooms", self.rooms.to_owned()),
            ("inventory", self.inventory.to_owned()),
        ]
    }

    fn render(&self) -> String {
        let words = self.words();
        let args: Vec<(&str, &dyn Display)> = words
            .iter()
            .map(|(name, value)| (*name, value as &dyn Display))
            .collect();
        moli_i18n::trf("assistant.prompt.chat", &args)
    }
}

/// The live electricity line of the prompt: power right now, circuit by
/// circuit, and the figures of the days.
fn power_line(s: &moli_energy::Summary) -> String {
    let watts = |w: f64| format!("{} W", w.round());
    let total = s
        .meters
        .iter()
        .find(|m| m.role == moli_energy::Role::Total)
        .and_then(|m| m.power_w);
    let mut circuits: Vec<(String, f64)> = s
        .meters
        .iter()
        .filter(|m| m.role == moli_energy::Role::Circuit)
        .filter_map(|m| m.power_w.map(|w| (m.name.clone(), w.max(0.0))))
        .collect();
    circuits.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut line = if let Some(total) = total {
        let rest = (total - circuits.iter().map(|c| c.1).sum::<f64>()).max(0.0);
        let mut line = moli_i18n::tr!("assistant.power.total", watts = watts(total));
        if !circuits.is_empty() {
            let parts: Vec<String> = circuits
                .iter()
                .map(|(n, w)| format!("{n} {}", watts(*w)))
                .collect();
            line.push_str(&moli_i18n::tr!(
                "assistant.power.circuits",
                rest = watts(rest),
                circuits = parts.join(", ")
            ));
        }
        line
    } else {
        moli_i18n::tr!("assistant.power.unknown")
    };
    let appliances: Vec<String> = s
        .meters
        .iter()
        .filter(|m| m.role == moli_energy::Role::Appliance)
        .filter_map(|m| {
            m.power_w.map(|w| {
                // An estimated light says so (« ≈ »).
                let about = if m.estimated { "≈ " } else { "" };
                format!("{} {about}{}", m.name, watts(w.max(0.0)))
            })
        })
        .collect();
    if !appliances.is_empty() {
        line.push_str(&moli_i18n::tr!(
            "assistant.power.appliances",
            appliances = appliances.join(", ")
        ));
    }
    if let Some(live) = &s.live
        && let Some(v) = live.value
    {
        let unit = live.unit.as_deref().unwrap_or("W");
        line.push_str(&moli_i18n::tr!(
            "assistant.power.grid",
            unit = unit,
            value = v.round(),
            max = live.max.round()
        ));
    }
    if let Some(period) = &s.period {
        let hc = period.to_lowercase().contains("hc");
        line.push_str(&moli_i18n::tr(if hc {
            "assistant.power.tariff_off_peak"
        } else {
            "assistant.power.tariff_peak"
        }));
        if let Some(price) = s.price_now {
            line.push_str(&moli_i18n::tr!(
                "assistant.power.price",
                price = format!("{price:.3}"),
                currency = s.currency
            ));
        }
    }
    let cost = |a: &moli_energy::Amount| {
        a.cost
            .map(|c| {
                moli_i18n::tr!(
                    "assistant.power.cost",
                    cost = format!("{c:.2}"),
                    currency = s.currency
                )
            })
            .unwrap_or_default()
    };
    line.push_str(&moli_i18n::tr!(
        "assistant.power.days",
        today = format!("{:.1}", s.today.total.kwh),
        today_cost = cost(&s.today.total),
        yesterday = format!("{:.1}", s.yesterday.total.kwh),
        yesterday_cost = cost(&s.yesterday.total),
        month = format!("{:.0}", s.month.total.kwh),
        month_cost = cost(&s.month.total)
    ));
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spoken_turn_asks_for_spoken_words() {
        let prompt = with_voice("Tu es Moli.".into(), true);
        assert!(prompt.starts_with("Tu es Moli."));
        assert!(prompt.contains("à voix haute"));
        assert_eq!(with_voice("Tu es Moli.".into(), false), "Tu es Moli.");
    }

    #[test]
    fn the_voice_window_is_its_own() {
        let sem = tokio::sync::Semaphore::new(1);
        let recent = Mutex::new(VecDeque::new());
        let held = admit_window(&sem, &recent, 2, TURN_WINDOW, 0).unwrap();
        assert!(matches!(
            admit_window(&sem, &recent, 2, TURN_WINDOW, 0),
            Err(AssistantError::Busy)
        ));
        drop(held);
        let second = admit_window(&sem, &recent, 2, TURN_WINDOW, 0).unwrap();
        drop(second);
        assert!(matches!(
            admit_window(&sem, &recent, 2, TURN_WINDOW, 0),
            Err(AssistantError::Busy)
        ));
    }

    #[test]
    fn old_configs_still_load() {
        let c: Config = toml::from_str("model = \"gpt-4.1-mini\"").unwrap();
        assert_eq!(c.speech_model, "gpt-4o-mini-tts");
        assert!((c.speech_speed - 1.2).abs() < f64::EPSILON);
        assert_eq!(c.local_speech, "127.0.0.1:10200");
        assert_eq!(c.local_listen, "127.0.0.1:10300");
        assert!(
            c.receipts_model.is_empty(),
            "receipts: the conversation's model"
        );
        assert!(Config::default().receipts_model.is_empty());
    }

    #[test]
    fn a_voice_pace_out_of_range_is_refused() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let config: Config = toml::from_str("speech_speed = 9.0").unwrap();
        let err = Assistant::new(hub, None, None, None, config).unwrap_err();
        assert!(err.to_string().contains("speech_speed"), "{err}");
    }

    /// The fake provider answers once: a second round trip would fail the turn.
    #[tokio::test]
    async fn cards_and_words_together_end_the_turn() {
        let answer = br#"{"choices":[{"message":{"role":"assistant",
            "content":"Il fait vingt-trois degres dehors.",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"show",
            "arguments":"{\"cards\":[{\"kind\":\"weather\"}]}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let (url, _task) = crate::llm::tests::fake_http(200, answer).await;
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        let turn: Turn = serde_json::from_value(json!({
            "spoken": true,
            "messages": [{ "role": "user", "content": "Quel temps fait-il ?" }],
        }))
        .unwrap();
        let reply = moli.turn(turn).await.unwrap();
        assert_eq!(reply.usage.steps, 1);
        assert_eq!(reply.reply, "Il fait vingt-trois degres dehors.");
        assert_eq!(reply.cards.len(), 1);
        assert_eq!(reply.cards[0]["kind"], "weather");
    }

    /// A lamp that does what it is told (or that nobody reaches: `offline`).
    pub(crate) struct Lamp {
        pub(crate) offline: bool,
    }

    impl moli_runtime::Driver for Lamp {
        fn kind(&self) -> &'static str {
            "fake"
        }

        fn run<'a>(
            &'a self,
            ctx: &'a mut moli_runtime::DriverCtx,
        ) -> moli_runtime::BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                let id = ctx.device_id("lamp");
                ctx.upsert_device(moli_core::Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: "lamp".into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    members: Vec::new(),
                    points: vec![moli_core::PointSpec {
                        key: "on".into(),
                        label: "On".into(),
                        kind: moli_core::Kind::Binary,
                        access: moli_core::Access {
                            read: true,
                            write: true,
                        },
                        unit: None,
                        semantic: moli_core::Semantic::OnOff,
                    }],
                });
                ctx.set_state(&id, "on", Value::Bool(false));
                if self.offline {
                    ctx.set_availability(&id, false);
                }
                ctx.ready();
                while let Some(cmd) = ctx.next_command().await {
                    ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                    cmd.reply(Ok(()));
                }
                Ok(())
            })
        }
    }

    /// A hub with the lamp running.
    pub(crate) async fn lamp_hub() -> moli_runtime::Hub {
        lamp_hub_with(false).await
    }

    pub(crate) async fn lamp_hub_with(offline: bool) -> moli_runtime::Hub {
        lamp_hub_guarded(offline, moli_runtime::guard::GuardPolicy::default()).await
    }

    /// The lamp under a guard policy (`protect_unassigned`: it has no room, so nobody acts on it unattended).
    pub(crate) async fn lamp_hub_guarded(
        offline: bool,
        guard: moli_runtime::guard::GuardPolicy,
    ) -> moli_runtime::Hub {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions {
            guard,
            ..moli_runtime::HubOptions::default()
        })
        .unwrap();
        moli_runtime::spawn_driver(
            &hub,
            InstanceId::from("fake"),
            Arc::new(Lamp { offline }),
            tokio_util::sync::CancellationToken::new(),
        );
        for _ in 0..200 {
            if hub.stats().drivers_running == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        hub
    }

    /// Moli over a hub with the lamp running, its provider answering `answer` once.
    async fn moli_with_lamp(answer: &'static [u8]) -> Assistant {
        let (url, _task) = crate::llm::tests::fake_http(200, answer).await;
        let hub = lamp_hub().await;
        let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
        Assistant::new(hub, None, None, None, config).unwrap()
    }

    /// A provider that takes every question and never answers.
    async fn silent_provider() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        url
    }

    /// A question from the dashboard, its turn named `id`.
    fn asked_as(id: &str, question: &str) -> Turn {
        serde_json::from_value(json!({
            "id": id,
            "surface": "bubble",
            "messages": [{ "role": "user", "content": question }],
        }))
        .unwrap()
    }

    /// Whether a turn runs under `id`, without stopping it.
    fn listed(moli: &Assistant, id: &str) -> bool {
        moli.0.stoppable.lock().unwrap().contains_key(id)
    }

    async fn until_listed(moli: &Assistant, id: &str) {
        for _ in 0..400 {
            if listed(moli, id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("turn {id} never became stoppable");
    }

    #[tokio::test]
    async fn a_turn_is_stopped_by_its_own_id_only() {
        let config: Config =
            serde_json::from_value(json!({ "base_url": silent_provider().await })).unwrap();
        let moli = Assistant::new(lamp_hub().await, None, None, None, config).unwrap();
        let ask = |id: &'static str| {
            let moli = moli.clone();
            tokio::spawn(async move { moli.turn(asked_as(id, "allume la lampe")).await })
        };
        let (a, b) = (ask("a"), ask("b"));
        until_listed(&moli, "a").await;
        until_listed(&moli, "b").await;
        assert!(!moli.stop("c"), "nothing runs under that id");

        assert!(moli.stop("a"));
        let reply = tokio::time::timeout(Duration::from_secs(5), a)
            .await
            .expect("a stopped turn ends at once")
            .unwrap()
            .unwrap();
        assert!(reply.interrupted);
        assert!(reply.reply.is_empty());
        assert!(!listed(&moli, "a"), "the turn took its entry with it");
        assert!(!moli.stop("a"));

        // The other turn was not touched: still waiting, still stoppable.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!b.is_finished());
        assert!(moli.stop("b"));
        let reply = tokio::time::timeout(Duration::from_secs(5), b)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(reply.interrupted);
        assert!(moli.0.stoppable.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_turn_id_is_checked_and_never_shared() {
        let config: Config =
            serde_json::from_value(json!({ "base_url": silent_provider().await })).unwrap();
        let moli = Assistant::new(lamp_hub().await, None, None, None, config).unwrap();
        let err = moli.turn(asked_as("a b", "?")).await.unwrap_err();
        assert!(matches!(err, AssistantError::Invalid(_)), "{err}");

        let first = {
            let moli = moli.clone();
            tokio::spawn(async move { moli.turn(asked_as("x", "?")).await })
        };
        until_listed(&moli, "x").await;
        let err = moli.turn(asked_as("x", "?")).await.unwrap_err();
        assert!(matches!(err, AssistantError::Invalid(_)), "{err}");
        // Refused, the second turn did not take the first one's entry away.
        assert!(listed(&moli, "x"));
        assert!(moli.stop("x"));
        let reply = tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(reply.interrupted);
    }

    fn spoken(question: &str) -> Turn {
        serde_json::from_value(json!({
            "spoken": true,
            "messages": [{ "role": "user", "content": question }],
        }))
        .unwrap()
    }

    /// A fake device: its name, its values, what they are now.
    pub(crate) type FakeDevice = (
        &'static str,
        Vec<moli_core::PointSpec>,
        Vec<(&'static str, Value)>,
    );

    /// Devices of the `fake` instance that do what they are told.
    pub(crate) struct Fake(pub(crate) Vec<FakeDevice>);

    impl moli_runtime::Driver for Fake {
        fn kind(&self) -> &'static str {
            "fake"
        }

        fn run<'a>(
            &'a self,
            ctx: &'a mut moli_runtime::DriverCtx,
        ) -> moli_runtime::BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                for (name, points, values) in &self.0 {
                    let id = ctx.device_id(name);
                    ctx.upsert_device(moli_core::Device {
                        id: id.clone(),
                        instance: ctx.instance().clone(),
                        native_name: (*name).into(),
                        manufacturer: None,
                        model: None,
                        description: None,
                        native_room: None,
                        members: Vec::new(),
                        points: points.clone(),
                    });
                    for (key, value) in values {
                        ctx.set_state(&id, key, value.clone());
                    }
                }
                ctx.ready();
                while let Some(cmd) = ctx.next_command().await {
                    ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                    cmd.reply(Ok(()));
                }
                Ok(())
            })
        }
    }

    /// A value of a fake device: read, and written when `write`.
    pub(crate) fn point(key: &str, kind: moli_core::Kind, write: bool) -> moli_core::PointSpec {
        moli_core::PointSpec {
            key: key.into(),
            label: key.into(),
            kind,
            access: moli_core::Access { read: true, write },
            unit: None,
            semantic: moli_core::Semantic::Other,
        }
    }

    /// A hub with the fake devices running.
    pub(crate) async fn hub_with(fake: Fake) -> moli_runtime::Hub {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        moli_runtime::spawn_driver(
            &hub,
            InstanceId::from("fake"),
            Arc::new(fake),
            tokio_util::sync::CancellationToken::new(),
        );
        for _ in 0..200 {
            if hub.stats().drivers_running == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        hub
    }

    /// Where someone is to the metre and the Wi-Fi they are on never reach
    /// the model: not in the prompt's inventory, not from `get_device`, not
    /// from `history`. Whether they are home and their zone do.
    #[tokio::test]
    async fn where_someone_is_to_the_metre_never_reaches_the_model() {
        let number = || moli_core::Kind::Numeric {
            min: None,
            max: None,
            step: None,
        };
        let hub = hub_with(Fake(vec![(
            "phone",
            vec![
                point("home", moli_core::Kind::Binary, false),
                point("zone", moli_core::Kind::Text, false),
                point("latitude", number(), false),
                point("longitude", number(), false),
                point("accuracy", number(), false),
                point("wifi", moli_core::Kind::Text, false),
                point("battery", number(), false),
            ],
            vec![
                ("home", Value::Bool(false)),
                ("zone", Value::Text("Travail".into())),
                ("latitude", Value::Float(43.219_87)),
                ("longitude", Value::Float(7.654_32)),
                ("accuracy", Value::Float(12.0)),
                ("wifi", Value::Text("Box-1234".into())),
                ("battery", Value::Float(81.0)),
            ],
        )]))
        .await;
        let dir = std::env::temp_dir().join(format!("moli-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = moli_history::Store::open(&dir.join("history.db")).unwrap();
        let at = moli_core::now_ms() - 60_000;
        store
            .insert(
                &[
                    (
                        PointId::from("fake:phone/latitude"),
                        Value::Float(43.219_87),
                        at,
                    ),
                    (
                        PointId::from("fake:phone/zone"),
                        Value::Text("Travail".into()),
                        at,
                    ),
                ],
                false,
            )
            .unwrap();
        let history = moli_history::History::from_store(Arc::new(store));
        let moli = Assistant::new(hub, None, Some(history), None, Config::default()).unwrap();
        let hidden = [
            "latitude",
            "longitude",
            "accuracy",
            "wifi",
            "43.2",
            "7.65",
            "Box-1234",
        ];

        let layout = Layout::default();
        let prompt = moli.system_prompt(&layout, false, "");
        let device = moli
            .get_device(&json!({ "device": "fake:phone" }), &layout)
            .to_string();
        for read in [&prompt, &device] {
            for word in hidden {
                assert!(!read.contains(word), "{word} in {read}");
            }
            for word in ["home", "Travail", "battery"] {
                assert!(read.contains(word), "{word} not in {read}");
            }
        }

        let past = moli
            .history(&json!({ "point": "fake:phone/latitude", "hours": 2 }))
            .await
            .to_string();
        assert!(!past.contains("43.2") && past.contains("error"), "{past}");
        let zone = moli
            .history(&json!({ "point": "fake:phone/zone", "hours": 2 }))
            .await
            .to_string();
        assert!(zone.contains("Travail"), "{zone}");
        drop(moli);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_output_room_follows_the_tools_offered() {
        let long = "allume la lampe du salon quand il fait nuit, ".repeat(40);
        assert!(long.chars().count() > 1_500);
        // `automation` is offered: the request may have to be written out again.
        assert_eq!(output_room(&tools(), "allume"), WORDS + 6);
        assert_eq!(output_room(&tools(), &long), WORDS + long.chars().count());
        let huge = "x".repeat(10 * MAX_MESSAGE_CHARS);
        assert_eq!(output_room(&tools(), &huge), WORDS + MAX_MESSAGE_CHARS);
        // Nothing offered copies it: the words alone.
        let mut without = tools();
        without
            .as_array_mut()
            .unwrap()
            .retain(|t| t["function"]["name"] != "automation");
        assert_eq!(output_room(&without, &long), WORDS);
    }

    /// The ceiling the provider is sent: room for a long request, measured on
    /// the request itself, not a constant that a long one outgrows.
    #[tokio::test]
    async fn a_long_request_leaves_room_to_be_written_out() {
        let answer =
            json!({ "choices": [{ "message": { "role": "assistant", "content": "D'accord." } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 } })
            .to_string();
        let (url, asked) = crate::llm::tests::fake_provider(vec![answer]).await;
        let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
        let moli = Assistant::new(lamp_hub().await, None, None, None, config).unwrap();
        let long =
            "quand quelqu'un rentre le soir et qu'il fait nuit, allume l'entrée puis le salon ; "
                .repeat(20);
        let turn: Turn = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": long }],
        }))
        .unwrap();
        moli.turn(turn).await.unwrap();
        let asked = asked.lock().unwrap().clone();
        let body: Json = serde_json::from_str(asked[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body["max_tokens"],
            json!(WORDS + long.chars().count().min(MAX_MESSAGE_CHARS))
        );
        assert!(body["max_tokens"].as_u64().unwrap() > 1_500);
    }

    /// Whatever the model asks in a turn, two web searches and one draft at
    /// most are paid for; the calls beyond hear why and the turn goes on.
    /// What the draft cost counts in the exchange.
    #[tokio::test]
    async fn a_turn_pays_for_two_searches_and_one_draft_at_most() {
        let call = |id: &str, name: &str, args: Json| {
            json!({ "id": id, "type": "function",
                "function": { "name": name, "arguments": args.to_string() } })
        };
        let search = |id| call(id, "web_search", json!({ "query": "la météo" }));
        let draft = |id| call(id, "automation", json!({ "request": "dis bonjour" }));
        let chat = |message: Json, prompt: u64| {
            json!({ "choices": [{ "message": message }],
                "usage": { "prompt_tokens": prompt, "completion_tokens": 10 } })
            .to_string()
        };
        let found = json!({ "output": [{ "type": "message", "content": [
            { "type": "output_text", "text": "Du soleil." }] }],
            "usage": { "input_tokens": 10, "output_tokens": 5 } })
        .to_string();
        let drafted = json!({ "name": "Bonjour", "mode": "restart", "note": null,
            "nodes": [{ "id": "t1", "type": "manual" },
                { "id": "a1", "type": "notify", "message": "Bonjour", "channels": ["maison"] }],
            "edges": [{ "from": "t1", "port": "out", "to": "a1" }] });
        let (url, asked) = crate::llm::tests::fake_provider(vec![
            chat(
                json!({ "role": "assistant", "content": null, "tool_calls": [
                    search("s1"), search("s2"), search("s3"), draft("a1"), draft("a2")] }),
                100,
            ),
            found.clone(),
            found,
            chat(json!({ "content": drafted.to_string() }), 1_000),
            chat(json!({ "role": "assistant", "content": "Du soleil." }), 200),
        ])
        .await;
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let (autos, _engine) = moli_automation::Automations::start(
            &hub,
            moli_automation::Config::default(),
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        moli.set_automations(autos.clone());
        let turn: Turn = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Il fera beau ? Et dis bonjour le matin." }],
        }))
        .unwrap();
        let reply = moli.turn(turn).await.unwrap();
        assert_eq!(reply.reply, "Du soleil.");

        let asked = asked.lock().unwrap().clone();
        let paid = |path: &str| asked.iter().filter(|r| r.contains(path)).count();
        assert_eq!(asked.len(), 5, "{asked:#?}");
        assert_eq!(paid("POST /v1/responses "), 2);
        assert_eq!(paid("json_schema"), 1);
        assert_eq!(autos.list().len(), 1);
        // The model is told, in the house's words.
        for (tool, max) in [("web_search", MAX_SEARCHES), ("automation", MAX_DRAFTS)] {
            let said = moli_i18n::tr!("assistant.tools.limit", tool = tool, max = max);
            assert!(asked[4].contains(&said), "{said}");
        }
        let search = pricing::SEARCH + pricing::think("gpt-4.1-mini", 10, 0, 5);
        assert!((reply.search_cost - 2.0 * search).abs() < 1e-12);
        let think =
            pricing::think("gpt-4.1-mini", 300, 0, 20) + pricing::think("gpt-4.1", 1_000, 0, 10);
        let kept = &moli.exchanges(1)[0];
        assert!((kept.cost.think - think).abs() < 1e-12, "{:?}", kept.cost);
    }

    /// The key in the vault never goes to a model reached in plain http
    /// (a local one, or `base_url` switched back to it).
    #[tokio::test]
    async fn the_stored_key_never_goes_to_a_model_in_clear() {
        let dir = std::env::temp_dir().join(format!("moli-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions {
            secrets: Some((
                dir.join("secrets.json"),
                moli_runtime::MasterKey::generate().unwrap(),
            )),
            ..moli_runtime::HubOptions::default()
        })
        .unwrap();
        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz";
        hub.store_secret(&InstanceId::from("assistant"), "api_key", key)
            .await
            .unwrap();
        let (url, task) = crate::llm::tests::fake_http(
            200,
            br#"{"choices":[{"message":{"role":"assistant","content":"Oui."}}]}"#,
        )
        .await;
        let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        let turn: Turn = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Bonjour" }],
        }))
        .unwrap();
        assert_eq!(moli.turn(turn).await.unwrap().reply, "Oui.");
        let request = task.await.unwrap();
        assert!(!request.contains(key), "{request}");
        assert!(
            !request.to_ascii_lowercase().contains("authorization"),
            "{request}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Moli running for weeks, nothing new recorded: a recording older than
    /// a week is not served, and is gone within the hour.
    #[tokio::test(start_paused = true)]
    async fn old_recordings_go_while_moli_runs() {
        let dir = std::env::temp_dir().join(format!("moli-recordings-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let moli = Assistant::new(hub, None, None, None, Config::default()).unwrap();
        moli.open_recordings(dir.clone());
        // Started a while ago: the first hourly pass is behind.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let now = jiff::Timestamp::now().as_millisecond();
        let (old, recent) = (
            format!("{}-1.wav", now - 8 * 24 * 3_600_000),
            format!("{}-2.wav", now - 24 * 3_600_000),
        );
        for name in [&old, &recent] {
            std::fs::write(dir.join(name), b"RIFF").unwrap();
        }
        assert!(moli.recording(&old).is_none(), "never served");
        assert!(moli.recording(&recent).is_some());
        tokio::time::sleep(recordings::EXPIRE_EVERY).await;
        assert!(!dir.join(&old).exists(), "gone within the hour");
        assert!(dir.join(&recent).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The provider answers once: a second round trip would fail the turn.
    #[tokio::test]
    async fn an_order_carried_out_ends_the_turn_with_its_words() {
        let moli = moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant",
            "content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:lamp/on\",\"value\":true,\"say\":\"C'est allume.\"}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .await;
        let reply = moli.turn(spoken("Allume la lampe")).await.unwrap();
        assert_eq!(reply.usage.steps, 1);
        assert_eq!(reply.reply, "C'est allume.");
        assert_eq!(reply.actions.len(), 1);
        assert_eq!(reply.actions[0].status, "done");
    }

    /// An order that failed is not announced as done: the model hears the
    /// result, a second round trip (which the one-answer provider fails).
    #[tokio::test]
    async fn an_order_that_failed_goes_back_to_the_model() {
        let moli = moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant",
            "content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:nothing/on\",\"value\":true,\"say\":\"C'est allume.\"}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .await;
        assert!(moli.turn(spoken("Allume le rien")).await.is_err());
    }

    /// « Bon, on arrête là » or any other words: the conversation ends,
    /// nothing to say (the satellite's chime marks it).
    #[tokio::test]
    async fn ending_the_conversation_needs_no_words() {
        let moli = moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"end_conversation",
            "arguments":"{}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .await;
        let reply = moli.turn(spoken("Bon, on arrête là")).await.unwrap();
        assert!(reply.end);
        assert_eq!(reply.reply, "");
        assert_eq!(reply.usage.steps, 1);
    }

    #[tokio::test]
    async fn only_a_spoken_turn_can_end_the_conversation() {
        let answer = br#"{"choices":[{"message":{"role":"assistant","content":"Oui."}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        for spoken in [true, false] {
            let (url, task) = crate::llm::tests::fake_http(200, answer).await;
            let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
            let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
            let moli = Assistant::new(hub, None, None, None, config).unwrap();
            let turn: Turn = serde_json::from_value(json!({
                "spoken": spoken,
                "messages": [{ "role": "user", "content": "Bonjour" }],
            }))
            .unwrap();
            let reply = moli.turn(turn).await.unwrap();
            assert!(!reply.end);
            let request = task.await.unwrap();
            assert_eq!(request.contains("end_conversation"), spoken);
        }
    }

    /// Two orders, the same words in each: said once.
    #[tokio::test]
    async fn the_words_of_several_orders_are_said_once() {
        let moli = moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant",
            "content":null,
            "tool_calls":[
            {"id":"c1","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:lamp/on\",\"value\":true,\"say\":\"C'est allume.\"}"}},
            {"id":"c2","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:lamp/on\",\"value\":false,\"say\":\"C'est allume.\"}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .await;
        let reply = moli.turn(spoken("Allume la lampe")).await.unwrap();
        assert_eq!(reply.usage.steps, 1);
        assert_eq!(reply.reply, "C'est allume.");
        assert_eq!(reply.actions.len(), 2);
    }

    #[test]
    fn dates_read_in_french() {
        let z: jiff::Zoned = "2026-10-03T21:05:00+02:00[Europe/Paris]".parse().unwrap();
        assert_eq!(french_date(&z), "samedi 3 octobre 2026, 21h05");
    }

    #[test]
    fn a_repeated_answer_is_said_once() {
        assert_eq!(
            once("Il fait 23 °C. Ciel couvert. Il fait 23 °C. Ciel couvert."),
            "Il fait 23 °C. Ciel couvert."
        );
        assert_eq!(once("Oui. Oui."), "Oui.");
        assert_eq!(
            once("Il fait 23 °C. show({cards:[{kind:\"weather\"}]})"),
            "Il fait 23 °C."
        );
        assert_eq!(once("show({cards:[]}"), "");
        assert_eq!(
            once("Personne n'a sonné. show [{\"id\":\"x\",\"kind\":\"device\"}]"),
            "Personne n'a sonné."
        );
        // A word that merely contains a tool's name stays.
        assert_eq!(
            once("Le reset (bouton) est fait."),
            "Le reset (bouton) est fait."
        );
        assert_eq!(
            once("C'est fait. Bonne soirée !"),
            "C'est fait. Bonne soirée !"
        );
    }

    #[test]
    fn filler_goes_answers_stay() {
        assert_eq!(
            trim_filler(
                "Ce mois-ci, 61 kWh pour 9,59 €. Tu veux voir autre chose ?",
                true
            ),
            "Ce mois-ci, 61 kWh pour 9,59 €."
        );
        assert_eq!(
            trim_filler("Personne dans la cour. Voici les images en direct.", true),
            "Personne dans la cour."
        );
        // Without cards, « voici » may be the answer itself.
        assert_eq!(trim_filler("Voici une blague.", false), "Voici une blague.");
        assert_eq!(
            trim_filler("Il fait 23 °C, non ?", false),
            "Il fait 23 °C, non ?"
        );
        assert_eq!(
            trim_filler("Il fait 22.9 °C. Veux-tu la météo ?", true),
            "Il fait 22.9 °C."
        );
    }

    #[test]
    fn reasoning_models_are_recognized() {
        assert!(is_reasoning("gpt-5-mini"));
        assert!(is_reasoning("o4-mini"));
        assert!(!is_reasoning("gpt-4.1-mini"));
        assert!(!is_reasoning("ollama"));
    }

    #[test]
    fn claude_models_take_no_temperature() {
        assert!(!takes_temperature("claude-haiku-5-5"));
        assert!(!takes_temperature("anthropic/claude-sonnet-5-5"));
        assert!(takes_temperature("gpt-4.1-mini"));
        assert!(takes_temperature("mistral-small-latest"));
        assert!(takes_temperature("ollama"));
    }

    #[test]
    fn obvious_questions_get_their_cards() {
        assert_eq!(
            guess_cards("Qu'est-ce qui consomme ?"),
            vec![json!({ "kind": "energy" })]
        );
        assert_eq!(
            guess_cards("Il pleut demain ?"),
            vec![json!({ "kind": "weather" })]
        );
        assert_eq!(
            guess_cards("Qui a sonné ?"),
            vec![json!({ "kind": "cameras" })]
        );
        assert!(guess_cards("Raconte une blague").is_empty());
    }

    #[test]
    fn tools_are_valid_json_schema_objects() {
        let tools = tools();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "automation",
                "show",
                "set",
                "ambiance",
                "get_device",
                "energy",
                "history"
            ]
        );
        // The ambiances offered are exactly Moli's.
        let offered: Vec<&str> =
            tools[3]["function"]["parameters"]["properties"]["ambiance"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
        let known: Vec<&str> = moli_ambiance::AMBIANCES.iter().map(|a| a.id).collect();
        assert_eq!(offered, known);
    }

    // ---- the words: French as it always was, English complete ---------------------------

    pub(crate) type Catalogue = std::collections::HashMap<String, String>;

    fn catalogue(json: &str) -> Catalogue {
        serde_json::from_str(json).unwrap()
    }

    pub(crate) fn french() -> Catalogue {
        catalogue(include_str!("../../../locales/fr/assistant.json"))
    }

    pub(crate) fn english() -> Catalogue {
        catalogue(include_str!("../../../locales/en/assistant.json"))
    }

    /// `{name}` placeholders of a text, sorted.
    fn placeholders(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            rest = &rest[open + 1..];
            if let Some(close) = rest.find('}')
                && close > 0
                && rest[..close]
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_')
            {
                found.push(rest[..close].to_owned());
            }
        }
        found.sort();
        found.dedup();
        found
    }

    /// A summary of the house's electricity, full or bare, for the lines
    /// that tell the model about it.
    pub(crate) fn sample_summary(full: bool) -> moli_energy::Summary {
        use moli_energy::{Amount, MeterInfo, Period, Role};
        let meter = |id: &str, name: &str, role, point: &str, power: Option<&str>, w| MeterInfo {
            id: id.into(),
            name: name.into(),
            role,
            point: point.into(),
            power: power.map(Into::into),
            power_w: w,
            power_ts: None,
            within: None,
            feeds: Vec::new(),
            estimated: false,
        };
        let period = |kwh, cost| Period {
            from: 0,
            to: 0,
            total: Amount { kwh, cost },
            meters: std::collections::BTreeMap::new(),
            unmeasured: None,
            production: None,
            export: None,
            solar: None,
        };
        let mut summary = moli_energy::Summary {
            currency: "€".into(),
            timezone: "Europe/Paris".into(),
            since: None,
            period: None,
            price_now: None,
            period_point: None,
            meters: vec![meter("g", "Linky", Role::Grid, "linky/index", None, None)],
            live: None,
            flows: None,
            today: period(0.0, None),
            yesterday: period(0.0, None),
            month: period(0.0, None),
            last_month: period(0.0, None),
            month_projection: None,
            monthly_fee: None,
            budget_point: None,
        };
        if full {
            let mut light = meter("l", "Suspension", Role::Appliance, "s/e", None, Some(12.0));
            light.estimated = true;
            summary.meters = vec![
                meter("t", "Maison", Role::Total, "t/e", Some("t/p"), Some(940.4)),
                meter(
                    "c1",
                    "Cuisine",
                    Role::Circuit,
                    "c/e",
                    Some("c/p"),
                    Some(300.2),
                ),
                meter(
                    "c2",
                    "Chauffe-eau",
                    Role::Circuit,
                    "w/e",
                    Some("w/p"),
                    Some(120.0),
                ),
                meter("c3", "Cave", Role::Circuit, "k/e", None, Some(-3.0)),
                meter(
                    "a",
                    "Lave-linge",
                    Role::Appliance,
                    "a/e",
                    Some("a/p"),
                    Some(85.0),
                ),
                light,
                meter("g", "Linky", Role::Grid, "linky/index", None, None),
            ];
            summary.live = Some(moli_energy::LiveInfo {
                point: "linky/papp".into(),
                max: 9000.0,
                warn: 2250.0,
                unit: Some("VA".into()),
                value: Some(1180.4),
                ts: None,
            });
            summary.period = Some("HC".into());
            summary.price_now = Some(0.1696);
            summary.today = period(7.25, Some(1.234));
            summary.yesterday = period(9.04, Some(1.5));
            summary.month = period(61.3, None);
        }
        summary
    }

    #[test]
    fn the_french_conversation_prompt_is_what_it_always_was() {
        let inventory = "## Salon\n- hue:1 · Lampe · on✎=true\n## Sans pièce\n- x:1 · Truc\n";
        for (title, page, protected, quiet, power) in [
            (
                Some("Chez nous"),
                true,
                "Chambre des enfants",
                "de 22:00 à 07:00 (en ce moment)",
                "Électricité en direct : 940 W.",
            ),
            (None, false, "aucune", "aucune", ""),
        ] {
            let before = before::chat_prompt(before::Chat {
                title,
                page,
                protected,
                quiet,
                date: "samedi 3 octobre 2026, 21h05",
                power,
                rooms: "Salon, Cuisine",
                inventory,
            });
            let now = ChatPrompt {
                title,
                page,
                protected,
                quiet,
                date: "samedi 3 octobre 2026, 21h05",
                power,
                rooms: "Salon, Cuisine",
                inventory,
            }
            .render();
            assert_eq!(now, before::reworded(&before));
        }
        assert_eq!(
            quiet_text("22:00", "07:00", true),
            "de 22:00 à 07:00 (en ce moment)"
        );
        assert_eq!(quiet_text("22:00", "07:00", false), "de 22:00 à 07:00");
        assert_eq!(moli_i18n::tr!("assistant.prompt.none"), "aucune");
    }

    #[test]
    fn a_device_named_like_a_placeholder_is_left_alone() {
        let prompt = ChatPrompt {
            title: None,
            page: false,
            protected: "aucune",
            quiet: "aucune",
            date: "demain",
            power: "",
            rooms: "Salon",
            inventory: "- x:1 · {inventory} {date}\n",
        }
        .render();
        assert!(prompt.contains("- x:1 · {inventory} {date}\n"), "{prompt}");
    }

    #[test]
    fn the_french_spoken_style_is_what_it_always_was() {
        assert_eq!(
            with_voice("Tu es Moli.".into(), true),
            format!("Tu es Moli.{}", before::reworded(before::SPOKEN_STYLE))
        );
    }

    #[test]
    fn the_french_electricity_line_is_what_it_always_was() {
        for full in [true, false] {
            let summary = sample_summary(full);
            assert_eq!(power_line(&summary), before::power_line(&summary));
        }
        assert!(
            power_line(&sample_summary(true)).starts_with("Électricité en direct : 940 W pour")
        );
    }

    #[test]
    fn the_french_tools_are_what_they_always_were() {
        // One deliberate change: `set` carries the words said once it is done.
        let mut then = before::tools();
        let set = then
            .as_array_mut()
            .and_then(|t| t.iter_mut().find(|t| t["function"]["name"] == "set"))
            .map(|t| &mut t["function"]["parameters"])
            .unwrap();
        set["properties"]["say"] = json!({
            "type": "string",
            "description": "Ta réponse, dite si la maison exécute tous les ordres : courte (« C'est allumé. »), avec la réponse à une éventuelle question. Sinon tu reçois le résultat et tu réponds."
        });
        set["required"] = json!(["point", "value", "say"]);
        assert_eq!(tools(), then);
        assert_eq!(ambiance_tool(), before::ambiance_tool());
    }

    #[test]
    fn a_filtered_answer_is_never_turned_into_an_admission_of_failure() {
        // Measured on 22 Oct. 2026: the model asked one question, the offer
        // filter erased it, and the house was told « Je n'ai pas su
        // répondre » — the words existed and nobody ever saw them.
        let asked = "Veux-tu que j'éteigne aussi la cuisine ?";
        let filtered = trim_filler(asked, false);
        assert_eq!(filtered, "", "the filter still trims an offer");
        let run = Run::default();
        assert_eq!(
            spoken_reply(asked, filtered, &run),
            asked,
            "what the model said is what the house hears"
        );
        assert_eq!(
            spoken_reply("", String::new(), &run),
            moli_i18n::tr!("assistant.reply.unknown")
        );
        let mut shown = Run::default();
        shown.cards.push(json!({ "kind": "device", "id": "salon" }));
        assert_eq!(
            spoken_reply("Voici les images.", String::new(), &shown),
            moli_i18n::tr!("assistant.reply.done")
        );
    }

    #[test]
    fn an_answer_cut_short_says_so_instead_of_pleading_ignorance() {
        let cut = Run {
            cut: true,
            ..Run::default()
        };
        assert_eq!(
            spoken_reply("", String::new(), &cut),
            moli_i18n::tr!("assistant.reply.too_long"),
            "running out of room is not the same as having nothing to say"
        );
        // Room ran out, but words did come: the words win.
        assert_eq!(
            spoken_reply("Presque fini.", String::new(), &cut),
            "Presque fini."
        );
    }

    #[test]
    fn ready_made_replies_and_filters_are_what_they_always_were() {
        assert_eq!(
            moli_i18n::tr!("assistant.reply.unknown"),
            "Je n’ai pas su répondre, tu peux reformuler ?"
        );
        assert_eq!(moli_i18n::tr!("assistant.reply.done"), "Voilà.");
        // Three questions back added on 9 Oct. 2026 (heard in conversation).
        let offers: Vec<&str> = ["que veux-tu", "que souhaites-tu", "qu'est-ce que tu veux"]
            .into_iter()
            .chain(before::OFFERS)
            .collect();
        assert_eq!(word_list("assistant.filter.offers"), offers);
        assert_eq!(word_list("assistant.filter.pointers"), before::POINTERS);
        assert_eq!(word_list("assistant.guess.energy"), before::GUESS_ENERGY);
        assert_eq!(word_list("assistant.guess.weather"), before::GUESS_WEATHER);
        assert_eq!(word_list("assistant.guess.cameras"), before::GUESS_CAMERAS);
        assert_eq!(
            moli_i18n::tr!("assistant.tools.white", kelvin = 2700),
            "blanc 2700 K"
        );
        assert_eq!(
            moli_i18n::tr!("assistant.tools.held_note"),
            "Pas exécuté : un adulte valide d'un geste à l'écran (sous ta réponse). Dis-le, ne réessaie pas."
        );
        assert_eq!(
            moli_i18n::tr!("assistant.tools.ambiance_held_note"),
            "Pièce protégée : un adulte valide à l'écran, ou choisit l'ambiance lui-même (Pièces → Ambiances). Ne réessaie pas."
        );
    }

    #[test]
    fn dates_read_in_the_houses_language() {
        let z: jiff::Zoned = "2026-10-03T21:05:00+02:00[Europe/Paris]".parse().unwrap();
        assert_eq!(date_in("fr", &z), "samedi 3 octobre 2026, 21h05");
        assert_eq!(date_in("en", &z), "Saturday 3 October 2026, 21:05");
        // A language without a date of its own yet speaks French, like its words.
        assert_eq!(date_in("xx", &z), date_in("fr", &z));
        assert_eq!(house_date(&z), date_in(&moli_i18n::language(), &z));
        let sunday: jiff::Zoned = "2026-12-27T07:30:00+01:00[Europe/Paris]".parse().unwrap();
        assert_eq!(english_date(&sunday), "Sunday 27 December 2026, 07:30");
    }

    #[test]
    fn english_and_french_have_the_same_words() {
        let (fr, en) = (french(), english());
        for (key, text) in &fr {
            assert!(key.starts_with("assistant."), "{key}");
            let other = en
                .get(key)
                .unwrap_or_else(|| panic!("{key} is missing in English"));
            assert_eq!(placeholders(text), placeholders(other), "{key}");
            assert!(!other.trim().is_empty(), "{key}");
        }
        for key in en.keys() {
            assert!(fr.contains_key(key), "{key} only exists in English");
        }
    }

    #[test]
    fn every_word_the_code_asks_for_exists() {
        let fr = french();
        let needle = ["\"", "assistant", "."].concat();
        for (file, source) in [
            ("lib.rs", include_str!("lib.rs")),
            ("auto.rs", include_str!("auto.rs")),
            ("door.rs", include_str!("door.rs")),
            ("house.rs", include_str!("house.rs")),
            ("house_model.rs", include_str!("house_model.rs")),
            ("import.rs", include_str!("import.rs")),
            ("settings.rs", include_str!("settings.rs")),
            ("voice.rs", include_str!("voice.rs")),
        ] {
            for part in source.split(needle.as_str()).skip(1) {
                // Words are `assistant.<group>.<name>`: `"assistant."` alone is a
                // prefix being checked, `assistant.json` the voice's file.
                let name = part.split('"').next().unwrap();
                if !name.contains('.') {
                    continue;
                }
                let key = format!("{}.{}", "assistant", name);
                assert!(
                    fr.contains_key(&key) || fr.contains_key(&format!("{key}_one")),
                    "{file}: {key} is not in the catalogue"
                );
            }
        }
    }

    #[test]
    fn the_english_conversation_prompt_has_no_placeholder_left() {
        let en = english();
        for page in [true, false] {
            let prompt = ChatPrompt {
                title: Some("Home"),
                page,
                protected: "none",
                quiet: "none",
                date: "Saturday 3 October 2026, 21:05",
                power: "Live electricity: 940 W",
                rooms: "Living room",
                inventory: "## Living room\n- hue:1 · Lamp\n",
            };
            let mut text = en["assistant.prompt.chat"].clone();
            for (name, value) in prompt.words() {
                text = text.replace(&format!("{{{name}}}"), &value);
            }
            assert!(placeholders(&text).is_empty(), "{:?}", placeholders(&text));
            assert!(text.contains("Style: English, two sentences at most (40 words)"));
            assert!(text.contains("Live electricity: 940 W"));
        }
        // Whatever the code fills in, the English words use no other name.
        let known = [
            "title",
            "cards",
            "protected",
            "quiet",
            "date",
            "power",
            "rooms",
            "inventory",
        ];
        for name in placeholders(&en["assistant.prompt.chat"]) {
            assert!(known.contains(&name.as_str()), "{name}");
        }
    }

    #[test]
    fn the_english_safety_nets_work_on_english_words() {
        let en = english();
        let list = |key: &str| -> Vec<String> {
            en[key]
                .split('|')
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(
            trim_filler_with("It is 23 °C. Would you like to see more?", true, &list),
            "It is 23 °C."
        );
        assert_eq!(
            trim_filler_with("Nobody in the yard. Here are the live images.", true, &list),
            "Nobody in the yard."
        );
        // Without cards, « here is » may be the answer itself.
        assert_eq!(
            trim_filler_with("Here is a joke.", false, &list),
            "Here is a joke."
        );
        assert_eq!(
            trim_filler_with("Should it rain today?", true, &list),
            "Should it rain today?"
        );
        assert_eq!(
            guess_cards_with("How much electricity are we using?", &list),
            vec![json!({ "kind": "energy" })]
        );
        assert_eq!(
            guess_cards_with("Is it going to rain tomorrow?", &list),
            vec![json!({ "kind": "weather" })]
        );
        assert_eq!(
            guess_cards_with("Who rang the doorbell?", &list),
            vec![json!({ "kind": "cameras" })]
        );
        assert!(guess_cards_with("Please arrange the Sunday plan", &list).is_empty());
        assert!(guess_cards_with("Tell me a joke", &list).is_empty());
    }

    #[tokio::test]
    async fn named_providers_set_the_model_and_the_privacy_status() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let config: Config = toml::from_str(
            "[[provider]]\nid = \"local\"\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"small-model\"\ntier = 1\n\n[routes]\nchat = \"local\"\n",
        )
        .unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        let status = moli.status();
        assert_eq!(status["model"], "small-model");
        assert_eq!(status["privacy"]["mode"], "named");
        assert_eq!(status["privacy"]["routes"]["chat"], "local");
        assert_eq!(status["privacy"]["providers"][0]["local"], true);
    }

    fn house_model_config(contract: &str) -> Config {
        toml::from_str(&format!(
            "[[provider]]\nid = \"house\"\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"moli-maison\"\ntier = 1\ncontract = \"{contract}\"\n\n[routes]\nchat = \"house\"\n"
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn a_provider_can_declare_the_house_model_contract() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let moli = Assistant::new(hub, None, None, None, house_model_config("moli-v4")).unwrap();
        assert_eq!(
            moli.status()["privacy"]["providers"][0]["contract"],
            "moli-v4"
        );
    }

    #[tokio::test]
    async fn a_contract_other_than_the_one_the_model_learned_is_refused() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        // Refused, not fatal: Moli starts and its assistant says why it is out.
        let moli = Assistant::new(hub, None, None, None, house_model_config("moli-v9")).unwrap();
        let status = moli.status();
        assert_eq!(status["ready"], false);
        assert!(
            status["refused"].as_str().unwrap().contains("moli-v9"),
            "{status}"
        );
    }

    #[tokio::test]
    async fn an_old_config_reports_the_legacy_mode_and_keeps_its_model() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let moli = Assistant::new(hub, None, None, None, Config::default()).unwrap();
        let status = moli.status();
        assert_eq!(status["model"], "gpt-4.1-mini");
        assert_eq!(status["privacy"]["mode"], "legacy");
    }
}

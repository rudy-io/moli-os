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
mod house;
mod import;
mod llm;
mod satellite;
mod settings;
mod voice;

pub use auto::Draft;
pub use import::HaAutomation;
pub use voice::{Heard, Speech, SpeechStream, Spoken};

#[cfg(test)]
mod before;

use std::collections::VecDeque;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use moli_core::{DeviceId, InstanceId, Origin, PointId, Value};
use moli_runtime::{CommandError, Hub};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::house::{Layout, name_of};
use crate::llm::Endpoint;

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
}

impl Default for Config {
    /// No `[assistant]` in moli.toml: Moli is there anyway, waiting for a key.
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: default_model(),
            builder_model: default_builder(),
            transcribe_model: default_transcribe(),
            speech_model: default_speech_model(),
            speech_voice: default_speech_voice(),
            speech_style: default_speech_style(),
            speech_speed: default_speech_speed(),
            local_speech: default_local_speech(),
            local_voice: default_local_voice(),
            local_listen: default_local_listen(),
            timezone: default_timezone(),
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

/// Model round trips per turn (tool calls included).
const MAX_STEPS: usize = 6;
/// Messages of the conversation sent back (the model has no memory).
const MAX_MESSAGES: usize = 14;
const MAX_MESSAGE_CHARS: usize = 2_000;
/// Turns per window, all clients together: a runaway page cannot run up a bill.
const MAX_TURNS: usize = 60;
const TURN_WINDOW: Duration = Duration::from_secs(10 * 60);
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
    /// A Home Assistant import is running.
    importing: std::sync::atomic::AtomicBool,
    /// SHA-256 of the HA automations translated since start: not paid for
    /// twice, even after their draft was renamed.
    imported: Mutex<std::collections::HashSet<String>>,
    /// Voices prepared for a satellite: id → (until when, text).
    prepared: Mutex<std::collections::HashMap<String, (Instant, String)>>,
}

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
}

#[derive(Debug, Default, Serialize)]
pub struct Usage {
    pub prompt_tokens: u64,
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
        let endpoint = Endpoint::parse(&config.base_url)?;
        let tz = jiff::tz::TimeZone::get(&config.timezone)?;
        anyhow::ensure!(
            (0.25..=4.0).contains(&config.speech_speed),
            "speech_speed must be between 0.25 and 4"
        );
        Ok(Self(Arc::new(Inner {
            hub,
            energy,
            history,
            layout_path,
            config,
            endpoint,
            tz,
            running: tokio::sync::Semaphore::new(2),
            recent: Mutex::new(VecDeque::new()),
            voice_running: tokio::sync::Semaphore::new(3),
            voice_recent: Mutex::new(VecDeque::new()),
            prefs: std::sync::RwLock::new(None),
            prefs_path: std::sync::RwLock::new(None),
            automations: std::sync::RwLock::new(None),
            importing: std::sync::atomic::AtomicBool::new(false),
            imported: Mutex::default(),
            prepared: Mutex::default(),
        })))
    }

    fn key(&self) -> Result<Option<String>, AssistantError> {
        let key = self.0.hub.secret(&InstanceId::from("assistant"), "api_key");
        if key.is_none() && !self.0.endpoint.is_local() {
            return Err(AssistantError::NotConfigured(
                "no API key: pipe it into `moli-os secrets set assistant api_key`".into(),
            ));
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

    /// One turn of the conversation: the model reads the house, may act and
    /// pick cards, then answers.
    pub async fn turn(&self, turn: Turn) -> Result<Reply, AssistantError> {
        if turn.messages.last().is_none_or(|m| m.role != Role::User) {
            return Err(AssistantError::Invalid(
                "the last message must be the user's".into(),
            ));
        }
        let key = self.key()?;
        let _permit = self.admit()?;
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        let page = turn.surface.as_deref() == Some("page");
        let question = turn
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let power = self.power_now().await;
        let mut messages = vec![
            json!({ "role": "system", "content": with_voice(self.system_prompt(&layout, page, &power), turn.spoken) }),
        ];
        let skip = turn.messages.len().saturating_sub(MAX_MESSAGES);
        for m in turn.messages.iter().skip(skip) {
            let content: String = m.content.chars().take(MAX_MESSAGE_CHARS).collect();
            messages.push(json!({ "role": m.role, "content": content }));
        }

        // Spoken, nobody waits for cards: no `show`, one round trip less
        // (the screen still gets the cards the question calls for).
        let mut offered = tools();
        if turn.spoken
            && let Some(list) = offered.as_array_mut()
        {
            list.retain(|t| t["function"]["name"] != "show");
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
                body["temperature"] = json!(0.3);
                body["max_tokens"] = json!(700);
            }
            if step + 1 == MAX_STEPS {
                body["tool_choice"] = json!("none");
            }
            let answer = self
                .0
                .endpoint
                .chat(key.as_deref(), &body)
                .await
                .map_err(|e| AssistantError::Upstream(e.to_string()))?;
            run.usage.steps = step + 1;
            run.usage.prompt_tokens += answer["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
            run.usage.completion_tokens +=
                answer["usage"]["completion_tokens"].as_u64().unwrap_or(0);
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
    /// conversation hears). An order held or failed goes back to the model,
    /// which must say so.
    async fn act(
        &self,
        message: &Json,
        calls: &[Json],
        layout: &Layout,
        run: &mut Run,
        messages: &mut Vec<Json>,
    ) -> Option<String> {
        let said = message["content"].as_str().unwrap_or_default().trim();
        let quick = calls
            .iter()
            .all(|c| matches!(c["function"]["name"].as_str(), Some("show" | "set")));
        let mut carried_out = quick && !said.is_empty();
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
            carried_out &= name != "set" || result["status"] == "done";
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call["id"],
                "content": result.to_string(),
            }));
        }
        carried_out.then(|| said.to_owned())
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
        let text = trim_filler(&once(text), !run.cards.is_empty());
        let reply = if !text.is_empty() {
            text
        } else if run.cards.is_empty() && run.actions.is_empty() {
            moli_i18n::tr!("assistant.reply.unknown")
        } else {
            moli_i18n::tr!("assistant.reply.done")
        };
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
            "energy" => self.energy(args).await,
            "history" => self.history(args).await,
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
    usage: Usage,
    /// The tools called, in order, for the log: where a slow turn went.
    tools: Vec<String>,
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
                        "value": { "anyOf": [{ "type": "boolean" }, { "type": "number" }, { "type": "string" }] }
                    },
                    "required": ["point", "value"]
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

    /// A lamp that does what it is told.
    struct Lamp;

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
                ctx.ready();
                while let Some(cmd) = ctx.next_command().await {
                    ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                    cmd.reply(Ok(()));
                }
                Ok(())
            })
        }
    }

    /// Moli over a hub with the lamp running, its provider answering `answer` once.
    async fn moli_with_lamp(answer: &'static [u8]) -> Assistant {
        let (url, _task) = crate::llm::tests::fake_http(200, answer).await;
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        moli_runtime::spawn_driver(
            &hub,
            InstanceId::from("fake"),
            Arc::new(Lamp),
            tokio_util::sync::CancellationToken::new(),
        );
        for _ in 0..200 {
            if hub.stats().drivers_running == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let config: Config = serde_json::from_value(json!({ "base_url": url })).unwrap();
        Assistant::new(hub, None, None, None, config).unwrap()
    }

    fn spoken(question: &str) -> Turn {
        serde_json::from_value(json!({
            "spoken": true,
            "messages": [{ "role": "user", "content": question }],
        }))
        .unwrap()
    }

    /// The provider answers once: a second round trip would fail the turn.
    #[tokio::test]
    async fn an_order_carried_out_ends_the_turn_with_its_words() {
        let moli = moli_with_lamp(
            br#"{"choices":[{"message":{"role":"assistant",
            "content":"C'est allume.",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:lamp/on\",\"value\":true}"}}]}}],
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
            "content":"C'est allume.",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"set",
            "arguments":"{\"point\":\"fake:nothing/on\",\"value\":true}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .await;
        assert!(moli.turn(spoken("Allume le rien")).await.is_err());
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
        assert_eq!(tools(), before::tools());
        assert_eq!(ambiance_tool(), before::ambiance_tool());
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
            ("house.rs", include_str!("house.rs")),
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
}

//! ESPHome devices over their native API (TCP 6053, Noise-encrypted),
//! without any ESPHome library: the transport is written from the Noise
//! specification ([`noise`], [`frame`]), the messages by hand ([`api`]).
//!
//! First use: voice satellites such as the Home Assistant Voice Preview
//! Edition. The driver keeps one connection open (the device restarts after
//! 15 minutes without any client), answers its pings and pings it every 20
//! s, and publishes the device: its volume and mute (its media player), and
//! what its voice is doing.

mod api;
mod frame;
mod noise;
mod satellite;
mod sound;
mod vad;

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The marker of a handshake the device refused for its key.
pub(crate) const WRONG_KEY: &str = "the device refused the API key";
const API_KEY: &str = "api_key";
const PING_EVERY: Duration = Duration::from_secs(20);
/// Nothing at all from the device for this long (it pings after 60 s of
/// silence): the connection is dead.
const SILENCE_LIMIT: Duration = Duration::from_secs(90);
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// What the voice is doing, as the dashboard and automations read it.
const STATES: [&str; 4] = ["idle", "listening", "thinking", "speaking"];

fn default_port() -> u16 {
    6053
}

fn default_window() -> u64 {
    8
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Moli as the device reaches it (`http://host:8790`): the sounds and
    /// the voice it plays.
    #[serde(default)]
    pub media_base: Option<String>,
    /// Seconds a conversation stays open after each answer.
    #[serde(default = "default_window")]
    pub window_s: u64,
    /// The wake words to keep active (their ids in the device's firmware,
    /// e.g. `hey_moli`); empty: as the device has them.
    #[serde(default)]
    pub wake_words: Vec<String>,
}

#[derive(Debug)]
pub struct Esphome {
    config: Config,
}

impl Esphome {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Esphome {
    fn kind(&self) -> &'static str {
        "esphome"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn spec(key: &str, label: &str, kind: Kind, write: bool, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::infer(key, unit.as_ref()),
        kind,
        access: Access { read: true, write },
        unit,
    }
}

fn device(
    id: &DeviceId,
    ctx: &DriverCtx,
    info: &api::DeviceInfo,
    hello: &frame::ServerHello,
) -> Device {
    let name = [&info.friendly_name, &info.name, &hello.name]
        .into_iter()
        .find(|n| !n.is_empty())
        .cloned()
        .unwrap_or_default();
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some(info.manufacturer.clone())
            .filter(|m| !m.is_empty())
            .map(Into::into),
        model: Some(
            if info.model.is_empty() {
                moli_i18n::tr!("pilotes.esphome.satellite")
            } else {
                info.model.clone()
            }
            .into(),
        ),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "state",
                &moli_i18n::tr!("pilotes.esphome.etat"),
                Kind::Enum {
                    values: STATES.iter().map(|s| (*s).into()).collect(),
                },
                false,
                None,
            ),
            spec(
                "volume",
                &moli_i18n::tr!("pilotes.esphome.volume"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                true,
                Some(Unit::Percent),
            ),
            spec(
                "muted",
                &moli_i18n::tr!("pilotes.esphome.sourdine"),
                Kind::Binary,
                true,
                None,
            ),
            spec(
                "wake_word",
                &moli_i18n::tr!("pilotes.esphome.mot_activation"),
                Kind::Text,
                false,
                None,
            ),
        ],
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some(key) = ctx.secret(API_KEY) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.esphome.cle_manquante",
            instance = ctx.instance(),
            key = API_KEY
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let Ok(psk) = noise::psk_from_base64(&key) else {
        ctx.wait_for(moli_i18n::tr!("pilotes.esphome.cle_invalide"));
        ctx.cancelled().await;
        return Ok(());
    };
    ctx.ready();
    let mut pause = RETRY_MIN;
    let mut last_error: Option<String> = None;
    let mut known: Option<DeviceId> = None;
    loop {
        let started = Instant::now();
        let outcome = session(config, ctx, &psk, &mut known).await;
        let error = match outcome {
            Ok(()) => return Ok(()), // shutting down
            Err(e) => format!("{e:#}"),
        };
        if let Some(id) = &known {
            ctx.set_availability(id, false);
        }
        if error.contains(WRONG_KEY) {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.esphome.cle_refusee",
                instance = ctx.instance(),
                key = API_KEY
            ));
            ctx.cancelled().await;
            return Ok(());
        }
        // Each new reason once; a device switched off says the same every time.
        if last_error.as_deref() == Some(error.as_str()) {
            tracing::debug!(instance = %ctx.instance(), error, "esphome device unreachable");
        } else {
            tracing::warn!(instance = %ctx.instance(), error, "esphome device unreachable");
            last_error = Some(error);
        }
        // A connection that lasted starts again quickly.
        pause = if started.elapsed() > RETRY_MAX {
            RETRY_MIN
        } else {
            (pause * 2).min(RETRY_MAX)
        };
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            () = ctx.cancelled() => return Ok(()),
        }
    }
}

/// Messages from the device, or why they stopped.
type Inbox = mpsc::Receiver<anyhow::Result<(u16, Vec<u8>)>>;
type Outbox = frame::Sender<tokio::net::tcp::OwnedWriteHalf>;

/// Aborts the reading task when the session ends, however it ends.
struct Reader(tokio::task::JoinHandle<()>);

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the session learnt of the device's entities.
#[derive(Debug, Default)]
struct Entities {
    media_player: Option<u32>,
    button: Option<u32>,
}

/// A connection open and introduced: hello said, the device and its entities known.
struct Opened {
    tx: Outbox,
    inbox: Inbox,
    _reader: Reader,
    hello: frame::ServerHello,
    info: api::DeviceInfo,
    entities: Entities,
}

/// Connects, does the handshake, says hello, reads the device and its entities.
async fn open(config: &Config, psk: &[u8; 32]) -> anyhow::Result<Opened> {
    let mut tcp = tokio::time::timeout(
        frame::HANDSHAKE_LIMIT,
        TcpStream::connect((config.host.as_str(), config.port)),
    )
    .await
    .context("no answer")?
    .with_context(|| format!("cannot reach {}:{}", config.host, config.port))?;
    tcp.set_nodelay(true)?;
    let (hello, send, receive) =
        tokio::time::timeout(frame::HANDSHAKE_LIMIT, frame::handshake(&mut tcp, psk))
            .await
            .context("handshake: no answer")??;
    let (read, write) = tcp.into_split();
    let mut tx = frame::Sender::new(write, send);
    let (inbox_tx, mut inbox) = mpsc::channel(64);
    let reader = Reader(tokio::spawn(async move {
        let mut rx = frame::Receiver::new(read, receive);
        loop {
            let next = rx.next().await;
            let failed = next.is_err();
            if inbox_tx.send(next).await.is_err() || failed {
                return;
            }
        }
    }));

    tx.send(api::HELLO_REQUEST, &api::hello()).await?;
    expect(&mut inbox, &mut tx, api::HELLO_RESPONSE).await?;
    tx.send(api::DEVICE_INFO_REQUEST, &[]).await?;
    let info = api::device_info(&expect(&mut inbox, &mut tx, api::DEVICE_INFO_RESPONSE).await?)
        .context("unreadable device info")?;
    tx.send(api::LIST_ENTITIES_REQUEST, &[]).await?;
    let mut entities = Entities::default();
    loop {
        let (kind, payload) = next(&mut inbox, &mut tx).await?;
        match kind {
            api::LIST_ENTITIES_DONE => break,
            api::LIST_MEDIA_PLAYER => entities.media_player = api::entity(&payload).map(|e| e.key),
            api::LIST_EVENT => entities.button = api::entity(&payload).map(|e| e.key),
            _ => {}
        }
    }
    Ok(Opened {
        tx,
        inbox,
        _reader: reader,
        hello,
        info,
        entities,
    })
}

/// One connection, from the handshake until it fails (`Err`) or Moli stops (`Ok`).
async fn session(
    config: &Config,
    ctx: &mut DriverCtx,
    psk: &[u8; 32],
    known: &mut Option<DeviceId>,
) -> anyhow::Result<()> {
    let Opened {
        mut tx,
        mut inbox,
        _reader,
        hello,
        info,
        entities,
    } = open(config, psk).await?;
    let id = ctx.device_id(&hello.mac);
    ctx.upsert_device(device(&id, ctx, &info, &hello));
    ctx.set_state(&id, "state", Value::Text("idle".into()));
    ctx.set_availability(&id, true);
    *known = Some(id.clone());
    tracing::info!(
        instance = %ctx.instance(),
        device = %info.friendly_name,
        esphome = %info.esphome_version,
        voice_flags = info.voice_flags,
        "esphome device connected"
    );
    tx.send(api::SUBSCRIBE_STATES, &[]).await?;
    // Moli is the device's voice assistant (the first to subscribe keeps it).
    tx.send(api::SUBSCRIBE_VOICE_ASSISTANT, &api::subscribe_voice())
        .await?;
    if !config.wake_words.is_empty() {
        let ids: Vec<&str> = config.wake_words.iter().map(String::as_str).collect();
        tx.send(api::VOICE_SET_CONFIGURATION, &api::set_wake_words(&ids))
            .await?;
    }
    tx.send(api::VOICE_CONFIGURATION_REQUEST, &[]).await?;
    let (mut voice, mut thoughts) = satellite::Voice::new(
        Duration::from_secs(config.window_s),
        config.media_base.clone(),
    );

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = Instant::now();
    loop {
        let deadline = voice.deadline();
        tokio::select! {
            message = inbox.recv() => {
                let (kind, payload) = message.context("the connection closed")??;
                heard = Instant::now();
                on_message(ctx, &id, &mut tx, &entities, &mut voice, kind, &payload).await?;
            }
            Some((generation, thought)) = thoughts.recv() => {
                voice.on_thought(ctx, &id, &mut tx, generation, thought).await?;
            }
            () = until(deadline) => voice.on_deadline(ctx, &id, &mut tx).await?,
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE_LIMIT {
                    bail!("the device went silent");
                }
                tx.send(api::PING_REQUEST, &[]).await?;
            }
            command = ctx.next_command() => {
                let Some(command) = command else {
                    let _ = tx.send(api::DISCONNECT_REQUEST, &[]).await;
                    return Ok(());
                };
                let result = order(&mut tx, &entities, &command).await;
                command.reply(result);
            }
        }
    }
}

/// One message from the device, once connected.
async fn on_message(
    ctx: &DriverCtx,
    id: &DeviceId,
    tx: &mut Outbox,
    entities: &Entities,
    voice: &mut satellite::Voice,
    kind: u16,
    payload: &[u8],
) -> anyhow::Result<()> {
    match kind {
        api::PING_REQUEST => tx.send(api::PING_RESPONSE, &[]).await?,
        api::DISCONNECT_REQUEST => {
            let _ = tx.send(api::DISCONNECT_RESPONSE, &[]).await;
            bail!("the device closed the connection");
        }
        api::VOICE_AUDIO => {
            if let Some(pcm) = api::voice_audio(payload) {
                voice.on_audio(ctx, id, tx, &pcm).await?;
            }
        }
        api::VOICE_REQUEST => {
            if let Some(request) = api::voice_request(payload) {
                voice.on_request(ctx, id, tx, &request).await?;
            }
        }
        api::VOICE_ANNOUNCE_FINISHED => voice.on_played(ctx, id),
        api::VOICE_CONFIGURATION_RESPONSE => {
            if let Some(words) = api::wake_words(payload) {
                tracing::info!(
                    instance = %ctx.instance(),
                    available = ?words.available.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
                    active = ?words.active,
                    max_active = words.max_active,
                    "wake words"
                );
            }
        }
        api::MEDIA_PLAYER_STATE => {
            if let Some(state) =
                api::media_state(payload).filter(|s| Some(s.key) == entities.media_player)
            {
                ctx.set_state(
                    id,
                    "volume",
                    Value::Float(f64::from((state.volume * 100.0).round())),
                );
                ctx.set_state(id, "muted", Value::Bool(state.muted));
            }
        }
        api::EVENT => {
            if let Some((_, event)) =
                api::event(payload).filter(|(key, _)| Some(*key) == entities.button)
            {
                voice.on_button(ctx, id, tx, &event).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Until `at`; forever without one (a `select!` branch that never fires).
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// A command for the device: the volume or mute of its media player.
async fn order(
    tx: &mut Outbox,
    entities: &Entities,
    command: &CommandRequest,
) -> Result<(), String> {
    let key = entities
        .media_player
        .ok_or_else(|| moli_i18n::tr!("pilotes.esphome.sans_lecteur"))?;
    let payload = match (&*command.key, &command.value) {
        ("volume", Value::Float(v)) => {
            #[allow(clippy::cast_possible_truncation)]
            let v = (*v / 100.0) as f32;
            api::set_volume(key, v)
        }
        ("volume", Value::Int(v)) => {
            #[allow(clippy::cast_precision_loss)]
            let v = *v as f32 / 100.0;
            api::set_volume(key, v)
        }
        ("muted", Value::Bool(m)) => api::set_muted(key, *m),
        (point, _) => {
            return Err(moli_i18n::tr!(
                "pilotes.esphome.lecture_seule",
                point = point
            ));
        }
    };
    tx.send(api::MEDIA_PLAYER_COMMAND, &payload)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// The next message, the device's pings answered on the way.
async fn next(inbox: &mut Inbox, tx: &mut Outbox) -> anyhow::Result<(u16, Vec<u8>)> {
    loop {
        let (kind, payload) = tokio::time::timeout(frame::HANDSHAKE_LIMIT, inbox.recv())
            .await
            .context("the device stopped answering")?
            .context("the connection closed")??;
        match kind {
            api::PING_REQUEST => tx.send(api::PING_RESPONSE, &[]).await?,
            api::DISCONNECT_REQUEST => {
                let _ = tx.send(api::DISCONNECT_RESPONSE, &[]).await;
                bail!("the device closed the connection");
            }
            _ => return Ok((kind, payload)),
        }
    }
}

/// The next message of type `kind` (others in between are skipped).
async fn expect(inbox: &mut Inbox, tx: &mut Outbox, kind: u16) -> anyhow::Result<Vec<u8>> {
    loop {
        let (got, payload) = next(inbox, tx).await?;
        if got == kind {
            return Ok(payload);
        }
    }
}

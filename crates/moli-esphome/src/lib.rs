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
mod find;
mod frame;
mod noise;
mod satellite;
mod sound;

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
/// The marker of a device speaking the API in clear: it has no key.
pub(crate) const IN_CLEAR: &str = "the device speaks the API in clear (it has no key)";
const API_KEY: &str = "api_key";
const PING_EVERY: Duration = Duration::from_secs(20);
/// Nothing at all from the device for this long (it pings after 60 s of
/// silence): the connection is dead.
const SILENCE_LIMIT: Duration = Duration::from_secs(90);
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// No answer at the device's address: it may have a new one.
const NOT_THERE: &str = "the device does not answer at its address";
/// One look around the home network for a device gone silent, at most this often.
const SEARCH_EVERY: Duration = Duration::from_secs(10 * 60);
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
    /// The device's MAC address: a device without a key is given one only
    /// if it is this one (an address may have gone to another device).
    #[serde(default)]
    pub mac: Option<String>,
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
            spec(
                "say",
                &moli_i18n::tr!("pilotes.esphome.dire"),
                Kind::Text,
                true,
                None,
            ),
        ],
    }
}

/// Waits for a person: says why, then idles until Moli stops.
async fn wait_for_person(ctx: &DriverCtx, why: String) {
    ctx.wait_for(why);
    ctx.cancelled().await;
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Its address may change (a new lease): the driver follows the device.
    let mut config = config.clone();
    let config = &mut config;
    let mut searched: Option<Instant> = None;
    // No key yet is fine: a device in clear is given one (see `provision`).
    let mut psk = match ctx.secret(API_KEY).map(|k| noise::psk_from_base64(&k)) {
        None => None,
        Some(Ok(psk)) => Some(psk),
        Some(Err(_)) => {
            wait_for_person(ctx, moli_i18n::tr!("pilotes.esphome.cle_invalide")).await;
            return Ok(());
        }
    };
    ctx.ready();
    let mut pause = RETRY_MIN;
    let mut last_error: Option<String> = None;
    let mut known: Option<DeviceId> = None;
    loop {
        let started = Instant::now();
        let outcome = match &psk {
            Some(psk) => session(config, ctx, psk, &mut known).await,
            None => Err(anyhow::anyhow!(IN_CLEAR)),
        };
        let error = match outcome {
            Ok(()) => return Ok(()), // shutting down
            Err(e) => format!("{e:#}"),
        };
        if let Some(id) = &known {
            ctx.set_availability(id, false);
        }
        if error.contains(WRONG_KEY) {
            let why = moli_i18n::tr!(
                "pilotes.esphome.cle_refusee",
                instance = ctx.instance(),
                key = API_KEY
            );
            wait_for_person(ctx, why).await;
            return Ok(());
        }
        let error = if error.contains(IN_CLEAR) {
            match provision(config, ctx, psk.is_some()).await {
                Ok(new) => {
                    psk = Some(new);
                    pause = RETRY_MIN;
                    // The device switches to encryption: a moment, then again.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                Err(Provision::Person(why)) => {
                    wait_for_person(ctx, why).await;
                    return Ok(());
                }
                Err(Provision::Retry(e)) => format!("{e:#}"),
            }
        } else {
            error
        };
        if error.contains(NOT_THERE) && searched.is_none_or(|at| at.elapsed() >= SEARCH_EVERY) {
            searched = Some(Instant::now());
            if let Some(ip) = search(config, ctx).await {
                config.host = ip.to_string();
                pause = RETRY_MIN;
                continue;
            }
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

/// The device's new address on the home network, found by its MAC (only
/// with `mac` set and an IPv4 `host`).
async fn search(config: &Config, ctx: &DriverCtx) -> Option<std::net::Ipv4Addr> {
    let mac = config.mac.as_deref()?;
    let old: std::net::Ipv4Addr = config.host.parse().ok()?;
    let found = find::find(&find::neighbours(old), config.port, mac).await;
    match found {
        Some(ip) => tracing::warn!(
            instance = %ctx.instance(),
            old = %old,
            new = %ip,
            "esphome device found at a new address (update `host` in the configuration)"
        ),
        None => {
            tracing::info!(instance = %ctx.instance(), "esphome device not found on the home network");
        }
    }
    found
}

/// Why a key could not be given: a person must act, or it may work later.
enum Provision {
    Person(String),
    Retry(anyhow::Error),
}

impl From<anyhow::Error> for Provision {
    fn from(e: anyhow::Error) -> Self {
        Self::Retry(e)
    }
}

impl From<std::io::Error> for Provision {
    fn from(e: std::io::Error) -> Self {
        Self::Retry(e.into())
    }
}

/// `AC:BC:…` or `acbc…` → `acbc…`.
fn mac_hex(mac: &str) -> String {
    mac.chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>()
        .to_lowercase()
}

/// The next message in clear of type `kind`, the device's pings answered.
async fn expect_plain(tcp: &mut TcpStream, kind: u16) -> anyhow::Result<Vec<u8>> {
    loop {
        let (got, payload) = tokio::time::timeout(frame::HANDSHAKE_LIMIT, frame::read_plain(tcp))
            .await
            .context("the device stopped answering")??;
        match got {
            k if k == kind => return Ok(payload),
            api::PING_REQUEST => frame::send_plain(tcp, api::PING_RESPONSE, &[]).await?,
            api::DISCONNECT_REQUEST => bail!("the device closed the connection"),
            _ => {}
        }
    }
}

/// The device speaks in clear: it has no key (Home Assistant clears it when
/// it lets a device go; a new one never had one). Moli gives it its own, after
/// checking it is the expected device, and files it in the vault first: never
/// a key the device holds and Moli lost.
async fn provision(config: &Config, ctx: &DriverCtx, had_key: bool) -> Result<[u8; 32], Provision> {
    let mut tcp = connect(config).await?;
    let info = match plain_info(&mut tcp).await {
        Ok(info) => info,
        // It holds a key after all, one Moli does not know.
        Err(e) if !had_key && format!("{e:#}").contains("asks for encryption") => {
            return Err(Provision::Person(moli_i18n::tr!(
                "pilotes.esphome.cle_manquante",
                instance = ctx.instance(),
                key = API_KEY
            )));
        }
        Err(e) => return Err(e.into()),
    };
    if !info.encryption_supported {
        return Err(Provision::Person(moli_i18n::tr!(
            "pilotes.esphome.sans_chiffrement"
        )));
    }
    let mac = mac_hex(&info.mac);
    if let Some(expected) = config.mac.as_deref().map(mac_hex).filter(|e| *e != mac) {
        return Err(Provision::Person(moli_i18n::tr!(
            "pilotes.esphome.autre_appareil",
            host = config.host,
            mac = mac,
            expected = expected
        )));
    }
    let key = noise::new_key_base64()?;
    ctx.store_secret(API_KEY, &key).await?;
    frame::send_plain(&mut tcp, api::NOISE_SET_KEY_REQUEST, &api::set_key(&key)).await?;
    let saved = expect_plain(&mut tcp, api::NOISE_SET_KEY_RESPONSE).await?;
    if !moli_net::proto::parse(&saved).is_some_and(|m| m.bool(1)) {
        return Err(Provision::Retry(anyhow::anyhow!(
            "the device did not save the key"
        )));
    }
    tracing::warn!(
        instance = %ctx.instance(),
        device = %info.friendly_name,
        mac,
        "the device had no API key: Moli gave it its own"
    );
    Ok(noise::psk_from_base64(&key)?)
}

/// A device in clear: our hello, then what it says of itself.
async fn plain_info(tcp: &mut TcpStream) -> anyhow::Result<api::DeviceInfo> {
    frame::send_plain(tcp, api::HELLO_REQUEST, &api::hello()).await?;
    expect_plain(tcp, api::HELLO_RESPONSE).await?;
    frame::send_plain(tcp, api::DEVICE_INFO_REQUEST, &[]).await?;
    api::device_info(&expect_plain(tcp, api::DEVICE_INFO_RESPONSE).await?)
        .context("unreadable device info")
}

async fn connect(config: &Config) -> anyhow::Result<TcpStream> {
    let tcp = tokio::time::timeout(
        frame::HANDSHAKE_LIMIT,
        TcpStream::connect((config.host.as_str(), config.port)),
    )
    .await
    .context("no answer")
    .context(NOT_THERE)?
    .with_context(|| format!("cannot reach {}:{}", config.host, config.port))
    .context(NOT_THERE)?;
    tcp.set_nodelay(true)?;
    Ok(tcp)
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
    let mut tcp = connect(config).await?;
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
    // What the firmware offers first: the configured words are set only once
    // the device says it has them (see `wake_words`).
    tx.send(api::VOICE_CONFIGURATION_REQUEST, &[]).await?;
    let mut words_set = false;
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
                if kind == api::VOICE_CONFIGURATION_RESPONSE {
                    words_set |= wake_words(ctx, &mut tx, &config.wake_words, words_set, &payload).await?;
                } else {
                    on_message(ctx, &id, &mut tx, &entities, &mut voice, kind, &payload).await?;
                }
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
                let result = order(ctx, &mut tx, &entities, config.media_base.as_deref(), &command).await;
                command.reply(result);
            }
        }
    }
}

/// The wake words to set: those of `wanted` the firmware has, unless they
/// are already the active ones. None of them in the firmware: nothing set, the
/// device keeps its own (a word it lacks would leave it deaf to all).
fn words_to_set<'a>(
    wanted: &'a [String],
    available: &[&str],
    active: &[String],
) -> Option<Vec<&'a str>> {
    let chosen: Vec<&str> = wanted
        .iter()
        .map(String::as_str)
        .filter(|w| available.contains(w))
        .collect();
    let same = chosen.len() == active.len() && chosen.iter().all(|c| active.iter().any(|a| a == c));
    (!chosen.is_empty() && !same).then_some(chosen)
}

/// The device's wake words: logged, and set to the configured ones it has
/// (once per connection). Returns whether they were set.
async fn wake_words(
    ctx: &DriverCtx,
    tx: &mut Outbox,
    wanted: &[String],
    already: bool,
    payload: &[u8],
) -> anyhow::Result<bool> {
    let Some(words) = api::wake_words(payload) else {
        return Ok(false);
    };
    let available: Vec<&str> = words.available.iter().map(|(id, _)| id.as_str()).collect();
    tracing::info!(
        instance = %ctx.instance(),
        ?available,
        active = ?words.active,
        max_active = words.max_active,
        "wake words"
    );
    if already || wanted.is_empty() {
        return Ok(false);
    }
    let Some(chosen) = words_to_set(wanted, &available, &words.active) else {
        if !wanted.iter().any(|w| available.contains(&w.as_str())) {
            tracing::warn!(
                instance = %ctx.instance(),
                ?wanted,
                "none of the configured wake words is in the device's firmware: it keeps its own"
            );
        }
        return Ok(false);
    };
    tx.send(api::VOICE_SET_CONFIGURATION, &api::set_wake_words(&chosen))
        .await?;
    // Read back what the device made of it (logged above, next time).
    tx.send(api::VOICE_CONFIGURATION_REQUEST, &[]).await?;
    Ok(true)
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

/// What a satellite may say at once.
const MAX_SAY: usize = 500;

/// A command for the device: the volume or mute of its media player, or a
/// sentence in Moli's voice (an announcement over what it plays).
async fn order(
    ctx: &DriverCtx,
    tx: &mut Outbox,
    entities: &Entities,
    media_base: Option<&str>,
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
        ("say", Value::Text(text)) => {
            let text = text.trim();
            if text.is_empty() || text.chars().count() > MAX_SAY {
                return Err(moli_i18n::tr!("pilotes.esphome.phrase"));
            }
            let url = ctx
                .voice_brain()
                .zip(media_base)
                .and_then(|(brain, base)| brain.voice_url(base, text))
                .ok_or_else(|| moli_i18n::tr!("pilotes.esphome.sans_voix"))?;
            api::announce(key, &url)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn only_the_wake_words_the_firmware_has_are_set() {
        let wanted = owned(&["hey_moli", "dis_moli"]);
        // The official firmware: none of ours, nothing set (it would go deaf).
        let official = ["okay_nabu", "hey_jarvis", "hey_mycroft", "stop"];
        assert_eq!(
            words_to_set(&wanted, &official, &owned(&["okay_nabu"])),
            None
        );
        // Ours: both, unless they are already active.
        let ours = ["hey_moli", "dis_moli", "stop"];
        assert_eq!(
            words_to_set(&wanted, &ours, &owned(&["hey_moli"])),
            Some(vec!["hey_moli", "dis_moli"])
        );
        assert_eq!(
            words_to_set(&wanted, &ours, &owned(&["dis_moli", "hey_moli"])),
            None
        );
        // A firmware with only one of them: that one.
        assert_eq!(
            words_to_set(&wanted, &["hey_moli"], &owned(&[])),
            Some(vec!["hey_moli"])
        );
    }
}

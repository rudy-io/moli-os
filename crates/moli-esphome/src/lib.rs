//! ESPHome devices over their native API (TCP 6053, Noise-encrypted),
//! without any ESPHome library: the transport is written from the Noise
//! specification ([`noise`], [`frame`]), the messages by hand ([`api`]).
//!
//! Two kinds of devices. Voice satellites such as the Home Assistant Voice
//! Preview Edition: their volume and mute (their media player), and what
//! their voice is doing. Any other device (a plug, a P1 meter reader…): its
//! sensors, binary sensors, text sensors and switches, one point each, keyed
//! by the entity's `object_id`; the device's own diagnostics (Wi-Fi signal,
//! uptime, address) are left out. The driver keeps one connection open (the
//! device restarts after 15 minutes without any client), answers its pings
//! and pings it every 20 s.

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
/// More entities than this from one device: something is wrong.
const MAX_ENTITIES: usize = 500;
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Just after Moli gave a device its key, the device may still answer in
/// clear for a moment: that is not a device that lost it.
const SWITCHING: Duration = Duration::from_secs(30);
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
    /// The device's own log in Moli's, from this level up: `none`,
    /// `error`, `warn`, `info`, `debug`.
    #[serde(default = "default_logs")]
    pub logs: String,
    /// A speaker that says the answers instead of the device (its
    /// `announce` point, e.g. a Sonos's); the device keeps its own sounds,
    /// and says an answer itself when the speaker does not.
    #[serde(default)]
    pub speaker: Option<String>,
}

fn default_logs() -> String {
    "warn".into()
}

/// The API's number for a log level (`None`: no log).
fn log_level(name: &str) -> Option<u64> {
    match name.trim().to_ascii_lowercase().as_str() {
        "error" => Some(1),
        "warn" | "warning" => Some(2),
        "info" => Some(3),
        "debug" => Some(5),
        _ => None,
    }
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
    entities: &Entities,
) -> Device {
    let points = if entities.satellite {
        satellite_points(entities)
    } else {
        entities.extras.iter().map(Extra::spec).collect()
    };
    let name = [&info.friendly_name, &info.name, &hello.name]
        .into_iter()
        .find(|n| !n.is_empty())
        .cloned()
        .unwrap_or_default();
    let model = if !info.model.is_empty() {
        info.model.clone()
    } else if entities.satellite {
        moli_i18n::tr!("pilotes.esphome.satellite")
    } else {
        "ESPHome".to_owned()
    };
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some(info.manufacturer.clone())
            .filter(|m| !m.is_empty())
            .map(Into::into),
        model: Some(model.into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// A voice satellite's points.
fn satellite_points(entities: &Entities) -> Vec<PointSpec> {
    let mut points = vec![
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
    ];
    if entities.mic_muted.is_some() {
        points.push(spec(
            MIC_MUTED,
            &moli_i18n::tr!("pilotes.esphome.micro_coupe"),
            Kind::Binary,
            true,
            None,
        ));
    }
    if entities.buttons_disabled.is_some() {
        points.push(spec(
            BUTTONS_DISABLED,
            &moli_i18n::tr!("pilotes.esphome.boutons_desactives"),
            Kind::Binary,
            true,
            None,
        ));
    }
    points
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
    let mut provisioned: Option<Instant> = None;
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
        // Moli holds its key: what speaks in clear at that address (the
        // device, reset, or another one) never gets a new one without a
        // person. It would replace the key the device holds, and go to
        // whoever answered (the MAC it gives is only its own word). Right
        // after Moli gave it, the device is only switching: try again.
        let switching = provisioned.is_some_and(|at| at.elapsed() < SWITCHING);
        if error.contains(IN_CLEAR) && psk.is_some() && !switching {
            let why = moli_i18n::tr!(
                "pilotes.esphome.en_clair",
                host = config.host,
                instance = ctx.instance(),
                key = API_KEY
            );
            wait_for_person(ctx, why).await;
            return Ok(());
        }
        let error = if error.contains(IN_CLEAR) && psk.is_none() {
            match provision(config, ctx).await {
                Ok(new) => {
                    psk = Some(new);
                    provisioned = Some(Instant::now());
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
/// a key the device holds and Moli lost. Only while Moli holds none: one it
/// holds is never replaced this way (see `run`).
async fn provision(config: &Config, ctx: &DriverCtx) -> Result<[u8; 32], Provision> {
    let mut tcp = connect(config).await?;
    let info = match plain_info(&mut tcp).await {
        Ok(info) => info,
        // It holds a key after all, one Moli does not know.
        Err(e) if format!("{e:#}").contains("asks for encryption") => {
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

/// The point of the firmware's switch that cuts the microphones.
const MIC_MUTED: &str = "mic_muted";
/// The point of the firmware's switch that disables its button and dial.
const BUTTONS_DISABLED: &str = "buttons_disabled";

/// What the session learnt of the device's entities.
#[derive(Debug, Default)]
struct Entities {
    /// A voice satellite (it has a voice or a media player): its own points,
    /// no generic ones.
    satellite: bool,
    media_player: Option<u32>,
    button: Option<u32>,
    mic_muted: Option<u32>,
    buttons_disabled: Option<u32>,
    /// Any other device: its entities, one point each.
    extras: Vec<Extra>,
}

/// How an entity's state becomes its point's value.
#[derive(Clone, Debug, PartialEq)]
enum Feed {
    /// A number, times `scale` (kW → W) and then rounded to `decimals`;
    /// as it arrives otherwise (the device's decimals are only a display
    /// hint, and energy is counted on an index).
    Sensor {
        scale: f64,
        decimals: Option<i32>,
    },
    Binary,
    Text,
    Switch,
}

/// An entity of a device that is not a satellite, and the point it feeds.
#[derive(Clone, Debug, PartialEq)]
struct Extra {
    key: u32,
    point: String,
    label: String,
    feed: Feed,
    unit: Option<Unit>,
    device_class: String,
}

impl Extra {
    /// The entities worth a point, keyed by `object_id` (else the name made
    /// a key), each key once. The device's own settings (`restart`,
    /// `factory_reset`…), its diagnostics and the entities the firmware
    /// hides by default are left out. Two kinds sharing a key: the first of
    /// sensor, binary sensor, text sensor, switch keeps it, the others take
    /// their kind as a suffix (`<key>_switch`), whatever the order of the
    /// list, so adding an entity to the firmware moves no other key.
    fn from_listed(listed: &[(u16, api::Listed)]) -> Vec<Self> {
        const ORDER: [u16; 4] = [
            api::LIST_SENSOR,
            api::LIST_BINARY_SENSOR,
            api::LIST_TEXT_SENSOR,
            api::LIST_SWITCH,
        ];
        let mut extras: Vec<Self> = Vec::new();
        let mut bases = std::collections::HashSet::new();
        for kind in ORDER {
            for (_, l) in listed.iter().filter(|(k, _)| *k == kind) {
                if l.category == api::CATEGORY_DIAGNOSTIC
                    || l.category == api::CATEGORY_CONFIG
                    || l.disabled_by_default
                {
                    continue;
                }
                let base = point_key(if l.object_id.is_empty() {
                    &l.name
                } else {
                    &l.object_id
                });
                if base.is_empty() {
                    continue;
                }
                // ESPHome keeps object_ids unique within a kind: a twin there
                // is a firmware mistake, and its entity gets no point.
                if !bases.insert((kind, base.clone())) {
                    continue;
                }
                let mut point = base.clone();
                if extras.iter().any(|e| e.point == point) {
                    point = format!("{base}_{}", kind_suffix(kind));
                }
                if extras.iter().any(|e| e.point == point) {
                    continue;
                }
                let (feed, unit) = match kind {
                    api::LIST_SENSOR => match l.unit.trim() {
                        "kW" => {
                            // In watts, three decimals fewer.
                            let decimals = (l.decimals >= 0).then(|| {
                                i32::try_from(l.decimals)
                                    .unwrap_or(0)
                                    .saturating_sub(3)
                                    .max(0)
                            });
                            (
                                Feed::Sensor {
                                    scale: 1000.0,
                                    decimals: Some(decimals.unwrap_or(1)),
                                },
                                Some(Unit::Watt),
                            )
                        }
                        other => (
                            Feed::Sensor {
                                scale: 1.0,
                                decimals: None,
                            },
                            Unit::parse(other),
                        ),
                    },
                    api::LIST_BINARY_SENSOR => (Feed::Binary, None),
                    api::LIST_TEXT_SENSOR => (Feed::Text, None),
                    _ => (Feed::Switch, None),
                };
                extras.push(Self {
                    key: l.key,
                    label: if l.name.is_empty() {
                        point.clone()
                    } else {
                        l.name.clone()
                    },
                    point,
                    feed,
                    unit,
                    device_class: l.device_class.clone(),
                });
            }
        }
        // The points in the order the device listed its entities.
        let rank = |key: u32| listed.iter().position(|(_, l)| l.key == key);
        extras.sort_by_key(|e| rank(e.key));
        extras
    }

    fn spec(&self) -> PointSpec {
        let kind = match self.feed {
            Feed::Sensor { .. } => Kind::Numeric {
                min: None,
                max: None,
                step: None,
            },
            Feed::Binary | Feed::Switch => Kind::Binary,
            Feed::Text => Kind::Text,
        };
        let write = self.feed == Feed::Switch;
        let mut spec = spec(&self.point, &self.label, kind, write, self.unit.clone());
        // What the device says it is beats a guess from the key.
        let semantic = match (&self.feed, self.device_class.as_str()) {
            (Feed::Switch, _) => Some(Semantic::OnOff),
            (Feed::Sensor { .. }, "humidity") => Some(Semantic::Humidity),
            (Feed::Sensor { .. }, "battery") => Some(Semantic::Battery),
            (Feed::Sensor { .. }, "temperature") => Some(Semantic::Temperature),
            (Feed::Binary, "motion" | "occupancy" | "presence") => Some(Semantic::Occupancy),
            (Feed::Binary, "moisture") => Some(Semantic::WaterLeak),
            (Feed::Binary, "smoke") => Some(Semantic::Smoke),
            _ => None,
        };
        if let Some(semantic) = semantic {
            spec.semantic = semantic;
        }
        spec
    }

    /// The point's value for a sensor's reading.
    fn number(&self, raw: f32) -> Option<Value> {
        let Feed::Sensor { scale, decimals } = self.feed else {
            return None;
        };
        // The f32's shortest decimal form: 531.473, not 531.4730224609375.
        let v = raw.to_string().parse::<f64>().ok()? * scale;
        let v = match decimals {
            Some(d) => {
                let f = 10f64.powi(d.min(9));
                (v * f).round() / f
            }
            None => v,
        };
        v.is_finite().then_some(Value::Float(v))
    }
}

/// The suffix of a key two kinds share.
fn kind_suffix(kind: u16) -> &'static str {
    match kind {
        api::LIST_BINARY_SENSOR => "binary",
        api::LIST_TEXT_SENSOR => "text",
        api::LIST_SWITCH => "switch",
        _ => "sensor",
    }
}

/// `LSC Plug 5 Prise` or `lsc_plug_5_prise` → `lsc_plug_5_prise`.
fn point_key(raw: &str) -> String {
    let mut key = String::with_capacity(raw.len());
    for c in raw.chars().flat_map(char::to_lowercase) {
        // A name in French keeps its letters: `Énergie` → `energie`.
        let c = match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ÿ' => 'y',
            other => other,
        };
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else if !key.ends_with('_') && !key.is_empty() {
            key.push('_');
        }
    }
    key.trim_end_matches('_').to_owned()
}

impl Entities {
    fn extra(&self, key: u32) -> Option<&Extra> {
        self.extras.iter().find(|e| e.key == key)
    }

    /// A switch of the firmware, if it is one Moli shows (by its name in the
    /// Voice PE's configuration; other firmwares simply have no such points).
    fn note_switch(&mut self, switch: &api::Entity) {
        match switch.name.as_str() {
            "Mute" => self.mic_muted = Some(switch.key),
            "Disable physical controls" => self.buttons_disabled = Some(switch.key),
            _ => {}
        }
    }

    /// The point a switch feeds.
    fn point_of_switch(&self, key: u32) -> Option<&'static str> {
        if self.mic_muted == Some(key) {
            Some(MIC_MUTED)
        } else if self.buttons_disabled == Some(key) {
            Some(BUTTONS_DISABLED)
        } else {
            None
        }
    }

    /// The switch behind a point.
    fn switch_of_point(&self, point: &str) -> Option<u32> {
        match point {
            MIC_MUTED => self.mic_muted,
            BUTTONS_DISABLED => self.buttons_disabled,
            _ => None,
        }
    }
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
    let mut listed = Vec::new();
    let mut seen = 0usize;
    loop {
        let (kind, payload) = next(&mut inbox, &mut tx).await?;
        seen += 1;
        if seen > MAX_ENTITIES {
            bail!("the device lists more than {MAX_ENTITIES} entities");
        }
        match kind {
            api::LIST_ENTITIES_DONE => break,
            api::LIST_MEDIA_PLAYER => entities.media_player = api::entity(&payload).map(|e| e.key),
            api::LIST_EVENT => entities.button = api::entity(&payload).map(|e| e.key),
            _ => {}
        }
        if kind == api::LIST_SWITCH
            && let Some(switch) = api::entity(&payload)
        {
            entities.note_switch(&switch);
        }
        if let Some(entity) = api::listed(kind, &payload) {
            listed.push((kind, entity));
        }
    }
    entities.satellite = info.voice_flags != 0 || entities.media_player.is_some();
    if !entities.satellite {
        entities.extras = Extra::from_listed(&listed);
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
    ctx.upsert_device(device(&id, ctx, &info, &hello, &entities));
    if entities.satellite {
        ctx.set_state(&id, "state", Value::Text("idle".into()));
    }
    ctx.set_availability(&id, true);
    *known = Some(id.clone());
    tracing::info!(
        instance = %ctx.instance(),
        device = %info.friendly_name,
        esphome = %info.esphome_version,
        voice_flags = info.voice_flags,
        points = entities.extras.len(),
        "esphome device connected"
    );
    tx.send(api::SUBSCRIBE_STATES, &[]).await?;
    if let Some(level) = log_level(&config.logs) {
        tx.send(api::SUBSCRIBE_LOGS, &api::subscribe_logs(level))
            .await?;
    }
    if entities.satellite {
        // Moli is the device's voice assistant (the first to subscribe keeps it).
        tx.send(api::SUBSCRIBE_VOICE_ASSISTANT, &api::subscribe_voice())
            .await?;
        // What the firmware offers first: the configured words are set only once
        // the device says it has them (see `wake_words`).
        tx.send(api::VOICE_CONFIGURATION_REQUEST, &[]).await?;
    }
    let mut words_set = false;
    let (mut voice, mut thoughts) = satellite::Voice::new(
        Duration::from_secs(config.window_s),
        config.media_base.clone(),
        id.clone(),
        config.speaker.clone(),
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
        api::SWITCH_STATE => {
            if let Some((key, on)) = api::switch_state(payload) {
                if let Some(point) = entities.point_of_switch(key) {
                    ctx.set_state(id, point, Value::Bool(on));
                } else if let Some(extra) = entities.extra(key) {
                    ctx.set_state(id, &extra.point, Value::Bool(on));
                }
            }
        }
        api::SENSOR_STATE => {
            if let Some((extra, value)) = api::sensor_state(payload).and_then(|(key, raw)| {
                let extra = entities.extra(key)?;
                Some((extra, extra.number(raw?)?))
            }) {
                ctx.set_state(id, &extra.point, value);
            }
        }
        api::BINARY_SENSOR_STATE => {
            if let Some((extra, on)) =
                api::binary_state(payload).and_then(|(key, on)| Some((entities.extra(key)?, on?)))
            {
                ctx.set_state(id, &extra.point, Value::Bool(on));
            }
        }
        api::TEXT_SENSOR_STATE => {
            if let Some((extra, text)) =
                api::text_state(payload).and_then(|(key, text)| Some((entities.extra(key)?, text?)))
            {
                ctx.set_state(id, &extra.point, Value::Text(text.into()));
            }
        }
        api::LOG => {
            if let Some((level, line)) = api::log_line(payload) {
                tracing::info!(instance = %ctx.instance(), level, "device: {line}");
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

/// A command for the device: one of its switches, the volume or mute of its
/// media player, or a sentence in Moli's voice (an announcement over what it
/// plays).
async fn order(
    ctx: &DriverCtx,
    tx: &mut Outbox,
    entities: &Entities,
    media_base: Option<&str>,
    command: &CommandRequest,
) -> Result<(), String> {
    if let (Some(switch), Value::Bool(on)) =
        (entities.switch_of_point(&command.key), &command.value)
    {
        return tx
            .send(api::SWITCH_COMMAND, &api::set_switch(switch, *on))
            .await
            .map_err(|e| format!("{e:#}"));
    }
    if let Some(extra) = entities.extras.iter().find(|e| *e.point == *command.key) {
        let (Feed::Switch, Value::Bool(on)) = (&extra.feed, &command.value) else {
            return Err(moli_i18n::tr!(
                "pilotes.esphome.lecture_seule",
                point = extra.point
            ));
        };
        return tx
            .send(api::SWITCH_COMMAND, &api::set_switch(extra.key, *on))
            .await
            .map_err(|e| format!("{e:#}"));
    }
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
                .and_then(|(brain, base)| brain.announcement_url(base, text))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use moli_core::{DriverStatus, InstanceId};
    use moli_net::proto::Writer;
    use moli_runtime::{Hub, HubOptions, MasterKey, spawn_driver};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    /// What answers at the device's address: in clear, with the expected
    /// MAC, and ready to take any key (a reset device, or another one).
    /// The flag rises when a key reaches it.
    async fn in_clear(mac: &'static str) -> (u16, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let given = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&given);
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let flag = Arc::clone(&flag);
                tokio::spawn(async move {
                    let mut first = [0u8; 1];
                    if tcp.peek(&mut first).await.is_err() {
                        return;
                    }
                    if first[0] != 0x00 {
                        // Moli's encrypted hello, answered in clear; then
                        // whatever comes, until Moli hangs up.
                        let _ = tcp.write_all(&[0x00, 0x00, 0x00]).await;
                        let _ = tcp.read_to_end(&mut Vec::new()).await;
                        return;
                    }
                    while let Ok((kind, _)) = frame::read_plain(&mut tcp).await {
                        let (answer, payload) = match kind {
                            api::HELLO_REQUEST => (api::HELLO_RESPONSE, Vec::new()),
                            api::DEVICE_INFO_REQUEST => (
                                api::DEVICE_INFO_RESPONSE,
                                Writer::new().str(3, mac).bool(19, true).finish(),
                            ),
                            api::NOISE_SET_KEY_REQUEST => {
                                flag.store(true, Ordering::SeqCst);
                                (
                                    api::NOISE_SET_KEY_RESPONSE,
                                    Writer::new().bool(1, true).finish(),
                                )
                            }
                            _ => continue,
                        };
                        if frame::send_plain(&mut tcp, answer, &payload).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (port, given)
    }

    #[tokio::test]
    async fn a_device_in_clear_never_replaces_the_key_moli_holds() {
        // The expected MAC: checking it is not enough to give a key.
        let (port, given) = in_clear("aa:bb:cc:dd:ee:ff").await;
        let vault = std::env::temp_dir().join(format!(
            "moli-esphome-secrets-{}-{port}.enc",
            std::process::id()
        ));
        let hub = Hub::new(HubOptions {
            secrets: Some((vault.clone(), MasterKey::generate().unwrap())),
            ..HubOptions::default()
        })
        .unwrap();
        let instance = InstanceId::from("voix");
        let held = noise::new_key_base64().unwrap();
        hub.store_secret(&instance, API_KEY, &held).await.unwrap();
        let config = Config {
            host: "127.0.0.1".into(),
            port,
            media_base: None,
            window_s: default_window(),
            wake_words: Vec::new(),
            mac: Some("AA:BB:CC:DD:EE:FF".into()),
            logs: "none".into(),
            speaker: None,
        };
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            instance.clone(),
            Arc::new(Esphome::new(config)),
            cancel.clone(),
        );
        let mut waiting = None;
        for _ in 0..500 {
            let status = hub
                .snapshot()
                .drivers
                .into_iter()
                .find(|d| d.instance == instance)
                .map(|d| d.status);
            if let Some(DriverStatus::Waiting { reason }) = status {
                waiting = Some(reason);
                break;
            }
            if given.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
        let _ = std::fs::remove_file(&vault);
        assert!(!given.load(Ordering::SeqCst), "no key sent to it");
        assert_eq!(
            hub.secret(&instance, API_KEY),
            Some(held),
            "the key Moli holds stays"
        );
        let reason = waiting.expect("a person is asked");
        assert!(
            reason.contains("moli-os secrets forget voix api_key"),
            "{reason}"
        );
    }

    #[test]
    fn the_switches_moli_shows_are_found_by_name() {
        let mut entities = Entities::default();
        for (key, name) in [
            (1, "Mute"),
            (2, "Disable physical controls"),
            (3, "Wake sound"),
        ] {
            entities.note_switch(&api::Entity {
                key,
                name: name.into(),
            });
        }
        assert_eq!(entities.point_of_switch(1), Some(MIC_MUTED));
        assert_eq!(entities.point_of_switch(2), Some(BUTTONS_DISABLED));
        assert_eq!(entities.point_of_switch(3), None);
        assert_eq!(entities.switch_of_point(MIC_MUTED), Some(1));
        assert_eq!(entities.switch_of_point(BUTTONS_DISABLED), Some(2));
        assert_eq!(entities.switch_of_point("volume"), None);
        // A firmware without them has no such points to write.
        let bare = Entities::default();
        assert_eq!(bare.switch_of_point(MIC_MUTED), None);
    }

    fn sensor(object_id: &str, key: u32, unit: &str, decimals: i64) -> (u16, api::Listed) {
        (
            api::LIST_SENSOR,
            api::Listed {
                object_id: object_id.into(),
                key,
                name: object_id.to_uppercase(),
                unit: unit.into(),
                decimals,
                ..api::Listed::default()
            },
        )
    }

    #[test]
    fn a_plug_or_a_meter_gets_a_point_per_entity() {
        let mut wifi = sensor("signal", 9, "dBm", 0);
        wifi.1.category = api::CATEGORY_DIAGNOSTIC;
        let mut hidden = sensor("debug", 10, "", -1);
        hidden.1.disabled_by_default = true;
        // The device's own settings never become points one could switch.
        let restart = (
            api::LIST_SWITCH,
            api::Listed {
                object_id: "restart".into(),
                key: 11,
                category: api::CATEGORY_CONFIG,
                ..api::Listed::default()
            },
        );
        let listed = vec![
            sensor("p1_momentary_active_import", 1, "kW", 3),
            sensor("p1mini_cumulative_active_import_day", 2, "kWh", 3),
            sensor("p1_gas_consumption", 3, "m³", 3),
            (
                api::LIST_SWITCH,
                api::Listed {
                    object_id: String::new(),
                    key: 4,
                    name: "LSC Plug 6 Prise".into(),
                    ..api::Listed::default()
                },
            ),
            (
                api::LIST_TEXT_SENSOR,
                api::Listed {
                    object_id: "p1_meter_id".into(),
                    key: 5,
                    ..api::Listed::default()
                },
            ),
            // A switch sharing a sensor's object_id, listed first: the
            // sensor keeps the key, the switch takes its kind.
            (
                api::LIST_SWITCH,
                api::Listed {
                    object_id: "p1_gas_consumption".into(),
                    key: 6,
                    ..api::Listed::default()
                },
            ),
            // A twin within one kind (a firmware mistake): no point.
            sensor("p1_momentary_active_import", 7, "kW", 3),
            wifi,
            hidden,
            restart,
        ];
        let extras = Extra::from_listed(&listed);
        let points: Vec<&str> = extras.iter().map(|e| e.point.as_str()).collect();
        assert_eq!(
            points,
            [
                "p1_momentary_active_import",
                "p1mini_cumulative_active_import_day",
                "p1_gas_consumption",
                "lsc_plug_6_prise",
                "p1_meter_id",
                "p1_gas_consumption_switch",
            ]
        );
        // kW becomes W: the dashboard and the energy page speak watts.
        let power = &extras[0];
        assert_eq!(power.unit, Some(Unit::Watt));
        assert_eq!(power.spec().semantic, Semantic::Power);
        assert_eq!(power.number(0.16), Some(Value::Float(160.0)));
        let energy = &extras[1];
        assert_eq!(energy.spec().semantic, Semantic::Energy);
        assert_eq!(energy.number(531.473), Some(Value::Float(531.473)));
        let switch = &extras[3];
        let spec = switch.spec();
        assert_eq!((spec.semantic, spec.access.write), (Semantic::OnOff, true));
        assert!(!extras[2].spec().access.write, "a sensor is read-only");
        assert_eq!(extras[4].number(1.0), None, "a text has no number");
        // As it arrives, whatever decimals the device announces.
        assert_eq!(extras[2].number(1234.567), Some(Value::Float(1234.567)));
        assert_eq!(extras[5].spec().semantic, Semantic::OnOff);
    }

    #[test]
    fn point_keys_are_plain() {
        assert_eq!(
            point_key("LSC Plug 5 Énergie du jour"),
            "lsc_plug_5_energie_du_jour"
        );
        assert_eq!(point_key("  Prise  "), "prise");
        assert_eq!(point_key("p1_voltage_phase_1"), "p1_voltage_phase_1");
        assert_eq!(point_key("…"), "");
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

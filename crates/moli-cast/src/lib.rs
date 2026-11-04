//! Google Cast speakers (Google Home, Nest Mini, Lenovo Smart Clock/Display…)
//! over TLS 8009: what plays, play/pause, volume, mute, and spoken sentences
//! (`say`, `announce`) in Moli's voice through the Default Media Receiver.

use std::time::Duration;

use moli_androidtv::cast::{self, Event, Order, Playing};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::PinnedTls;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

const CAST_PIN: &str = "cast_pin";
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);
const MAX_SAY: usize = 500;

fn default_port() -> u16 {
    cast::PORT
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub room: Option<String>,
    /// Moli as the speaker reaches it (`http://192.168.1.x:8790`): where the
    /// speaker fetches `/api/voice/<id>.flac` when it speaks.
    #[serde(default)]
    pub media_base: Option<String>,
}

#[derive(Debug)]
pub struct Cast {
    config: Config,
}

impl Cast {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Cast {
    fn kind(&self) -> &'static str {
        "cast"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn point(
    key: &str,
    label: &str,
    kind: Kind,
    write: bool,
    unit: Option<Unit>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

#[allow(clippy::too_many_lines)] // one entry per point
fn device(config: &Config, ctx: &DriverCtx, id: &DeviceId) -> Device {
    let name = config
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Google Home");
    let room = config
        .room
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Into::into);
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some("Google".into()),
        model: Some("Google Cast".into()),
        description: None,
        native_room: room,
        members: Vec::new(),
        points: vec![
            point(
                "playing",
                &moli_i18n::tr!("pilotes.cast.lecture"),
                Kind::Binary,
                true,
                None,
                Semantic::OnOff,
            ),
            point(
                "volume",
                &moli_i18n::tr!("pilotes.cast.volume"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                true,
                Some(Unit::Percent),
                Semantic::infer("volume", None),
            ),
            point(
                "mute",
                &moli_i18n::tr!("pilotes.cast.sourdine"),
                Kind::Binary,
                true,
                None,
                Semantic::infer("mute", None),
            ),
            point(
                "state",
                &moli_i18n::tr!("pilotes.cast.etat"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "title",
                &moli_i18n::tr!("pilotes.cast.titre"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "artist",
                &moli_i18n::tr!("pilotes.cast.artiste"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "image",
                &moli_i18n::tr!("pilotes.cast.pochette"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "next",
                &moli_i18n::tr!("pilotes.cast.suivant"),
                Kind::Binary,
                true,
                None,
                Semantic::Other,
            ),
            point(
                "previous",
                &moli_i18n::tr!("pilotes.cast.precedent"),
                Kind::Binary,
                true,
                None,
                Semantic::Other,
            ),
            point(
                "say",
                &moli_i18n::tr!("pilotes.cast.dire"),
                Kind::Text,
                true,
                None,
                Semantic::Other,
            ),
            point(
                "announce",
                &moli_i18n::tr!("pilotes.cast.annoncer"),
                Kind::Text,
                true,
                None,
                Semantic::Other,
            ),
            point(
                "play",
                &moli_i18n::tr!("pilotes.cast.jouer"),
                Kind::Text,
                true,
                None,
                Semantic::Other,
            ),
        ],
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let id = ctx.device_id("speaker");
    ctx.upsert_device(device(config, ctx, &id));
    ctx.set_availability(&id, false);
    ctx.ready();

    let identity_pin = ctx.secret("cast_device_pin");
    let pin = ctx.secret(CAST_PIN);
    let tls = PinnedTls::new(pin)?;
    let identity_pin = if let Some(pin) = identity_pin {
        pin
    } else {
        let identity = cast::enroll_device_port(&tls, &config.host, config.port).await?;
        ctx.store_secret("cast_device_pin", &identity).await?;
        identity
    };
    let (orders_tx, mut orders_rx) = mpsc::channel::<Order>(16);
    let (events_tx, mut events_rx) = mpsc::channel::<Event>(32);

    let host = config.host.clone();
    let port = config.port;
    let worker = tokio::spawn(async move {
        let mut pause = RETRY_MIN;
        let mut last_error: Option<String> = None;
        loop {
            let started = Instant::now();
            let result =
                cast::session_device_port(&host, port, &identity_pin, &mut orders_rx, &events_tx)
                    .await;
            if events_tx.is_closed() || result.is_ok() {
                return;
            }
            if let Err(e) = result {
                let msg = format!("{e:#}");
                if last_error.as_deref() == Some(msg.as_str()) {
                    tracing::debug!(host = %host, error = %msg, "cast speaker unreachable");
                } else {
                    tracing::warn!(host = %host, error = %msg, "cast speaker unreachable");
                    last_error = Some(msg);
                }
            }
            let _ = events_tx.send(Event::Lost).await;
            pause = if started.elapsed() > RETRY_MAX {
                RETRY_MIN
            } else {
                (pause * 2).min(RETRY_MAX)
            };
            tokio::time::sleep(pause).await;
        }
    });

    let mut online = false;
    loop {
        tokio::select! {
            event = events_rx.recv() => {
                let Some(event) = event else { break };
                match event {
                    Event::Playing(playing) => {
                        if !online {
                            online = true;
                            ctx.set_availability(&id, true);
                        }
                        publish_state(ctx, &id, &playing);
                    }
                    Event::Lost => {
                        if online {
                            online = false;
                            ctx.set_availability(&id, false);
                        }
                    }
                }
            }
            command = ctx.next_command() => {
                let Some(command) = command else { break };
                let result = handle_command(
                    ctx,
                    &orders_tx,
                    online,
                    config.media_base.as_deref(),
                    &command,
                )
                .await;
                command.reply(result);
            }
        }
    }
    worker.abort();
    Ok(())
}

fn publish_state(ctx: &DriverCtx, id: &DeviceId, p: &Playing) {
    let active = matches!(p.state.as_deref(), Some("playing" | "buffering"));
    ctx.set_state(id, "playing", Value::Bool(active));
    if let Some(v) = p.volume {
        ctx.set_state(id, "volume", Value::Float(v));
    }
    if let Some(m) = p.muted {
        ctx.set_state(id, "mute", Value::Bool(m));
    }
    let state = p.state.as_deref().unwrap_or("idle");
    ctx.set_state(id, "state", Value::Text(state.into()));
    ctx.set_state(
        id,
        "title",
        p.title
            .as_deref()
            .or(p.app.as_deref())
            .map_or(Value::Null, |s| Value::Text(s.into())),
    );
    ctx.set_state(
        id,
        "artist",
        p.subtitle
            .as_deref()
            .map_or(Value::Null, |s| Value::Text(s.into())),
    );
    ctx.set_state(
        id,
        "image",
        p.image
            .as_deref()
            .map_or(Value::Null, |s| Value::Text(s.into())),
    );
}

async fn handle_command(
    ctx: &DriverCtx,
    orders: &mpsc::Sender<Order>,
    online: bool,
    media_base: Option<&str>,
    command: &CommandRequest,
) -> Result<(), String> {
    if !online {
        return Err(moli_i18n::tr!("pilotes.cast.injoignable"));
    }
    let order = match (&*command.key, &command.value) {
        ("playing", Value::Bool(true)) => Order::Play,
        ("playing", Value::Bool(false)) => Order::Pause,
        ("next", _) => Order::Next,
        ("previous", _) => Order::Previous,
        ("volume", Value::Float(v)) => Order::Volume((*v / 100.0).clamp(0.0, 1.0)),
        ("volume", Value::Int(v)) => {
            #[allow(clippy::cast_precision_loss)]
            let level = (*v as f64 / 100.0).clamp(0.0, 1.0);
            Order::Volume(level)
        }
        ("mute", Value::Bool(m)) => Order::Mute(*m),
        ("say" | "announce", Value::Text(text)) => {
            let text = text.trim();
            if text.is_empty() || text.chars().count() > MAX_SAY {
                return Err(moli_i18n::tr!("pilotes.cast.phrase"));
            }
            let url = ctx
                .voice_brain()
                .zip(media_base)
                .and_then(|(brain, base)| brain.announcement_url(base, text))
                .ok_or_else(|| moli_i18n::tr!("pilotes.cast.sans_voix"))?;
            Order::Load {
                url,
                mime: "audio/flac".into(),
            }
        }
        ("play", Value::Text(phrase)) => {
            let music = ctx
                .music()
                .ok_or_else(|| moli_i18n::tr!("pilotes.cast.sans_musique"))?;
            let items = music
                .resolve(phrase.trim())
                .await?
                .iter()
                .map(|t| cast::Queued {
                    url: t.url.clone(),
                    mime: t.mime.clone(),
                    title: Some(t.title.clone()),
                    artist: Some(t.artist.clone()),
                    image: t.image.clone(),
                })
                .collect();
            Order::Queue { items }
        }
        (point, _) => {
            return Err(moli_i18n::tr!("pilotes.cast.lecture_seule", point = point));
        }
    };
    orders
        .try_send(order)
        .map_err(|_| moli_i18n::tr!("pilotes.cast.injoignable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_config_defaults() {
        let config = Config {
            host: "192.168.1.50".into(),
            port: default_port(),
            name: Some("Enceinte salon".into()),
            room: Some("Salon".into()),
            media_base: Some("http://192.168.1.10:8790".into()),
        };
        assert_eq!(config.port, 8009);
        assert_eq!(config.name.as_deref(), Some("Enceinte salon"));
        assert_eq!(config.room.as_deref(), Some("Salon"));
    }
}

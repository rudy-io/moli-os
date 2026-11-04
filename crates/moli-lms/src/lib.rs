//! Lyrion Music Server (LMS, formerly Logitech Media Server) and
//! piCorePlayer / SqueezeLite players over JSON-RPC (`POST /jsonrpc.js` on
//! HTTP 9000): play/pause, next/previous, volume, mute, current track
//! metadata (title, artist, cover art), and Qobuz music search (`play`).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::Full;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::Body as Bytes;
use moli_runtime::music::MusicService;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::time::MissedTickBehavior;

const DEFAULT_PORT: u16 = 9000;
const POLL: Duration = Duration::from_secs(2);
const HTTP_LIMIT: Duration = Duration::from_secs(5);
const MAX_BODY: usize = 1024 * 1024;

fn default_port() -> u16 {
    DEFAULT_PORT
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional player MAC (`b8:27:eb:…`) or name (`piCorePlayer`). When
    /// omitted, prefers a native `SqueezeLite` player, or the first player.
    #[serde(default)]
    pub player: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub room: Option<String>,
}

#[derive(Debug)]
pub struct Lms {
    config: Config,
}

impl Lms {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Lms {
    fn kind(&self) -> &'static str {
        "lms"
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
fn device(config: &Config, ctx: &DriverCtx, id: &DeviceId, discovered_name: &str) -> Device {
    let name = config
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(discovered_name);
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
        manufacturer: Some("Lyrion".into()),
        model: Some("piCorePlayer".into()),
        description: None,
        native_room: room,
        members: Vec::new(),
        points: vec![
            point(
                "playing",
                &moli_i18n::tr!("pilotes.lms.lecture"),
                Kind::Binary,
                true,
                None,
                Semantic::OnOff,
            ),
            point(
                "volume",
                &moli_i18n::tr!("pilotes.lms.volume"),
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
                &moli_i18n::tr!("pilotes.lms.sourdine"),
                Kind::Binary,
                true,
                None,
                Semantic::infer("mute", None),
            ),
            point(
                "state",
                &moli_i18n::tr!("pilotes.lms.etat"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "title",
                &moli_i18n::tr!("pilotes.lms.titre"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "artist",
                &moli_i18n::tr!("pilotes.lms.artiste"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "image",
                &moli_i18n::tr!("pilotes.lms.pochette"),
                Kind::Text,
                false,
                None,
                Semantic::Other,
            ),
            point(
                "next",
                &moli_i18n::tr!("pilotes.lms.suivant"),
                Kind::Binary,
                true,
                None,
                Semantic::Other,
            ),
            point(
                "previous",
                &moli_i18n::tr!("pilotes.lms.precedent"),
                Kind::Binary,
                true,
                None,
                Semantic::Other,
            ),
            point(
                "play",
                &moli_i18n::tr!("pilotes.lms.jouer"),
                Kind::Text,
                true,
                None,
                Semantic::Other,
            ),
        ],
    }
}

async fn rpc(host: &str, port: u16, player: &str, cmd: &[&str]) -> anyhow::Result<Json> {
    let body = json!({
        "id": 1,
        "method": "slim.request",
        "params": [player, cmd],
    })
    .to_string();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/jsonrpc.js")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))?;
    let (status, bytes) = moli_net::web(host, port, false, req, HTTP_LIMIT, MAX_BODY).await?;
    if !status.is_success() {
        bail!("LMS HTTP {}", status.as_u16());
    }
    let parsed: Json = serde_json::from_slice(&bytes).context("invalid LMS JSON")?;
    Ok(parsed["result"].clone())
}

/// Discovers the target player's `(playerid, name)` from `players 0 50`.
async fn discover_player(config: &Config) -> anyhow::Result<(String, String)> {
    let res = rpc(&config.host, config.port, "", &["players", "0", "50"]).await?;
    let players = res["players_loop"]
        .as_array()
        .context("no players_loop from LMS")?;
    pick_player(players, config.player.as_deref())
        .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("pilotes.lms.aucun_lecteur")))
}

fn pick_player(players: &[Json], wanted: Option<&str>) -> Option<(String, String)> {
    let wanted = wanted.map(str::trim).filter(|s| !s.is_empty());
    let chosen = if let Some(w) = wanted {
        players.iter().find(|p| {
            p["playerid"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(w))
                || p["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(w))
        })
    } else {
        players
            .iter()
            .find(|p| p["modelname"].as_str() == Some("SqueezeLite"))
            .or_else(|| players.first())
    }?;
    let id = chosen["playerid"].as_str()?.to_owned();
    let name = chosen["name"].as_str().unwrap_or("piCorePlayer").to_owned();
    Some((id, name))
}

fn artwork_url(host: &str, port: u16, raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Some(raw.to_owned());
    }
    if let Some(rest) = raw.strip_prefix("/imageproxy/")
        && let Some((encoded, _)) = rest.split_once('/')
        && let Some(decoded) = pct_decode(encoded)
        && (decoded.starts_with("https://") || decoded.starts_with("http://"))
    {
        return Some(decoded);
    }
    let path = if raw.starts_with('/') {
        raw.to_owned()
    } else {
        format!("/{raw}")
    };
    Some(format!("http://{host}:{port}{path}"))
}

fn pct_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_status(host: &str, port: u16, res: &Json) -> (bool, Vec<(&'static str, Value)>) {
    let connected = res["player_connected"].as_i64().unwrap_or(1) != 0;
    let mode = res["mode"].as_str().unwrap_or("stop");
    let playing = mode == "play";
    let state = match mode {
        "play" => "playing",
        "pause" => "paused",
        _ => "idle",
    };
    let track = res["playlist_loop"]
        .as_array()
        .and_then(|items| items.first())
        .or_else(|| res.get("remoteMeta").filter(|v| v.is_object()));
    let text = |key: &str| {
        track
            .and_then(|t| t[key].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let title = text("title");
    let artist = text("artist");
    let image = artwork_url(
        host,
        port,
        track.and_then(|t| t["artwork_url"].as_str().or_else(|| t["cover"].as_str())),
    );
    let raw_vol = res["mixer volume"]
        .as_f64()
        .or_else(|| res["mixer volume"].as_str()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    let muted = raw_vol < 0.0;
    let volume = raw_vol.abs().clamp(0.0, 100.0);
    (
        connected,
        vec![
            ("playing", Value::Bool(playing)),
            ("state", Value::Text(state.into())),
            ("volume", Value::Float(volume)),
            ("mute", Value::Bool(muted)),
            (
                "title",
                title.map_or(Value::Null, |s| Value::Text(s.into())),
            ),
            (
                "artist",
                artist.map_or(Value::Null, |s| Value::Text(s.into())),
            ),
            (
                "image",
                image.map_or(Value::Null, |s| Value::Text(s.into())),
            ),
        ],
    )
}

async fn apply(
    host: &str,
    port: u16,
    player: &str,
    music: Option<Arc<dyn MusicService>>,
    command: &CommandRequest,
) -> anyhow::Result<()> {
    match (&*command.key, &command.value) {
        ("playing", Value::Bool(true)) => {
            rpc(host, port, player, &["play"]).await?;
        }
        ("playing", Value::Bool(false)) => {
            rpc(host, port, player, &["pause", "1"]).await?;
        }
        ("next", _) => {
            rpc(host, port, player, &["playlist", "index", "+1"]).await?;
        }
        ("previous", _) => {
            rpc(host, port, player, &["playlist", "index", "-1"]).await?;
        }
        ("mute", Value::Bool(m)) => {
            rpc(
                host,
                port,
                player,
                &["mixer", "muting", if *m { "1" } else { "0" }],
            )
            .await?;
        }
        ("volume", v) => {
            #[allow(clippy::cast_possible_truncation)]
            let level = v.as_f64().context("volume")?.round().clamp(0.0, 100.0) as i64;
            rpc(host, port, player, &["mixer", "volume", &level.to_string()]).await?;
        }
        ("play", Value::Text(phrase)) => {
            let music = music.context(moli_i18n::tr!("pilotes.lms.sans_musique"))?;
            let tracks = music
                .resolve(phrase.trim())
                .await
                .map_err(anyhow::Error::msg)?;
            rpc(host, port, player, &["power", "1"]).await?;
            for (idx, track) in tracks.iter().enumerate() {
                let action = if idx == 0 { "play" } else { "add" };
                let uri = if track.id > 0 {
                    format!("qobuz://{}.flac", track.id)
                } else {
                    track.url.clone()
                };
                rpc(
                    host,
                    port,
                    player,
                    &["playlist", action, &uri, &track.title],
                )
                .await?;
            }
        }
        (point, _) => bail!(moli_i18n::tr!("pilotes.lms.lecture_seule", point = point)),
    }
    Ok(())
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (player_id, discovered_name) = discover_player(config)
        .await
        .with_context(|| format!("lms {}:{}", config.host, config.port))?;
    let slug = player_id.replace(':', "");
    let id = ctx.device_id(&slug);
    ctx.upsert_device(device(config, ctx, &id, &discovered_name));
    ctx.ready();

    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut online = None;

    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = apply(&config.host, config.port, &player_id, ctx.music(), &command)
                    .await
                    .map_err(|e| format!("{e:#}"));
                command.reply(result);
            }
            _ = tick.tick() => {
                match rpc(&config.host, config.port, &player_id, &["status", "-", "1", "tags:aKlNu"]).await {
                    Ok(res) => {
                        let (connected, values) = parse_status(&config.host, config.port, &res);
                        if online != Some(connected) {
                            online = Some(connected);
                            ctx.set_availability(&id, connected);
                        }
                        for (k, v) in values {
                            ctx.set_state(&id, k, v);
                        }
                    }
                    Err(e) => {
                        if online != Some(false) {
                            online = Some(false);
                            ctx.set_availability(&id, false);
                            tracing::debug!(host = %config.host, error = %e, "lms unreachable");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_squeezelite_by_default_or_named_player() {
        let players = json!([
            { "playerid": "aa:bb:cc:dd:ee:01", "name": "Home", "modelname": "CastBridge" },
            { "playerid": "aa:bb:cc:dd:ee:ff", "name": "piCorePlayer", "modelname": "SqueezeLite" }
        ]);
        let list = players.as_array().unwrap();
        assert_eq!(
            pick_player(list, None),
            Some(("aa:bb:cc:dd:ee:ff".into(), "piCorePlayer".into()))
        );
        assert_eq!(
            pick_player(list, Some("Home")),
            Some(("aa:bb:cc:dd:ee:01".into(), "Home".into()))
        );
    }

    #[test]
    fn unwraps_qobuz_imageproxy_and_parses_status() {
        let status = json!({
            "player_connected": 1,
            "mode": "play",
            "mixer volume": 80,
            "playlist_loop": [{
                "title": "Tereza My Love",
                "artist": "Antonio Carlos Jobim",
                "artwork_url": "/imageproxy/https%3A%2F%2Fstatic.qobuz.com%2Fimages%2Fcovers%2Fcover.jpg/image.jpg"
            }]
        });
        let (connected, values) = parse_status("192.168.1.10", 9000, &status);
        assert!(connected);
        assert!(values.contains(&("playing", Value::Bool(true))));
        assert!(values.contains(&("title", Value::Text("Tereza My Love".into()))));
        assert!(values.contains(&(
            "image",
            Value::Text("https://static.qobuz.com/images/covers/cover.jpg".into())
        )));
    }
}

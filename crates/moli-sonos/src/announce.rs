//! Spoken announcements: text → speech (a local Piper server) → a sound Moli
//! publishes for a few minutes → played by the speaker as an *audio clip*,
//! over what it was playing (the music dips, then comes back by itself).
//!
//! The audio clip goes through the speaker's local WebSocket API (port 1443,
//! the public key every local client uses): the household id, then the group
//! list to find this player, then `loadAudioClip`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use moli_net::PinnedTls;
use moli_net::ws::WebSocket;
use moli_runtime::MediaPublisher;
use moli_runtime::media::Image;
use serde::Deserialize;
use serde_json::{Value, json};

const WS_PORT: u16 = 1443;
const WS_PATH: &str = "/websocket/api";
/// The local API's public key (the same for every local client).
const API_KEY: &str = "123e4567-e89b-12d3-a456-426655440000";
const PROTOCOL: &str = "v1.api.smartspeaker.audio";
/// An announcement is a sentence or two, not a speech.
pub const MAX_TEXT: usize = 500;

/// `[driver.options.announce]`
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Moli as the speaker reaches it (`http://192.168.0.62:8790`): the
    /// speaker fetches the sound from there.
    pub media_base: String,
    /// The Wyoming speech server (Piper), `host:port`.
    #[serde(default = "default_tts")]
    pub tts: String,
    /// Piper voice (default: the server's).
    #[serde(default)]
    pub voice: Option<String>,
    /// Volume of the announcement, 0–100 (the speaker's own volume comes
    /// back afterwards). Home Assistant's script used 35.
    #[serde(default = "default_volume")]
    pub volume: u8,
}

fn default_tts() -> String {
    "127.0.0.1:10200".into()
}

fn default_volume() -> u8 {
    35
}

#[derive(Debug)]
pub struct Announcer {
    host: String,
    /// This player's id (`RINCON_…`, the UDN).
    player: String,
    config: Config,
    /// The speaker's certificate, pinned at first contact.
    pin: Option<String>,
    publisher: MediaPublisher,
    /// One announcement at a time, in order.
    turn: tokio::sync::Mutex<()>,
    /// Announcements waiting or playing.
    waiting: std::sync::atomic::AtomicUsize,
}

/// Announcements waiting at most: past it, a new one is refused (a burst
/// of alerts must not keep the speaker talking for minutes).
pub const MAX_WAITING: usize = 3;
/// An announcement that could not start within this delay is dropped: it
/// is no longer news.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

impl Announcer {
    pub fn new(
        host: &str,
        player: &str,
        config: Config,
        pin: Option<String>,
        publisher: MediaPublisher,
    ) -> Arc<Self> {
        Arc::new(Self {
            host: host.to_owned(),
            player: player.to_owned(),
            config,
            pin,
            publisher,
            turn: tokio::sync::Mutex::new(()),
            waiting: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Queues `text` and answers at once: the announcement itself takes
    /// seconds (speech, then the clip). Refused when too many are waiting;
    /// dropped if it could not start in time; the house is told of a failure.
    pub fn enqueue(self: &Arc<Self>, text: String) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return Err(moli_i18n::tr!(
                "pilotes.sonos.file_pleine",
                count = MAX_WAITING
            ));
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let since = tokio::time::Instant::now();
            let outcome = {
                let _turn = this.turn.lock().await;
                if since.elapsed() > STALE_AFTER {
                    Err(anyhow::anyhow!(moli_i18n::tr!(
                        "pilotes.sonos.trop_tard",
                        seconds = STALE_AFTER.as_secs()
                    )))
                } else {
                    this.announce(&text).await
                }
            };
            this.waiting.fetch_sub(1, Ordering::SeqCst);
            if let Err(e) = outcome {
                tracing::warn!(error = %format!("{e:#}"), "announcement not played");
                this.publisher.tell(
                    &moli_i18n::tr!("pilotes.sonos.annonce_vocale"),
                    moli_i18n::tr!(
                        "pilotes.sonos.non_annoncee",
                        text = short(&text),
                        error = format!("{e:#}")
                    ),
                );
            }
        });
        Ok(())
    }

    /// Says `text` on this speaker (the caller holds the turn).
    async fn announce(&self, text: &str) -> anyhow::Result<()> {
        let (tts_host, tts_port) = self
            .config
            .tts
            .rsplit_once(':')
            .and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?)))
            .context("tts must be host:port")?;
        let wav = moli_net::wyoming::synthesize(
            tts_host,
            tts_port,
            self.config.voice.as_deref(),
            text,
            Duration::from_secs(30),
        )
        .await?;
        let name = self
            .publisher
            .publish(
                "wav",
                Image {
                    content_type: "audio/wav".into(),
                    bytes: wav,
                },
            )
            .context("no random name for the sound")?;
        let url = format!(
            "{}/api/media/{name}",
            self.config.media_base.trim_end_matches('/')
        );
        self.play(&url).await
    }

    async fn play(&self, url: &str) -> anyhow::Result<()> {
        let tls = PinnedTls::new(self.pin.clone())?;
        let mut ws = WebSocket::connect(
            &self.host,
            WS_PORT,
            WS_PATH,
            &[
                ("X-Sonos-Api-Key", API_KEY),
                ("Sec-WebSocket-Protocol", PROTOCOL),
            ],
            &tls,
        )
        .await?;
        // An empty request: the speaker answers with an error, but its
        // header carries the household id (what sonos-websocket does too).
        let (header, _) = exchange(&mut ws, json!([{}, {}])).await?;
        let household = header
            .get("householdId")
            .and_then(Value::as_str)
            .context("no household id")?
            .to_owned();
        let (_, groups) = ask(
            &mut ws,
            json!([{ "namespace": "groups:1", "command": "getGroups", "householdId": household }, {}]),
        )
        .await?;
        let player = player_id(&groups, &self.player, &self.host)
            .context("this speaker is not in its household's player list")?;
        // The clip's fate (fetched, played, failed) comes as events.
        if let Err(e) = ask(
            &mut ws,
            json!([{ "namespace": "audioClip:1", "command": "subscribe", "playerId": player }, {}]),
        )
        .await
        {
            tracing::debug!(error = %format!("{e:#}"), "audio clip events not subscribed");
        }
        let (header, body) = ask(
            &mut ws,
            json!([
                { "namespace": "audioClip:1", "command": "loadAudioClip", "playerId": player },
                {
                    "name": "Moli",
                    "appId": "local.moli.os",
                    "streamUrl": url,
                    "volume": self.config.volume.min(100),
                }
            ]),
        )
        .await?;
        refused(&header, &body)?;
        tracing::info!(clip = %body, "announcement accepted by the speaker");
        follow(&mut ws).await
    }
}

/// Listens to the clip a little: a speaker that cannot fetch or play the
/// sound says so only there.
async fn follow(ws: &mut WebSocket) -> anyhow::Result<()> {
    let until = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let left = until.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        let Ok(text) = ws.recv_text(left).await else {
            return Ok(());
        };
        let event: Value = serde_json::from_str(&text).unwrap_or_default();
        let clips = event
            .get(1)
            .and_then(|b| b.get("audioClips"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for clip in &clips {
            let status = clip.get("status").and_then(Value::as_str).unwrap_or("");
            tracing::info!(status, clip = %clip, "announcement status");
            match status {
                "ERROR" => bail!(moli_i18n::tr!(
                    "pilotes.sonos.lecture_impossible",
                    reason = clip.get("errorCode").and_then(Value::as_str).map_or_else(
                        || moli_i18n::tr!("pilotes.sonos.sans_raison"),
                        str::to_owned
                    )
                )),
                "DONE" | "DISMISSED" => return Ok(()),
                _ => {}
            }
        }
    }
}

/// One request, one answer: `[header, body]`, a refusal being an error.
async fn ask(ws: &mut WebSocket, request: Value) -> anyhow::Result<(Value, Value)> {
    let (header, body) = exchange(ws, request).await?;
    refused(&header, &body)?;
    Ok((header, body))
}

/// One request, one answer, whatever it says.
async fn exchange(ws: &mut WebSocket, request: Value) -> anyhow::Result<(Value, Value)> {
    ws.send_text(&request.to_string()).await?;
    let answer: Value = serde_json::from_str(&ws.recv_text(Duration::from_secs(10)).await?)
        .context("the speaker's answer is not JSON")?;
    let header = answer.get(0).cloned().unwrap_or_default();
    let body = answer.get(1).cloned().unwrap_or_default();
    Ok((header, body))
}

/// The speaker's way of saying no.
fn refused(header: &Value, body: &Value) -> anyhow::Result<()> {
    let error = header.get("success") == Some(&Value::Bool(false))
        || body.get("_objectType").and_then(Value::as_str) == Some("globalError");
    if error {
        bail!(moli_i18n::tr!(
            "pilotes.sonos.refus_enceinte",
            reason = body
                .get("errorCode")
                .or_else(|| body.get("reason"))
                .and_then(Value::as_str)
                .map_or_else(
                    || moli_i18n::tr!("pilotes.sonos.erreur_inconnue"),
                    str::to_owned
                )
        ));
    }
    Ok(())
}

/// This player in `getGroups`: by id (the UDN), else by its websocket URL.
fn player_id(groups: &Value, udn: &str, host: &str) -> Option<String> {
    let players = groups.get("players")?.as_array()?;
    let url = format!("wss://{host}:{WS_PORT}{WS_PATH}");
    players
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some(udn))
        .or_else(|| {
            players
                .iter()
                .find(|p| p.get("websocketUrl").and_then(Value::as_str) == Some(url.as_str()))
        })
        .and_then(|p| p.get("id").and_then(Value::as_str))
        .map(str::to_owned)
}

/// What an announcement may say.
/// The start of an announcement, for a message about it.
fn short(text: &str) -> String {
    let mut s: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        s.push('…');
    }
    s
}

pub fn check_text(text: &str) -> anyhow::Result<&str> {
    let text = text.trim();
    ensure!(
        !text.is_empty(),
        moli_i18n::tr!("pilotes.sonos.rien_a_dire")
    );
    ensure!(
        text.chars().count() <= MAX_TEXT,
        moli_i18n::tr!("pilotes.sonos.annonce_longueur", max = MAX_TEXT)
    );
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_this_player_and_reads_refusals() {
        let groups = json!({ "players": [
            { "id": "RINCON_AAA01400", "websocketUrl": "wss://192.168.0.10:1443/websocket/api" },
            { "id": "RINCON_BBB01400", "websocketUrl": "wss://192.168.0.11:1443/websocket/api" }
        ]});
        assert_eq!(
            player_id(&groups, "RINCON_BBB01400", "x").as_deref(),
            Some("RINCON_BBB01400")
        );
        assert_eq!(
            player_id(&groups, "other", "192.168.0.10").as_deref(),
            Some("RINCON_AAA01400")
        );
        assert_eq!(player_id(&groups, "other", "192.168.0.99"), None);
        assert!(refused(&json!({"success": true}), &json!({})).is_ok());
        let no = refused(
            &json!({"success": false}),
            &json!({"_objectType": "globalError", "errorCode": "ERROR_INVALID_PARAMETER"}),
        );
        assert!(format!("{no:?}").contains("ERROR_INVALID_PARAMETER"));
        assert!(check_text("  ").is_err());
        assert!(check_text(&"a".repeat(MAX_TEXT + 1)).is_err());
        assert_eq!(check_text(" Quelqu'un sonne ").unwrap(), "Quelqu'un sonne");
    }
}

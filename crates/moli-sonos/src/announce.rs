//! Spoken announcements on a Sonos (the queue, the silence, the level and
//! the speech are `moli-announce`'s): the sound Moli publishes for a few
//! minutes, played by the speaker as an *audio clip*, over what it was
//! playing (the music dips, then comes back by itself).
//!
//! The audio clip goes through the speaker's local WebSocket API (port 1443,
//! the public key every local client uses): the household id, then the group
//! list to find this player, then `loadAudioClip`.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_announce::Speech;
use moli_net::PinnedTls;
use moli_net::ws::WebSocket;
use moli_runtime::{BoxFuture, MediaPublisher};
use serde::Deserialize;
use serde_json::{Value, json};

const WS_PORT: u16 = 1443;
const WS_PATH: &str = "/websocket/api";
/// The local API's public key (the same for every local client).
const API_KEY: &str = "123e4567-e89b-12d3-a456-426655440000";
const PROTOCOL: &str = "v1.api.smartspeaker.audio";

/// `[driver.options.announce]`
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Moli as the speaker reaches it (`http://192.168.0.62:8790`): the
    /// speaker fetches the sound from there.
    pub media_base: String,
    /// The Wyoming speech server (Piper), `host:port`.
    #[serde(default = "moli_announce::default_tts")]
    pub tts: String,
    /// Piper voice (default: the server's).
    #[serde(default)]
    pub voice: Option<String>,
    /// Volume of the announcement, 0–100 (the speaker's own volume comes
    /// back afterwards). Home Assistant's script used 35. Where the
    /// `announce_volume` point starts; once written, the point is kept.
    #[serde(default = "default_volume")]
    pub volume: u8,
}

fn default_volume() -> u8 {
    35
}

impl Config {
    /// Where the speech is made.
    pub fn speech(&self) -> Speech {
        Speech {
            tts: self.tts.clone(),
            voice: self.voice.clone(),
        }
    }
}

/// A Sonos as `moli-announce` sees it: a speaker that fetches the sound.
#[derive(Debug)]
pub struct Clip {
    pub host: String,
    /// This player's id (`RINCON_…`, the UDN).
    pub player: String,
    /// Moli as the speaker reaches it.
    pub media_base: String,
    /// The speaker's certificate, pinned at first contact.
    pub pin: Option<String>,
    pub publisher: MediaPublisher,
}

impl moli_announce::Speaker for Clip {
    fn play(&self, wav: Vec<u8>, level: u8) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            let url = moli_announce::publish_sound(&self.publisher, &self.media_base, wav)?;
            self.clip(&url, level).await
        })
    }
}

impl Clip {
    async fn clip(&self, url: &str, level: u8) -> anyhow::Result<()> {
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
                    "volume": level,
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
    }
}

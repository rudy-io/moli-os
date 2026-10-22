//! Somfy's event feed: a websocket (`websocket.myfox.io`) that pushes what
//! the app shows live: mode changes, alarms, devices' status. Each message
//! carrying a `message_id` is acknowledged, as the app does.
//!
//! Runs in its own task with the latest token (`watch`); a token the feed
//! refuses is reported, and the feed waits for the next one.

use std::time::Duration;

use moli_net::ws::WebSocket;
use serde_json::{Map, Value as Json};
use tokio::sync::{mpsc, watch};

use crate::cloud::escape;

const HOST: &str = "websocket.myfox.io";
const PATH: &str = "/events/websocket";
/// Quiet this long: a ping keeps the connection alive.
const QUIET: Duration = Duration::from_secs(45);
/// Reconnection, doubled after each failure.
const FIRST_RETRY: Duration = Duration::from_secs(5);
const LAST_RETRY: Duration = Duration::from_secs(300);

/// What the feed tells the driver.
#[derive(Debug)]
pub enum Up {
    Connected,
    Change(Change),
    /// The token was refused: a fresh one is needed.
    TokenRefused,
}

/// A message, as far as Moli reads it.
#[derive(Debug, PartialEq)]
pub enum Change {
    /// A site's new level (`disarmed`, `partial`, `armed`).
    Level { site: String, level: String },
    /// The alarm rings (`trespass`, `panic`, `smoke`) or stops (`none`).
    Alarm { site: String, kind: &'static str },
    /// A device's status (battery, radio…), as in the device list.
    Status {
        device: String,
        status: Map<String, Json>,
    },
    /// Anything else: its key only.
    Other(String),
}

/// One message of the feed.
pub fn read(message: &Json) -> Option<Change> {
    let key = message["key"].as_str()?;
    let text = |name: &str| message[name].as_str().map(str::to_owned);
    Some(match key {
        "security.level.change" => Change::Level {
            site: text("site_id")?,
            level: text("security_level")?,
        },
        "alarm.trespass" | "alarm.panic" | "alarm.domestic.fire" | "alarm.end" => Change::Alarm {
            site: text("site_id")?,
            kind: match key {
                "alarm.trespass" => "trespass",
                "alarm.panic" => "panic",
                "alarm.domestic.fire" => "smoke",
                _ => "none",
            },
        },
        "device.status" => Change::Status {
            device: text("device_id")?,
            status: message.as_object().cloned().unwrap_or_default(),
        },
        other => Change::Other(other.to_owned()),
    })
}

/// The feed, for as long as the driver listens.
pub async fn run(mut token: watch::Receiver<String>, up: mpsc::Sender<Up>, instance: String) {
    let mut retry = FIRST_RETRY;
    loop {
        let current = token.borrow_and_update().clone();
        match listen(&current, &up).await {
            Ok(Ended::Closed) => return,
            Ok(Ended::TokenRefused) => {
                if up.send(Up::TokenRefused).await.is_err() || token.changed().await.is_err() {
                    return;
                }
                continue;
            }
            Ok(Ended::Dropped) => retry = FIRST_RETRY,
            Err(e) => {
                tracing::info!(
                    instance,
                    error = format!("{e:#}"),
                    retry_s = retry.as_secs(),
                    "somfy: event feed lost"
                );
            }
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(LAST_RETRY);
    }
}

enum Ended {
    /// The driver stopped listening.
    Closed,
    TokenRefused,
    /// The connection lived, then went: reconnect soon.
    Dropped,
}

async fn listen(token: &str, up: &mpsc::Sender<Up>) -> anyhow::Result<Ended> {
    let path = format!("{PATH}?token={}", escape(token));
    let mut ws = WebSocket::connect_public(HOST, 443, &path, &[]).await?;
    let mut ready = false;
    loop {
        let text = match ws.recv_text(QUIET).await {
            Ok(text) => text,
            Err(e) if e.to_string().contains("in time") => {
                ws.ping().await?;
                continue;
            }
            Err(e) if ready => {
                tracing::debug!(error = %e, "somfy: event feed closed");
                return Ok(Ended::Dropped);
            }
            Err(e) => return Err(e),
        };
        if text.trim() == "websocket.error.token" {
            return Ok(Ended::TokenRefused);
        }
        let Ok(message) = serde_json::from_str::<Json>(&text) else {
            continue;
        };
        if let Some(id) = message["message_id"].as_str() {
            let ack = serde_json::json!({ "ack": true, "message_id": id, "client": "Android" });
            ws.send_text(&ack.to_string()).await?;
        }
        let sent = if message["key"] == "websocket.connection.ready" {
            ready = true;
            up.send(Up::Connected).await
        } else if let Some(change) = read(&message) {
            up.send(Up::Change(change)).await
        } else {
            Ok(())
        };
        if sent.is_err() {
            return Ok(Ended::Closed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_changes_and_alarms_are_read() {
        assert_eq!(
            read(
                &json!({ "key": "security.level.change", "site_id": "s", "security_level": "armed" })
            ),
            Some(Change::Level {
                site: "s".into(),
                level: "armed".into()
            })
        );
        let alarm = |key: &str| read(&json!({ "key": key, "site_id": "s", "device_id": "d" }));
        assert_eq!(
            alarm("alarm.trespass"),
            Some(Change::Alarm {
                site: "s".into(),
                kind: "trespass"
            })
        );
        assert_eq!(
            alarm("alarm.domestic.fire"),
            Some(Change::Alarm {
                site: "s".into(),
                kind: "smoke"
            })
        );
        assert_eq!(
            alarm("alarm.end"),
            Some(Change::Alarm {
                site: "s".into(),
                kind: "none"
            })
        );
    }

    #[test]
    fn a_device_status_carries_its_fields() {
        let Some(Change::Status { device, status }) = read(&json!({
            "key": "device.status", "site_id": "s", "device_id": "d", "battery_level": 40
        })) else {
            panic!("not a status");
        };
        assert_eq!(device, "d");
        assert_eq!(status["battery_level"], 40);
    }

    #[test]
    fn other_or_incomplete_messages_are_not_guessed() {
        assert_eq!(
            read(&json!({ "key": "presence_in", "site_id": "s" })),
            Some(Change::Other("presence_in".into()))
        );
        assert_eq!(read(&json!({ "key": "security.level.change" })), None);
        assert_eq!(read(&json!({ "no": "key" })), None);
    }
}

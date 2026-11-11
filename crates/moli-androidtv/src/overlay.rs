//! TvOverlay (an app for Android TVs, from its store; REST on port 5001):
//! notifications drawn over whatever plays, in a corner, for some seconds:
//! a title, a message, a picture, or a live video (RTSP, HLS). The app
//! needs to draw over other apps, a permission a TV only grants through
//! ADB (`appops set com.tabdeveloper.tvoverlay SYSTEM_ALERT_WINDOW allow`).

use anyhow::{Context as _, bail};
use serde_json::{Value as Json, json};

pub const PORT: u16 = 5001;
/// TvOverlay's own fields: anything else is a mistake said at once, not a
/// notification shown without it.
const FIELDS: [&str; 11] = [
    "id",
    "title",
    "message",
    "source",
    "image",
    "video",
    "largeIcon",
    "smallIcon",
    "smallIconColor",
    "corner",
    "duration",
];

/// The notification for a written value: a JSON object of TvOverlay's
/// fields, or plain text (the message). Moli is the source unless said.
pub fn notification(value: &str) -> anyhow::Result<Json> {
    let value = value.trim();
    if value.is_empty() {
        bail!("an empty notification");
    }
    let mut n = if value.starts_with('{') {
        let n: Json = serde_json::from_str(value).context("notification: not JSON")?;
        let Some(fields) = n.as_object() else {
            bail!("notification: a JSON object");
        };
        if let Some(unknown) = fields.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            bail!("notification: unknown field {unknown:?}");
        }
        n
    } else {
        json!({ "message": value })
    };
    if n.get("source").is_none() {
        n["source"] = json!("Moli");
    }
    Ok(n)
}

/// Shows `notification` on the TV.
pub async fn notify(host: &str, port: u16, notification: &Json) -> anyhow::Result<()> {
    let request = moli_net::json_body(
        http::Request::post("/notify").header("host", format!("{host}:{port}")),
        notification,
    )?;
    let (status, body) = moli_net::plain(host, port, request)
        .await
        .context("tvoverlay")?;
    let answer: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
    if !status.is_success() || answer["success"] == json!(false) {
        bail!(
            "tvoverlay answered {status}: {}",
            answer["message"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[test]
    fn text_is_the_message_and_json_goes_as_it_is() {
        assert_eq!(
            notification(" Le linge est sec ").unwrap(),
            json!({"message": "Le linge est sec", "source": "Moli"})
        );
        let door = r#"{"title": "Sonnette", "video": "rtsp://nvr:8554/door", "corner": "bottom_end", "duration": 30}"#;
        let n = notification(door).unwrap();
        assert_eq!(n["video"], "rtsp://nvr:8554/door");
        assert_eq!(n["source"], "Moli");
        let own = notification(r#"{"message": "x", "source": "Cave"}"#).unwrap();
        assert_eq!(own["source"], "Cave");
        assert!(notification("").is_err());
        assert!(notification(r#"{"titel": "typo"}"#).is_err());
        assert!(notification("{not json").is_err());
    }

    #[tokio::test]
    async fn posts_to_the_tv_and_reads_its_answer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let tv = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !String::from_utf8_lossy(&seen).contains("Sonnette") {
                let n = socket.read(&mut buf).await.unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            let body = br#"{"success":true,"message":"Notification received"}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
            String::from_utf8_lossy(&seen).into_owned()
        });
        notify("127.0.0.1", port, &json!({"title": "Sonnette"}))
            .await
            .unwrap();
        let request = tv.await.unwrap();
        assert!(request.starts_with("POST /notify HTTP/1.1"));
        assert!(
            request
                .to_lowercase()
                .contains("content-type: application/json")
        );
    }
}

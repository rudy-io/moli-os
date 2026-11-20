//! A go2rtc serving Frigate's cameras (Frigate's own when its API port is
//! open, or another one reading the same cameras): still images
//! (`GET /api/frame.jpeg?src=<stream>`) and live video the browser plays
//! itself (WebRTC, `POST /api/webrtc?src=<stream>` with the browser's SDP
//! offer, the answer back). Only the signalling passes through Moli; the
//! video goes from go2rtc to the browser.

use std::fmt::Write as _;
use std::net::IpAddr;

use anyhow::bail;
use http::{Method, Request};
use moli_runtime::BoxFuture;
use moli_runtime::media::{Image, LiveSource, SnapshotSource};

/// How long go2rtc may take to answer an offer.
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(9);

/// One camera's stream on a go2rtc.
#[derive(Clone, Debug)]
pub struct Stream {
    pub host: String,
    pub port: u16,
    pub name: String,
}

impl Stream {
    fn path(&self, what: &str) -> String {
        format!("/api/{what}?src={}", encode(&self.name))
    }
}

impl SnapshotSource for Stream {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            let request = Request::builder()
                .uri(self.path("frame.jpeg"))
                .header("host", format!("{}:{}", self.host, self.port));
            let request = moli_net::empty(request)?;
            let (status, bytes) = moli_net::plain(&self.host, self.port, request).await?;
            if !status.is_success() || !bytes.starts_with(&[0xFF, 0xD8]) {
                anyhow::bail!(moli_i18n::tr!(
                    "pilotes.frigate.aucune_image",
                    status = status
                ));
            }
            Ok(Image {
                content_type: "image/jpeg".into(),
                bytes: bytes.to_vec(),
            })
        })
    }
}

impl LiveSource for Stream {
    fn answer(
        &self,
        offer: String,
        viewer: Option<IpAddr>,
    ) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move {
            let request = Request::builder()
                .method(Method::POST)
                .uri(self.path("webrtc"))
                .header("host", format!("{}:{}", self.host, self.port));
            let request =
                moli_net::text_body(request, "application/sdp", for_go2rtc(&offer, viewer))?;
            // go2rtc answers once it knows its addresses (STUN included):
            // up to ANSWER_WAIT (9 s), within the hub's 10.
            let (status, reply) =
                moli_net::plain_within(&self.host, self.port, request, ANSWER_WAIT).await?;
            if !status.is_success() {
                bail!("go2rtc: HTTP {} for stream {}", status.as_u16(), self.name);
            }
            let answer = String::from_utf8_lossy(&reply).into_owned();
            if !answer.starts_with("v=0") {
                bail!("go2rtc: no SDP answer for stream {}", self.name);
            }
            Ok(answer)
        })
    }
}

/// The browser's offer as go2rtc can use it: a candidate hidden behind a
/// `.local` name (which go2rtc may not resolve) gets the browser's address
/// on the home network, or is left out when it is unknown.
#[must_use]
pub fn for_go2rtc(offer: &str, viewer: Option<IpAddr>) -> String {
    let mut out = String::with_capacity(offer.len());
    for line in offer.lines().map(str::trim_end) {
        if let Some(candidate) = line.strip_prefix("a=candidate:") {
            let mut fields: Vec<&str> = candidate.split_whitespace().collect();
            let hidden = fields
                .get(4)
                .is_some_and(|a| a.to_ascii_lowercase().ends_with(".local"));
            if hidden {
                let Some(ip) = viewer else { continue };
                let ip = ip.to_string();
                fields[4] = &ip;
                out.push_str("a=candidate:");
                out.push_str(&fields.join(" "));
                out.push_str("\r\n");
                continue;
            }
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// A stream name in a query string.
pub(crate) fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_browser_candidate_gets_its_address() {
        let offer = "v=0\r\na=ice-ufrag:abcd\r\na=candidate:1 1 udp 2113937151 1f2e3d4c.local 54321 typ host generation 0\r\na=candidate:2 1 udp 2113937151 192.0.2.50 54322 typ host\r\n";
        let viewer: IpAddr = "192.0.2.50".parse().unwrap();
        let out = for_go2rtc(offer, Some(viewer));
        assert!(
            out.contains(
                "a=candidate:1 1 udp 2113937151 192.0.2.50 54321 typ host generation 0\r\n"
            )
        );
        assert!(out.contains("a=candidate:2 1 udp 2113937151 192.0.2.50 54322 typ host\r\n"));
        assert!(!out.contains(".local"));
        // Without the browser's address, the hidden candidate is left out.
        let out = for_go2rtc(offer, None);
        assert!(!out.contains("candidate:1 "));
        assert!(out.contains("candidate:2 "));
    }

    #[test]
    fn stream_names_are_escaped_in_paths() {
        let s = Stream {
            host: "nvr.example".into(),
            port: 1984,
            name: "jardin nord&1".into(),
        };
        assert_eq!(s.path("webrtc"), "/api/webrtc?src=jardin%20nord%261");
        assert_eq!(encode("camera-allee"), "camera-allee");
    }

    /// A go2rtc that answers with an error page naming a camera's address
    /// (its URLs carry the camera's credentials).
    fn go2rtc_failing() -> Stream {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut socket in listener.incoming().flatten() {
                let mut buf = vec![0u8; 65536];
                let _ = socket.read(&mut buf);
                let body = "streams: jardin: rtsp://admin:secret@192.0.2.10:554/live";
                let reply = format!(
                    "HTTP/1.1 500 Internal Server Error\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes());
            }
        });
        Stream {
            host: "127.0.0.1".into(),
            port,
            name: "jardin".into(),
        }
    }

    #[tokio::test]
    async fn nothing_go2rtc_says_comes_out_in_an_error() {
        let stream = go2rtc_failing();
        let offer = "v=0\r\na=candidate:2 1 udp 1 192.0.2.50 5 typ host\r\n".to_owned();
        let error = stream.answer(offer, None).await.unwrap_err().to_string();
        assert!(
            !error.contains("secret") && !error.contains("rtsp"),
            "{error}"
        );
        let error = stream.snapshot().await.unwrap_err().to_string();
        assert!(
            !error.contains("secret") && !error.contains("rtsp"),
            "{error}"
        );
    }
}

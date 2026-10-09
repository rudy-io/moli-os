//! Minimal HTTPS client for a Hue bridge: the pinned connection of
//! `moli-net` (trust on first use, at pairing), plus the application key.

use anyhow::{Context as _, bail};
use http::StatusCode;
use hyper::body::{Bytes, Incoming};
use moli_net::{Method, PinnedHttps};

pub use moli_net::PIN_MISMATCH;

const PORT: u16 = 443;

/// An event message (or a line) this long with no end is not the bridge's:
/// the stream is dropped and reopened.
pub const MAX_EVENT: usize = 1024 * 1024;

/// One bridge and its application key.
pub struct Bridge {
    http: PinnedHttps,
    key: Option<String>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key: it commands every light of the house.
        f.debug_struct("Bridge")
            .field("http", &self.http)
            .field("has_key", &self.key.is_some())
            .finish()
    }
}

impl Bridge {
    /// `pin`: the bridge's certificate fingerprint, always known before a
    /// key is sent (see [`Bridge::first_contact`]).
    pub fn new(host: &str, pin: String, key: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            http: PinnedHttps::new(host, PORT, Some(pin))?,
            key,
        })
    }

    /// A bare handshake, nothing sent: the fingerprint of the certificate
    /// the bridge presents, to store before any key leaves.
    pub async fn first_contact(host: &str) -> anyhow::Result<String> {
        let probe = PinnedHttps::new(host, PORT, None)?;
        probe.handshake().await.context("first contact")?;
        probe.pinned_fingerprint().context("no certificate seen")
    }

    /// Fingerprint of the certificate presented during the last handshake.
    pub fn seen_fingerprint(&self) -> Option<String> {
        self.http.seen_fingerprint()
    }

    fn builder(&self, method: Method, path: &str) -> http::request::Builder {
        let builder = self.http.request(method, path);
        match &self.key {
            Some(key) => builder.header("hue-application-key", key),
            None => builder,
        }
    }

    /// A request on the shared connection (every request we send is
    /// idempotent: GET, PUT, and the pairing POST).
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<(StatusCode, Bytes)> {
        let builder = self.builder(method, path);
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(http_body_util::Full::new(Bytes::from(body)))?,
            None => moli_net::empty(builder)?,
        };
        self.http.send(request).await
    }

    /// Opens the CLIP v2 event stream on a dedicated connection.
    pub async fn events(&self) -> anyhow::Result<Incoming> {
        let request = moli_net::empty(
            self.builder(Method::GET, "/eventstream/clip/v2")
                .header("accept", "text/event-stream"),
        )?;
        self.http.stream(request).await
    }
}

/// Incremental server-sent-events parser: feed bytes, get `data` payloads.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    data: String,
}

impl SseParser {
    /// Errs once more than [`MAX_EVENT`] bytes wait for the end of a
    /// message: the parser is then empty again.
    pub fn feed(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<String>> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(std::mem::take(&mut self.data));
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            }
            // `id:`, `event:` and `:` comments (keep-alives) carry nothing we need.
        }
        if self.buffer.len() + self.data.len() > MAX_EVENT {
            self.buffer = Vec::new();
            self.data = String::new();
            bail!("hue event over {MAX_EVENT} bytes without an end");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parsing_across_chunks() {
        let mut parser = SseParser::default();
        assert_eq!(
            parser.feed(b": hi\n\nid: 1:0\ndata: [{\"a\"").unwrap(),
            Vec::<String>::new()
        );
        let events = parser
            .feed(b":1}]\n\nid: 2:0\r\ndata: [2]\r\n\r\n")
            .unwrap();
        assert_eq!(events, vec!["[{\"a\":1}]".to_owned(), "[2]".to_owned()]);
    }

    #[test]
    fn sse_messages_without_end_are_capped() {
        // One endless line.
        let mut parser = SseParser::default();
        let chunk = vec![b'x'; 64 * 1024];
        let mut fed = 0;
        let error = loop {
            match parser.feed(&chunk) {
                Ok(events) => assert!(events.is_empty()),
                Err(e) => break e,
            }
            fed += chunk.len();
            assert!(fed <= MAX_EVENT, "no cap");
        };
        assert!(error.to_string().contains("without an end"));
        // Emptied: the next message goes through.
        assert_eq!(parser.feed(b"data: [1]\n\n").unwrap(), vec!["[1]"]);

        // Endless `data:` lines, never closed by a blank line.
        let mut parser = SseParser::default();
        let line = format!("data: {}\n", "y".repeat(1000));
        let error = (0..2000).find_map(|_| parser.feed(line.as_bytes()).err());
        assert!(error.is_some(), "no cap");
    }
}

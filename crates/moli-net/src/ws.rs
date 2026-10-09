//! A minimal WebSocket client (RFC 6455) over TLS, pinned for appliances
//! that speak JSON over a websocket (Sonos audio clips), public CAs for
//! cloud feeds (Tuya's message service). Text messages only; pings are
//! answered; anything else ends the conversation.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use ring::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};
use ring::rand::{SecureRandom, SystemRandom};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::{PinnedTls, TIMEOUT, public_tls};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_HANDSHAKE: usize = 16 * 1024;
const MAX_MESSAGE: usize = 1024 * 1024;

pub struct WebSocket {
    stream: TlsStream<TcpStream>,
    /// Bytes read but not yet consumed (a frame split across reads).
    buf: Vec<u8>,
}

impl std::fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocket").finish_non_exhaustive()
    }
}

impl WebSocket {
    /// Opens `wss://host:port/path` with extra handshake `headers`, the
    /// device's certificate pinned.
    pub async fn connect(
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
        tls: &PinnedTls,
    ) -> anyhow::Result<Self> {
        Self::open(host, port, path, headers, Arc::clone(&tls.config)).await
    }

    /// Same, to a service with a public certificate.
    pub async fn connect_public(
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<Self> {
        Self::open(host, port, path, headers, public_tls()).await
    }

    async fn open(
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
        config: Arc<ClientConfig>,
    ) -> anyhow::Result<Self> {
        let tcp = tokio::time::timeout(TIMEOUT, TcpStream::connect((host, port)))
            .await
            .context("connect timeout")?
            .with_context(|| format!("cannot reach {host}:{port}"))?;
        let name = ServerName::try_from(host.to_owned()).context("host name")?;
        let mut stream =
            tokio::time::timeout(TIMEOUT, TlsConnector::from(config).connect(name, tcp))
                .await
                .context("tls handshake timeout")?
                .context("tls handshake")?;

        let mut raw = [0u8; 16];
        SystemRandom::new()
            .fill(&mut raw)
            .map_err(|_| anyhow::anyhow!("no randomness"))?;
        let key = base64(&raw);
        let mut request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
        );
        for (name, value) in headers {
            ensure!(
                !name.contains(['\r', '\n']) && !value.contains(['\r', '\n']),
                "bad header"
            );
            let _ = write!(request, "{name}: {value}\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await?;

        let mut buf = Vec::new();
        let end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            ensure!(buf.len() < MAX_HANDSHAKE, "handshake answer too long");
            let mut chunk = [0u8; 2048];
            let n = tokio::time::timeout(TIMEOUT, stream.read(&mut chunk))
                .await
                .context("handshake timeout")??;
            ensure!(n > 0, "connection closed during the handshake");
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap_or_default();
        ensure!(
            status.starts_with("HTTP/1.1 101"),
            "websocket refused: {status}"
        );
        let expected =
            base64(digest(&SHA1_FOR_LEGACY_USE_ONLY, format!("{key}{GUID}").as_bytes()).as_ref());
        let accept = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("sec-websocket-accept"))
            .map(|(_, v)| v.trim().to_owned());
        ensure!(
            accept.as_deref() == Some(expected.as_str()),
            "bad websocket accept key"
        );
        buf.drain(..end);
        Ok(Self { stream, buf })
    }

    pub async fn send_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.send(0x1, text.as_bytes()).await
    }

    /// Keeps a quiet connection alive (the answer is skipped on receive).
    pub async fn ping(&mut self) -> anyhow::Result<()> {
        self.send(0x9, b"moli").await
    }

    /// The next text message (pings answered on the way).
    pub async fn recv_text(&mut self, limit: Duration) -> anyhow::Result<String> {
        tokio::time::timeout(limit, self.recv_inner())
            .await
            .context("no answer in time")?
    }

    async fn recv_inner(&mut self) -> anyhow::Result<String> {
        let mut message = Vec::new();
        loop {
            let (fin, opcode, payload) = self.frame().await?;
            match opcode {
                0x0 | 0x1 => {
                    ensure!(
                        message.len() + payload.len() <= MAX_MESSAGE,
                        "message too large"
                    );
                    message.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(message).context("message is not UTF-8");
                    }
                }
                0x9 => self.send(0xA, &payload).await?,
                0xA => {}
                0x8 => bail!("the device closed the websocket"),
                other => bail!("unexpected websocket frame {other:#x}"),
            }
        }
    }

    /// Client frames are always masked.
    async fn send(&mut self, opcode: u8, payload: &[u8]) -> anyhow::Result<()> {
        let mut mask = [0u8; 4];
        SystemRandom::new()
            .fill(&mut mask)
            .map_err(|_| anyhow::anyhow!("no randomness"))?;
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | u8::try_from(n)?),
            n if let Ok(short) = u16::try_from(n) => {
                frame.push(0x80 | 0x7e);
                frame.extend_from_slice(&short.to_be_bytes());
            }
            n => {
                frame.push(0x80 | 0x7f);
                frame.extend_from_slice(&u64::try_from(n)?.to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        self.stream.write_all(&frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn fill(&mut self, n: usize) -> anyhow::Result<()> {
        while self.buf.len() < n {
            let mut chunk = [0u8; 8192];
            let read = self.stream.read(&mut chunk).await?;
            ensure!(read > 0, "the device closed the connection");
            self.buf.extend_from_slice(&chunk[..read]);
        }
        Ok(())
    }

    async fn frame(&mut self) -> anyhow::Result<(bool, u8, Vec<u8>)> {
        self.fill(2).await?;
        let (b0, b1) = (self.buf[0], self.buf[1]);
        let masked = b1 & 0x80 != 0;
        let (mut at, len) = match b1 & 0x7f {
            126 => {
                self.fill(4).await?;
                (
                    4,
                    usize::from(u16::from_be_bytes([self.buf[2], self.buf[3]])),
                )
            }
            127 => {
                self.fill(10).await?;
                let mut n = [0u8; 8];
                n.copy_from_slice(&self.buf[2..10]);
                (10, usize::try_from(u64::from_be_bytes(n))?)
            }
            n => (2, usize::from(n)),
        };
        ensure!(len <= MAX_MESSAGE, "frame too large");
        let mut mask = None;
        if masked {
            self.fill(at + 4).await?;
            mask = Some([
                self.buf[at],
                self.buf[at + 1],
                self.buf[at + 2],
                self.buf[at + 3],
            ]);
            at += 4;
        }
        self.fill(at + len).await?;
        let mut payload: Vec<u8> = self.buf[at..at + len].to_vec();
        if let Some(mask) = mask {
            for (b, m) in payload.iter_mut().zip(mask.iter().cycle()) {
                *b ^= m;
            }
        }
        self.buf.drain(..at + len);
        Ok((b0 & 0x80 != 0, b0 & 0x0f, payload))
    }
}

/// Standard base64 with padding (handshake keys).
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(
                    T[usize::try_from((n >> (18 - 6 * i)) & 63).unwrap_or(0)],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        // RFC 4648 § 10.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
        // RFC 6455 § 1.3: the handshake example.
        let accept = base64(
            digest(
                &SHA1_FOR_LEGACY_USE_ONLY,
                format!("dGhlIHNhbXBsZSBub25jZQ=={GUID}").as_bytes(),
            )
            .as_ref(),
        );
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}

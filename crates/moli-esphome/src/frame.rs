//! The encrypted transport of the ESPHome API: frames `[0x01][len u16 BE]
//! [body]`, the Noise handshake, then sealed messages whose plain text is
//! `[type u16 BE][length u16 BE][protobuf]`.

use std::time::Duration;

use anyhow::{Context as _, bail};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::noise::{CipherState, Initiator, MSG1_LEN, PROLOGUE};

const INDICATOR: u8 = 0x01;
/// Bodies the device accepts during the handshake, and once encrypted (ESP32).
const MAX_HANDSHAKE: usize = 128;
const MAX_DATA: usize = 32_768;
/// The device must answer the handshake within this.
pub const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);

/// What the device says of itself before the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerHello {
    pub name: String,
    /// 12 lowercase hex digits, no separator.
    pub mac: String,
}

/// One frame's body; `max` bounds what a device may send.
async fn read_frame<R: AsyncRead + Unpin>(read: &mut R, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut head = [0u8; 3];
    read.read_exact(&mut head).await?;
    match head[0] {
        INDICATOR => {}
        0x00 => bail!(crate::IN_CLEAR),
        other => bail!("bad frame indicator {other:#04x}"),
    }
    let len = usize::from(u16::from_be_bytes([head[1], head[2]]));
    if len > max {
        bail!("frame of {len} bytes from the device");
    }
    let mut body = vec![0; len];
    read.read_exact(&mut body).await?;
    Ok(body)
}

fn frame(body: &[u8], out: &mut Vec<u8>) -> anyhow::Result<()> {
    let len = u16::try_from(body.len()).context("frame too long")?;
    out.push(INDICATOR);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(())
}

/// `01 <name> 00 <mac> 00`: the chosen protocol, then two strings.
fn server_hello(body: &[u8]) -> anyhow::Result<ServerHello> {
    let (&protocol, rest) = body.split_first().context("empty server hello")?;
    if protocol != 0x01 {
        bail!("the device chose protocol {protocol}");
    }
    let mut parts = rest.split(|b| *b == 0);
    let name = String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
    let mac = String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
    Ok(ServerHello { name, mac })
}

/// The handshake, both directions' keys out.
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    psk: &[u8; 32],
) -> anyhow::Result<(ServerHello, CipherState, CipherState)> {
    let (initiator, first) = Initiator::start(psk, PROLOGUE)?;
    debug_assert_eq!(first.len(), MSG1_LEN);
    // The client hello (empty) and the first handshake message, at once.
    let mut out = Vec::with_capacity(8 + MSG1_LEN);
    frame(&[], &mut out)?;
    let mut message = Vec::with_capacity(1 + MSG1_LEN);
    message.push(0x00);
    message.extend_from_slice(&first);
    frame(&message, &mut out)?;
    stream.write_all(&out).await?;
    stream.flush().await?;
    let hello = server_hello(&read_frame(stream, MAX_HANDSHAKE).await?)?;
    let answer = read_frame(stream, MAX_HANDSHAKE).await?;
    match answer.split_first() {
        Some((0x00, second)) => {
            let session = initiator.finish(second)?;
            Ok((hello, session.send, session.receive))
        }
        Some((_, reason)) => {
            let reason = String::from_utf8_lossy(reason);
            if reason.contains("MAC failure") {
                bail!(crate::WRONG_KEY);
            }
            bail!("the device refused the handshake: {reason}")
        }
        None => bail!("empty handshake answer"),
    }
}

/// The writing half: one message at a time, in order (the counter).
pub struct Sender<W> {
    write: W,
    cipher: CipherState,
}

impl<W> std::fmt::Debug for Sender<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl<W: AsyncWrite + Unpin> Sender<W> {
    pub fn new(write: W, cipher: CipherState) -> Self {
        Self { write, cipher }
    }

    pub async fn send(&mut self, kind: u16, payload: &[u8]) -> anyhow::Result<()> {
        let len = u16::try_from(payload.len()).context("message too long")?;
        let mut plain = Vec::with_capacity(4 + payload.len());
        plain.extend_from_slice(&kind.to_be_bytes());
        plain.extend_from_slice(&len.to_be_bytes());
        plain.extend_from_slice(payload);
        let sealed = self.cipher.seal(&plain)?;
        let mut out = Vec::with_capacity(3 + sealed.len());
        frame(&sealed, &mut out)?;
        self.write.write_all(&out).await?;
        self.write.flush().await?;
        Ok(())
    }
}

/// The reading half: one message after the other.
pub struct Receiver<R> {
    read: R,
    cipher: CipherState,
}

impl<R> std::fmt::Debug for Receiver<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver").finish_non_exhaustive()
    }
}

impl<R: AsyncRead + Unpin> Receiver<R> {
    pub fn new(read: R, cipher: CipherState) -> Self {
        Self { read, cipher }
    }

    /// The next message: its type and protobuf payload.
    pub async fn next(&mut self) -> anyhow::Result<(u16, Vec<u8>)> {
        let sealed = read_frame(&mut self.read, MAX_DATA).await?;
        let plain = self.cipher.open(&sealed)?;
        if plain.len() < 4 {
            bail!("message shorter than its header");
        }
        let kind = u16::from_be_bytes([plain[0], plain[1]]);
        let len = usize::from(u16::from_be_bytes([plain[2], plain[3]]));
        let payload = plain
            .get(4..4 + len)
            .context("message longer than its frame")?
            .to_vec();
        Ok((kind, payload))
    }
}

/// A message in clear (`0x00`, varint length, varint type, payload): only to
/// give a device that lost its key a new one.
pub async fn send_plain<W: AsyncWrite + Unpin>(
    write: &mut W,
    kind: u16,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut out = vec![0x00];
    moli_net::proto::varint(payload.len() as u64, &mut out);
    moli_net::proto::varint(u64::from(kind), &mut out);
    out.extend_from_slice(payload);
    write.write_all(&out).await?;
    write.flush().await?;
    Ok(())
}

async fn read_varint<R: AsyncRead + Unpin>(read: &mut R) -> anyhow::Result<u64> {
    let mut value = 0u64;
    for shift in [0, 7, 14, 21] {
        let byte = read.read_u8().await?;
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("varint too long from the device")
}

/// The next message in clear.
pub async fn read_plain<R: AsyncRead + Unpin>(read: &mut R) -> anyhow::Result<(u16, Vec<u8>)> {
    match read.read_u8().await? {
        0x00 => {}
        INDICATOR => bail!("the device asks for encryption"),
        other => bail!("bad frame indicator {other:#04x}"),
    }
    let len = usize::try_from(read_varint(read).await?)?;
    if len > MAX_DATA {
        bail!("frame of {len} bytes from the device");
    }
    let kind = u16::try_from(read_varint(read).await?).context("message type")?;
    let mut payload = vec![0; len];
    read.read_exact(&mut payload).await?;
    Ok((kind, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::respond;

    #[tokio::test]
    async fn messages_in_clear_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        send_plain(&mut a, 124, b"key").await.unwrap();
        send_plain(&mut a, 1, &[]).await.unwrap();
        let big = vec![7u8; 300];
        send_plain(&mut a, 10, &big).await.unwrap();
        assert_eq!(read_plain(&mut b).await.unwrap(), (124, b"key".to_vec()));
        assert_eq!(read_plain(&mut b).await.unwrap(), (1, Vec::new()));
        assert_eq!(read_plain(&mut b).await.unwrap(), (10, big));
    }

    /// A device's side of the handshake over an in-memory pipe.
    async fn device(
        mut io: tokio::io::DuplexStream,
        psk: [u8; 32],
    ) -> (CipherState, CipherState, tokio::io::DuplexStream) {
        let hello = read_frame(&mut io, MAX_HANDSHAKE).await.unwrap();
        assert!(hello.is_empty(), "the client hello is empty");
        let message = read_frame(&mut io, MAX_HANDSHAKE).await.unwrap();
        assert_eq!(message[0], 0x00);
        let (answer, session) = respond(&psk, PROLOGUE, &message[1..]).unwrap();
        let mut out = Vec::new();
        frame(b"\x01voice-pe\x00aabbccddeeff\x00", &mut out).unwrap();
        let mut body = vec![0x00];
        body.extend(answer);
        frame(&body, &mut out).unwrap();
        io.write_all(&out).await.unwrap();
        (session.send, session.receive, io)
    }

    #[tokio::test]
    async fn handshake_then_messages_both_ways() {
        let psk = [5u8; 32];
        let (mut moli, dev) = tokio::io::duplex(4096);
        let task = tokio::spawn(device(dev, psk));
        let (hello, send, receive) = handshake(&mut moli, &psk).await.unwrap();
        assert_eq!(
            hello,
            ServerHello {
                name: "voice-pe".into(),
                mac: "aabbccddeeff".into()
            }
        );
        let (dev_send, dev_receive, dev_io) = task.await.unwrap();
        let (moli_read, moli_write) = tokio::io::split(moli);
        let (dev_read, dev_write) = tokio::io::split(dev_io);
        let mut moli_tx = Sender::new(moli_write, send);
        let mut moli_rx = Receiver::new(moli_read, receive);
        let mut dev_tx = Sender::new(dev_write, dev_send);
        let mut dev_rx = Receiver::new(dev_read, dev_receive);
        moli_tx.send(1, b"hello").await.unwrap();
        assert_eq!(dev_rx.next().await.unwrap(), (1, b"hello".to_vec()));
        dev_tx.send(2, b"").await.unwrap();
        dev_tx.send(7, b"ping").await.unwrap();
        assert_eq!(moli_rx.next().await.unwrap(), (2, Vec::new()));
        assert_eq!(moli_rx.next().await.unwrap(), (7, b"ping".to_vec()));
    }

    #[tokio::test]
    async fn a_refused_handshake_says_why() {
        let (mut moli, mut dev) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let _ = read_frame(&mut dev, MAX_HANDSHAKE).await;
            let _ = read_frame(&mut dev, MAX_HANDSHAKE).await;
            let mut out = Vec::new();
            frame(b"\x01pe\x00aabbccddeeff\x00", &mut out).unwrap();
            frame(b"\x01Handshake MAC failure", &mut out).unwrap();
            dev.write_all(&out).await.unwrap();
        });
        let err = handshake(&mut moli, &[1u8; 32]).await.unwrap_err();
        assert_eq!(err.to_string(), crate::WRONG_KEY);
        task.await.unwrap();
        let (mut moli, mut dev) = tokio::io::duplex(64);
        dev.write_all(&[0x00, 0x00, 0x00]).await.unwrap();
        assert!(
            handshake(&mut moli, &[1u8; 32])
                .await
                .unwrap_err()
                .to_string()
                .contains("in clear")
        );
    }
}

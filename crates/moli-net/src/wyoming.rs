//! Speech with a local Wyoming server (Rhasspy): Piper speaks, Whisper
//! listens, and the house does both without the cloud.
//!
//! Wyoming events: one JSON header line, then the event's JSON data block
//! (`data_length` bytes), then its binary payload (`payload_length` bytes).
//! Piper answers `synthesize` with `audio-start`, `audio-chunk`s of raw PCM
//! and `audio-stop`; the PCM becomes a WAV file. Whisper takes `transcribe`,
//! then the audio the same way, and answers `transcript`.

use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// About ten minutes of speech: far beyond any announcement.
const MAX_AUDIO: usize = 8 * 1024 * 1024;
const MAX_BLOCK: usize = 64 * 1024;
const VERSION: &str = "1.5.2";
/// Bytes of PCM per `audio-chunk` sent to Whisper (~0.25 s at 16 kHz).
const CHUNK: usize = 8 * 1024;

/// The WAV file of `text` spoken by the server's voice (or `voice`).
pub async fn synthesize(
    host: &str,
    port: u16,
    voice: Option<&str>,
    text: &str,
    limit: Duration,
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(limit, exchange(host, port, voice, text))
        .await
        .context("speech synthesis took too long")?
}

/// What a Wyoming speech-to-text server (Whisper) hears in `pcm`: 16-bit
/// mono little-endian samples at `rate` Hz.
pub async fn transcribe(
    host: &str,
    port: u16,
    rate: u32,
    pcm: &[u8],
    limit: Duration,
) -> anyhow::Result<String> {
    tokio::time::timeout(limit, listen(host, port, rate, pcm))
        .await
        .context("speech recognition took too long")?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Format {
    rate: u32,
    width: u16,
    channels: u16,
}

impl Format {
    fn of(data: &Value) -> anyhow::Result<Self> {
        let field = |k: &str| {
            data.get(k)
                .and_then(Value::as_u64)
                .with_context(|| format!("audio without {k}"))
        };
        Ok(Self {
            rate: u32::try_from(field("rate")?)?,
            width: u16::try_from(field("width")?)?,
            channels: u16::try_from(field("channels")?)?,
        })
    }
}

#[derive(Debug)]
struct Event {
    kind: String,
    data: Value,
    payload: Vec<u8>,
}

async fn send<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    kind: &str,
    data: &Value,
    payload: &[u8],
) -> anyhow::Result<()> {
    let data = serde_json::to_vec(data)?;
    let mut header = json!({ "type": kind, "version": VERSION, "data_length": data.len() });
    if !payload.is_empty() {
        header["payload_length"] = json!(payload.len());
    }
    write.write_all(format!("{header}\n").as_bytes()).await?;
    write.write_all(&data).await?;
    write.write_all(payload).await?;
    Ok(())
}

/// The next event; its payload may be at most `room` bytes.
async fn receive<R: AsyncBufReadExt + Unpin>(reader: &mut R, room: usize) -> anyhow::Result<Event> {
    let mut line = Vec::new();
    let n = (&mut *reader)
        .take(MAX_BLOCK as u64)
        .read_until(b'\n', &mut line)
        .await?;
    ensure!(n > 0, "the speech server closed the connection");
    let header: Value = serde_json::from_slice(&line).context("speech server: bad event")?;
    let mut data = header.get("data").cloned().unwrap_or_else(|| json!({}));
    if let Some(len) = header.get("data_length").and_then(Value::as_u64) {
        let len = usize::try_from(len)?;
        ensure!(len <= MAX_BLOCK, "speech server: event data too large");
        let mut block = vec![0; len];
        reader.read_exact(&mut block).await?;
        if let (Some(into), Value::Object(more)) =
            (data.as_object_mut(), serde_json::from_slice(&block)?)
        {
            into.extend(more);
        }
    }
    let mut payload = Vec::new();
    if let Some(len) = header.get("payload_length").and_then(Value::as_u64) {
        let len = usize::try_from(len)?;
        ensure!(len <= room, "speech too long");
        payload = vec![0; len];
        reader.read_exact(&mut payload).await?;
    }
    let kind = header
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if kind == "error" {
        bail!(
            "speech server: {}",
            data.get("text").and_then(Value::as_str).unwrap_or("error")
        );
    }
    Ok(Event {
        kind,
        data,
        payload,
    })
}

async fn exchange(
    host: &str,
    port: u16,
    voice: Option<&str>,
    text: &str,
) -> anyhow::Result<Vec<u8>> {
    let tcp = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("speech server {host}:{port} unreachable"))?;
    let (read, mut write) = tcp.into_split();
    let mut data = json!({ "text": text });
    if let Some(voice) = voice {
        data["voice"] = json!({ "name": voice });
    }
    send(&mut write, "synthesize", &data, b"").await?;
    write.flush().await?;

    let mut reader = BufReader::new(read);
    let mut format = None;
    let mut pcm = Vec::new();
    loop {
        let event = receive(&mut reader, MAX_AUDIO - pcm.len()).await?;
        match event.kind.as_str() {
            "audio-start" => format = Some(Format::of(&event.data)?),
            "audio-chunk" => {
                if format.is_none() {
                    format = Some(Format::of(&event.data)?);
                }
                pcm.extend_from_slice(&event.payload);
            }
            "audio-stop" => break,
            _ => {}
        }
    }
    let format = format.context("speech server sent no audio")?;
    ensure!(!pcm.is_empty(), "speech server sent no audio");
    Ok(wav(format, &pcm))
}

async fn listen(host: &str, port: u16, rate: u32, pcm: &[u8]) -> anyhow::Result<String> {
    ensure!(!pcm.is_empty(), "no audio to transcribe");
    let tcp = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("speech server {host}:{port} unreachable"))?;
    let (read, mut write) = tcp.into_split();
    let format = json!({ "rate": rate, "width": 2, "channels": 1 });
    send(&mut write, "transcribe", &json!({ "language": "fr" }), b"").await?;
    send(&mut write, "audio-start", &format, b"").await?;
    for chunk in pcm.chunks(CHUNK) {
        send(&mut write, "audio-chunk", &format, chunk).await?;
    }
    send(&mut write, "audio-stop", &json!({}), b"").await?;
    write.flush().await?;

    let mut reader = BufReader::new(read);
    loop {
        let event = receive(&mut reader, MAX_BLOCK).await?;
        if event.kind == "transcript" {
            return Ok(event
                .data
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned());
        }
    }
}

/// A canonical 44-byte WAV header around raw PCM.
fn wav(f: Format, pcm: &[u8]) -> Vec<u8> {
    let block = u32::from(f.channels).saturating_mul(u32::from(f.width));
    let len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&len.saturating_add(36).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&f.channels.to_le_bytes());
    out.extend_from_slice(&f.rate.to_le_bytes());
    out.extend_from_slice(&f.rate.saturating_mul(block).to_le_bytes());
    out.extend_from_slice(&u16::try_from(block).unwrap_or(u16::MAX).to_le_bytes());
    out.extend_from_slice(&f.width.saturating_mul(8).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A Piper stand-in: checks the request, answers two chunks.
    async fn fake_piper() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let header: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(header["type"], "synthesize");
            let mut data =
                vec![0; usize::try_from(header["data_length"].as_u64().unwrap()).unwrap()];
            reader.read_exact(&mut data).await.unwrap();
            let data: Value = serde_json::from_slice(&data).unwrap();
            assert_eq!(data["text"], "Quelqu'un sonne");
            assert_eq!(data["voice"]["name"], "fr_FR-siwis-medium");
            let fmt = br#"{"rate":22050,"width":2,"channels":1}"#;
            let send = |kind: &str, data: &[u8], payload: &[u8]| {
                let mut out = format!(
                    "{{\"type\":\"{kind}\",\"data_length\":{},\"payload_length\":{}}}\n",
                    data.len(),
                    payload.len()
                )
                .into_bytes();
                out.extend_from_slice(data);
                out.extend_from_slice(payload);
                out
            };
            write
                .write_all(&send("audio-start", fmt, b""))
                .await
                .unwrap();
            write
                .write_all(&send("audio-chunk", fmt, &[1, 2, 3, 4]))
                .await
                .unwrap();
            write
                .write_all(&send("audio-chunk", fmt, &[5, 6]))
                .await
                .unwrap();
            write
                .write_all(&send("audio-stop", b"{}", b""))
                .await
                .unwrap();
        });
        port
    }

    #[tokio::test]
    async fn speech_becomes_a_wav_file() {
        let port = fake_piper().await;
        let wav = synthesize(
            "127.0.0.1",
            port,
            Some("fr_FR-siwis-medium"),
            "Quelqu'un sonne",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 22_050);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(&wav[44..], &[1, 2, 3, 4, 5, 6]);
    }

    /// A Whisper stand-in: reads transcribe + audio events, answers the text.
    async fn fake_whisper(expect: Vec<u8>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut reader = BufReader::new(read);
            let mut kinds = Vec::new();
            let mut heard = Vec::new();
            loop {
                let event = receive(&mut reader, MAX_AUDIO).await.unwrap();
                if event.kind == "audio-chunk" {
                    assert_eq!(event.data["rate"], 16_000);
                    heard.extend_from_slice(&event.payload);
                }
                let stop = event.kind == "audio-stop";
                kinds.push(event.kind);
                if stop {
                    break;
                }
            }
            assert_eq!(kinds.first().map(String::as_str), Some("transcribe"));
            assert_eq!(kinds.get(1).map(String::as_str), Some("audio-start"));
            assert_eq!(heard, expect);
            send(
                &mut write,
                "transcript",
                &json!({ "text": " Allume les combles. " }),
                b"",
            )
            .await
            .unwrap();
        });
        port
    }

    #[tokio::test]
    async fn whisper_hears_the_pcm() {
        let pcm: Vec<u8> = (0..20_000u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let port = fake_whisper(pcm.clone()).await;
        let text = transcribe("127.0.0.1", port, 16_000, &pcm, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(text, "Allume les combles.");
    }
}

//! Talking through a doorbell (or any camera with a speaker): the ONVIF back
//! channel the station's RTSP server offers when asked for it (an audio
//! track the *client* sends), fed with Moli's voice as G.711 µ-law at
//! 8 kHz, 20 ms per RTP packet, in real time, interleaved on the RTSP
//! connection.
//!
//! A doorbell whose back channel stays open stays in talk mode (its ring
//! goes blue and its chime stops ringing): every session ends with a
//! TEARDOWN and a closed connection, whatever happened in between.

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context as _, bail};
use md5::{Digest as _, Md5};
use moli_runtime::voice::Pcm;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// What a client says to get the back channel, on every request.
const BACK_CHANNEL: &str = "www.onvif.org/ver20/backchannel";
/// The back channel's sound: G.711 at 8 kHz.
const RATE: u32 = 8_000;
/// 20 ms of sound per packet.
const PACKET: usize = 160;
const REQUEST_LIMIT: Duration = Duration::from_secs(5);
/// Packets sent at once before keeping time, so the speaker never starves.
const AHEAD: usize = 3;
/// After the last packet, the speaker's own buffer plays out.
const PLAY_OUT: Duration = Duration::from_millis(400);
const MAX_RESPONSE: usize = 64 * 1024;

/// Where the station's RTSP server is, and who Moli is there. Never printed.
#[derive(Clone)]
pub(crate) struct Station {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) password: String,
}

impl std::fmt::Debug for Station {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Station")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Station {
    /// A channel's stream (the small one: only its tracks' list matters).
    fn uri(&self, channel: u64) -> String {
        format!(
            "rtsp://{}:{}/h264Preview_{:02}_sub",
            self.host,
            self.port,
            channel + 1
        )
    }
}

/// The track Moli sends on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Track {
    uri: String,
    payload: u8,
}

/// Whether `channel` has a speaker Moli can talk through.
pub(crate) async fn offers(station: &Station, channel: u64) -> anyhow::Result<bool> {
    let mut rtsp = Rtsp::open(station).await?;
    let base = station.uri(channel);
    let answer = rtsp.request("DESCRIBE", &base, &[]).await?;
    Ok(back_channel(&answer.body, answer.base.as_deref().unwrap_or(&base)).is_some())
}

/// Says `pcm` through `channel`'s speaker.
pub(crate) async fn say(station: &Station, channel: u64, pcm: &Pcm) -> anyhow::Result<()> {
    let sound: Vec<u8> = resample(&pcm.samples, pcm.rate, RATE)
        .into_iter()
        .map(ulaw)
        .collect();
    let mut rtsp = Rtsp::open(station).await?;
    let base = station.uri(channel);
    let answer = rtsp.request("DESCRIBE", &base, &[]).await?;
    let track = back_channel(&answer.body, answer.base.as_deref().unwrap_or(&base))
        .context("this camera offers no back channel")?;
    if track.payload != 0 {
        bail!(
            "the back channel wants payload {}, not µ-law",
            track.payload
        );
    }
    let setup = rtsp
        .request(
            "SETUP",
            &track.uri,
            &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")],
        )
        .await?;
    let session = setup.session.context("no session in the SETUP answer")?;
    let result = async {
        rtsp.request(
            "PLAY",
            &base,
            &[("Session", &session), ("Range", "npt=0.000-")],
        )
        .await?;
        rtsp.send_sound(&sound, track.payload).await
    }
    .await;
    // Always: a back channel left open keeps the doorbell in talk mode.
    let _ = rtsp
        .request("TEARDOWN", &base, &[("Session", &session)])
        .await;
    rtsp.close().await;
    result
}

/// An RTSP answer: the parts Moli reads.
#[derive(Debug, Default)]
struct Answer {
    status: u16,
    session: Option<String>,
    /// `Content-Base`: what relative track addresses hang from.
    base: Option<String>,
    challenge: Option<Challenge>,
    body: String,
}

/// A digest challenge (`WWW-Authenticate: Digest …`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Challenge {
    realm: String,
    nonce: String,
    qop: bool,
    opaque: Option<String>,
}

struct Rtsp {
    stream: TcpStream,
    user: String,
    password: String,
    cseq: u32,
    challenge: Option<Challenge>,
    /// What came in and is not read yet.
    inbox: Vec<u8>,
}

impl Rtsp {
    async fn open(station: &Station) -> anyhow::Result<Self> {
        let stream = tokio::time::timeout(
            REQUEST_LIMIT,
            TcpStream::connect((station.host.as_str(), station.port)),
        )
        .await
        .context("the station's RTSP server does not answer")??;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            user: station.user.clone(),
            password: station.password.clone(),
            cseq: 0,
            challenge: None,
            inbox: Vec::new(),
        })
    }

    /// One request, signed again once if the server asks for it.
    async fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<Answer> {
        for _ in 0..2 {
            self.cseq += 1;
            let mut text = format!(
                "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: Moli\r\nRequire: {BACK_CHANNEL}\r\n",
                self.cseq
            );
            if method == "DESCRIBE" {
                text.push_str("Accept: application/sdp\r\n");
            }
            for (name, value) in headers {
                let _ = write!(text, "{name}: {value}\r\n");
            }
            if let Some(challenge) = &self.challenge {
                let cnonce = format!("{:08x}", nanos() ^ self.cseq);
                let signed = authorization(
                    challenge,
                    &self.user,
                    &self.password,
                    (method, uri),
                    self.cseq,
                    &cnonce,
                );
                let _ = write!(text, "Authorization: {signed}\r\n");
            }
            text.push_str("\r\n");
            self.stream.write_all(text.as_bytes()).await?;
            let answer = tokio::time::timeout(REQUEST_LIMIT, self.answer())
                .await
                .with_context(|| format!("no answer to {method}"))??;
            match answer.status {
                401 if answer.challenge.is_some() && self.challenge != answer.challenge => {
                    self.challenge = answer.challenge;
                }
                200..=299 => return Ok(answer),
                status => bail!("{method}: RTSP {status}"),
            }
        }
        bail!("{method}: the station refuses Moli's user and password")
    }

    /// The next answer, interleaved packets skipped.
    async fn answer(&mut self) -> anyhow::Result<Answer> {
        loop {
            skip_frames(&mut self.inbox);
            if let Some((answer, used)) = parse_answer(&self.inbox)? {
                self.inbox.drain(..used);
                return Ok(answer);
            }
            if self.inbox.len() > MAX_RESPONSE {
                bail!("an RTSP answer too long");
            }
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                bail!("the station closed the connection");
            }
            self.inbox.extend_from_slice(&chunk[..n]);
        }
    }

    /// The sound, packet by packet in real time; what the station sends
    /// meanwhile (reports) is read and dropped.
    async fn send_sound(&mut self, sound: &[u8], payload: u8) -> anyhow::Result<()> {
        let ssrc = nanos();
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        for (i, chunk) in sound.chunks(PACKET).enumerate() {
            if i >= AHEAD {
                tick.tick().await;
            }
            #[allow(clippy::cast_possible_truncation)]
            let packet = rtp(payload, i as u16, (i * PACKET) as u32, ssrc, i == 0, chunk);
            self.stream.write_all(&interleaved(0, &packet)).await?;
            self.drain()?;
        }
        tokio::time::sleep(PLAY_OUT).await;
        self.drain()
    }

    /// Whatever arrived, without waiting; its packets dropped.
    fn drain(&mut self) -> anyhow::Result<()> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.try_read(&mut chunk) {
                Ok(0) => bail!("the station closed the connection"),
                Ok(n) => self.inbox.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        skip_frames(&mut self.inbox);
        if self.inbox.len() > MAX_RESPONSE {
            self.inbox.clear();
        }
        Ok(())
    }

    async fn close(mut self) {
        let _ = self.stream.shutdown().await;
    }
}

/// Drops the complete interleaved packets (`$`, channel, length) at the
/// start of what came in.
fn skip_frames(inbox: &mut Vec<u8>) {
    while inbox.len() >= 4 && inbox[0] == b'$' {
        let len = usize::from(u16::from_be_bytes([inbox[2], inbox[3]]));
        if inbox.len() < 4 + len {
            return;
        }
        inbox.drain(..4 + len);
    }
}

/// A whole answer at the start of `inbox` (and how many bytes it took), or
/// `None` while it is not all there.
fn parse_answer(inbox: &[u8]) -> anyhow::Result<Option<(Answer, usize)>> {
    if inbox.is_empty() || inbox[0] == b'$' {
        return Ok(None);
    }
    let Some(end) = inbox.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&inbox[..end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("RTSP/1.0 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .with_context(|| format!("not an RTSP answer: {status_line:.40}"))?;
    let mut answer = Answer {
        status,
        ..Answer::default()
    };
    let mut length = 0;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().unwrap_or(0),
            "session" => answer.session = value.split(';').next().map(str::to_owned),
            "content-base" => answer.base = Some(value.to_owned()),
            "www-authenticate" => {
                if let Some(c) = challenge(value) {
                    answer.challenge = Some(c);
                }
            }
            _ => {}
        }
    }
    let start = end + 4;
    if inbox.len() < start + length {
        return Ok(None);
    }
    answer.body = String::from_utf8_lossy(&inbox[start..start + length]).into_owned();
    Ok(Some((answer, start + length)))
}

/// A `Digest …` challenge (a `Basic` one is not taken).
fn challenge(header: &str) -> Option<Challenge> {
    let rest = header.strip_prefix("Digest ")?;
    let mut c = Challenge::default();
    for part in rest.split(',') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        let value = value.trim_matches('"');
        match key.trim() {
            "realm" => value.clone_into(&mut c.realm),
            "nonce" => value.clone_into(&mut c.nonce),
            "qop" => c.qop = value.split(',').any(|q| q.trim() == "auth"),
            "opaque" => c.opaque = Some(value.to_owned()),
            _ => {}
        }
    }
    (!c.nonce.is_empty()).then_some(c)
}

fn hex_md5(text: &str) -> String {
    Md5::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(32), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Something that changes at each call (an SSRC, a client nonce).
fn nanos() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x4d6f_6c69, |d| d.subsec_nanos())
}

/// The `Authorization` value answering `challenge` (RFC 2617) for
/// `(method, uri)`, the `count`-th request.
fn authorization(
    challenge: &Challenge,
    user: &str,
    password: &str,
    (method, uri): (&str, &str),
    count: u32,
    cnonce: &str,
) -> String {
    let ha1 = hex_md5(&format!("{user}:{}:{password}", challenge.realm));
    let ha2 = hex_md5(&format!("{method}:{uri}"));
    let mut header = if challenge.qop {
        let response = hex_md5(&format!(
            "{ha1}:{}:{count:08x}:{cnonce}:auth:{ha2}",
            challenge.nonce
        ));
        format!(
            r#"Digest username="{user}", realm="{}", nonce="{}", uri="{uri}", qop=auth, nc={count:08x}, cnonce="{cnonce}", response="{response}""#,
            challenge.realm, challenge.nonce
        )
    } else {
        let response = hex_md5(&format!("{ha1}:{}:{ha2}", challenge.nonce));
        format!(
            r#"Digest username="{user}", realm="{}", nonce="{}", uri="{uri}", response="{response}""#,
            challenge.realm, challenge.nonce
        )
    };
    if let Some(opaque) = &challenge.opaque {
        let _ = write!(header, r#", opaque="{opaque}""#);
    }
    header
}

/// The audio track the client sends (`a=sendonly`), its address and payload.
fn back_channel(sdp: &str, base: &str) -> Option<Track> {
    let mut sections = sdp.split("\nm=").skip(1);
    sections.find_map(|section| {
        let mut lines = section.lines().map(str::trim);
        let media = lines.next()?;
        if !media.starts_with("audio") {
            return None;
        }
        let lines: Vec<&str> = lines.collect();
        if !lines.contains(&"a=sendonly") {
            return None;
        }
        let payload = media.split_whitespace().nth(3)?.parse().ok()?;
        let control = lines.iter().find_map(|l| l.strip_prefix("a=control:"))?;
        let uri = if control.starts_with("rtsp://") {
            control.to_owned()
        } else {
            format!("{}/{control}", base.trim_end_matches('/'))
        };
        Some(Track { uri, payload })
    })
}

/// The resampling filter's length.
const TAPS: usize = 63;

/// `samples` at `to` per second: a windowed-sinc low-pass under the new
/// Nyquist frequency, then linear interpolation.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let filtered: Vec<f64> = if from > to {
        let cutoff = 0.45 / ratio; // of the input rate
        let middle = (TAPS / 2) as f64;
        let kernel: Vec<f64> = (0..TAPS)
            .map(|n| {
                let x = n as f64 - middle;
                let sinc = if x == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let window =
                    0.54 - 0.46 * (2.0 * std::f64::consts::PI * n as f64 / (TAPS - 1) as f64).cos();
                sinc * window
            })
            .collect();
        (0..samples.len())
            .map(|i| {
                kernel
                    .iter()
                    .enumerate()
                    .map(|(k, h)| {
                        (i + k)
                            .checked_sub(TAPS / 2)
                            .and_then(|j| samples.get(j))
                            .map_or(0.0, |s| f64::from(*s) * h)
                    })
                    .sum()
            })
            .collect()
    } else {
        samples.iter().map(|s| f64::from(*s)).collect()
    };
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let at = i as f64 * ratio;
            let j = at.floor() as usize;
            let frac = at - j as f64;
            let a = filtered[j.min(filtered.len() - 1)];
            let b = filtered[(j + 1).min(filtered.len() - 1)];
            (a + (b - a) * frac)
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// G.711 µ-law of one sample.
fn ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32_635;
    let mut s = i32::from(sample);
    let sign = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    s = s.min(CLIP) + BIAS;
    let mut exponent = 7;
    let mut mask = 0x4000;
    while s & mask == 0 && exponent > 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (s >> (exponent + 3)) & 0x0F;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = !(sign | (exponent << 4) | mantissa) as u8;
    byte
}

/// One RTP packet (RFC 3550): version 2, no padding, extension or CSRC.
fn rtp(payload: u8, seq: u16, timestamp: u32, ssrc: u32, first: bool, data: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(12 + data.len());
    packet.push(0x80);
    packet.push(if first { 0x80 | payload } else { payload });
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(data);
    packet
}

/// A packet on the RTSP connection: `$`, its channel, its length.
fn interleaved(channel: u8, packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + packet.len());
    out.push(b'$');
    out.push(channel);
    #[allow(clippy::cast_possible_truncation)]
    out.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    out.extend_from_slice(packet);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As a Reolink Home Hub described its doorbell (2026-10-10).
    const SDP: &str = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Session\r\nt=0 0\r\na=control:*\r\n\
        m=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=recvonly\r\na=control:track1\r\n\
        m=audio 0 RTP/AVP 97\r\na=rtpmap:97 MPEG4-GENERIC/16000\r\na=recvonly\r\na=control:track2\r\n\
        m=audio 0 RTP/AVP 0\r\na=control:track3\r\na=rtpmap:0 PCMU/8000\r\na=sendonly\r\n";

    #[test]
    fn the_back_channel_is_the_track_the_client_sends() {
        let base = "rtsp://192.168.1.x:554/h264Preview_02_sub";
        assert_eq!(
            back_channel(SDP, base),
            Some(Track {
                uri: format!("{base}/track3"),
                payload: 0
            })
        );
        let one_way = SDP.split("m=audio 0 RTP/AVP 0").next().unwrap();
        assert_eq!(back_channel(one_way, base), None);
    }

    #[test]
    fn mu_law_matches_g711() {
        assert_eq!(ulaw(0), 0xFF);
        assert_eq!(ulaw(i16::MAX), 0x80);
        assert_eq!(ulaw(i16::MIN), 0x00);
        assert_eq!(ulaw(-1), 0x7F);
        assert_eq!(ulaw(1000) & 0x80, 0x80, "positive keeps the sign bit");
    }

    #[test]
    fn the_voice_comes_down_to_eight_kilohertz() {
        let second = vec![1000i16; 24_000];
        let out = resample(&second, 24_000, 8_000);
        assert_eq!(out.len(), 8_000);
        // A steady level stays (the filter's gain is 1), away from the edges.
        assert!((i32::from(out[4_000]) - 1000).abs() < 20, "{}", out[4_000]);
        // A tone above 4 kHz is filtered out, one below stays.
        let tone = |hz: f64| -> Vec<i16> {
            (0..24_000)
                .map(|i| {
                    #[allow(clippy::cast_possible_truncation)]
                    let s = (10_000.0
                        * (2.0 * std::f64::consts::PI * hz * f64::from(i) / 24_000.0).sin())
                        as i16;
                    s
                })
                .collect()
        };
        let peak = |s: &[i16]| {
            s[1000..7000]
                .iter()
                .map(|x| x.unsigned_abs())
                .max()
                .unwrap()
        };
        assert!(peak(&resample(&tone(6_000.0), 24_000, 8_000)) < 1_500);
        assert!(peak(&resample(&tone(500.0), 24_000, 8_000)) > 9_000);
    }

    #[test]
    fn packets_are_rtp_on_the_rtsp_connection() {
        let p = rtp(0, 7, 1120, 0xAABB_CCDD, true, &[1, 2]);
        assert_eq!(p[..2], [0x80, 0x80]);
        assert_eq!(u16::from_be_bytes([p[2], p[3]]), 7);
        assert_eq!(u32::from_be_bytes([p[4], p[5], p[6], p[7]]), 1120);
        assert_eq!(p[12..], [1, 2]);
        assert_eq!(rtp(0, 8, 1280, 1, false, &[])[1], 0);
        let framed = interleaved(0, &p);
        assert_eq!(framed[..4], [b'$', 0, 0, 14]);
    }

    #[test]
    fn answers_are_read_between_packets() {
        let mut inbox = interleaved(1, &[9; 8]);
        inbox.extend_from_slice(
            b"RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\nWWW-Authenticate: Digest realm=\"BC Streaming Media\", nonce=\"abc\"\r\n\r\n",
        );
        inbox.extend_from_slice(b"RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: 1234;timeout=60\r\nContent-Length: 3\r\n\r\nv=0");
        skip_frames(&mut inbox);
        let (first, used) = parse_answer(&inbox).unwrap().unwrap();
        assert_eq!(first.status, 401);
        assert_eq!(
            first.challenge,
            Some(Challenge {
                realm: "BC Streaming Media".into(),
                nonce: "abc".into(),
                qop: false,
                opaque: None
            })
        );
        inbox.drain(..used);
        let (second, _) = parse_answer(&inbox).unwrap().unwrap();
        assert_eq!(
            (
                second.status,
                second.session.as_deref(),
                second.body.as_str()
            ),
            (200, Some("1234"), "v=0")
        );
        assert!(
            parse_answer(b"RTSP/1.0 200 OK\r\nContent-Length: 9\r\n\r\nv=0")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn digest_answers_rfc_2617() {
        // RFC 2617, 3.5.
        let c = challenge(
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
        )
        .unwrap();
        assert!(c.qop);
        let signed = authorization(
            &c,
            "Mufasa",
            "Circle Of Life",
            ("GET", "/dir/index.html"),
            1,
            "0a4f113b",
        );
        assert!(
            signed.contains(r#"response="6629fae49393a05397450978507c4ef1""#),
            "{signed}"
        );
        assert!(signed.ends_with(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
    }
}

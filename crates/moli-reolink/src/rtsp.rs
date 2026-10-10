//! The little of RTSP Moli needs to talk and listen through a camera: one
//! connection to the station's RTSP server, digest sign-in, the tracks the
//! SDP lists, and RTP packets interleaved on the connection (`$`, channel,
//! length). Every request asks for the ONVIF back channel, so that the SDP
//! also lists the track a client sends on (the camera's speaker).

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context as _, bail};
use md5::{Digest as _, Md5};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const BACK_CHANNEL: &str = "www.onvif.org/ver20/backchannel";
pub(crate) const REQUEST_LIMIT: Duration = Duration::from_secs(5);
const MAX_INBOX: usize = 256 * 1024;

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
    /// A channel's small stream (its tracks are the same as the main one's).
    pub(crate) fn uri(&self, channel: u64) -> String {
        format!(
            "rtsp://{}:{}/h264Preview_{:02}_sub",
            self.host,
            self.port,
            channel + 1
        )
    }
}

/// Which way a track goes, from the client's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Way {
    /// The camera sends (`a=recvonly`): its picture, its microphone.
    In,
    /// The client sends (`a=sendonly`): the back channel, its speaker.
    Out,
}

/// One track of the SDP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Track {
    pub(crate) audio: bool,
    pub(crate) way: Way,
    pub(crate) uri: String,
    pub(crate) payload: u8,
    /// `a=rtpmap:` after the payload number (`PCMU/8000`, `MPEG4-GENERIC/16000`).
    pub(crate) codec: String,
    /// `a=fmtp:` after the payload number.
    pub(crate) fmtp: String,
}

/// The tracks an SDP lists, their addresses under `base`.
pub(crate) fn tracks(sdp: &str, base: &str) -> Vec<Track> {
    sdp.split("\nm=")
        .skip(1)
        .filter_map(|section| {
            let mut lines = section.lines().map(str::trim);
            let media = lines.next()?;
            let lines: Vec<&str> = lines.collect();
            let payload = media.split_whitespace().nth(3)?.parse().ok()?;
            let control = lines.iter().find_map(|l| l.strip_prefix("a=control:"))?;
            let uri = if control.starts_with("rtsp://") {
                control.to_owned()
            } else {
                format!("{}/{control}", base.trim_end_matches('/'))
            };
            let after_payload = |prefix: &str| {
                lines
                    .iter()
                    .find_map(|l| l.strip_prefix(prefix))
                    .and_then(|rest| rest.split_once(' '))
                    .map(|(_, v)| v.trim().to_owned())
                    .unwrap_or_default()
            };
            Some(Track {
                audio: media.starts_with("audio"),
                way: if lines.contains(&"a=sendonly") {
                    Way::Out
                } else {
                    Way::In
                },
                uri,
                payload,
                codec: after_payload("a=rtpmap:"),
                fmtp: after_payload("a=fmtp:"),
            })
        })
        .collect()
}

/// An RTSP answer: the parts Moli reads.
#[derive(Debug, Default)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) session: Option<String>,
    /// `Content-Base`: what relative track addresses hang from.
    pub(crate) base: Option<String>,
    challenge: Option<Challenge>,
    pub(crate) body: String,
}

/// A digest challenge (`WWW-Authenticate: Digest …`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Challenge {
    realm: String,
    nonce: String,
    qop: bool,
    opaque: Option<String>,
}

/// What came in on the connection.
pub(crate) enum Incoming {
    /// An interleaved packet: its channel, its bytes.
    Packet(u8, Vec<u8>),
    Answer(Answer),
}

pub(crate) struct Rtsp {
    stream: TcpStream,
    user: String,
    password: String,
    cseq: u32,
    challenge: Option<Challenge>,
    /// What came in and is not read yet.
    inbox: Vec<u8>,
}

impl Rtsp {
    pub(crate) async fn open(station: &Station) -> anyhow::Result<Self> {
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

    /// The tracks of `channel`'s stream (and the address they hang from).
    pub(crate) async fn describe(&mut self, base: &str) -> anyhow::Result<Vec<Track>> {
        let answer = self.request("DESCRIBE", base, &[]).await?;
        Ok(tracks(&answer.body, answer.base.as_deref().unwrap_or(base)))
    }

    /// Opens `track` on interleaved channels `channel`, `channel + 1`:
    /// the session.
    pub(crate) async fn setup(&mut self, track: &Track, channel: u8) -> anyhow::Result<String> {
        let transport = format!("RTP/AVP/TCP;unicast;interleaved={channel}-{}", channel + 1);
        self.request("SETUP", &track.uri, &[("Transport", &transport)])
            .await?
            .session
            .context("no session in the SETUP answer")
    }

    pub(crate) async fn play(&mut self, base: &str, session: &str) -> anyhow::Result<()> {
        self.request(
            "PLAY",
            base,
            &[("Session", session), ("Range", "npt=0.000-")],
        )
        .await
        .map(drop)
    }

    /// Ends the session and the connection, whatever state they are in.
    pub(crate) async fn finish(mut self, base: &str, session: Option<&str>) {
        if let Some(session) = session {
            let _ = self
                .request("TEARDOWN", base, &[("Session", session)])
                .await;
        }
        let _ = self.stream.shutdown().await;
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

    /// The next answer, packets in between dropped.
    async fn answer(&mut self) -> anyhow::Result<Answer> {
        loop {
            if let Incoming::Answer(answer) = self.next().await? {
                return Ok(answer);
            }
        }
    }

    /// The next packet or answer.
    pub(crate) async fn next(&mut self) -> anyhow::Result<Incoming> {
        loop {
            if let Some(incoming) = take(&mut self.inbox)? {
                return Ok(incoming);
            }
            if self.inbox.len() > MAX_INBOX {
                bail!("the station sends what Moli cannot read");
            }
            // Straight into the inbox: no buffer held across the wait.
            self.inbox.reserve(8192);
            if self.stream.read_buf(&mut self.inbox).await? == 0 {
                bail!("the station closed the connection");
            }
        }
    }

    /// Sends one interleaved packet.
    pub(crate) async fn send(&mut self, channel: u8, packet: &[u8]) -> anyhow::Result<()> {
        self.stream
            .write_all(&interleaved(channel, packet))
            .await
            .map_err(Into::into)
    }

    /// Reads what arrived, without waiting, and drops its packets: while
    /// Moli sends, the station's reports must not pile up.
    pub(crate) fn drain(&mut self) -> anyhow::Result<()> {
        let mut chunk = [0u8; 8192];
        loop {
            match self.stream.try_read(&mut chunk) {
                Ok(0) => bail!("the station closed the connection"),
                Ok(n) => self.inbox.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        while let Some(Incoming::Packet(..)) = take(&mut self.inbox)? {}
        if self.inbox.len() > MAX_INBOX {
            self.inbox.clear();
        }
        Ok(())
    }
}

/// A whole packet or answer at the start of `inbox`, taken out of it.
fn take(inbox: &mut Vec<u8>) -> anyhow::Result<Option<Incoming>> {
    if inbox.first() == Some(&b'$') {
        if inbox.len() < 4 {
            return Ok(None);
        }
        let len = usize::from(u16::from_be_bytes([inbox[2], inbox[3]]));
        if inbox.len() < 4 + len {
            return Ok(None);
        }
        let channel = inbox[1];
        let packet = inbox[4..4 + len].to_vec();
        inbox.drain(..4 + len);
        return Ok(Some(Incoming::Packet(channel, packet)));
    }
    Ok(parse_answer(inbox)?.map(|(answer, used)| {
        inbox.drain(..used);
        Incoming::Answer(answer)
    }))
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
pub(crate) fn nanos() -> u32 {
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

/// One RTP packet (RFC 3550): version 2, no padding, extension or CSRC.
pub(crate) fn rtp(
    payload: u8,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    first: bool,
    data: &[u8],
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(12 + data.len());
    packet.push(0x80);
    packet.push(if first { 0x80 | payload } else { payload });
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(data);
    packet
}

/// An RTP packet's payload (CSRCs, extension and padding skipped).
pub(crate) fn payload_of(packet: &[u8]) -> Option<&[u8]> {
    let first = *packet.first()?;
    if first >> 6 != 2 {
        return None;
    }
    let mut start = 12 + 4 * usize::from(first & 0x0F);
    if first & 0x10 != 0 {
        let words = packet.get(start + 2..start + 4)?;
        start += 4 + 4 * usize::from(u16::from_be_bytes([words[0], words[1]]));
    }
    let mut end = packet.len();
    if first & 0x20 != 0 {
        end = end.checked_sub(usize::from(*packet.last()?))?;
    }
    packet.get(start..end)
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
    pub(crate) const SDP: &str = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Session\r\nt=0 0\r\na=control:*\r\n\
        m=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=recvonly\r\na=control:track1\r\n\
        m=audio 0 RTP/AVP 97\r\na=rtpmap:97 MPEG4-GENERIC/16000\r\na=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1408\r\na=recvonly\r\na=control:track2\r\n\
        m=audio 0 RTP/AVP 0\r\na=control:track3\r\na=rtpmap:0 PCMU/8000\r\na=sendonly\r\n";

    #[test]
    fn the_sdp_lists_the_microphone_and_the_speaker() {
        let base = "rtsp://192.168.1.x:554/h264Preview_02_sub";
        let all = tracks(SDP, base);
        assert_eq!(all.len(), 3);
        let speaker = all.iter().find(|t| t.audio && t.way == Way::Out).unwrap();
        assert_eq!(
            (
                speaker.uri.as_str(),
                speaker.payload,
                speaker.codec.as_str()
            ),
            (format!("{base}/track3").as_str(), 0, "PCMU/8000")
        );
        let mic = all.iter().find(|t| t.audio && t.way == Way::In).unwrap();
        assert_eq!(mic.codec, "MPEG4-GENERIC/16000");
        assert!(mic.fmtp.ends_with("config=1408"), "{}", mic.fmtp);
        let one_way = SDP.split("m=audio 0 RTP/AVP 0").next().unwrap();
        assert!(!tracks(one_way, base).iter().any(|t| t.way == Way::Out));
    }

    #[test]
    fn packets_are_rtp_on_the_rtsp_connection() {
        let p = rtp(0, 7, 1120, 0xAABB_CCDD, true, &[1, 2]);
        assert_eq!(p[..2], [0x80, 0x80]);
        assert_eq!(u16::from_be_bytes([p[2], p[3]]), 7);
        assert_eq!(u32::from_be_bytes([p[4], p[5], p[6], p[7]]), 1120);
        assert_eq!(p[12..], [1, 2]);
        assert_eq!(payload_of(&p), Some(&[1u8, 2][..]));
        assert_eq!(rtp(0, 8, 1280, 1, false, &[])[1], 0);
        let framed = interleaved(0, &p);
        assert_eq!(framed[..4], [b'$', 0, 0, 14]);
        // A CSRC and padding are skipped.
        let mut odd = vec![
            0xA1, 97, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 9, 9, 9, 9, 5, 6, 0, 2,
        ];
        assert_eq!(payload_of(&odd), Some(&[5u8, 6][..]));
        odd[0] = 0x40;
        assert_eq!(payload_of(&odd), None);
    }

    #[test]
    fn answers_are_read_between_packets() {
        let mut inbox = interleaved(1, &[9; 8]);
        inbox.extend_from_slice(
            b"RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\nWWW-Authenticate: Digest realm=\"BC Streaming Media\", nonce=\"abc\"\r\n\r\n",
        );
        inbox.extend_from_slice(b"RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: 1234;timeout=60\r\nContent-Length: 3\r\n\r\nv=0");
        let Some(Incoming::Packet(1, packet)) = take(&mut inbox).unwrap() else {
            panic!("a packet first");
        };
        assert_eq!(packet, [9; 8]);
        let Some(Incoming::Answer(first)) = take(&mut inbox).unwrap() else {
            panic!("then an answer");
        };
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
        let Some(Incoming::Answer(second)) = take(&mut inbox).unwrap() else {
            panic!("and another");
        };
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

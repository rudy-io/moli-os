//! AirPlay 1 (RAOP) to a receiver that takes plain PCM: neither encryption
//! nor compression nor a password, and no pairing: no crypto here. Which
//! receivers those are is read from their `_raop._tcp` TXT ([`plain`]): the
//! lists `et` and `cn` hold 0 on AirPlay 2 receivers too, so the features
//! (`ft`) and flags (`sf`) must say AirPlay 1 and nothing asked first.
//! IPv4 only.
//!
//! One session: RTSP on TCP (`ANNOUNCE`, `SETUP`, `RECORD`, then volume and
//! `TEARDOWN`), and three UDP channels:
//!
//! - audio: RTP packets of 352 frames, sent ahead of time (the receiver
//!   keeps [`LATENCY`] before playing; a packet on its own timestamp arrives
//!   late and is dropped);
//! - control: a SYNC each second to the receiver's control port (without
//!   it, a receiver takes everything and plays nothing), and the packets it
//!   asks for again (type 85);
//! - timing: answers to the receiver's clock questions.
//!
//! Each of those was found by ear on a Freebox Delta Player (`AppleTV3,2`,
//! `srcvers=220.68`): a receiver says nothing when it drops what it gets.
//!
//! ⚠️ Every session cuts what the receiver plays, reading the volume too.

use std::collections::hash_map::RandomState;
use std::fmt::Write as _;
use std::hash::{BuildHasher, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, ensure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};

/// The RAOP port receivers listen on.
pub const PORT: u16 = 5000;
/// Frames in one RTP packet, as every RAOP sender does.
const FRAMES: u32 = 352;
/// Bytes in one packet's sound (16-bit stereo).
const PACKET: usize = FRAMES as usize * 4;
/// What the receiver keeps before playing: 2 s at 44.1 kHz (AirPlay 1).
const LATENCY: u64 = 88_200;
const RATE: f64 = 44_100.0;
/// Seconds between 1900 (NTP) and 1970.
const NTP_EPOCH: u64 = 2_208_988_800;
/// RAOP's volume: -30 dB is the lowest, 0 the loudest, -144 mute.
pub const MUTED: f64 = -144.0;
/// Packets kept to send again on request: about 16 s of sound.
const KEPT: usize = 2048;
/// At most this many packets sent again per request.
const MAX_RESEND: u16 = 128;
const TIMEOUT: Duration = Duration::from_secs(12);
const MAX_ANSWER: usize = 64 * 1024;
/// Where a receiver answers DNS-SD questions.
pub const MDNS_PORT: u16 = 5353;
/// A receiver that has not answered its TXT by then will not.
const MDNS_WAIT: Duration = Duration::from_millis(1500);
const SERVICE: &str = "_raop._tcp.local";

/// A percentage (0–100) as RAOP decibels; 0 % is mute, not -30 dB.
#[must_use]
pub fn db_of_percent(percent: f64) -> f64 {
    let p = percent.clamp(0.0, 100.0);
    if p <= 0.0 {
        MUTED
    } else {
        (p / 100.0 - 1.0) * 30.0
    }
}

/// RAOP decibels as a percentage; anything under -30 dB is 0 %.
#[must_use]
pub fn percent_of_db(db: f64) -> f64 {
    ((db / 30.0 + 1.0) * 100.0).clamp(0.0, 100.0)
}

/// The receiver answered a step of the session (`OPTIONS`, `ANNOUNCE`,
/// `SETUP`, `RECORD`) with something else than 200: a password, encryption
/// or pairing it wants and this sender does not do. A volume it will not
/// give or take is not that.
#[derive(Debug)]
pub struct Refused {
    pub method: String,
    /// Its status line (`RTSP/1.0 403 Forbidden`).
    pub answer: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the receiver refused {}: {}", self.method, self.answer)
    }
}

impl std::error::Error for Refused {}

/// The receiver answered a volume with something else than 200.
#[derive(Debug)]
struct VolumeRefused(String);

impl std::fmt::Display for VolumeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the receiver did not take the volume: {}", self.0)
    }
}

impl std::error::Error for VolumeRefused {}

/// Why a receiver will not take this sender's sound, read from its TXT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotPlain {
    /// `et` has no 0: it wants the sound encrypted.
    Encrypted,
    /// `cn` has no 0: it wants the sound compressed.
    Compressed,
    /// Bit 38 or 48 of `ft`: AirPlay 2 (HomePod, recent Apple TV, Mac).
    AirPlay2,
    /// `pw=true`, or bit `0x80` of `sf`.
    Password,
    /// Bit `0x8` or `0x200` of `sf`: a pairing first.
    Pairing,
}

impl std::fmt::Display for NotPlain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Encrypted => "it wants the sound encrypted (no 0 in et)",
            Self::Compressed => "it wants the sound compressed (no 0 in cn)",
            Self::AirPlay2 => "it is an AirPlay 2 receiver (bit 38 or 48 in ft)",
            Self::Password => "it wants a password (pw, or 0x80 in sf)",
            Self::Pairing => "it wants a pairing (0x8 or 0x200 in sf)",
        })
    }
}

impl std::error::Error for NotPlain {}

/// Whether a receiver takes plain sound with no password nor pairing, from
/// its `_raop._tcp` TXT (`key=value`): what pyatv reads
/// (`pyatv/protocols/airplay/utils.py`). The two lists must hold 0, and
/// nothing may say AirPlay 2, a password or a pairing.
///
/// # Errors
/// The first reason it will not.
pub fn plain(txt: &[String]) -> Result<(), NotPlain> {
    let get = |key: &str| {
        txt.iter()
            .find_map(|t| t.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
    };
    let has_zero = |key: &str| get(key).is_some_and(|v| v.split(',').any(|x| x.trim() == "0"));
    if !has_zero("et") {
        return Err(NotPlain::Encrypted);
    }
    if !has_zero("cn") {
        return Err(NotPlain::Compressed);
    }
    let features = get("ft").or_else(|| get("features")).map_or(0, features);
    if features & (1 << 38 | 1 << 48) != 0 {
        return Err(NotPlain::AirPlay2);
    }
    let flags = get("sf").or_else(|| get("flags")).map_or(0, hex);
    if get("pw").is_some_and(|v| v.eq_ignore_ascii_case("true")) || flags & 0x80 != 0 {
        return Err(NotPlain::Password);
    }
    if flags & (0x8 | 0x200) != 0 {
        return Err(NotPlain::Pairing);
    }
    Ok(())
}

/// `0x1234` (or bare hex); unreadable is 0.
fn hex(v: &str) -> u64 {
    let v = v.trim();
    let digits = v
        .strip_prefix("0x")
        .or_else(|| v.strip_prefix("0X"))
        .unwrap_or(v);
    u64::from_str_radix(digits, 16).unwrap_or(0)
}

/// `ft` as one number: `low,high` (two 32-bit words), or one word.
fn features(v: &str) -> u64 {
    match v.split_once(',') {
        Some((low, high)) => hex(low) & 0xFFFF_FFFF | hex(high) << 32,
        None => hex(v),
    }
}

/// The `_raop._tcp` TXT of the receiver's instance on `port`, asked of it
/// alone (a DNS-SD question sent to its `mdns_port`, not to the whole
/// network). A host may announce several (AirPlay 2 on 7000, AirPlay 1 on
/// 5000): the one judged is the one whose SRV gives `port`. `None`: it did
/// not answer in time, or not with that instance, which says nothing about it.
///
/// # Errors
/// The address does not resolve, or no socket could be opened.
pub async fn announced(
    host: &str,
    mdns_port: u16,
    port: u16,
) -> anyhow::Result<Option<Vec<String>>> {
    let target = tokio::net::lookup_host((host, mdns_port))
        .await?
        .find(SocketAddr::is_ipv4)
        .context("no IPv4 address")?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.send_to(&dns_question(SERVICE), target).await?;
    let until = Instant::now() + MDNS_WAIT;
    let mut buf = vec![0u8; 9000];
    // What every answer said so far: an instance's SRV and TXT may come apart.
    let mut said = Vec::new();
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(until, socket.recv_from(&mut buf)).await {
        if from.ip() != target.ip() {
            continue;
        }
        said.extend(raop_records(&buf[..n]).unwrap_or_default());
        if let Some(txt) = txt_on(&said, port) {
            return Ok(Some(txt));
        }
    }
    Ok(None)
}

/// A DNS question for the PTR of `name`, the answer asked in unicast.
fn dns_question(name: &str) -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.').filter(|l| !l.is_empty()) {
        q.push(u8::try_from(label.len()).unwrap_or(63));
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 12, 0x80, 1]);
    q
}

/// What a DNS answer says of one `_raop._tcp` instance.
#[derive(Debug, PartialEq)]
enum Said {
    Txt(Vec<String>),
    /// The port of its SRV.
    Port(u16),
}

/// The TXT of the instance whose SRV gives `port`: an instance with no SRV
/// is not known to be the receiver configured.
fn txt_on(said: &[(String, Said)], port: u16) -> Option<Vec<String>> {
    said.iter().find_map(|(instance, s)| {
        (*s == Said::Port(port))
            .then(|| {
                said.iter().find_map(|(name, s)| match s {
                    Said::Txt(txt) if name == instance => Some(txt.clone()),
                    _ => None,
                })
            })
            .flatten()
    })
}

/// The TXT and SRV of the `_raop._tcp` instances in a DNS answer, by
/// instance name (lowercase): bounded reads only, a malformed answer is
/// `None`.
fn raop_records(packet: &[u8]) -> Option<Vec<(String, Said)>> {
    let count = |at: usize| {
        Some(usize::from(u16::from_be_bytes([
            *packet.get(at)?,
            *packet.get(at + 1)?,
        ])))
    };
    let (questions, records) = (count(4)?, count(6)? + count(8)? + count(10)?);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(packet, at)? + 4;
    }
    let mut said = Vec::new();
    for _ in 0..records {
        let (name, end) = read_name(packet, at)?;
        let kind = count(end)?;
        let length = count(end + 8)?;
        let data = packet.get(end + 10..end + 10 + length)?;
        at = end + 10 + length;
        let name = name.to_ascii_lowercase();
        if !name.ends_with(SERVICE) {
            continue;
        }
        match kind {
            16 => {
                let mut txt = Vec::new();
                let mut i = 0;
                while let Some(&n) = data.get(i) {
                    txt.push(
                        String::from_utf8_lossy(data.get(i + 1..i + 1 + usize::from(n))?)
                            .into_owned(),
                    );
                    i += 1 + usize::from(n);
                }
                said.push((name, Said::Txt(txt)));
            }
            // Priority, weight, then the port.
            33 => said.push((
                name,
                Said::Port(u16::from_be_bytes([*data.get(4)?, *data.get(5)?])),
            )),
            _ => {}
        }
    }
    Some(said)
}

/// Where the name at `at` ends in the packet.
fn skip_name(packet: &[u8], at: usize) -> Option<usize> {
    read_name(packet, at).map(|(_, end)| end)
}

/// A name, its compression pointers followed (a few at most), and where
/// it ends at its first place.
fn read_name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut end = None;
    for _ in 0..32 {
        let n = *packet.get(at)?;
        if n == 0 {
            return Some((name, end.unwrap_or(at + 1)));
        }
        if n & 0xC0 == 0xC0 {
            end.get_or_insert(at + 2);
            at = usize::from(u16::from_be_bytes([n & 0x3F, *packet.get(at + 1)?]));
            continue;
        }
        let label = packet.get(at + 1..at + 1 + usize::from(n))?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(label));
        at += 1 + usize::from(n);
    }
    None
}

/// The receiver's volume in dB (a session of its own: what it plays is cut).
///
/// # Errors
/// The receiver is unreachable, or refused the session.
pub async fn read_volume(host: &str, port: u16) -> anyhow::Result<Option<f64>> {
    let mut session = Session::open(host, port).await?;
    let db = session.volume().await?;
    finish(session).await;
    Ok(db)
}

/// Sets the receiver's volume (dB) in a session of its own.
///
/// # Errors
/// The receiver is unreachable, or refused the session or the volume.
pub async fn set_volume(host: &str, port: u16, db: f64) -> anyhow::Result<()> {
    let mut session = Session::open(host, port).await?;
    let set = session.set_volume(db).await;
    finish(session).await;
    set
}

/// Says `pcm` at `during` dB, then puts the receiver's volume back, even
/// when the sound failed (it must not stay at the level of an announcement).
/// The volume put back is the receiver's own, read in the same session
/// (set from a remote meanwhile, it is kept); `after` only when it will not
/// say it. A receiver that will not take `during` still says it, at its own
/// volume, with a warning: nothing is put back then. Returns the receiver's
/// volume once done, when it is known. A `TEARDOWN` that fails after the
/// sound was heard is only a warning.
///
/// # Errors
/// The receiver is unreachable, refused a step, or the sound did not go.
pub async fn say(
    host: &str,
    port: u16,
    pcm: &[u8],
    during: f64,
    after: Option<f64>,
) -> anyhow::Result<Option<f64>> {
    let mut session = Session::open(host, port).await?;
    let after = session.volume().await?.or(after);
    let mut raised = true;
    let played = async {
        if let Err(e) = session.set_volume(during).await {
            if e.downcast_ref::<VolumeRefused>().is_none() {
                return Err(e);
            }
            // One with a fixed volume is still worth hearing.
            tracing::warn!(host, error = %e, "airplay announcement said at the receiver's own volume");
            raised = false;
        }
        session.play(pcm).await
    }
    .await;
    let put_back = match after.filter(|_| raised) {
        Some(db) => session.set_volume(db).await,
        None => Ok(()),
    };
    finish(session).await;
    played?;
    put_back?;
    Ok(after)
}

/// Ends a session whose work is done: a `TEARDOWN` that fails changes
/// nothing of what was heard or set.
async fn finish(session: Session) {
    let target = session.target;
    if let Err(e) = session.close().await {
        tracing::warn!(%target, error = %format!("{e:#}"), "airplay session not closed cleanly");
    }
}

/// What a sound cost the receiver: how many packets went, how many it asked
/// for again, and how often it asked for the time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub packets: usize,
    pub resent: usize,
    pub clock_requests: usize,
    pub control_packets: usize,
}

#[derive(Debug, Default)]
struct Counters {
    resent: AtomicUsize,
    clock: AtomicUsize,
    control: AtomicUsize,
}

/// The packets last sent, by sequence number.
type Kept = Arc<Mutex<Vec<Option<(u16, Vec<u8>)>>>>;

pub struct Session {
    rtsp: TcpStream,
    /// What was read past the last answer: the start of the next.
    pending: Vec<u8>,
    cseq: u32,
    /// The receiver's id for this session, given by `SETUP`.
    rtsp_id: Option<String>,
    url: String,
    instance: String,
    target: IpAddr,
    audio_port: u16,
    /// The receiver's control port, where the SYNC go.
    remote_control: Option<u16>,
    control: Arc<UdpSocket>,
    kept: Kept,
    counters: Arc<Counters>,
    tasks: Vec<JoinHandle<()>>,
}

impl Session {
    /// Opens a session: `ANNOUNCE` (L16, 44.1 kHz, stereo), `SETUP`,
    /// `RECORD`. Nothing plays yet.
    ///
    /// # Errors
    /// The receiver is unreachable, or refused a step.
    pub async fn open(host: &str, port: u16) -> anyhow::Result<Self> {
        let rtsp = timeout(TIMEOUT, TcpStream::connect((host, port)))
            .await
            .context("the receiver did not answer")??;
        let me = rtsp.local_addr()?.ip();
        let target = rtsp.peer_addr()?.ip();
        // The SDP and the session URL are written for IPv4.
        ensure!(target.is_ipv4(), "AirPlay over IPv6 is not supported");
        let control = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        let timing = UdpSocket::bind("0.0.0.0:0").await?;
        let (control_port, timing_port) =
            (control.local_addr()?.port(), timing.local_addr()?.port());
        let counters = Arc::new(Counters::default());
        let kept: Kept = Arc::new(Mutex::new(vec![None; KEPT]));
        let id = random() >> 32;
        let mut session = Self {
            rtsp,
            pending: Vec::new(),
            cseq: 0,
            rtsp_id: None,
            url: format!("rtsp://{me}/{id}"),
            instance: format!("{:016X}", random()),
            target,
            audio_port: 0,
            remote_control: None,
            control: Arc::clone(&control),
            kept: Arc::clone(&kept),
            counters: Arc::clone(&counters),
            tasks: vec![tokio::spawn(answer_clock(
                timing,
                target,
                Arc::clone(&counters),
            ))],
        };
        let challenge = base64(&random().to_be_bytes().repeat(2));
        session
            .request("OPTIONS", "*", &[("Apple-Challenge", &challenge)], b"")
            .await?;
        let sdp = format!(
            "v=0\r\no=iTunes {id} 0 IN IP4 {me}\r\ns=iTunes\r\nc=IN IP4 {target}\r\nt=0 0\r\n\
             m=audio 0 RTP/AVP 96\r\na=rtpmap:96 L16/44100/2\r\n"
        );
        session
            .ok(
                "ANNOUNCE",
                &[("Content-Type", "application/sdp")],
                sdp.as_bytes(),
            )
            .await?;
        let transport = format!(
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={control_port};timing_port={timing_port}"
        );
        let setup = session
            .ok("SETUP", &[("Transport", &transport)], b"")
            .await?;
        session.rtsp_id = Some(
            header(&setup.head, "session")
                .and_then(|v| v.split_whitespace().next())
                .context("SETUP gave no session")?
                .to_owned(),
        );
        session.audio_port =
            number_after(&setup.head, "server_port=").context("SETUP gave no audio port")?;
        session.remote_control = number_after(&setup.head, "control_port=");
        session.tasks.push(tokio::spawn(answer_control(
            control,
            target,
            session.remote_control,
            kept,
            counters,
        )));
        session
            .ok(
                "RECORD",
                &[("Range", "npt=0-"), ("RTP-Info", "seq=0;rtptime=0")],
                b"",
            )
            .await?;
        Ok(session)
    }

    /// The receiver's volume in dB, as it says it now; `None` when it will
    /// not say it.
    ///
    /// # Errors
    /// The receiver did not answer.
    pub async fn volume(&mut self) -> anyhow::Result<Option<f64>> {
        let url = self.url.clone();
        let answer = self
            .request(
                "GET_PARAMETER",
                &url,
                &[("Content-Type", "text/parameters")],
                b"volume\r\n",
            )
            .await?;
        if answer.status != 200 {
            return Ok(None);
        }
        Ok(said_volume(&answer.body).or_else(|| said_volume(&answer.head)))
    }

    /// Sets the volume (dB); it outlasts the session.
    ///
    /// # Errors
    /// The receiver refused it, or did not answer.
    pub async fn set_volume(&mut self, db: f64) -> anyhow::Result<()> {
        let body = format!("volume: {db:.6}\r\n");
        let url = self.url.clone();
        let answer = self
            .request(
                "SET_PARAMETER",
                &url,
                &[("Content-Type", "text/parameters")],
                body.as_bytes(),
            )
            .await?;
        if answer.status != 200 {
            let status = answer.head.lines().next().unwrap_or_default();
            return Err(VolumeRefused(status.to_owned()).into());
        }
        Ok(())
    }

    /// Plays `pcm` (44.1 kHz, 16-bit stereo, little-endian: not the
    /// big-endian of RTP's L16, which a receiver plays as noise) and returns once
    /// the receiver has played it: ahead of time but never in a burst
    /// (4× real time at most: a burst fills its buffer and makes the gaps),
    /// a SYNC each second that keeps moving until the end has been heard.
    ///
    /// # Errors
    /// The audio socket failed.
    pub async fn play(&mut self, pcm: &[u8]) -> anyhow::Result<Stats> {
        let audio = UdpSocket::bind("0.0.0.0:0").await?;
        let to = SocketAddr::new(self.target, self.audio_port);
        #[allow(clippy::cast_possible_truncation)]
        let ssrc = random() as u32;
        let start = Instant::now();
        let mut sent: u64 = 0;
        let mut packets = 0;
        self.sync(true, 0).await;
        let mut last_sync = start;
        for (n, sound) in pcm.chunks(PACKET).enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let seq = n as u16;
            let mut packet = rtp(n == 0, seq, sent, ssrc, sound);
            // The last, shorter one: completed with silence, not dropped.
            packet.resize(12 + PACKET, 0);
            audio.send_to(&packet, to).await?;
            self.kept.lock().expect("kept packets")[usize::from(seq) % KEPT] = Some((seq, packet));
            packets += 1;
            sent += u64::from(FRAMES);
            if last_sync.elapsed() >= Duration::from_secs(1) {
                self.sync(false, sent).await;
                last_sync = Instant::now();
            }
            let ahead = seconds(sent.saturating_sub(LATENCY)).max(seconds(sent) / 4.0);
            sleep_until(start + Duration::from_secs_f64(ahead)).await;
        }
        // Played ahead: wait until the receiver's playhead is past the end,
        // the SYNC still moving (a frozen SYNC says time stopped).
        let end = sent;
        loop {
            let played = frames(start.elapsed());
            if played > end + 2 * 44_100 {
                break;
            }
            self.sync(false, end.min(played + LATENCY)).await;
            sleep(Duration::from_millis(400)).await;
        }
        Ok(Stats {
            packets,
            resent: self.counters.resent.load(Ordering::Relaxed),
            clock_requests: self.counters.clock.load(Ordering::Relaxed),
            control_packets: self.counters.control.load(Ordering::Relaxed),
        })
    }

    /// Ends the session (the volume stays as set).
    ///
    /// # Errors
    /// The receiver did not answer.
    pub async fn close(mut self) -> anyhow::Result<()> {
        self.request("TEARDOWN", &self.url.clone(), &[], b"")
            .await?;
        Ok(())
    }

    async fn sync(&self, first: bool, sent: u64) {
        if let Some(port) = self.remote_control {
            // A SYNC lost is replaced a second later: nothing to do.
            let packet = sync_packet(first, sent, ntp_now());
            let _ = self.control.send_to(&packet, (self.target, port)).await;
        }
    }

    async fn ok(
        &mut self,
        method: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> anyhow::Result<Answer> {
        let url = self.url.clone();
        let answer = self.request(method, &url, headers, body).await?;
        if answer.status != 200 {
            return Err(Refused {
                method: method.to_owned(),
                answer: answer.head.lines().next().unwrap_or_default().to_owned(),
            }
            .into());
        }
        Ok(answer)
    }

    async fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> anyhow::Result<Answer> {
        self.cseq += 1;
        let mut out = format!(
            "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: iTunes/11.0.5 (Windows; N)\r\n\
             Client-Instance: {i}\r\nDACP-ID: {i}\r\nActive-Remote: {}\r\n",
            self.cseq,
            random() >> 33,
            i = self.instance,
        );
        if let Some(session) = &self.rtsp_id {
            let _ = write!(out, "Session: {session}\r\n");
        }
        for (k, v) in headers {
            let _ = write!(out, "{k}: {v}\r\n");
        }
        if !body.is_empty() {
            let _ = write!(out, "Content-Length: {}\r\n", body.len());
        }
        out.push_str("\r\n");
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(body);
        timeout(TIMEOUT, self.exchange(&bytes))
            .await
            .with_context(|| format!("no answer to {method}"))?
    }

    /// Sends `request` and gives its answer: the one with its `CSeq`. An
    /// older answer (to a request given up after a delay) is skipped.
    async fn exchange(&mut self, request: &[u8]) -> anyhow::Result<Answer> {
        self.rtsp.write_all(request).await?;
        loop {
            let answer = self.answer().await?;
            let cseq: u32 = header(&answer.head, "cseq")
                .and_then(|v| v.parse().ok())
                .context("an answer without CSeq")?;
            match cseq.cmp(&self.cseq) {
                std::cmp::Ordering::Equal => return Ok(answer),
                std::cmp::Ordering::Less => {
                    tracing::debug!(cseq, "late airplay answer skipped");
                }
                std::cmp::Ordering::Greater => {
                    anyhow::bail!("an answer to a request not sent ({cseq})")
                }
            }
        }
    }

    /// The next answer on the connection; what follows it stays pending.
    async fn answer(&mut self) -> anyhow::Result<Answer> {
        let mut buf = [0u8; 4096];
        let head_end = loop {
            if let Some(at) = self.pending.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            ensure!(self.pending.len() < MAX_ANSWER, "an answer too long");
            let n = self.rtsp.read(&mut buf).await?;
            ensure!(n > 0, "the receiver closed the connection");
            self.pending.extend_from_slice(&buf[..n]);
        };
        let head = String::from_utf8_lossy(&self.pending[..head_end]).into_owned();
        let length: usize = header(&head, "content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        ensure!(length <= MAX_ANSWER, "an answer too long");
        let end = head_end + 4 + length;
        while self.pending.len() < end {
            let n = self.rtsp.read(&mut buf).await?;
            ensure!(n > 0, "the receiver closed the connection");
            self.pending.extend_from_slice(&buf[..n]);
        }
        let body = String::from_utf8_lossy(&self.pending[head_end + 4..end]).into_owned();
        self.pending.drain(..end);
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .context("not an RTSP answer")?;
        Ok(Answer { status, head, body })
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("url", &self.url)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

/// A header's value, its name in any case.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// The digits right after `key` (`server_port=6000`).
fn number_after<T: std::str::FromStr>(text: &str, key: &str) -> Option<T> {
    let rest = &text[text.find(key)? + key.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The number after `key` (`volume: -6.000000`), sign and decimals included.
fn value_after(text: &str, key: &str) -> Option<f64> {
    let rest = text[text.find(key)? + key.len()..].trim_start();
    rest.split_whitespace().next()?.parse().ok()
}

/// The volume a receiver says, as a volume it can be given back: `nan` and
/// `inf` are none, anything else is held within mute and 0 dB.
fn said_volume(text: &str) -> Option<f64> {
    value_after(text, "volume:")
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(MUTED, 0.0))
}

/// One audio packet: RTP, payload type 96, the marker on the first.
fn rtp(first: bool, seq: u16, timestamp: u64, ssrc: u32, sound: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + sound.len());
    p.extend_from_slice(&[0x80, if first { 0xE0 } else { 0x60 }]);
    p.extend_from_slice(&seq.to_be_bytes());
    #[allow(clippy::cast_possible_truncation)]
    p.extend_from_slice(&(timestamp as u32).to_be_bytes());
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(sound);
    p
}

/// The SYNC: what is playing now (sent − latency), the time, what was sent.
fn sync_packet(first: bool, sent: u64, now: u64) -> [u8; 20] {
    let mut p = [0u8; 20];
    p[0] = if first { 0x90 } else { 0x80 };
    p[1] = 0xD4;
    p[2..4].copy_from_slice(&7u16.to_be_bytes());
    #[allow(clippy::cast_possible_truncation)]
    p[4..8].copy_from_slice(&(sent.saturating_sub(LATENCY) as u32).to_be_bytes());
    p[8..16].copy_from_slice(&now.to_be_bytes());
    #[allow(clippy::cast_possible_truncation)]
    p[16..20].copy_from_slice(&(sent as u32).to_be_bytes());
    p
}

/// The answer to a clock question: its sequence, its own send time as
/// origin, then our time twice (received, sent).
fn clock_answer(question: &[u8], now: u64) -> Option<[u8; 32]> {
    if question.len() < 32 {
        return None;
    }
    let mut p = [0u8; 32];
    p[0] = 0x80;
    p[1] = 0xD3;
    p[2..4].copy_from_slice(&question[2..4]);
    p[4..8].copy_from_slice(&7u32.to_be_bytes());
    p[8..16].copy_from_slice(&question[24..32]);
    p[16..24].copy_from_slice(&now.to_be_bytes());
    p[24..32].copy_from_slice(&now.to_be_bytes());
    Some(p)
}

/// A request to send packets again (type 85): the first and how many.
fn resend_request(packet: &[u8]) -> Option<(u16, u16)> {
    if packet.len() < 8 || packet[1] & 0x7F != 85 {
        return None;
    }
    Some((
        u16::from_be_bytes([packet[4], packet[5]]),
        u16::from_be_bytes([packet[6], packet[7]]),
    ))
}

/// Answers the receiver's clock questions, and nobody else's.
async fn answer_clock(socket: UdpSocket, target: IpAddr, counters: Arc<Counters>) {
    let mut buf = [0u8; 128];
    while let Ok((n, from)) = socket.recv_from(&mut buf).await {
        if from.ip() != target {
            continue;
        }
        if let Some(answer) = clock_answer(&buf[..n], ntp_now()) {
            counters.clock.fetch_add(1, Ordering::Relaxed);
            let _ = socket.send_to(&answer, from).await;
        }
    }
}

/// Sends again the packets the receiver asks for: a request from anyone
/// else would make Moli send up to [`MAX_RESEND`] packets for nothing.
async fn answer_control(
    socket: Arc<UdpSocket>,
    target: IpAddr,
    port: Option<u16>,
    kept: Kept,
    counters: Arc<Counters>,
) {
    let mut buf = [0u8; 2048];
    while let Ok((n, from)) = socket.recv_from(&mut buf).await {
        if from.ip() != target {
            continue;
        }
        counters.control.fetch_add(1, Ordering::Relaxed);
        let (Some((first, count)), Some(port)) = (resend_request(&buf[..n]), port) else {
            continue;
        };
        for k in 0..count.min(MAX_RESEND) {
            let seq = first.wrapping_add(k);
            let packet = match &kept.lock().expect("kept packets")[usize::from(seq) % KEPT] {
                Some((s, p)) if *s == seq => p.clone(),
                _ => continue,
            };
            let mut again = vec![0x80, 0xD6, 0, 0];
            again.extend_from_slice(&packet);
            if socket.send_to(&again, (target, port)).await.is_ok() {
                counters.resent.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Now, as NTP: seconds since 1900 in the high half, the fraction below.
fn ntp_now() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let fraction = (u64::from(since.subsec_nanos()) << 32) / 1_000_000_000;
    ((since.as_secs() + NTP_EPOCH) << 32) | fraction
}

#[allow(clippy::cast_precision_loss)]
fn seconds(frames: u64) -> f64 {
    frames as f64 / RATE
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn frames(elapsed: Duration) -> u64 {
    (elapsed.as_secs_f64() * RATE) as u64
}

/// Not for secrets: ids and the challenge only need to differ.
fn random() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.finish()
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_linear_in_db_and_zero_is_mute() {
        assert!((db_of_percent(40.0) + 18.0).abs() < 1e-9);
        assert!((db_of_percent(80.0) + 6.0).abs() < 1e-9);
        assert!((db_of_percent(100.0)).abs() < 1e-9);
        assert!((db_of_percent(0.0) - MUTED).abs() < 1e-9);
        assert!((percent_of_db(-6.0) - 80.0).abs() < 1e-9);
        assert!(percent_of_db(MUTED).abs() < 1e-9);
    }

    #[test]
    fn an_audio_packet_marks_the_first_and_keeps_the_sound_little_endian() {
        let p = rtp(true, 1, 352, 0xAABB_CCDD, &[1, 2, 3, 4]);
        assert_eq!(
            &p[..12],
            &[0x80, 0xE0, 0, 1, 0, 0, 1, 0x60, 0xAA, 0xBB, 0xCC, 0xDD]
        );
        assert_eq!(&p[12..], &[1, 2, 3, 4]);
        assert_eq!(rtp(false, 2, 0, 0, &[])[1], 0x60);
    }

    #[test]
    fn a_sync_says_what_plays_now_two_seconds_behind_what_was_sent() {
        let p = sync_packet(true, LATENCY + 1000, 0x0102_0304_0506_0708);
        assert_eq!(&p[..4], &[0x90, 0xD4, 0, 7]);
        assert_eq!(u32::from_be_bytes(p[4..8].try_into().unwrap()), 1000);
        assert_eq!(&p[8..16], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(u32::from_be_bytes(p[16..20].try_into().unwrap()), 89_200);
        assert_eq!(sync_packet(false, 10, 0)[0], 0x80);
        assert_eq!(
            u32::from_be_bytes(sync_packet(false, 10, 0)[4..8].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn a_clock_question_is_answered_with_its_own_send_time_as_origin() {
        let mut q = [0u8; 32];
        q[1] = 0xD2;
        q[2..4].copy_from_slice(&[0x12, 0x34]);
        q[24..32].copy_from_slice(&[9; 8]);
        let a = clock_answer(&q, 5).unwrap();
        assert_eq!(&a[..8], &[0x80, 0xD3, 0x12, 0x34, 0, 0, 0, 7]);
        assert_eq!(&a[8..16], &[9; 8]);
        assert_eq!(u64::from_be_bytes(a[16..24].try_into().unwrap()), 5);
        assert!(clock_answer(&q[..20], 5).is_none());
    }

    #[test]
    fn a_resend_request_is_type_85_only() {
        assert_eq!(
            resend_request(&[0x80, 0xD5, 0, 1, 0, 40, 0, 3]),
            Some((40, 3))
        );
        assert_eq!(resend_request(&[0x80, 0xD4, 0, 1, 0, 40, 0, 3]), None);
        assert_eq!(resend_request(&[0x80, 0xD5, 0, 1]), None);
    }

    #[test]
    fn what_the_receiver_answers_is_read_whatever_the_case() {
        let head = "RTSP/1.0 200 OK\r\nCSeq: 3\r\nsession: 1A2B3C;timeout=60\r\n\
                    Transport: RTP/AVP/UDP;unicast;mode=record;server_port=6000;control_port=6001;timing_port=6002";
        assert_eq!(header(head, "Session"), Some("1A2B3C;timeout=60"));
        assert_eq!(number_after::<u16>(head, "server_port="), Some(6000));
        assert_eq!(number_after::<u16>(head, "control_port="), Some(6001));
        assert_eq!(value_after("volume: -6.000000\r\n", "volume:"), Some(-6.0));
        assert_eq!(value_after("volume: x", "volume:"), None);
        // A volume to give back: finite, between mute and 0 dB.
        assert_eq!(said_volume("volume: -6.5"), Some(-6.5));
        for none in ["volume: nan", "volume: NaN", "volume: inf", "volume: -inf"] {
            assert_eq!(said_volume(none), None, "{none}");
        }
        assert_eq!(said_volume("volume: 12"), Some(0.0));
        assert_eq!(said_volume("volume: -200"), Some(MUTED));
    }

    #[test]
    fn the_challenge_is_plain_base64_without_padding() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE");
        assert_eq!(base64(b"M"), "TQ");
        assert_eq!(base64(&[0; 16]).len(), 22);
    }

    /// A fake receiver's RTSP side: answers each step, as a Freebox does
    /// (or refuses `refuse`, as one that wants a password), and keeps the
    /// methods it was asked, in order.
    async fn fake_rtsp(
        rtsp: tokio::net::TcpListener,
        audio_port: u16,
        control_port: u16,
        log: Arc<Mutex<Vec<String>>>,
        refuse: Option<&'static str>,
    ) {
        let (mut tcp, _) = rtsp.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") else {
                let n = tcp.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                got.extend_from_slice(&buf[..n]);
                continue;
            };
            let request = String::from_utf8_lossy(&got[..end]).into_owned();
            let length: usize =
                header(&request, "content-length").map_or(0, |v| v.parse().unwrap());
            while got.len() < end + 4 + length {
                let n = tcp.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            let asked = String::from_utf8_lossy(&got[end + 4..end + 4 + length])
                .trim()
                .to_owned();
            got.drain(..end + 4 + length);
            let method = request.split_whitespace().next().unwrap().to_owned();
            let cseq = header(&request, "cseq").unwrap().to_owned();
            let (extra, body) = match method.as_str() {
                "SETUP" => (
                    format!(
                        "Session: 42\r\nTransport: RTP/AVP/UDP;unicast;mode=record;\
                         server_port={audio_port};control_port={control_port}\r\n"
                    ),
                    "",
                ),
                "GET_PARAMETER" => ("Content-Length: 19\r\n".to_owned(), "volume: -6.000000\r\n"),
                _ => (String::new(), ""),
            };
            log.lock().unwrap().push(if method == "SET_PARAMETER" {
                format!("{method} {asked}")
            } else {
                method.clone()
            });
            // Refusing TEARDOWN: hanging up without a word, a real failure.
            if refuse == Some("TEARDOWN") && method == "TEARDOWN"
                || refuse.and_then(|r| r.strip_prefix("hang up ")) == Some(method.as_str())
            {
                return;
            }
            let answer = if refuse == Some(method.as_str()) {
                format!("RTSP/1.0 403 Forbidden\r\nCSeq: {cseq}\r\n\r\n")
            } else {
                format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n{extra}\r\n{body}")
            };
            tcp.write_all(answer.as_bytes()).await.unwrap();
            if method == "TEARDOWN" {
                return;
            }
        }
    }

    /// A receiver on loopback: the RTSP steps, the audio counted, one packet
    /// asked for again on the control channel and checked when it comes back.
    #[tokio::test]
    async fn a_whole_session_against_a_fake_receiver() {
        let rtsp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = rtsp.local_addr().unwrap().port();
        let methods = Arc::new(Mutex::new(Vec::<String>::new()));
        let receiver = tokio::spawn(fake_rtsp(
            rtsp,
            audio.local_addr().unwrap().port(),
            control.local_addr().unwrap().port(),
            Arc::clone(&methods),
            None,
        ));
        let mut session = Session::open("127.0.0.1", port).await.unwrap();
        session.set_volume(-6.0).await.unwrap();
        assert_eq!(session.volume().await.unwrap(), Some(-6.0));
        // 20 packets of a known sound; the fake asks for packet 2 again.
        let pcm: Vec<u8> = (0..20 * PACKET)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let asks = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (mut packets, mut first) = (Vec::new(), None);
            while packets.len() < 20 {
                let (n, _) = audio.recv_from(&mut buf).await.unwrap();
                packets.push(buf[..n].to_vec());
            }
            let (mut syncs, mut resent) = (0, None);
            loop {
                let (n, from) = control.recv_from(&mut buf).await.unwrap();
                match buf[1] {
                    0xD4 => {
                        syncs += 1;
                        first.get_or_insert(buf[0]);
                        if syncs == 1 {
                            control
                                .send_to(&[0x80, 0xD5, 0, 1, 0, 2, 0, 1], from)
                                .await
                                .unwrap();
                        }
                    }
                    0xD6 => resent = Some(buf[4..n].to_vec()),
                    _ => {}
                }
                if resent.is_some() && syncs >= 2 {
                    return (packets, first, resent);
                }
            }
        });
        let stats = session.play(&pcm).await.unwrap();
        // Bounded: without SYNC or a packet sent again, fail, do not hang.
        let (packets, first_sync, resent) = timeout(Duration::from_secs(5), asks)
            .await
            .expect("no SYNC, or the packet asked for never came back")
            .unwrap();
        session.close().await.unwrap();
        receiver.await.unwrap();
        assert_eq!(
            *methods.lock().unwrap(),
            [
                "OPTIONS",
                "ANNOUNCE",
                "SETUP",
                "RECORD",
                "SET_PARAMETER volume: -6.000000",
                "GET_PARAMETER",
                "TEARDOWN"
            ]
        );
        assert_eq!(stats.packets, 20);
        assert_eq!(packets[0][1], 0xE0, "the first packet is marked");
        assert_eq!(u16::from_be_bytes([packets[3][2], packets[3][3]]), 3);
        assert_eq!(
            u32::from_be_bytes(packets[3][4..8].try_into().unwrap()),
            3 * FRAMES
        );
        assert_eq!(
            &packets[3][12..],
            &pcm[3 * PACKET..4 * PACKET],
            "the sound as given"
        );
        assert_eq!(first_sync, Some(0x90));
        assert_eq!(
            resent.as_deref(),
            Some(&packets[2][..]),
            "packet 2, sent again whole"
        );
        assert_eq!(stats.resent, 1);
    }

    /// A fake receiver on loopback, for one session, and what it was asked.
    async fn receiver(
        refuse: Option<&'static str>,
    ) -> (u16, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
        let rtsp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        // Nobody reads there: the sound and the SYNC go nowhere.
        let sink = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sink_port = sink.local_addr().unwrap().port();
        let port = rtsp.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let fake = fake_rtsp(rtsp, sink_port, sink_port, Arc::clone(&log), refuse);
        let task = tokio::spawn(async move {
            fake.await;
            drop(sink);
        });
        (port, log, task)
    }

    /// Over a second of sending, the SYNC must not wait for the end: a
    /// receiver left without one takes everything and plays nothing.
    #[tokio::test]
    async fn a_long_sound_gets_a_sync_while_it_is_still_being_sent() {
        let rtsp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = rtsp.local_addr().unwrap().port();
        let fake = tokio::spawn(fake_rtsp(
            rtsp,
            audio.local_addr().unwrap().port(),
            control.local_addr().unwrap().port(),
            Arc::new(Mutex::new(Vec::new())),
            None,
        ));
        // 3.5 s of sound: about 1.5 s of sending, past the first second.
        let count = 438;
        let watch = tokio::spawn(async move {
            let (mut a, mut c) = ([0u8; 2048], [0u8; 64]);
            let mut syncs = 0;
            loop {
                tokio::select! {
                    got = audio.recv_from(&mut a) => {
                        let (n, _) = got.unwrap();
                        if n >= 4 && u16::from_be_bytes([a[2], a[3]]) == count - 1 {
                            return syncs;
                        }
                    }
                    got = control.recv_from(&mut c) => {
                        if c[1] == 0xD4 && got.is_ok() {
                            syncs += 1;
                        }
                    }
                }
            }
        });
        let mut session = Session::open("127.0.0.1", port).await.unwrap();
        session
            .play(&vec![0; usize::from(count) * PACKET])
            .await
            .unwrap();
        let syncs = timeout(Duration::from_secs(5), watch)
            .await
            .expect("the last packet never came")
            .unwrap();
        session.close().await.unwrap();
        fake.await.unwrap();
        assert!(
            syncs >= 2,
            "{syncs} SYNC before the last packet: the first only"
        );
    }

    /// The volume put back is the receiver's own, read in the session: set
    /// from a remote since Moli last knew it (-3 dB), it is kept (-6 dB).
    #[tokio::test]
    async fn the_volume_put_back_is_the_one_the_receiver_says_in_the_session() {
        let (port, log, fake) = receiver(None).await;
        let after = say("127.0.0.1", port, &[0; 4 * PACKET], -18.0, Some(-3.0))
            .await
            .unwrap();
        fake.await.unwrap();
        assert_eq!(after, Some(-6.0), "what the fake says it is");
        assert_eq!(
            *log.lock().unwrap(),
            [
                "OPTIONS",
                "ANNOUNCE",
                "SETUP",
                "RECORD",
                "GET_PARAMETER",
                "SET_PARAMETER volume: -18.000000",
                "SET_PARAMETER volume: -6.000000",
                "TEARDOWN"
            ]
        );
    }

    /// A receiver that will not give its volume is not refusing the
    /// session: the volume Moli followed is put back instead.
    #[tokio::test]
    async fn a_receiver_that_keeps_its_volume_to_itself_gets_the_one_moli_followed() {
        let (port, log, fake) = receiver(Some("GET_PARAMETER")).await;
        let after = say("127.0.0.1", port, &[0; 4 * PACKET], -18.0, Some(-3.0))
            .await
            .unwrap();
        fake.await.unwrap();
        assert_eq!(after, Some(-3.0));
        assert_eq!(log.lock().unwrap()[6], "SET_PARAMETER volume: -3.000000");
        let (port, _, fake) = receiver(Some("GET_PARAMETER")).await;
        assert_eq!(read_volume("127.0.0.1", port).await.unwrap(), None);
        fake.await.unwrap();
    }

    /// A volume refused is an error of its own, never « the receiver
    /// wants a password »; a receiver that hangs up on `TEARDOWN` after
    /// the sound changes nothing of what was heard.
    #[tokio::test]
    async fn only_the_steps_of_a_session_can_be_refused() {
        let (port, _, fake) = receiver(Some("SET_PARAMETER")).await;
        let e = set_volume("127.0.0.1", port, -6.0).await.unwrap_err();
        fake.await.unwrap();
        assert!(e.downcast_ref::<Refused>().is_none(), "{e:#}");
        assert!(
            format!("{e:#}").contains("did not take the volume"),
            "{e:#}"
        );
        // Said all the same, at the receiver's own volume: nothing put back.
        let (port, log, fake) = receiver(Some("SET_PARAMETER")).await;
        let after = say("127.0.0.1", port, &[0; 4 * PACKET], -18.0, Some(-3.0))
            .await
            .expect("a receiver with a fixed volume is still heard");
        fake.await.unwrap();
        assert_eq!(after, Some(-6.0));
        assert_eq!(
            *log.lock().unwrap(),
            [
                "OPTIONS",
                "ANNOUNCE",
                "SETUP",
                "RECORD",
                "GET_PARAMETER",
                "SET_PARAMETER volume: -18.000000",
                "TEARDOWN"
            ]
        );
        // Hanging up on the volume is no fixed volume: the announcement stops.
        let (port, _, fake) = receiver(Some("hang up SET_PARAMETER")).await;
        say("127.0.0.1", port, &[0; 4 * PACKET], -18.0, Some(-3.0))
            .await
            .expect_err("a receiver gone is not one with a fixed volume");
        fake.await.unwrap();
        let (port, _, fake) = receiver(Some("TEARDOWN")).await;
        say("127.0.0.1", port, &[0; 4 * PACKET], -18.0, None)
            .await
            .expect("heard: a failed TEARDOWN is only a warning");
        fake.await.unwrap();
    }

    /// Answers are matched by their `CSeq`: a late one (to a request given
    /// up after a delay) is skipped, and two that arrive together are both
    /// kept, each for its own request.
    #[tokio::test]
    async fn each_answer_goes_to_its_own_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let fake = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf).await.unwrap();
            // A late answer to nothing, then those to OPTIONS and to the
            // GET_PARAMETER not even sent yet, in one write.
            tcp.write_all(
                b"RTSP/1.0 200 OK\r\nCSeq: 0\r\n\r\n\
                  RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n\
                  RTSP/1.0 200 OK\r\nCSeq: 2\r\nContent-Length: 19\r\n\r\nvolume: -9.000000\r\n",
            )
            .await
            .unwrap();
            // Keep the connection open until the client is done.
            let _ = tcp.read(&mut buf).await;
        });
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let control = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let mut session = Session {
            rtsp: tcp,
            pending: Vec::new(),
            cseq: 0,
            rtsp_id: None,
            url: "rtsp://127.0.0.1/1".into(),
            instance: "0".into(),
            target: "127.0.0.1".parse().unwrap(),
            audio_port: 0,
            remote_control: None,
            control,
            kept: Arc::new(Mutex::new(Vec::new())),
            counters: Arc::default(),
            tasks: Vec::new(),
        };
        let options = session.request("OPTIONS", "*", &[], b"").await.unwrap();
        assert_eq!(header(&options.head, "cseq"), Some("1"));
        assert_eq!(session.volume().await.unwrap(), Some(-9.0));
        drop(session);
        fake.await.unwrap();
    }

    /// The control socket answers the receiver only: a resend request from
    /// another address sends nothing.
    #[tokio::test]
    async fn a_resend_request_from_anyone_else_is_ignored() {
        let control = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let at = control.local_addr().unwrap();
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let kept: Kept = Arc::new(Mutex::new(vec![None; KEPT]));
        kept.lock().unwrap()[2] = Some((2, vec![7; 16]));
        let counters = Arc::new(Counters::default());
        // The receiver is said to be 127.0.0.2: the request from 127.0.0.1
        // comes from someone else.
        let task = tokio::spawn(answer_control(
            Arc::clone(&control),
            "127.0.0.2".parse().unwrap(),
            Some(receiver.local_addr().unwrap().port()),
            Arc::clone(&kept),
            Arc::clone(&counters),
        ));
        let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        stranger
            .send_to(&[0x80, 0xD5, 0, 1, 0, 2, 0, 1], at)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            counters.control.load(Ordering::Relaxed),
            0,
            "not even counted"
        );
        assert_eq!(counters.resent.load(Ordering::Relaxed), 0);
        task.abort();
        // The clock socket too: a question from elsewhere gets no answer.
        let timing = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = timing.local_addr().unwrap();
        let counters = Arc::new(Counters::default());
        let task = tokio::spawn(answer_clock(
            timing,
            "127.0.0.2".parse().unwrap(),
            Arc::clone(&counters),
        ));
        stranger.send_to(&[0x80; 32], at).await.unwrap();
        let mut buf = [0u8; 64];
        assert!(
            timeout(Duration::from_millis(200), stranger.recv_from(&mut buf))
                .await
                .is_err(),
            "the stranger got the time"
        );
        assert_eq!(counters.clock.load(Ordering::Relaxed), 0);
        task.abort();
    }

    /// The last packet, shorter, goes out completed with silence.
    #[tokio::test]
    async fn the_last_packet_is_completed_not_dropped() {
        let rtsp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = rtsp.local_addr().unwrap().port();
        let audio_port = audio.local_addr().unwrap().port();
        let fake = tokio::spawn(fake_rtsp(
            rtsp,
            audio_port,
            audio_port,
            Arc::new(Mutex::new(Vec::new())),
            None,
        ));
        let mut session = Session::open("127.0.0.1", port).await.unwrap();
        let pcm = vec![5u8; PACKET + 8];
        let stats = session.play(&pcm).await.unwrap();
        session.close().await.unwrap();
        fake.await.unwrap();
        assert_eq!(stats.packets, 2);
        let mut buf = [0u8; 2048];
        let mut last = Vec::new();
        while let Ok(Ok((n, _))) =
            timeout(Duration::from_millis(100), audio.recv_from(&mut buf)).await
        {
            if buf[1] & 0x7F == 0x60 && u16::from_be_bytes([buf[2], buf[3]]) == 1 {
                last = buf[..n].to_vec();
            }
        }
        assert_eq!(last.len(), 12 + PACKET, "a whole packet");
        assert_eq!(&last[12..20], &[5; 8], "the sound left");
        assert!(last[20..].iter().all(|b| *b == 0), "then silence");
    }

    fn txt(fields: &str) -> Vec<String> {
        fields.split_whitespace().map(str::to_owned).collect()
    }

    /// What theomarie measured in #68 (HomePod mini, Apple TV 4K, macOS),
    /// and the Delta Player: `et` and `cn` hold 0 everywhere, the rest
    /// tells them apart.
    #[test]
    fn only_a_plain_airplay_1_receiver_passes() {
        let delta = "et=0,1,3,5 cn=0,1,2,3 pw=false sf=0x44 ft=0x12BFFEBB am=AppleTV3,2";
        assert_eq!(plain(&txt(delta)), Ok(()));
        let homepod = "et=0,3,5 cn=0,1,2,3 sf=0xa0c04 ft=0x4A7FCA00,0x3C356BD0";
        assert_eq!(plain(&txt(homepod)), Err(NotPlain::AirPlay2));
        let apple_tv = "et=0,3,5 cn=0,1,2,3 sf=0x644 ft=0x4A7FDFD5,0x3C175FDE";
        assert_eq!(plain(&txt(apple_tv)), Err(NotPlain::AirPlay2));
        let mac = "et=0,3,5 cn=0,1,2,3 sf=0x204 ft=0x4A7FCFD5,0x38174FDE";
        assert_eq!(plain(&txt(mac)), Err(NotPlain::AirPlay2));
        // AirPlay 1, but closed.
        let one = "et=0,1 cn=0,1 ft=0x5A7FFFF7";
        assert_eq!(
            plain(&txt(&format!("{one} pw=true"))),
            Err(NotPlain::Password)
        );
        assert_eq!(
            plain(&txt(&format!("{one} sf=0x80"))),
            Err(NotPlain::Password)
        );
        assert_eq!(
            plain(&txt(&format!("{one} sf=0x200"))),
            Err(NotPlain::Pairing)
        );
        assert_eq!(
            plain(&txt(&format!("{one} sf=0x8"))),
            Err(NotPlain::Pairing)
        );
        assert_eq!(plain(&txt("et=1 cn=0")), Err(NotPlain::Encrypted));
        assert_eq!(plain(&txt("et=0 cn=1")), Err(NotPlain::Compressed));
        assert_eq!(
            plain(&txt("cn=0")),
            Err(NotPlain::Encrypted),
            "said nothing"
        );
        // `features` and `flags`, as `_airplay._tcp` writes them.
        assert_eq!(
            plain(&txt("et=0 cn=0 features=0x0,0x40")),
            Err(NotPlain::AirPlay2)
        );
        assert_eq!(plain(&txt("et=0 cn=0 flags=0x200")), Err(NotPlain::Pairing));
    }

    const DELTA: [&str; 4] = ["et=0,1,3,5", "cn=0,1,2,3", "sf=0x44", "ft=0x12BFFEBB"];

    /// A DNS answer as the Delta Player gives it, for each instance (name,
    /// port, TXT): a PTR, then its TXT and its SRV under a compressed name.
    fn dns_answer(instances: &[(&str, u16, &[&str])]) -> Vec<u8> {
        let records = u8::try_from(3 * instances.len()).unwrap();
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, records, 0, 0, 0, 0];
        let service_at = u8::try_from(p.len()).unwrap();
        for label in ["_raop", "_tcp", "local"] {
            p.push(u8::try_from(label.len()).unwrap());
            p.extend_from_slice(label.as_bytes());
        }
        p.push(0);
        for (i, (name, port, fields)) in instances.iter().enumerate() {
            // PTR _raop._tcp.local -> "<name>._raop._tcp.local"
            if i > 0 {
                p.extend_from_slice(&[0xC0, service_at]);
            }
            p.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0x11, 0x94]);
            p.extend_from_slice(&u16::try_from(name.len() + 3).unwrap().to_be_bytes());
            let instance_at = u8::try_from(p.len()).unwrap();
            p.push(u8::try_from(name.len()).unwrap());
            p.extend_from_slice(name.as_bytes());
            p.extend_from_slice(&[0xC0, service_at]);
            // TXT under a pointer to the instance name.
            p.extend_from_slice(&[0xC0, instance_at, 0, 16, 0x80, 1, 0, 0, 0x11, 0x94]);
            let data: Vec<u8> = fields
                .iter()
                .flat_map(|f| {
                    [vec![u8::try_from(f.len()).unwrap()], f.as_bytes().to_vec()].concat()
                })
                .collect();
            p.extend_from_slice(&u16::try_from(data.len()).unwrap().to_be_bytes());
            p.extend_from_slice(&data);
            // SRV: priority, weight, port, target.
            p.extend_from_slice(&[0xC0, instance_at, 0, 33, 0x80, 1, 0, 0, 0x11, 0x94, 0, 8]);
            p.extend_from_slice(&[0, 0, 0, 0]);
            p.extend_from_slice(&port.to_be_bytes());
            p.extend_from_slice(&[0xC0, service_at]);
        }
        p
    }

    fn delta_answer() -> Vec<u8> {
        dns_answer(&[("AB@Salon", PORT, &DELTA)])
    }

    #[test]
    fn the_txt_is_read_from_a_dns_answer_and_nothing_else_is_trusted() {
        let answer = delta_answer();
        let got = txt_on(&raop_records(&answer).unwrap(), PORT).unwrap();
        assert_eq!(got, DELTA);
        assert_eq!(plain(&got), Ok(()));
        // Cut anywhere, or looping on itself: no TXT, no panic.
        for end in 0..answer.len() {
            assert!(raop_records(&answer[..end]).is_none(), "{end}");
        }
        let mut looping = answer.clone();
        let at = looping
            .windows(4)
            .position(|w| w == [0, 16, 0x80, 1])
            .unwrap()
            - 2;
        looping[at..at + 2].copy_from_slice(&[0xC0, u8::try_from(at).unwrap()]);
        assert!(raop_records(&looping).is_none());
        // The question asks the PTR of the service, in unicast.
        let q = dns_question(SERVICE);
        assert_eq!(&q[4..6], &[0, 1]);
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0x80, 1]);
    }

    /// A host that announces two instances (AirPlay 2 on 7000, AirPlay 1 on
    /// 5000) is judged on the one at the port configured; a TXT whose SRV
    /// did not come is not known to be that receiver.
    #[test]
    fn the_txt_judged_is_the_one_of_the_port_configured() {
        let homepod = [
            "et=0,3,5",
            "cn=0,1,2,3",
            "sf=0xa0c04",
            "ft=0x4A7FCA00,0x3C356BD0",
        ];
        let answer = dns_answer(&[("AirPlay 2", 7000, &homepod), ("AirPlay 1", PORT, &DELTA)]);
        let said = raop_records(&answer).unwrap();
        assert_eq!(txt_on(&said, PORT).unwrap(), DELTA);
        assert_eq!(
            plain(&txt_on(&said, 7000).unwrap()),
            Err(NotPlain::AirPlay2)
        );
        assert_eq!(txt_on(&said, 6000), None);
        let no_srv: Vec<_> = said
            .into_iter()
            .filter(|(_, s)| !matches!(s, Said::Port(_)))
            .collect();
        assert_eq!(txt_on(&no_srv, PORT), None);
    }

    /// Asked of a fake receiver on loopback: its TXT; one that stays silent
    /// is `None`, not an error.
    #[tokio::test]
    async fn the_txt_is_asked_of_the_receiver_alone() {
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = responder.local_addr().unwrap().port();
        let fake = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (n, from) = responder.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], &dns_question(SERVICE)[..]);
            responder.send_to(&delta_answer(), from).await.unwrap();
        });
        let got = announced("127.0.0.1", port, PORT).await.unwrap().unwrap();
        fake.await.unwrap();
        assert!(got.contains(&"sf=0x44".to_owned()));
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = silent.local_addr().unwrap().port();
        assert_eq!(announced("127.0.0.1", port, PORT).await.unwrap(), None);
    }

    /// The TXT in one answer, the SRV in the next: both are kept.
    #[tokio::test]
    async fn an_instance_told_in_two_answers_is_put_together() {
        let full = delta_answer();
        let srv_at = full
            .windows(4)
            .rposition(|w| w[0] == 0xC0 && w[2..] == [0, 33])
            .unwrap();
        let mut txt_only = full[..srv_at].to_vec();
        txt_only[7] = 2;
        let mut srv_only = full.clone();
        let txt_at = full.windows(4).position(|w| w == [0, 16, 0x80, 1]).unwrap();
        srv_only[txt_at + 1] = 99;
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = responder.local_addr().unwrap().port();
        let fake = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (_, from) = responder.recv_from(&mut buf).await.unwrap();
            responder.send_to(&txt_only, from).await.unwrap();
            responder.send_to(&srv_only, from).await.unwrap();
        });
        let got = announced("127.0.0.1", port, PORT).await.unwrap();
        fake.await.unwrap();
        assert_eq!(got.unwrap(), DELTA);
    }

    #[tokio::test]
    async fn a_receiver_that_wants_more_is_told_from_one_that_is_away() {
        let (port, _, fake) = receiver(Some("ANNOUNCE")).await;
        let e = read_volume("127.0.0.1", port).await.unwrap_err();
        fake.await.unwrap();
        let refused = e
            .downcast_ref::<Refused>()
            .expect("a refusal, not a failure");
        assert_eq!(refused.method, "ANNOUNCE");
        assert_eq!(refused.answer, "RTSP/1.0 403 Forbidden");
        // Nothing listens on a port just freed: away, not refused.
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let e = read_volume("127.0.0.1", port).await.unwrap_err();
        assert!(e.downcast_ref::<Refused>().is_none(), "{e:#}");
    }

    #[test]
    fn ntp_time_counts_from_1900() {
        let seconds_since_1900 = ntp_now() >> 32;
        assert!(seconds_since_1900 > NTP_EPOCH + 1_700_000_000);
    }
}

//! ADB over the network (TCP 5555, once the TV's « network debugging » is
//! on in its developer options): what plays, read from the TV's media
//! sessions, whichever app plays it (Cast only knows what was cast).
//!
//! The protocol: 24-byte headers (`CNXN`, `AUTH`, `OPEN`, `OKAY`, `WRTE`,
//! `CLSE`) and their payload. The TV sends a token, signed with Moli's RSA
//! key; a key it does not know yet goes as a public key, and the TV asks on
//! screen whether to allow it (« always allow » keeps it). A shell command
//! is a stream opened, read until the TV closes it.

use std::time::Duration;

use anyhow::{Context as _, bail};
use num_bigint::BigUint;
use num_traits::{One as _, Zero as _};
use ring::rand::{SecureRandom as _, SystemRandom};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::cast::Playing;

pub const PORT: u16 = 5555;
const CNXN: u32 = u32::from_le_bytes(*b"CNXN");
const AUTH: u32 = u32::from_le_bytes(*b"AUTH");
const OPEN: u32 = u32::from_le_bytes(*b"OPEN");
const OKAY: u32 = u32::from_le_bytes(*b"OKAY");
const WRTE: u32 = u32::from_le_bytes(*b"WRTE");
const CLSE: u32 = u32::from_le_bytes(*b"CLSE");
const STLS: u32 = u32::from_le_bytes(*b"STLS");
const VERSION: u32 = 0x0100_0001;
const MAX_DATA: u32 = 256 * 1024;
const AUTH_TOKEN: u32 = 1;
const AUTH_SIGNATURE: u32 = 2;
const AUTH_PUBLIC_KEY: u32 = 3;
const BITS: usize = 2048;
/// The time a person has to answer the TV's question.
pub const ASKING: Duration = Duration::from_secs(120);
const ANSWER: Duration = Duration::from_secs(10);
/// `DigestInfo` of a SHA-1 digest: the token is signed as one (as `adb` does).
const SHA1_INFO: [u8; 15] = [
    0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14,
];

/// Moli's RSA key (e = 65537): what the TV allows once and knows after.
#[derive(Clone)]
pub struct Key {
    n: BigUint,
    d: BigUint,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("adb::Key(..)")
    }
}

impl Key {
    /// A new key (a few seconds: run it off the async threads).
    pub fn generate() -> anyhow::Result<Self> {
        Self::generate_bits(BITS)
    }

    fn generate_bits(bits: usize) -> anyhow::Result<Self> {
        let rng = SystemRandom::new();
        let e = BigUint::from(65_537u32);
        loop {
            let p = prime(bits / 2, &rng)?;
            let q = prime(bits / 2, &rng)?;
            if p == q {
                continue;
            }
            let n = &p * &q;
            if n.bits() != bits as u64 {
                continue;
            }
            let one = BigUint::one();
            let (p1, q1) = (&p - &one, &q - &one);
            let lcm = &p1 / gcd(&p1, &q1) * &q1;
            if let Some(d) = e.modinv(&lcm) {
                return Ok(Self { n, d });
            }
        }
    }

    /// As stored in the secret store: `n` and `d`, hexadecimal.
    #[must_use]
    pub fn to_secret(&self) -> String {
        format!("{:x}:{:x}", self.n, self.d)
    }

    pub fn from_secret(s: &str) -> anyhow::Result<Self> {
        let (n, d) = s.trim().split_once(':').context("adb key: n:d")?;
        let n = BigUint::parse_bytes(n.as_bytes(), 16).context("adb key: n")?;
        let d = BigUint::parse_bytes(d.as_bytes(), 16).context("adb key: d")?;
        if n.bits() < 1024 || d.is_zero() {
            bail!("adb key: not a key");
        }
        Ok(Self { n, d })
    }

    fn len(&self) -> usize {
        usize::try_from(self.n.bits().div_ceil(8)).unwrap_or(0)
    }

    /// PKCS #1 v1.5 over the token taken as a SHA-1 digest.
    fn sign(&self, token: &[u8]) -> Vec<u8> {
        let len = self.len();
        let mut em = vec![0xFF; len];
        em[0] = 0;
        em[1] = 1;
        let tail = SHA1_INFO.len() + token.len();
        em[len - tail - 1] = 0;
        em[len - tail..len - token.len()].copy_from_slice(&SHA1_INFO);
        em[len - token.len()..].copy_from_slice(token);
        let s = BigUint::from_bytes_be(&em).modpow(&self.d, &self.n);
        let s = s.to_bytes_be();
        let mut out = vec![0; len - s.len()];
        out.extend(s);
        out
    }

    /// Android's own public key format (what `adbkey.pub` holds), base64,
    /// then a name the TV shows.
    #[must_use]
    pub fn public(&self, name: &str) -> String {
        let words = self.len() / 4;
        let r32 = BigUint::one() << 32;
        let n0: BigUint = &self.n % &r32;
        let n0inv = n0.modinv(&r32).map_or_else(BigUint::zero, |inv| &r32 - inv);
        let rr = (BigUint::one() << (words * 64)) % &self.n;
        let mut out = Vec::with_capacity(12 + words * 8);
        out.extend(u32::try_from(words).unwrap_or(0).to_le_bytes());
        out.extend(le_u32(&n0inv, 1));
        out.extend(le_u32(&self.n, words));
        out.extend(le_u32(&rr, words));
        out.extend(65_537u32.to_le_bytes());
        format!("{} {name}\0", base64(&out))
    }
}

/// `v` as `words` little-endian 32-bit words.
fn le_u32(v: &BigUint, words: usize) -> Vec<u8> {
    let mut bytes = v.to_bytes_le();
    bytes.resize(words * 4, 0);
    bytes
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn gcd(a: &BigUint, b: &BigUint) -> BigUint {
    let (mut a, mut b) = (a.clone(), b.clone());
    while !b.is_zero() {
        let r = &a % &b;
        a = b;
        b = r;
    }
    a
}

fn random(bits: usize, rng: &SystemRandom) -> anyhow::Result<BigUint> {
    let mut bytes = vec![0; bits.div_ceil(8)];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(BigUint::from_bytes_be(&bytes))
}

/// A probable prime of exactly `bits` bits, its two top bits set (so that
/// two of them make a key of twice the bits).
fn prime(bits: usize, rng: &SystemRandom) -> anyhow::Result<BigUint> {
    const SMALL: [u32; 24] = [
        3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    ];
    let top = (BigUint::one() << (bits - 1)) | (BigUint::one() << (bits - 2));
    loop {
        let mut c = random(bits, rng)? | &top | BigUint::one();
        c &= (BigUint::one() << bits) - BigUint::one();
        if SMALL.iter().any(|&s| (&c % s).is_zero()) {
            continue;
        }
        // e = 65537 must be invertible: p - 1 not a multiple of it.
        if (&c % 65_537u32).is_one() {
            continue;
        }
        if probably_prime(&c, 40, rng)? {
            return Ok(c);
        }
    }
}

/// Miller–Rabin, `rounds` random bases.
fn probably_prime(n: &BigUint, rounds: usize, rng: &SystemRandom) -> anyhow::Result<bool> {
    let one = BigUint::one();
    let two = &one + &one;
    let minus_one = n - &one;
    let twos = minus_one.trailing_zeros().unwrap_or(0);
    let odd = &minus_one >> twos;
    let bits = usize::try_from(n.bits()).unwrap_or(0);
    'round: for _ in 0..rounds {
        let base = random(bits, rng)? % (n - 3u32) + &two;
        let mut x = base.modpow(&odd, n);
        if x == one || x == minus_one {
            continue;
        }
        for _ in 1..twos {
            x = x.modpow(&two, n);
            if x == minus_one {
                continue 'round;
            }
        }
        return Ok(false);
    }
    Ok(true)
}

struct Message {
    command: u32,
    arg0: u32,
    arg1: u32,
    data: Vec<u8>,
}

async fn send<W: AsyncWrite + Unpin>(
    w: &mut W,
    command: u32,
    arg0: u32,
    arg1: u32,
    data: &[u8],
) -> anyhow::Result<()> {
    let sum = data.iter().map(|&b| u32::from(b)).sum::<u32>();
    let mut frame = Vec::with_capacity(24 + data.len());
    for word in [
        command,
        arg0,
        arg1,
        u32::try_from(data.len())?,
        sum,
        command ^ 0xFFFF_FFFF,
    ] {
        frame.extend(word.to_le_bytes());
    }
    frame.extend(data);
    w.write_all(&frame).await?;
    w.flush().await?;
    Ok(())
}

async fn read<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Message> {
    let mut header = [0u8; 24];
    r.read_exact(&mut header).await?;
    let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap_or([0; 4]));
    let (command, len) = (word(0), word(3));
    if word(5) != command ^ 0xFFFF_FFFF {
        bail!("adb: bad header");
    }
    if len > MAX_DATA {
        bail!("adb: message of {len} bytes");
    }
    let mut data = vec![0; usize::try_from(len)?];
    r.read_exact(&mut data).await?;
    Ok(Message {
        command,
        arg0: word(1),
        arg1: word(2),
        data,
    })
}

/// How the TV took Moli's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// It knew it: connected.
    Known,
    /// It did not: it asks on screen (`asking` was called), and someone
    /// allowed it.
    Allowed,
}

/// A connection to the TV's ADB daemon.
pub struct Adb<S> {
    stream: S,
    next_id: u32,
}

impl<S> std::fmt::Debug for Adb<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adb")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl Adb<TcpStream> {
    /// Connects and authenticates; `asking` is called when the TV shows its
    /// question (then the answer is awaited for [`ASKING`]).
    pub async fn connect(
        host: &str,
        key: &Key,
        name: &str,
        asking: impl FnOnce(),
    ) -> anyhow::Result<(Self, Auth)> {
        let tcp = tokio::time::timeout(ANSWER, TcpStream::connect((host, PORT)))
            .await
            .context("adb: no answer")??;
        tcp.set_nodelay(true)?;
        Self::handshake(tcp, key, name, asking).await
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Adb<S> {
    pub async fn handshake(
        mut stream: S,
        key: &Key,
        name: &str,
        asking: impl FnOnce(),
    ) -> anyhow::Result<(Self, Auth)> {
        send(&mut stream, CNXN, VERSION, MAX_DATA, b"host::\0").await?;
        let mut signed = false;
        let mut asking = Some(asking);
        let mut wait = ANSWER;
        loop {
            let m = tokio::time::timeout(wait, read(&mut stream))
                .await
                .context("adb: the TV did not answer (or no one allowed Moli on it)")??;
            match (m.command, m.arg0) {
                (CNXN, _) => {
                    let auth = if asking.is_none() {
                        Auth::Allowed
                    } else {
                        Auth::Known
                    };
                    return Ok((Self { stream, next_id: 1 }, auth));
                }
                (AUTH, AUTH_TOKEN) if !signed => {
                    signed = true;
                    send(&mut stream, AUTH, AUTH_SIGNATURE, 0, &key.sign(&m.data)).await?;
                }
                (AUTH, AUTH_TOKEN) => {
                    let Some(asking) = asking.take() else {
                        bail!("adb: the TV refused Moli's key");
                    };
                    send(
                        &mut stream,
                        AUTH,
                        AUTH_PUBLIC_KEY,
                        0,
                        key.public(name).as_bytes(),
                    )
                    .await?;
                    asking();
                    wait = ASKING;
                }
                (STLS, _) => bail!("adb: this TV wants TLS (wireless debugging), not supported"),
                (other, _) => bail!("adb: unexpected message {other:#x}"),
            }
        }
    }

    /// A shell command's output (stdout and stderr together).
    pub async fn shell(&mut self, command: &str) -> anyhow::Result<String> {
        let local = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut service = format!("shell:{command}").into_bytes();
        service.push(0);
        send(&mut self.stream, OPEN, local, 0, &service).await?;
        let mut out = Vec::new();
        let mut remote = 0;
        loop {
            let m = tokio::time::timeout(ANSWER, read(&mut self.stream))
                .await
                .context("adb: the TV went quiet")??;
            if m.arg1 != local {
                // An old stream's message (a command given up on).
                continue;
            }
            match m.command {
                OKAY => remote = m.arg0,
                WRTE => {
                    remote = m.arg0;
                    out.extend(m.data);
                    send(&mut self.stream, OKAY, local, remote, &[]).await?;
                }
                CLSE => {
                    if remote != 0 {
                        send(&mut self.stream, CLSE, local, remote, &[]).await?;
                    }
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                other => bail!("adb: unexpected message {other:#x}"),
            }
        }
    }
}

/// The command that says what plays: the media sessions, then the TV's
/// clock (positions are told at a moment of that clock).
pub const MEDIA: &str = "dumpsys media_session; echo; cat /proc/uptime";

/// What plays, from [`MEDIA`]'s output: the session playing (or paused,
/// or the last one with something to show), its position brought to now.
/// `None`: no session has anything.
#[must_use]
pub fn playing(out: &str) -> Option<(String, Playing)> {
    let uptime_ms = out
        .lines()
        .rev()
        .find_map(|l| l.split_whitespace().next()?.parse::<f64>().ok())
        .map(|s| s * 1000.0);
    let stack = out.split("Sessions Stack").nth(1)?;
    let stack = stack
        .split(
            "
Audio playback",
        )
        .next()
        .unwrap_or(stack);
    // Each session: its package, how much it is playing, what it shows.
    let mut sessions: Vec<(String, u8, Playing)> = Vec::new();
    for line in stack.lines().map(str::trim) {
        if let Some(package) = line.strip_prefix("package=") {
            sessions.push((package.to_owned(), 0, Playing::default()));
        } else if let Some(state) = line.strip_prefix("state=PlaybackState {") {
            let Some((_, rank, p)) = sessions.last_mut() else {
                continue;
            };
            (*rank, p.state, p.position) = playback(state, uptime_ms);
        } else if let Some(meta) = line.strip_prefix("metadata: ") {
            let Some((_, _, p)) = sessions.last_mut() else {
                continue;
            };
            // `description=title, subtitle, description`: a title may hold
            // commas, the two others are taken from the end.
            let Some((_, desc)) = meta.split_once("description=") else {
                continue;
            };
            let mut parts = desc.rsplitn(3, ", ");
            let _description = parts.next();
            let subtitle = parts.next();
            let clean = |s: Option<&str>| {
                s.map(str::trim)
                    .filter(|s| !s.is_empty() && *s != "null")
                    .map(str::to_owned)
            };
            p.subtitle = clean(subtitle);
            p.title = clean(parts.next());
        }
    }
    // The most playing first; among equals, the first in the stack (the
    // most recent).
    let mut best: Option<(String, u8, Playing)> = None;
    for s in sessions {
        if (s.1 > 0 || s.2.title.is_some()) && best.as_ref().is_none_or(|b| s.1 > b.1) {
            best = Some(s);
        }
    }
    best.map(|(package, _, p)| (package, p))
}

/// `PlaybackState {state=PLAYING(3), position=1576, speed=1.0, updated=…}`:
/// how much it plays (rank), its word, its position (s) at `now_ms`.
fn playback(state: &str, now_ms: Option<f64>) -> (u8, Option<String>, Option<f64>) {
    let field = |name: &str| {
        state
            .split(", ")
            .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
    };
    let number = |name: &str| field(name).and_then(|v| v.parse::<f64>().ok());
    // Android 14 writes `PLAYING(3)`, Android 11 the bare code `3`.
    let raw = field("state").unwrap_or("");
    let code = raw
        .split_once('(')
        .map_or(raw, |(_, code)| code.trim_end_matches(')'));
    let (rank, word) = match code.parse::<u8>().unwrap_or(0) {
        // PLAYING, FAST_FORWARDING, REWINDING
        3..=5 => (3, "playing"),
        // BUFFERING, CONNECTING
        6 | 8 => (2, "buffering"),
        2 => (2, "paused"),
        _ => (0, "idle"),
    };
    let position = number("position").filter(|p| *p >= 0.0).map(|p| {
        // A stopped or paused session may still say `speed=1.0`: only a
        // playing one moves on.
        let speed = if rank == 3 {
            number("speed").unwrap_or(0.0)
        } else {
            0.0
        };
        let since = match (now_ms, number("updated")) {
            (Some(now), Some(at)) if now > at => now - at,
            _ => 0.0,
        };
        (p + since * speed) / 1000.0
    });
    (rank, Some(word.to_owned()), position)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key::generate_bits(1024).unwrap()
    }

    #[test]
    fn a_key_signs_what_its_public_half_checks() {
        let key = key();
        let token = [7u8; 20];
        let sig = key.sign(&token);
        assert_eq!(sig.len(), 128);
        let em = BigUint::from_bytes_be(&sig)
            .modpow(&BigUint::from(65_537u32), &key.n)
            .to_bytes_be();
        // The leading 0 is dropped by the number: 01 FF … 00 DigestInfo token.
        assert_eq!(em[0], 1);
        assert!(em.ends_with(&token));
        assert!(em[..em.len() - 20].ends_with(&SHA1_INFO));
        let again = Key::from_secret(&key.to_secret()).unwrap();
        assert_eq!(again.sign(&token), sig);
    }

    #[test]
    fn the_public_key_is_in_androids_format() {
        let key = key();
        let public = key.public("moli-os");
        assert!(public.ends_with(" moli-os\0"));
        let words = 32;
        // 4 + 4 + n + rr + 4 bytes, base64.
        assert_eq!(
            public.split(' ').next().unwrap().len(),
            (12 + words * 8usize).div_ceil(3) * 4
        );
        let r32 = BigUint::one() << 32;
        let n0: BigUint = &key.n % &r32;
        let n0inv = &r32 - n0.modinv(&r32).unwrap();
        assert_eq!((&n0inv * &key.n + 1u32) % &r32, BigUint::zero());
    }

    #[tokio::test]
    async fn connects_with_a_new_key_then_runs_a_command() {
        let key = key();
        let (client, mut tv) = tokio::io::duplex(64 * 1024);
        let device = tokio::spawn(async move {
            let hello = read(&mut tv).await.unwrap();
            assert_eq!((hello.command, hello.arg0), (CNXN, VERSION));
            send(&mut tv, AUTH, AUTH_TOKEN, 0, &[1; 20]).await.unwrap();
            let sig = read(&mut tv).await.unwrap();
            assert_eq!(
                (sig.command, sig.arg0, sig.data.len()),
                (AUTH, AUTH_SIGNATURE, 128)
            );
            // Unknown key: the TV asks for the public one.
            send(&mut tv, AUTH, AUTH_TOKEN, 0, &[2; 20]).await.unwrap();
            let public = read(&mut tv).await.unwrap();
            assert_eq!(public.arg0, AUTH_PUBLIC_KEY);
            assert!(public.data.ends_with(b" moli-os\0"));
            send(&mut tv, CNXN, VERSION, MAX_DATA, b"device::\0")
                .await
                .unwrap();
            let open = read(&mut tv).await.unwrap();
            assert_eq!(open.command, OPEN);
            assert_eq!(open.data, b"shell:echo hi\0");
            let local = open.arg0;
            // A message for another stream is skipped.
            send(&mut tv, WRTE, 9, local + 5, b"noise").await.unwrap();
            send(&mut tv, OKAY, 42, local, &[]).await.unwrap();
            send(&mut tv, WRTE, 42, local, b"h").await.unwrap();
            assert_eq!(read(&mut tv).await.unwrap().command, OKAY);
            send(&mut tv, WRTE, 42, local, b"i\n").await.unwrap();
            assert_eq!(read(&mut tv).await.unwrap().command, OKAY);
            send(&mut tv, CLSE, 42, local, &[]).await.unwrap();
            assert_eq!(read(&mut tv).await.unwrap().command, CLSE);
        });
        let mut asked = false;
        let (mut adb, auth) = Adb::handshake(client, &key, "moli-os", || asked = true)
            .await
            .unwrap();
        assert!(asked);
        assert_eq!(auth, Auth::Allowed);
        assert_eq!(adb.shell("echo hi").await.unwrap(), "hi\n");
        device.await.unwrap();
    }

    /// As a Philips Google TV (Android 14) printed it, YouTube playing.
    const DUMP: &str = "MEDIA SESSION SERVICE (dumpsys media_session)
  Sessions Stack - have 3 sessions:
    BluetoothMediaBrowserService com.android.bluetooth/BluetoothMediaBrowserService (userId=0)
      package=com.android.bluetooth
      active=false
      state=PlaybackState {state=ERROR(7), position=0, buffered position=0, speed=0.0, updated=24635, actions=0, custom actions=[], active item id=-1, error=Audio Bluetooth déconnecté}
      metadata: null
    starboard com.google.android.youtube.tv/starboard (userId=0)
      package=com.google.android.youtube.tv
      active=true
      state=PlaybackState {state=PLAYING(3), position=1576, buffered position=0, speed=1.0, updated=169344629, actions=379, custom actions=[], active item id=-1, error=null}
      metadata: size=5, description=Big Buck Bunny, 60fps 4K - Official Blender Foundation Short Film, Blender, null
      queueTitle=null, size=0
Audio playback (lastly played comes first)
  uid=10065 packages=com.google.android.youtube.tv

169354.629 90677.49
";

    #[test]
    fn reads_what_plays_and_brings_its_position_to_now() {
        let (app, p) = playing(DUMP).unwrap();
        assert_eq!(app, "com.google.android.youtube.tv");
        assert_eq!(p.state.as_deref(), Some("playing"));
        assert_eq!(
            p.title.as_deref(),
            Some("Big Buck Bunny, 60fps 4K - Official Blender Foundation Short Film")
        );
        assert_eq!(p.subtitle.as_deref(), Some("Blender"));
        // 1.576 s when told, told 10 s before the clock line.
        assert!((p.position.unwrap() - 11.576).abs() < 0.01);
    }

    #[test]
    fn nothing_to_show_is_none() {
        let idle = DUMP.replace("PLAYING(3)", "NONE(0)").replace(
            "Big Buck Bunny, 60fps 4K - Official Blender Foundation Short Film, Blender, null",
            "null, null, null",
        );
        assert_eq!(playing(&idle), None);
        assert_eq!(playing("no sessions here"), None);
    }

    #[test]
    fn a_paused_session_is_shown() {
        let paused = DUMP
            .replace("PLAYING(3)", "PAUSED(2)")
            .replace("speed=1.0", "speed=0.0");
        let (_, p) = playing(&paused).unwrap();
        assert_eq!(p.state.as_deref(), Some("paused"));
        assert!((p.position.unwrap() - 1.576).abs() < 0.01);
    }

    #[test]
    fn android_11_writes_the_bare_code() {
        // Seen on a Xiaomi Mi TV Stick 4K (Android 11), by Hogun974 in #112.
        let (_, p) = playing(&DUMP.replace("PLAYING(3)", "3")).unwrap();
        assert_eq!(p.state.as_deref(), Some("playing"));
        assert!((p.position.unwrap() - 11.576).abs() < 0.01);
        let (_, p) = playing(&DUMP.replace("PLAYING(3)", "2")).unwrap();
        assert_eq!(p.state.as_deref(), Some("paused"));
    }

    #[test]
    fn a_session_not_playing_keeps_its_position() {
        // Paused, yet still telling `speed=1.0`: no drift.
        let (_, p) = playing(&DUMP.replace("PLAYING(3)", "PAUSED(2)")).unwrap();
        assert!((p.position.unwrap() - 1.576).abs() < 0.01);
    }
}

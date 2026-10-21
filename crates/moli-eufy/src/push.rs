//! Eufy's events (motion, person, doorbell, mode changes) come as push
//! notifications, as they do to the app on an Android phone: Moli registers
//! itself as the Eufy app with Firebase and Google (checkin, GCM), gives the
//! token to Eufy, and keeps one TLS connection to Google's MCS
//! (`mtalk.google.com:5228`) where they arrive.
//!
//! Ported from eufy-security-client (`src/push`, MIT, see NOTICE). The
//! identity (Firebase installation, Android id, tokens) is kept in the
//! vault: one registration, not one per restart.

use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::Full;
use moli_net::proto::{self, Writer};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::crypto;

// The Eufy Security Android app, as Firebase knows it.
const APP_PACKAGE: &str = "com.oceanwing.battery.cam";
const APP_ID: &str = "1:348804314802:android:440a6773b3620da7";
const SENDER_ID: &str = "348804314802";
const APP_CERT_SHA1: &str = "F051262F9F99B638F3C76DE349830638555B4A0A";
const PROJECT_ID: &str = "batterycam-3250a";
// Not a secret: shipped in the app, the key of Eufy's Firebase project.
const API_KEY: &str = concat!("AIzaSyCSz1uxGrHXsEktm7O3", "_wv-uLGpC9BvXR8");

const MCS_HOST: &str = "mtalk.google.com";
const MCS_PORT: u16 = 5228;
const MCS_VERSION: u8 = 41;
const HEARTBEAT: Duration = Duration::from_secs(5 * 60);
/// Nothing at all from Google this long: the connection is dead.
const SILENCE: Duration = Duration::from_secs(12 * 60);
const LIMIT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 1 << 20;
/// One MCS message; Eufy's are a few kilobytes.
const MAX_MESSAGE: usize = 256 << 10;
/// Persistent ids kept to tell Google what was already received.
const MAX_RECEIVED: usize = 100;

// MCS message tags.
const HEARTBEAT_PING: u8 = 0;
const HEARTBEAT_ACK: u8 = 1;
const LOGIN_REQUEST: u8 = 2;
const LOGIN_RESPONSE: u8 = 3;
const CLOSE: u8 = 4;
const DATA_MESSAGE: u8 = 8;

/// Moli's identity with Google's push service. Never printed.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Credentials {
    pub fid: String,
    pub refresh_token: String,
    pub android_id: u64,
    pub security_token: u64,
    /// What Eufy is given: where to push.
    pub token: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(<redacted>)")
    }
}

/// A new installation of "the app": Firebase, then Android checkin, then
/// the GCM token.
pub async fn register() -> anyhow::Result<Credentials> {
    let fid = new_fid()?;
    let installed = post_json(
        "firebaseinstallations.googleapis.com",
        &format!("/v1/projects/{PROJECT_ID}/installations"),
        &json!({ "fid": fid, "appId": APP_ID, "authVersion": "FIS_v2", "sdkVersion": "a:16.3.1" }),
        None,
    )
    .await
    .context("Firebase installation")?;
    let auth_token = installed["authToken"]["token"]
        .as_str()
        .context("Firebase: no auth token")?;
    let refresh_token = installed["refreshToken"]
        .as_str()
        .context("Firebase: no refresh token")?
        .to_owned();
    let fid = installed["fid"].as_str().unwrap_or(&fid).to_owned();
    let (android_id, security_token) = checkin().await.context("Android checkin")?;
    let token = register_gcm(&fid, auth_token, android_id, security_token)
        .await
        .context("GCM registration")?;
    Ok(Credentials {
        fid,
        refresh_token,
        android_id,
        security_token,
        token,
    })
}

/// A Firebase installation id: 17 random bytes, the first four bits
/// `0111`, URL-safe base64, 22 characters.
fn new_fid() -> anyhow::Result<String> {
    let mut bytes = crypto::random::<17>()?;
    bytes[0] = 0b0111_0000 | (bytes[0] & 0x0F);
    let fid: String = crypto::base64(&bytes)
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .take(22)
        .collect();
    Ok(fid)
}

async fn post_json(
    host: &str,
    path: &str,
    body: &Json,
    auth: Option<&str>,
) -> anyhow::Result<Json> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("Content-Type", "application/json")
        .header("X-Android-Package", APP_PACKAGE)
        .header("X-Android-Cert", APP_CERT_SHA1)
        .header("x-goog-api-key", API_KEY);
    if let Some(auth) = auth {
        builder = builder.header("Authorization", auth);
    }
    let request = builder.body(Full::new(serde_json::to_vec(body)?.into()))?;
    let (status, bytes) = moli_net::web(host, 443, true, request, LIMIT, MAX_BODY).await?;
    if !status.is_success() {
        bail!("{host} answered {status}");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("{host}: not JSON"))
}

/// The checkin request of an old Android tablet, as the library sends it.
fn checkin_request() -> Vec<u8> {
    let build = Writer::new()
        .str(1, "google/razor/flo:5.0.1/LRX22C/1602158:user/release-keys")
        .str(2, "flo")
        .str(3, "google")
        .str(4, "FLO-04.04")
        .str(6, "android-google");
    let checkin = Writer::new().msg(1, build).uint(2, 0);
    Writer::new()
        .str(1, "109269993813709")
        .uint(2, 0)
        .msg(4, checkin)
        .str(6, "en")
        .uint(7, 1_234_567_890)
        .str(9, "A1B2C3D4E5F6")
        .str(10, "109269993813709")
        .str(12, "GMT")
        .uint(14, 3)
        .str(15, "71Q6Rn2DDZl1zPDVaaeEHItd+Yg=")
        .str(17, "ABCDEF01")
        .str(19, "wifi")
        .uint(20, 0)
        .uint(22, 0)
        .finish()
}

/// The Android id and security token Google gives (`fixed64` fields 7, 8).
fn read_checkin(answer: &[u8]) -> anyhow::Result<(u64, u64)> {
    let fields = proto::parse(answer).context("checkin: not protobuf")?;
    let fixed = |n| -> Option<u64> { Some(u64::from_le_bytes(fields.bytes(n)?.try_into().ok()?)) };
    match (fixed(7), fixed(8)) {
        (Some(id), Some(token)) if id != 0 && token != 0 => Ok((id, token)),
        _ => bail!("checkin: no Android id"),
    }
}

async fn checkin() -> anyhow::Result<(u64, u64)> {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/checkin")
        .header("Content-Type", "application/x-protobuf")
        .body(Full::new(checkin_request().into()))?;
    let (status, body) = moli_net::web(
        "android.clients.google.com",
        443,
        true,
        request,
        LIMIT,
        MAX_BODY,
    )
    .await?;
    if !status.is_success() {
        bail!("checkin answered {status}");
    }
    read_checkin(&body)
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&encode(key));
        out.push('=');
        out.push_str(&encode(value));
    }
    out
}

fn encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

async fn register_gcm(
    fid: &str,
    auth_token: &str,
    android_id: u64,
    security_token: u64,
) -> anyhow::Result<String> {
    let device = android_id.to_string();
    let body = form(&[
        ("X-subtype", SENDER_ID),
        ("sender", SENDER_ID),
        ("X-app_ver", "741"),
        ("X-osv", "25"),
        ("X-cliv", "fiid-20.2.0"),
        ("X-gmsv", "201216023"),
        ("X-appid", fid),
        ("X-scope", "*"),
        ("X-Goog-Firebase-Installations-Auth", auth_token),
        ("X-gmp_app_id", APP_ID),
        ("X-app_ver_name", "v2.2.2_741"),
        ("app", APP_PACKAGE),
        ("device", &device),
        ("app_ver", "741"),
        ("gcm_ver", "201216023"),
        ("plat", "0"),
        ("cert", APP_CERT_SHA1),
        ("target_ver", "28"),
    ]);
    // Google sometimes answers « PHONE_REGISTRATION_ERROR » at first.
    for attempt in 1..=3u64 {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/c2dm/register3")
            .header(
                "Authorization",
                format!("AidLogin {android_id}:{security_token}"),
            )
            .header("app", APP_PACKAGE)
            .header("gcm_ver", "201216023")
            .header("User-Agent", "Android-GCM/1.5")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Full::new(body.clone().into()))?;
        let (status, answer) = moli_net::web(
            "android.clients.google.com",
            443,
            true,
            request,
            LIMIT,
            MAX_BODY,
        )
        .await?;
        let answer = String::from_utf8_lossy(&answer);
        if !status.is_success() {
            bail!("register3 answered {status}");
        }
        match answer.trim().split_once('=') {
            Some(("token", token)) if !token.is_empty() => return Ok(token.to_owned()),
            Some(("Error", error)) => {
                tracing::debug!(error, attempt, "eufy push: GCM registration refused");
            }
            _ => bail!("register3: unexpected answer"),
        }
        tokio::time::sleep(Duration::from_secs(5 * attempt)).await;
    }
    bail!("GCM registration refused")
}

/// One push from Eufy: its `app_data`, `payload` decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Push {
    pub device_type: i64,
    pub device_sn: String,
    pub station_sn: String,
    pub payload: Json,
}

/// What the connection tells its owner.
#[derive(Debug)]
pub enum Up {
    Connected,
    Push(Push),
    /// Google no longer knows this identity: register again.
    Refused,
}

/// Listens until the owner goes away, reconnecting with a growing delay
/// (5 s to 5 min). A refused login is told once, then the task ends.
pub async fn run(creds: Credentials, up: mpsc::Sender<Up>, instance: String) {
    let mut received = Vec::new();
    let mut delay = Duration::from_secs(5);
    loop {
        let started = tokio::time::Instant::now();
        match listen(&creds, &mut received, &up).await {
            Ok(()) => return,
            Err(e) if e.to_string().contains("login refused") => {
                tracing::warn!(instance, error = %e, "eufy push");
                let _ = up.send(Up::Refused).await;
                return;
            }
            Err(e) => tracing::warn!(
                instance,
                error = format!("{e:#}"),
                "eufy push: reconnecting"
            ),
        }
        if up.is_closed() {
            return;
        }
        if started.elapsed() > Duration::from_secs(10 * 60) {
            delay = Duration::from_secs(5);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5 * 60));
    }
}

fn login_request(creds: &Credentials, received: &[String]) -> Vec<u8> {
    let id = creds.android_id.to_string();
    let mut login = Writer::new()
        .str(1, "chrome-63.0.3234.0")
        .str(2, "mcs.android.com")
        .str(3, &id)
        .str(4, &id)
        .str(5, &creds.security_token.to_string())
        .str(6, &format!("android-{:x}", creds.android_id))
        .msg(8, Writer::new().str(1, "new_vc").str(2, "1"));
    for persistent in received {
        login = login.str(10, persistent);
    }
    let login = login
        .bool(12, false)
        .bool(14, true)
        .uint(16, 2)
        .uint(17, 1)
        .finish();
    let mut out = vec![MCS_VERSION, LOGIN_REQUEST];
    out.extend(frame_body(&login));
    out
}

fn frame_body(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 5);
    proto::varint(message.len() as u64, &mut out);
    out.extend_from_slice(message);
    out
}

fn tagged(tag: u8, message: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(frame_body(message));
    out
}

/// Cuts MCS messages (tag, varint size, protobuf) out of the stream; the
/// first one is preceded by the protocol version.
#[derive(Debug, Default)]
struct Framer {
    buf: Vec<u8>,
    versioned: bool,
}

impl Framer {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn next(&mut self) -> anyhow::Result<Option<(u8, Vec<u8>)>> {
        let mut pos = 0;
        if !self.versioned {
            let Some(&version) = self.buf.first() else {
                return Ok(None);
            };
            if version < MCS_VERSION && version != 38 {
                bail!("MCS version {version}");
            }
            pos = 1;
        }
        let Some(&tag) = self.buf.get(pos) else {
            return Ok(None);
        };
        pos += 1;
        let start = pos;
        let Some(size) = proto::read_varint(&self.buf, &mut pos) else {
            if self.buf.len() - start >= 10 {
                bail!("MCS size unreadable");
            }
            return Ok(None);
        };
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        if size > MAX_MESSAGE {
            bail!("MCS message too large ({size} bytes)");
        }
        if self.buf.len() < pos + size {
            return Ok(None);
        }
        let body = self.buf[pos..pos + size].to_vec();
        self.buf.drain(..pos + size);
        self.versioned = true;
        Ok(Some((tag, body)))
    }
}

/// A data message's `app_data` as Eufy fills it.
fn read_push(body: &[u8]) -> Option<(Option<String>, Push)> {
    let fields = proto::parse(body)?;
    let persistent = fields.str(9).map(str::to_owned);
    let mut data = serde_json::Map::new();
    for pair in fields.all_msgs(7) {
        let (Some(key), Some(value)) = (pair.str(1), pair.str(2)) else {
            continue;
        };
        data.insert(key.to_owned(), Json::String(value.to_owned()));
    }
    let text = |key: &str| {
        data.get(key)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let payload = data
        .get("payload")
        .and_then(Json::as_str)
        .and_then(crypto::unbase64)
        .map(|mut bytes| {
            if let Some(end) = bytes.iter().position(|&b| b == 0) {
                bytes.truncate(end);
            }
            bytes
        })
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Json::Null);
    Some((
        persistent,
        Push {
            device_type: text("type").trim().parse().unwrap_or(-1),
            device_sn: text("device_sn"),
            station_sn: text("station_sn"),
            payload,
        },
    ))
}

/// Connected to Google until the connection drops (an error, or `Ok` if
/// the owner went away). `received` carries what was already delivered
/// across reconnections.
pub async fn listen(
    creds: &Credentials,
    received: &mut Vec<String>,
    up: &mpsc::Sender<Up>,
) -> anyhow::Result<()> {
    let tcp = tokio::time::timeout(LIMIT, tokio::net::TcpStream::connect((MCS_HOST, MCS_PORT)))
        .await
        .context("MCS: connect timeout")??;
    let name = rustls::pki_types::ServerName::try_from(MCS_HOST)?;
    let mut tls = tokio::time::timeout(
        LIMIT,
        tokio_rustls::TlsConnector::from(moli_net::public_tls()).connect(name, tcp),
    )
    .await
    .context("MCS: TLS timeout")??;
    tls.write_all(&login_request(creds, received)).await?;
    let mut framer = Framer::default();
    let mut buf = vec![0u8; 16 << 10];
    let mut stream_id: u64 = 0;
    let mut reported: u64 = 0;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.tick().await;
    let mut heard = tokio::time::Instant::now();
    loop {
        tokio::select! {
            read = tls.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    bail!("MCS: closed by Google");
                }
                heard = tokio::time::Instant::now();
                framer.push(&buf[..n]);
                while let Some((tag, body)) = framer.next()? {
                    stream_id += 1;
                    match tag {
                        LOGIN_RESPONSE => {
                            let fields = proto::parse(&body).context("MCS: login answer")?;
                            if let Some(error) = fields.bytes(3).and_then(proto::parse) {
                                bail!("MCS: login refused ({})", error.uint(1).unwrap_or(0));
                            }
                            received.clear();
                            if up.send(Up::Connected).await.is_err() {
                                return Ok(());
                            }
                        }
                        HEARTBEAT_PING => {
                            let ack = Writer::new().uint(2, stream_id).finish();
                            reported = stream_id;
                            tls.write_all(&tagged(HEARTBEAT_ACK, &ack)).await?;
                        }
                        DATA_MESSAGE => {
                            if let Some((persistent, push)) = read_push(&body) {
                                if let Some(id) = persistent {
                                    received.push(id);
                                    if received.len() > MAX_RECEIVED {
                                        received.remove(0);
                                    }
                                }
                                if up.send(Up::Push(push)).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        CLOSE => bail!("MCS: closed by Google"),
                        _ => {}
                    }
                }
            }
            _ = heartbeat.tick() => {
                if heard.elapsed() > SILENCE {
                    bail!("MCS: silent");
                }
                let mut ping = Writer::new();
                if reported != stream_id {
                    ping = ping.uint(2, stream_id);
                    reported = stream_id;
                }
                tls.write_all(&tagged(HEARTBEAT_PING, &ping.finish())).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fid_looks_like_firebase_s() {
        for _ in 0..50 {
            let fid = new_fid().unwrap();
            assert_eq!(fid.len(), 22);
            assert!(
                matches!(fid.as_bytes()[0], b'c' | b'd' | b'e' | b'f'),
                "{fid}"
            );
            assert!(
                fid.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            );
        }
    }

    #[test]
    fn the_checkin_answer_gives_the_android_id() {
        let mut answer = Writer::new().bool(1, true).finish();
        answer.push((7 << 3) | 1);
        answer.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        answer.push((8 << 3) | 1);
        answer.extend_from_slice(&42u64.to_le_bytes());
        assert_eq!(read_checkin(&answer).unwrap(), (0x1122_3344_5566_7788, 42));
        assert!(read_checkin(&Writer::new().bool(1, true).finish()).is_err());
        assert!(proto::parse(&checkin_request()).is_some());
    }

    #[test]
    fn messages_are_cut_out_of_the_stream() {
        let mut framer = Framer::default();
        let mut stream = vec![MCS_VERSION];
        stream.extend(tagged(LOGIN_RESPONSE, &Writer::new().str(1, "x").finish()));
        stream.extend(tagged(HEARTBEAT_PING, &[]));
        // In two pieces: the second message's size arrives later.
        let cut = stream.len() - 1;
        framer.push(&stream[..cut]);
        assert_eq!(framer.next().unwrap().unwrap().0, LOGIN_RESPONSE);
        assert!(framer.next().unwrap().is_none());
        framer.push(&stream[cut..]);
        assert_eq!(framer.next().unwrap(), Some((HEARTBEAT_PING, Vec::new())));
        assert!(framer.next().unwrap().is_none());

        let mut old = Framer::default();
        old.push(&[7, 3, 0]);
        assert!(old.next().is_err());
    }

    #[test]
    fn an_eufy_push_is_read_from_its_app_data() {
        let inner = br#"{"a":3101,"pic_url":"https://example.org/p.jpg"}"#;
        let mut encoded = inner.to_vec();
        encoded.push(0);
        let pair = |k: &str, v: &str| Writer::new().str(1, k).str(2, v);
        let body = Writer::new()
            .str(3, "348804314802")
            .str(5, APP_PACKAGE)
            .msg(7, pair("type", "31"))
            .msg(7, pair("device_sn", "T8410P0000000000"))
            .msg(7, pair("station_sn", "T8410P0000000000"))
            .msg(7, pair("payload", &crypto::base64(&encoded)))
            .str(9, "0:123%abc")
            .finish();
        let (persistent, push) = read_push(&body).unwrap();
        assert_eq!(persistent.as_deref(), Some("0:123%abc"));
        assert_eq!(push.device_type, 31);
        assert_eq!(push.device_sn, "T8410P0000000000");
        assert_eq!(push.payload["a"], 3101);
    }

    #[test]
    fn the_login_names_the_android_device() {
        let creds = Credentials {
            android_id: 255,
            security_token: 7,
            ..Credentials::default()
        };
        let login = login_request(&creds, &["p1".into()]);
        assert_eq!(&login[..2], &[MCS_VERSION, LOGIN_REQUEST]);
        let mut pos = 2;
        let size = usize::try_from(proto::read_varint(&login, &mut pos).unwrap()).unwrap();
        let fields = proto::parse(&login[pos..pos + size]).unwrap();
        assert_eq!(fields.str(3), Some("255"));
        assert_eq!(fields.str(5), Some("7"));
        assert_eq!(fields.str(6), Some("android-ff"));
        assert_eq!(fields.str(10), Some("p1"));
        assert_eq!(fields.uint(16), Some(2));
    }

    #[test]
    fn forms_are_percent_encoded() {
        assert_eq!(
            form(&[("X-scope", "*"), ("a b", "c=d")]),
            "X-scope=%2A&a%20b=c%3Dd"
        );
    }
}

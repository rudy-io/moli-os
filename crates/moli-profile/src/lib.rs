//! Declarative device profiles.
//!
//! A profile (TOML) describes an HTTP device: which requests to make, how
//! often, in which format, and how each answer maps to typed points. One
//! generic driver runs any profile: adding a brand is adding a file, which
//! an agent can write and `moli-os profile check` validates before use. A
//! file runs only once a human approved it (`moli-os profile trust`), and it
//! changes a device only through its declared writes, which are commands.
//!
//! A device of the home network whose address changes (the box gave it a
//! new one) is followed: when it goes silent, the driver looks for its MAC
//! on the same network, checks its identity there, and talks to it at its
//! new address, journaled and remembered (`data/addresses.json`).

mod profile;
mod trust;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use moli_core::{Device, DeviceId, Value};
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::time::Instant;

pub use profile::{
    Answer, BUILTIN, Call, Compiled, CompiledWrite, Format, Profile, ProfileError, Source, builtin,
    parse_every,
};
pub use trust::{ProfileFile, Recorded, TRUST_FILE, record as trust, sha256};

const TIMEOUT: Duration = Duration::from_secs(5);
/// Consecutive failed polls before the device is shown offline.
const OFFLINE_AFTER: u32 = 3;
const MAX_BODY: usize = 1024 * 1024;
/// An image weighs more than a reading: a still from a camera or a screen
/// does not fit in `MAX_BODY`. Bounded all the same, so a device that
/// answers with a film never fills memory.
const MAX_IMAGE: usize = 8 * 1024 * 1024;
/// A failed request is retried after at most this long.
const RETRY_SOON: Duration = Duration::from_secs(15);
/// A silent device is looked for at once, then after this pause, doubled
/// each time it is still nowhere (unplugged), up to `SEARCH_MAX`.
const SEARCH_PAUSE: Duration = Duration::from_secs(5 * 60);
const SEARCH_MAX: Duration = Duration::from_secs(60 * 60);

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// A built-in profile id (`daikin-brp069`) or a path to a `.toml`.
    pub profile: String,
    /// The device's address (web APIs: the profile's own host by default).
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// Values for the profile's `{placeholders}` (`lat`, `lon`…).
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
}

/// Where and how to reach the device.
#[derive(Clone, Debug)]
struct Target {
    host: String,
    port: u16,
    tls: bool,
    vars: std::collections::BTreeMap<String, String>,
}

impl Target {
    fn of(config: &Config, profile: &Compiled) -> Result<Self, ProfileError> {
        let fail = |message: String| ProfileError {
            profile: profile.meta.id.clone(),
            message,
        };
        let host = config
            .host
            .clone()
            .or_else(|| profile.meta.host.clone())
            .ok_or_else(|| fail("no host: set `host` in the driver options".into()))?;
        let missing: Vec<&str> = profile
            .vars
            .iter()
            .filter(|v| !config.vars.contains_key(*v))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(fail(format!(
                "the driver options must give vars {{ {} }}",
                missing.join(", ")
            )));
        }
        // A key (sent, or signing messages) goes only to the host its profile
        // names; `compile` lets only a built-in profile go without one.
        if profile.meta.secret.is_some()
            && let (Some(own), Some(given)) = (&profile.meta.host, &config.host)
            && own != given
        {
            return Err(fail(format!(
                "this profile sends its key to {own}: it cannot be pointed at {given}"
            )));
        }
        let tls = profile.meta.https;
        Ok(Self {
            host,
            port: config
                .port
                .or(profile.meta.port)
                .unwrap_or(if tls { 443 } else { 80 }),
            tls,
            vars: config.vars.clone(),
        })
    }
}

/// Reads and validates a profile: a built-in id, or a file (then also its
/// fingerprint, what a human approves).
pub fn open(profile: &str) -> Result<(Compiled, Option<ProfileFile>), ProfileError> {
    if let Some(text) = builtin(profile) {
        return Ok((Profile::parse(text)?.compile(Source::Builtin)?, None));
    }
    let text = std::fs::read_to_string(profile).map_err(|e| ProfileError {
        profile: profile.to_owned(),
        message: format!("neither a built-in profile nor a readable file: {e}"),
    })?;
    let file = ProfileFile {
        path: std::fs::canonicalize(profile)
            .map_or_else(|_| profile.to_owned(), |p| p.display().to_string()),
        sha256: sha256(&text),
    };
    Ok((Profile::parse(&text)?.compile(Source::File)?, Some(file)))
}

#[derive(Debug)]
pub struct ProfileDriver {
    config: Config,
    profile: Arc<Compiled>,
    /// Set for a profile file: it runs only once approved in `trust_file`.
    file: Option<ProfileFile>,
    trust_file: PathBuf,
    /// When the device may next be looked for (kept across restarts).
    search: Mutex<Search>,
    lan: Lan,
}

/// How the driver sees the home network: the real one, or a test's.
#[derive(Clone, Copy, Debug)]
struct Lan {
    /// Whether an address belongs to the home network (where a device is followed).
    home: fn(Ipv4Addr) -> bool,
    /// The network's neighbour table now: address, MAC.
    table: fn() -> BoxFuture<'static, Vec<(Ipv4Addr, String)>>,
    /// Where a MAC may answer once the network was asked, other than the
    /// addresses already tried.
    ask: fn(String, Ipv4Addr, Vec<Ipv4Addr>) -> BoxFuture<'static, Vec<Ipv4Addr>>,
}

impl Lan {
    const REAL: Self = Self {
        home: |ip| ip.is_private(),
        table: || Box::pin(moli_net::lan::neighbours()),
        ask: |mac, old, skip| Box::pin(async move { moli_net::lan::ask(&mac, old, &skip).await }),
    };

    /// The MAC the network sees at `host`.
    async fn mac_of(self, host: &str) -> Option<String> {
        let ip: Ipv4Addr = host.parse().ok()?;
        (self.table)()
            .await
            .into_iter()
            .find(|(at, _)| *at == ip)
            .map(|(_, mac)| mac)
    }

    /// Whether the network sees another device than `mac` at `host` (the
    /// box gave its address to someone else).
    async fn someone_else_at(self, host: &str, mac: Option<&str>) -> bool {
        let Some(mac) = mac else {
            return false;
        };
        self.mac_of(host).await.is_some_and(|seen| seen != mac)
    }
}

/// Another device than the one expected answers at the address: not ours.
#[derive(Debug)]
struct Elsewhere;

impl std::fmt::Display for Elsewhere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another device answers at this address")
    }
}

impl std::error::Error for Elsewhere {}

/// The pace of looking for a silent device: at once, then less and less often.
#[derive(Debug, Default)]
struct Search {
    next: Option<std::time::Instant>,
    pause: Duration,
}

impl Search {
    /// Whether to look now (and, if so, the next look is put off).
    fn due(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self.next.is_some_and(|at| now < at) {
            return false;
        }
        self.pause = if self.pause.is_zero() {
            SEARCH_PAUSE
        } else {
            (self.pause * 2).min(SEARCH_MAX)
        };
        self.next = Some(now + self.pause);
        true
    }
}

/// How a session with the device ended, short of a failure.
enum Ended {
    Shutdown,
    /// Found at this address: talk to it there.
    Moved(String),
}

/// Whether the driver may follow its device to another address: one the
/// instance gives on the home network (a web API's own host never), for a
/// built-in profile that says who the device is. A profile file reaches
/// only the host a person gave it.
fn followable(config: &Config, profile: &Compiled, lan: Lan) -> bool {
    profile.source == Source::Builtin
        && profile.meta.identity.is_some()
        && profile.meta.host.is_none()
        && config
            .host
            .as_deref()
            .and_then(|h| h.parse::<Ipv4Addr>().ok())
            .is_some_and(lan.home)
}

/// Ids are kept with `/` replaced: compared the same way.
fn same_identity(found: &str, expected: &str) -> bool {
    found.replace('/', "_") == expected.replace('/', "_")
}

/// Who the device is and its MAC, as known before it answers in this run:
/// remembered with its address, or the one device this instance published
/// before (whose identity is its MAC, when the profile says so).
fn expected(
    ctx: &DriverCtx,
    profile: &Compiled,
    configured: &str,
) -> (Option<String>, Option<String>) {
    let known = ctx.known_address();
    if known.as_ref().is_some_and(|a| a.configured != configured) {
        // A person gave another address: whatever answers there is taken.
        return (None, None);
    }
    let identity = known.as_ref().and_then(|a| a.identity.clone()).or_else(|| {
        match ctx.known_devices().as_slice() {
            [only] => only
                .as_str()
                .split_once(':')
                .map(|(_, native)| native.to_owned()),
            _ => None,
        }
    });
    let mac = known.and_then(|a| a.mac).or_else(|| {
        let mac_is_identity =
            profile.meta.mac.is_some() && profile.meta.mac == profile.meta.identity;
        identity
            .as_deref()
            .filter(|_| mac_is_identity)
            .and_then(moli_net::lan::mac_hex)
    });
    (identity, mac)
}

impl ProfileDriver {
    /// `trust_file`: the approvals of the data directory (see [`TRUST_FILE`]).
    /// Given by the host program, never by the driver options: the driver
    /// and `moli-os profile trust` must read the same list.
    pub fn new(config: Config, trust_file: PathBuf) -> Result<Self, ProfileError> {
        let (profile, file) = open(&config.profile)?;
        Target::of(&config, &profile)?;
        Ok(Self {
            config,
            profile: Arc::new(profile),
            file,
            trust_file,
            search: Mutex::new(Search::default()),
            lan: Lan::REAL,
        })
    }

    /// Looks for the device elsewhere on the home network (by its MAC),
    /// checks it is the one (its identity, asked there) and says where it
    /// is now: journaled, and remembered for the next start.
    async fn relocate(
        &self,
        ctx: &DriverCtx,
        target: &Target,
        configured: &str,
        secret: Option<&str>,
        (identity, mac): (Option<&str>, Option<&str>),
    ) -> Looked {
        let (Ok(old), Some(identity), Some(mac)) = (target.host.parse::<Ipv4Addr>(), identity, mac)
        else {
            return Looked::Unsearchable;
        };
        let table = (self.lan.table)().await;
        if moli_net::lan::still_at(&table, mac, old) {
            return Looked::Later; // it is there: only its service is silent
        }
        if !self
            .search
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .due()
        {
            return Looked::Later;
        }
        // The table may still hold old entries naming it: each is checked,
        // then the network is asked.
        let mut tried = moli_net::lan::pick(&table, mac, old, &[]);
        for &ip in &tried {
            if answers_as(&target.at(ip), &self.profile, secret, identity).await {
                return Looked::Found(self.found(ctx, target, configured, identity, mac, ip).await);
            }
        }
        for ip in (self.lan.ask)(mac.to_owned(), old, tried.clone()).await {
            if answers_as(&target.at(ip), &self.profile, secret, identity).await {
                return Looked::Found(self.found(ctx, target, configured, identity, mac, ip).await);
            }
            tried.push(ip);
        }
        if tried.is_empty() {
            tracing::info!(instance = %ctx.instance(), "device not found elsewhere on the home network");
        } else {
            tracing::info!(instance = %ctx.instance(), at = ?tried, "its MAC is seen there, but nothing answers as this device");
        }
        Looked::Nowhere
    }

    /// The device answers at `ip`: remembered, journaled when it is news,
    /// its new address.
    async fn found(
        &self,
        ctx: &DriverCtx,
        target: &Target,
        configured: &str,
        identity: &str,
        mac: &str,
        ip: Ipv4Addr,
    ) -> String {
        let host = ip.to_string();
        let address = KnownAddress {
            configured: configured.to_owned(),
            host: host.clone(),
            mac: Some(mac.to_owned()),
            identity: Some(identity.to_owned()),
        };
        let known = ctx
            .known_address()
            .is_some_and(|a| a.configured == configured && a.host == host);
        if known {
            // Back where it was found before (it failed there a moment).
            ctx.remember_address(address).await;
        } else {
            tracing::info!(instance = %ctx.instance(), old = %target.host, new = %ip, "device found at a new address");
            ctx.moved(Some(ctx.device_id(identity)), &target.host, address)
                .await;
        }
        host
    }

    /// Looked for long enough without finding it: gone for good, perhaps.
    fn long_sought(&self) -> bool {
        self.search
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pause
            >= SEARCH_MAX
    }
}

/// What looking for the device gave.
enum Looked {
    /// It answers there (journaled, remembered).
    Found(String),
    /// Looked for: nowhere on the home network.
    Nowhere,
    /// Not looked for now (too soon, or the network still sees it at its address).
    Later,
    /// Nothing to look for it with (its identity or MAC is not known).
    Unsearchable,
}

impl Target {
    /// The same device at another address.
    fn at(&self, ip: Ipv4Addr) -> Self {
        Self {
            host: ip.to_string(),
            ..self.clone()
        }
    }
}

/// Whether the device at `target` says it is `identity` (its identity
/// request, answered there).
async fn answers_as(
    target: &Target,
    profile: &Compiled,
    secret: Option<&str>,
    identity: &str,
) -> bool {
    let Some((request, path)) = profile
        .meta
        .identity
        .as_deref()
        .and_then(|r| r.split_once('.'))
    else {
        return false;
    };
    let Some(request) = profile.requests.iter().find(|r| r.id == request) else {
        return false;
    };
    let path: Vec<String> = path.split('.').map(str::to_owned).collect();
    fetch(target, request, secret)
        .await
        .ok()
        .and_then(|answer| answer.text(&path))
        .is_some_and(|found| same_identity(&found, identity))
}

impl Driver for ProfileDriver {
    fn kind(&self) -> &'static str {
        "profile"
    }

    /// A built-in profile is its catalogue package; a file belongs to none
    /// (its id is whatever the file says).
    fn integration(&self) -> Arc<str> {
        match self.file {
            None => Arc::from(self.profile.meta.id.as_str()),
            Some(_) => Arc::from("profile"),
        }
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// An image on demand: the profile's `snapshot` path, asked only when a
/// surface wants one. Nothing is polled, nothing is kept.
#[derive(Debug)]
struct Snapshots {
    target: Target,
    path: String,
}

impl SnapshotSource for Snapshots {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            let bytes =
                exchange(&self.target, &self.path, &[], &self.path, None, MAX_IMAGE).await?;
            let Some(content_type) = image_type(&bytes) else {
                bail!(
                    "{}: the answer is not a PNG, JPEG, GIF or WebP image",
                    self.path
                );
            };
            Ok(Image {
                content_type: content_type.to_owned(),
                bytes: bytes.to_vec(),
            })
        })
    }
}

/// What the bytes are, read from them: a device often says `text/html` or
/// nothing at all, and an image announced wrong simply does not show.
/// `None` when they are no image at all (an error page served with a 200):
/// it must not reach a dashboard, nor an agent, labelled as a photo.
fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

/// One HTTP(S) exchange (`GET`, or `POST` of a JSON body), bounded in time
/// and size. `label` names the request in errors: the path *template*, so a
/// `{secret}` never ends up in a log.
async fn exchange(
    target: &Target,
    path: &str,
    headers: &[(String, String)],
    label: &str,
    json_body: Option<Vec<u8>>,
    limit: usize,
) -> anyhow::Result<Bytes> {
    let (host, port) = (target.host.as_str(), target.port);
    let fetch = async {
        let tcp = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("cannot reach {host}:{port}"))?;
        let builder = http::Request::builder()
            .uri(path)
            .header("host", host_header(host, port, target.tls))
            .header("accept", "application/json, text/plain, */*");
        let builder = headers.iter().fold(builder, |b, (name, value)| {
            b.header(name.as_str(), value.as_str())
        });
        let request = match json_body {
            None => builder
                .method(http::Method::GET)
                .body(Full::new(Bytes::new()))?,
            Some(body) => builder
                .method(http::Method::POST)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body)))?,
        };
        // Embedded HTTP servers are not always RFC-tolerant: Daikin's BRP069
        // answers 403 to `host:` and 200 to `Host:`. Title-Case headers
        // satisfy every device seen so far.
        let mut handshake = hyper::client::conn::http1::Builder::new();
        handshake.title_case_headers(true);
        let response = if target.tls {
            let name = rustls::pki_types::ServerName::try_from(host.to_owned())
                .with_context(|| format!("{host}: not a valid TLS name"))?;
            let tls = tokio_rustls::TlsConnector::from(moli_net::public_tls())
                .connect(name, tcp)
                .await
                .with_context(|| format!("{host}: TLS"))?;
            let (mut sender, connection) = handshake.handshake(TokioIo::new(tls)).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender.send_request(request).await?
        } else {
            let (mut sender, connection) = handshake.handshake(TokioIo::new(tcp)).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender.send_request(request).await?
        };
        if !response.status().is_success() {
            bail!("{label}: HTTP {}", response.status());
        }
        let body = http_body_util::Limited::new(response.into_body(), limit)
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?
            .to_bytes();
        anyhow::Ok(body)
    };
    tokio::time::timeout(TIMEOUT, fetch)
        .await
        .with_context(|| format!("{host}{label}: no answer in time"))?
}

/// `Host` value: with the port unless it is 80, IPv6 in brackets.
fn host_header(host: &str, port: u16, tls: bool) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    if port == if tls { 443 } else { 80 } {
        host
    } else {
        format!("{host}:{port}")
    }
}

async fn fetch(
    target: &Target,
    request: &profile::CompiledRequest,
    secret: Option<&str>,
) -> anyhow::Result<Answer> {
    let value = |name: &str| {
        if name == "secret" {
            secret.map(str::to_owned)
        } else {
            target.vars.get(name).cloned()
        }
    };
    let path = profile::expand(&request.path, value, true);
    let headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(n, v)| (n.clone(), profile::expand(v, value, false)))
        .collect();
    let body = match &request.call {
        Call::Get => exchange(target, &path, &headers, &request.path, None, MAX_BODY).await?,
        Call::Meross { namespace, payload } => {
            let key = secret.context("meross request without its key")?;
            let message = meross_message(namespace, payload, key);
            exchange(
                target,
                &path,
                &headers,
                &request.path,
                Some(message.to_string().into_bytes()),
                MAX_BODY,
            )
            .await?
        }
    };
    let answer = Answer::parse(request.format, &body)
        .with_context(|| format!("{}: unreadable {:?} answer", request.path, request.format))?;
    if let (Call::Meross { namespace, .. }, Answer::Json(json)) = (&request.call, &answer) {
        let method = json["header"]["method"].as_str().unwrap_or_default();
        if method != "GETACK" {
            bail!("{namespace}: device answered {method:?} (wrong key or namespace?)");
        }
    }
    Ok(answer)
}

/// A signed Meross local-API `GET` message:
/// `sign = md5(messageId ‖ key ‖ timestamp)`.
fn meross_message(namespace: &str, payload: &serde_json::Value, key: &str) -> serde_json::Value {
    use md5::{Digest as _, Md5};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let unique = format!(
        "{}-{}",
        now.as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let message_id = hex(&Md5::digest(unique.as_bytes()));
    let timestamp = now.as_secs();
    let sign = hex(&Md5::digest(
        format!("{message_id}{key}{timestamp}").as_bytes(),
    ));
    serde_json::json!({
        "header": {
            "from": "/appliance/moli/subscribe",
            "messageId": message_id,
            "method": "GET",
            "namespace": namespace,
            "payloadVersion": 1,
            "sign": sign,
            "timestamp": timestamp,
        },
        "payload": payload,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The key a signing transport needs. `None` once a human has been told
/// how to provide it and the system is shutting down; `Some(None)` when the
/// profile needs no secret.
async fn required_secret(profile: &Compiled, ctx: &DriverCtx) -> Option<Option<String>> {
    let Some(name) = &profile.meta.secret else {
        return Some(None);
    };
    if let Some(value) = ctx.secret(name) {
        return Some(Some(value));
    }
    ctx.wait_for(moli_i18n::tr!(
        "pilotes.profile.cle_absente",
        profile = profile.meta.name,
        key = name,
        instance = ctx.instance()
    ));
    ctx.cancelled().await;
    None
}

/// Whether a human approved this profile file. If not, says how on every
/// surface and waits for shutdown (`false`): an unreviewed file never sends
/// a single request.
async fn approved(ctx: &DriverCtx, driver: &ProfileDriver, file: &ProfileFile) -> bool {
    let profile = &driver.config.profile;
    match trust::is_trusted(&driver.trust_file, &file.sha256).await {
        Ok(true) => return true,
        Ok(false) => ctx.wait_for(moli_i18n::tr!(
            "pilotes.profile.non_valide",
            profile = profile
        )),
        Err(e) => ctx.wait_for(moli_i18n::tr!(
            "pilotes.profile.illisible",
            file = driver.trust_file.display(),
            error = e
        )),
    }
    ctx.cancelled().await;
    false
}

/// The device as last read: latest answers, each request's schedule and
/// failure count.
struct Session<'a> {
    target: &'a Target,
    profile: &'a Compiled,
    secret: Option<&'a str>,
    answers: HashMap<String, Answer>,
    due: Vec<Instant>,
    failures: Vec<u32>,
    offline: bool,
    /// Polls in a row that reached nobody at the address (any request).
    silent: u32,
}

/// Nobody answered at the address (refused, unreachable, silent), as
/// opposed to a device that answered something wrong.
fn unreachable(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.is::<std::io::Error>() || c.is::<tokio::time::error::Elapsed>())
}

impl<'a> Session<'a> {
    fn new(target: &'a Target, profile: &'a Compiled, secret: Option<&'a str>) -> Self {
        Self {
            target,
            profile,
            secret,
            answers: HashMap::new(),
            due: vec![Instant::now(); profile.requests.len()],
            failures: vec![0; profile.requests.len()],
            offline: false,
            silent: 0,
        }
    }

    /// Nobody has answered at the address for a while: it may have moved.
    fn gone(&self) -> bool {
        self.silent >= OFFLINE_AFTER
    }

    /// Every request once. Only what tells who the device is must succeed;
    /// the rest may be late (a printer still booting) and is retried soon.
    async fn first_round(&mut self) -> anyhow::Result<()> {
        let profile = self.profile;
        let needed = identity_requests(profile);
        for (i, request) in profile.requests.iter().enumerate() {
            match fetch(self.target, request, self.secret).await {
                Ok(answer) => {
                    self.answers.insert(request.id.clone(), answer);
                    self.due[i] = Instant::now() + request.every;
                }
                Err(e) if needed.contains(&request.id.as_str()) => return Err(e),
                Err(e) => {
                    tracing::debug!(error = %e, request = %request.id, "first poll failed, retrying soon");
                    self.failures[i] = 1;
                    self.due[i] = Instant::now() + RETRY_SOON;
                }
            }
        }
        Ok(())
    }

    /// The `(request, when)` due first. `compile` guarantees one request.
    fn next_due(&self) -> Option<(usize, Instant)> {
        self.due
            .iter()
            .enumerate()
            .min_by_key(|(_, at)| **at)
            .map(|(i, at)| (i, *at))
    }

    /// Request `i` now: fresh values published and kept, or its failure counted.
    async fn poll(&mut self, ctx: &DriverCtx, device: &DeviceId, i: usize) -> anyhow::Result<()> {
        let profile = self.profile;
        let request = &profile.requests[i];
        let result = match fetch(self.target, request, self.secret).await {
            Ok(answer) => {
                self.failures[i] = 0;
                self.silent = 0;
                self.due[i] = Instant::now() + request.every;
                publish(ctx, profile, device, &request.id, &answer);
                self.answers.insert(request.id.clone(), answer);
                Ok(())
            }
            Err(e) => {
                self.failures[i] += 1;
                if unreachable(&e) {
                    self.silent += 1;
                }
                // Retry sooner than the normal pace, never faster than RETRY_SOON.
                self.due[i] = Instant::now() + request.every.min(RETRY_SOON.max(request.every / 4));
                tracing::debug!(error = %e, request = %request.id, failures = self.failures[i], "poll failed");
                if self.failures[i] == OFFLINE_AFTER {
                    // Its values are no longer current: say so instead of
                    // showing stale numbers as if they were fresh.
                    clear(ctx, profile, device, &request.id);
                    tracing::warn!(device = %device, request = %request.id, error = %e, "request failing");
                }
                Err(e)
            }
        };
        // Offline only when nothing answers any more.
        let all_failing = self.failures.iter().all(|f| *f >= OFFLINE_AFTER);
        if all_failing != self.offline {
            self.offline = all_failing;
            ctx.set_availability(device, !self.offline);
        }
        result
    }

    /// One order, through its declared write; any other point is read-only.
    /// Nothing reaches the device once the hub stopped waiting for it.
    async fn command(&mut self, ctx: &DriverCtx, device: &DeviceId, command: CommandRequest) {
        let profile = self.profile;
        let Some(write) = profile.writes.iter().find(|w| w.key == command.key) else {
            command.reply(Err(moli_i18n::tr!("pilotes.profile.lecture_seule")));
            return;
        };
        let deadline = Instant::from_std(command.deadline);
        let outcome =
            tokio::time::timeout_at(deadline, self.write(ctx, device, write, &command.value))
                .await
                .unwrap_or_else(|_| Err(moli_i18n::tr!("pilotes.profile.pas_a_temps")));
        if let Err(e) = &outcome {
            tracing::info!(device = %device, point = %write.key, error = %e, "write not done");
        }
        let done = outcome.is_ok();
        command.reply(outcome);
        if done && let Some(i) = write.refresh {
            // Show what the device did, not what was asked.
            let _ = self.poll(ctx, device, i).await;
        }
    }

    /// Re-reads what the path echoes (the remote may have changed it since
    /// the last poll: echoing a stale value would undo that), then writes.
    async fn write(
        &mut self,
        ctx: &DriverCtx,
        device: &DeviceId,
        write: &CompiledWrite,
        value: &Value,
    ) -> Result<(), String> {
        for &i in &write.reads {
            self.poll(ctx, device, i).await.map_err(|e| {
                moli_i18n::tr!("pilotes.profile.relecture", error = format!("{e:#}"))
            })?;
        }
        let path = render(write, value, &self.answers, self.target, self.secret)?;
        let body = exchange(self.target, &path, &[], &write.path, None, MAX_BODY)
            .await
            .map_err(|e| format!("{e:#}"))?;
        if let Some(expect) = &write.expect {
            let answer = String::from_utf8_lossy(&body);
            if !answer.contains(expect.as_str()) {
                let answer: String = answer.trim().chars().take(80).collect();
                return Err(moli_i18n::tr!("pilotes.profile.refuse", answer = answer));
            }
        }
        Ok(())
    }
}

/// The path a write sends: the command's value, the instance's vars and the
/// device's latest answers.
fn render(
    write: &CompiledWrite,
    value: &Value,
    answers: &HashMap<String, Answer>,
    target: &Target,
    secret: Option<&str>,
) -> Result<String, String> {
    write.render(value, answers, |name| {
        if name == "secret" {
            secret.map(str::to_owned)
        } else {
            target.vars.get(name).cloned()
        }
    })
}

/// Request ids the device's identity and name come from.
fn identity_requests(profile: &Compiled) -> Vec<&str> {
    [&profile.meta.identity, &profile.meta.name_from]
        .into_iter()
        .flatten()
        .filter_map(|r| r.split_once('.').map(|(request, _)| request))
        .collect()
}

async fn run(driver: &ProfileDriver, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let profile = &*driver.profile;
    let configured = Target::of(&driver.config, profile)?;
    if let Some(file) = &driver.file
        && !approved(ctx, driver, file).await
    {
        return Ok(()); // waiting for a human, then shut down
    }
    let Some(secret) = required_secret(profile, ctx).await else {
        return Ok(()); // waiting for a human, then shut down
    };
    let follow = followable(&driver.config, profile, driver.lan);
    let mut target = configured.clone();
    if follow
        && let Some(known) = ctx
            .known_address()
            .filter(|a| a.configured == configured.host && a.host != configured.host)
    {
        tracing::info!(instance = %ctx.instance(), host = %known.host, "device reached at the address found earlier");
        target.host = known.host;
    }
    loop {
        let (identity, mac) = if follow {
            expected(ctx, profile, &configured.host)
        } else {
            (None, None)
        };
        let wanted = (identity.as_deref(), mac.as_deref());
        let failure = match serve(
            driver,
            ctx,
            &target,
            &configured.host,
            secret.as_deref(),
            follow.then_some(wanted),
        )
        .await
        {
            Ok(Ended::Shutdown) => return Ok(()),
            Ok(Ended::Moved(host)) => {
                target.host = host;
                continue;
            }
            Err(e) => e,
        };
        if !follow {
            return Err(failure);
        }
        if target.host != configured.host {
            // What was learned no longer holds: the configured address first.
            tracing::info!(instance = %ctx.instance(), host = %target.host, error = %failure, "the address found earlier no longer answers as before");
            target.host.clone_from(&configured.host);
            continue;
        }
        if not_there(driver.lan, &failure, &target, wanted.1).await
            && let Looked::Found(host) = driver
                .relocate(ctx, &target, &configured.host, secret.as_deref(), wanted)
                .await
        {
            target.host = host;
            continue;
        }
        return Err(failure);
    }
}

/// The text at `request.path` in the latest answers.
fn read(answers: &HashMap<String, Answer>, reference: Option<&str>) -> Option<String> {
    let (request, path) = reference?.split_once('.')?;
    let path: Vec<String> = path.split('.').map(str::to_owned).collect();
    answers.get(request)?.text(&path)
}

/// Whether a failure says the device is not at `target` any more: nobody
/// answers there, another device does, or the network sees another MAC.
async fn not_there(lan: Lan, e: &anyhow::Error, target: &Target, mac: Option<&str>) -> bool {
    unreachable(e) || e.is::<Elsewhere>() || lan.someone_else_at(&target.host, mac).await
}

/// When the device answering at `target` (`native`) is not the one
/// expected: never taken at an address only learned (`Elsewhere`); at the
/// configured address, the expected one is looked for first (it moved and
/// another got its address: `Some`, where it is). This one is taken (a
/// person replaced it: `None`) only once the expected one was looked for
/// long enough, or cannot be.
async fn check(
    driver: &ProfileDriver,
    ctx: &DriverCtx,
    target: &Target,
    configured: &str,
    secret: Option<&str>,
    wanted: (Option<&str>, Option<&str>),
    native: &str,
) -> anyhow::Result<Option<String>> {
    let Some(expected) = wanted.0.filter(|expected| !same_identity(native, expected)) else {
        return Ok(None);
    };
    if target.host != configured {
        return Err(Elsewhere.into());
    }
    match driver
        .relocate(ctx, target, configured, secret, wanted)
        .await
    {
        Looked::Found(host) => Ok(Some(host)),
        Looked::Nowhere if !driver.long_sought() => Err(Elsewhere.into()),
        Looked::Later => Err(Elsewhere.into()),
        Looked::Nowhere | Looked::Unsearchable => {
            tracing::info!(instance = %ctx.instance(), device = %native, before = %expected, "another device answers at the configured address: it is this instance's now");
            Ok(None)
        }
    }
}

/// What the poll loop keeps an eye on, for a device that may be followed.
struct Watch {
    native: String,
    mac: Option<String>,
    was_gone: bool,
    /// When the network was last asked who it sees at the address.
    looked: Instant,
}

/// How often the poll loop asks the network who it sees at the address.
const LOOK_EVERY: Duration = Duration::from_secs(60);

impl Watch {
    /// After request `i` was polled: whether the device is still the one at
    /// its address. `Ok(Some(host))`: found elsewhere; `Err(Elsewhere)`:
    /// another device answers there now (two swapped their addresses).
    async fn polled(
        &mut self,
        driver: &ProfileDriver,
        ctx: &DriverCtx,
        session: &mut Session<'_>,
        (i, polled): (usize, &anyhow::Result<()>),
        configured: &str,
    ) -> anyhow::Result<Option<String>> {
        let (lan, profile, target) = (driver.lan, &*driver.profile, session.target);
        match polled {
            Ok(()) => {
                let identity = profile.meta.identity.as_deref();
                if identity_requests(profile).first() == Some(&profile.requests[i].id.as_str())
                    && read(&session.answers, identity)
                        .is_some_and(|now| !same_identity(&now, &self.native))
                {
                    return Err(Elsewhere.into());
                }
                if self.looked.elapsed() >= LOOK_EVERY {
                    self.looked = Instant::now();
                    if lan.someone_else_at(&target.host, self.mac.as_deref()).await {
                        return Err(Elsewhere.into());
                    }
                }
            }
            // Something answers there, wrongly: is it still the device?
            Err(e) if !unreachable(e) => {
                session.silent = if lan.someone_else_at(&target.host, self.mac.as_deref()).await {
                    session.silent + 1
                } else {
                    0
                };
            }
            Err(_) => {}
        }
        if self.was_gone && !session.gone() {
            // Back where it was: next time it goes silent, looked for at once.
            *driver.search.lock().unwrap_or_else(PoisonError::into_inner) = Search::default();
        }
        self.was_gone = session.gone();
        if !self.was_gone {
            return Ok(None);
        }
        let wanted = (Some(self.native.as_str()), self.mac.as_deref());
        match driver
            .relocate(ctx, target, configured, session.secret, wanted)
            .await
        {
            Looked::Found(host) => Ok(Some(host)),
            _ => Ok(None),
        }
    }
}

/// The device at `target`, until it moves, the system stops, or it fails.
/// `follow`: who the device should be, when it may be followed elsewhere.
async fn serve(
    driver: &ProfileDriver,
    ctx: &mut DriverCtx,
    target: &Target,
    configured: &str,
    secret: Option<&str>,
    follow: Option<(Option<&str>, Option<&str>)>,
) -> anyhow::Result<Ended> {
    let profile = &*driver.profile;
    let mut session = Session::new(target, profile, secret);
    session.first_round().await?;
    let lookup = |reference: &Option<String>| read(&session.answers, reference.as_deref());
    // A declared identity is mandatory: falling back to the address would
    // split one device in two the day the identity comes back.
    let native = match &profile.meta.identity {
        Some(reference) => lookup(&profile.meta.identity)
            .with_context(|| format!("identity {reference} missing from the device's answer"))?,
        None => format!("{}:{}", target.host, target.port),
    };
    let name = lookup(&profile.meta.name_from).unwrap_or_else(|| profile.meta.name.clone());
    let declared_mac = lookup(&profile.meta.mac).and_then(|m| moli_net::lan::mac_hex(&m));
    if let Some(wanted) = follow
        && let Some(host) = check(driver, ctx, target, configured, secret, wanted, &native).await?
    {
        return Ok(Ended::Moved(host));
    }
    let id = ctx.device_id(&native);
    ctx.upsert_device(Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.as_str().into(),
        manufacturer: profile.meta.manufacturer.as_deref().map(Into::into),
        model: profile.meta.model.as_deref().map(Into::into),
        description: Some(profile.meta.name.as_str().into()),
        native_room: None,
        members: Vec::new(),
        points: profile.points.iter().map(|p| p.spec.clone()).collect(),
    });
    if let Some(path) = &profile.meta.snapshot {
        ctx.provide_snapshots(
            &id,
            Arc::new(Snapshots {
                target: target.clone(),
                path: path.clone(),
            }),
        );
    }
    for (request, answer) in &session.answers {
        publish(ctx, profile, &id, request, answer);
    }
    ctx.set_availability(&id, true);
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), profile = %profile.meta.id, device = %id, "profile device ready");
    let mut watch = match follow {
        Some(_) => Some(Watch {
            mac: learn(ctx, driver.lan, target, configured, &native, declared_mac).await,
            native,
            was_gone: false,
            looked: Instant::now(),
        }),
        None => None,
    };
    // It answers: the next time it goes silent, it is looked for at once.
    *driver.search.lock().unwrap_or_else(PoisonError::into_inner) = Search::default();

    // Then each request on its own schedule, orders as they come.
    loop {
        let Some((next, at)) = session.next_due() else {
            bail!("profile without requests");
        };
        let command = tokio::select! {
            () = tokio::time::sleep_until(at) => None,
            command = ctx.next_command() => match command {
                Some(command) => Some(command),
                None => return Ok(Ended::Shutdown),
            },
        };
        if let Some(command) = command {
            session.command(ctx, &id, command).await;
            continue;
        }
        let polled = session.poll(ctx, &id, next).await;
        if let Some(watch) = &mut watch
            && let Some(host) = watch
                .polled(driver, ctx, &mut session, (next, &polled), configured)
                .await?
        {
            return Ok(Ended::Moved(host));
        }
    }
}

/// Remembers where the device answered, with its MAC (the one the network
/// sees at that address, else the one the device declares, else the one
/// known before): what it takes to find it again. Returns the MAC.
async fn learn(
    ctx: &DriverCtx,
    lan: Lan,
    target: &Target,
    configured: &str,
    identity: &str,
    declared_mac: Option<String>,
) -> Option<String> {
    let seen = lan.mac_of(&target.host).await;
    if let (Some(seen), Some(declared)) = (&seen, &declared_mac)
        && seen != declared
    {
        // A device chooses what it says; the network's word is kept.
        tracing::info!(instance = %ctx.instance(), "the device declares another MAC than the network sees");
    }
    let before = ctx
        .known_address()
        .filter(|a| {
            a.configured == configured
                && a.identity
                    .as_deref()
                    .is_none_or(|known| same_identity(known, identity))
        })
        .and_then(|a| a.mac);
    let mac = seen.or(declared_mac).or(before);
    ctx.remember_address(KnownAddress {
        configured: configured.to_owned(),
        host: target.host.clone(),
        mac: mac.clone(),
        identity: Some(identity.to_owned()),
    })
    .await;
    mac
}

fn clear(ctx: &DriverCtx, profile: &Compiled, device: &DeviceId, request: &str) {
    for point in profile.points.iter().filter(|p| p.request == request) {
        ctx.set_state(device, &point.spec.key, moli_core::Value::Null);
    }
}

fn publish(ctx: &DriverCtx, profile: &Compiled, device: &DeviceId, request: &str, answer: &Answer) {
    for point in profile.points.iter().filter(|p| p.request == request) {
        if let Some(value) = point.read(answer) {
            ctx.set_state(device, &point.spec.key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_image_bytes_are_an_image() {
        assert_eq!(image_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(
            image_type(&[0x89, 0x50, 0x4E, 0x47, 0x0D]),
            Some("image/png")
        );
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"<!DOCTYPE html><title>404</title>"), None);
        assert_eq!(image_type(b""), None);
    }
    use md5::{Digest as _, Md5};
    use moli_core::DriverStatus;

    /// `get_control_info` of a real BRP069, switched off in auto mode.
    /// Each mode remembers its set point (`dt<mode>`), humidity (`dh<mode>`)
    /// and fan speed (`dfr<mode>`); fan-only (6) has no set point.
    const DAIKIN_CONTROL: &[u8] =
        include_bytes!("../../../integrations/daikin-brp069/fixtures/control_info.kv");

    fn config(profile: &str) -> Config {
        Config {
            profile: profile.into(),
            // TEST-NET: nothing here ever connects.
            host: Some("192.0.2.1".into()),
            port: None,
            vars: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn daikin_writes_echo_every_parameter_of_the_latest_answer() {
        let (profile, file) = open("daikin-brp069").unwrap();
        assert!(file.is_none(), "built in");
        let target = Target::of(&config("daikin-brp069"), &profile).unwrap();
        let mut answers = HashMap::new();
        answers.insert(
            "control".to_owned(),
            Answer::parse(Format::Kv, DAIKIN_CONTROL).unwrap(),
        );
        let write = |key: &str| profile.writes.iter().find(|w| &*w.key == key).unwrap();
        let path = |key: &str, value: Value| {
            render(write(key), &value, &answers, &target, None)
                .unwrap()
                .strip_prefix("/aircon/set_control_info?")
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            path("on", Value::Bool(false)),
            "pow=0&mode=1&stemp=25.0&shum=0&f_rate=A&f_dir=1"
        );
        // A mode brings back its own set point, humidity and fan speed…
        assert_eq!(
            path("mode", Value::from("froid")),
            "pow=0&mode=3&stemp=24.0&shum=0&f_rate=A&f_dir=1"
        );
        assert_eq!(
            path("mode", Value::from("chaud")),
            "pow=0&mode=4&stemp=25.0&shum=0&f_rate=5&f_dir=1"
        );
        // …and fan-only, which has no set point, keeps the current one.
        assert_eq!(
            path("mode", Value::from("ventilation")),
            "pow=0&mode=6&stemp=25.0&shum=0&f_rate=5&f_dir=1"
        );
        assert_eq!(
            path("target_temperature", Value::Float(22.0)),
            "pow=0&mode=1&stemp=22.0&shum=0&f_rate=A&f_dir=1"
        );
        assert_eq!(
            path("target_temperature", Value::Float(21.5)),
            "pow=0&mode=1&stemp=21.5&shum=0&f_rate=A&f_dir=1"
        );
        assert_eq!(
            path("fan", Value::from("3")),
            "pow=0&mode=1&stemp=25.0&shum=0&f_rate=5&f_dir=1"
        );
        // Never a partial order: without an answer to echo, nothing is sent.
        let err = render(
            write("on"),
            &Value::Bool(true),
            &HashMap::new(),
            &target,
            None,
        )
        .unwrap_err();
        assert!(err.contains("control.mode"), "{err}");
        for w in &profile.writes {
            assert_eq!(w.reads, [2], "control is read again before writing");
            assert_eq!(w.refresh, Some(2), "and after");
            assert_eq!(w.expect.as_deref(), Some("ret=OK"));
        }
    }

    #[test]
    fn a_builtin_profile_is_its_catalogue_package() {
        let driver = ProfileDriver::new(config("daikin-brp069"), TRUST_FILE.into()).unwrap();
        assert_eq!(&*driver.integration(), "daikin-brp069");
    }

    #[tokio::test]
    async fn an_unapproved_profile_file_waits_for_a_human() {
        let dir = std::env::temp_dir().join(format!("moli-profile-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clim.toml");
        std::fs::write(&path, builtin("daikin-brp069").unwrap()).unwrap();
        let driver =
            ProfileDriver::new(config(&path.display().to_string()), dir.join(TRUST_FILE)).unwrap();
        assert_eq!(
            driver.file.as_ref().unwrap().sha256,
            sha256(builtin("daikin-brp069").unwrap())
        );
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let task =
            moli_runtime::spawn_driver(&hub, "clim".into(), Arc::new(driver), cancel.clone());
        assert_eq!(
            &*hub.snapshot().drivers[0].integration,
            "profile",
            "a file belongs to no catalogue package"
        );
        let mut status = DriverStatus::Starting;
        for _ in 0..200 {
            status = hub.snapshot().drivers[0].status.clone();
            if matches!(status, DriverStatus::Waiting { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let DriverStatus::Waiting { reason } = status else {
            panic!("{status:?}");
        };
        assert!(
            reason.contains("profil non validé") && reason.contains("profile trust"),
            "{reason}"
        );
        assert!(
            hub.snapshot().devices.is_empty(),
            "nothing read, nothing published"
        );
        cancel.cancel();
        task.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meross_messages_are_signed_like_the_device_expects() {
        let message = meross_message("Appliance.System.All", &serde_json::json!({}), "secret-key");
        let header = &message["header"];
        let expected = hex(&Md5::digest(
            format!(
                "{}secret-key{}",
                header["messageId"].as_str().unwrap(),
                header["timestamp"].as_u64().unwrap()
            )
            .as_bytes(),
        ));
        assert_eq!(header["sign"], expected);
        assert_eq!(header["method"], "GET");
        assert_eq!(header["messageId"].as_str().unwrap().len(), 32);
        let other = meross_message("Appliance.System.All", &serde_json::json!({}), "secret-key");
        assert_ne!(
            other["header"]["messageId"], header["messageId"],
            "ids never repeat"
        );
        assert!(
            !message.to_string().contains("secret-key"),
            "the key never travels"
        );
    }

    #[test]
    fn host_headers() {
        assert_eq!(host_header("192.168.0.21", 80, false), "192.168.0.21");
        assert_eq!(
            host_header("192.168.0.56", 7125, false),
            "192.168.0.56:7125"
        );
        assert_eq!(host_header("fe80::1", 8080, false), "[fe80::1]:8080");
    }

    #[test]
    fn a_silent_device_is_looked_for_less_and_less_often() {
        let mut search = Search::default();
        assert!(search.due(), "at once");
        assert!(!search.due(), "not again right away");
        let mut pauses = Vec::new();
        for _ in 0..6 {
            search.next = None; // the pause went by
            assert!(search.due());
            pauses.push(search.pause.as_secs() / 60);
        }
        assert_eq!(pauses, [10, 20, 40, 60, 60, 60]);
    }

    /// The fixtures' adapter (MAC `020000000003`).
    const MAC: &str = "020000000003";
    /// Another adapter of the same kind.
    const OTHER: &str = "020000000099";

    /// A Daikin adapter answering the fixtures at `ip` (port 0: any), `mac`
    /// replacing the one its basic info gives. Returns its port.
    async fn adapter(ip: Ipv4Addr, port: u16, mac: &'static str) -> u16 {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind((ip, port)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tcp.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let line = String::from_utf8_lossy(&request);
                    let fixture = |name: &str| {
                        let path = format!(
                            "{}/../../integrations/daikin-brp069/fixtures/{name}",
                            env!("CARGO_MANIFEST_DIR")
                        );
                        std::fs::read_to_string(path).unwrap()
                    };
                    let body = if line.contains("/common/basic_info") {
                        fixture("basic_info.kv").replace(MAC, mac)
                    } else if line.contains("/aircon/get_sensor_info") {
                        fixture("sensor_info.kv")
                    } else {
                        fixture("control_info.kv")
                    };
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tcp.write_all(answer.as_bytes()).await;
                });
            }
        });
        port
    }

    const CONFIGURED: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
    const ELSEWHERE: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 3);

    /// The home network of these tests: the loopback, where asking the
    /// network finds the adapter's MAC at 127.0.0.1.
    const LOOPBACK: Lan = Lan {
        home: |ip| ip.is_loopback(),
        table: || Box::pin(async { Vec::new() }),
        ask: |_, _, _| Box::pin(async { vec![Ipv4Addr::LOCALHOST] }),
    };

    /// Nothing to find: whoever looks finds nobody.
    const NOBODY: Lan = Lan {
        ask: |_, _, _| Box::pin(async { Vec::new() }),
        ..LOOPBACK
    };

    /// The network sees the adapter at 127.0.0.3 (and another one at the
    /// configured address).
    const TWIN: Lan = Lan {
        table: || {
            Box::pin(async { vec![(CONFIGURED, OTHER.to_owned()), (ELSEWHERE, MAC.to_owned())] })
        },
        ask: |_, _, _| Box::pin(async { Vec::new() }),
        ..LOOPBACK
    };

    struct House {
        dir: PathBuf,
        hub: moli_runtime::Hub,
        cancel: tokio_util::sync::CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    impl House {
        /// A house that had met the adapter before (named it), whose
        /// configuration still gives its old address (127.0.0.2), with what
        /// it remembers of addresses (`book`, written before it starts).
        fn new(name: &str, port: u16, lan: Lan, book: Option<serde_json::Value>) -> Self {
            let dir =
                std::env::temp_dir().join(format!("moli-profile-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let labels = dir.join("labels.toml");
            if !labels.exists() {
                std::fs::write(&labels, format!("[\"clim:{MAC}\"]\nname = \"Clim\"\n")).unwrap();
            }
            if let Some(book) = book {
                std::fs::write(dir.join("addresses.json"), book.to_string()).unwrap();
            }
            let hub = moli_runtime::Hub::new(moli_runtime::HubOptions {
                labels_path: Some(labels),
                addresses_path: Some(dir.join("addresses.json")),
                ..moli_runtime::HubOptions::default()
            })
            .unwrap();
            let config = Config {
                profile: "daikin-brp069".into(),
                host: Some(CONFIGURED.to_string()),
                port: Some(port),
                vars: std::collections::BTreeMap::new(),
            };
            let driver = ProfileDriver {
                lan,
                ..ProfileDriver::new(config, dir.join(TRUST_FILE)).unwrap()
            };
            let cancel = tokio_util::sync::CancellationToken::new();
            let task =
                moli_runtime::spawn_driver(&hub, "clim".into(), Arc::new(driver), cancel.clone());
            Self {
                dir,
                hub,
                cancel,
                task,
            }
        }

        /// Whether the adapter with `mac` comes online here soon.
        async fn online(&self, mac: &str) -> bool {
            let id = DeviceId::from(format!("clim:{mac}"));
            for _ in 0..300 {
                if self.hub.device(&id).is_some_and(|d| d.online == Some(true)) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }

        fn moves(&self) -> Vec<(String, String)> {
            self.hub
                .journal(50)
                .iter()
                .filter_map(|e| match &e.action {
                    moli_core::Action::Address {
                        from, to, device, ..
                    } => {
                        assert_eq!(
                            device.as_ref().map(DeviceId::as_str),
                            Some("clim:020000000003")
                        );
                        assert_eq!(e.origin, moli_core::Origin::System);
                        Some((from.clone(), to.clone()))
                    }
                    _ => None,
                })
                .collect()
        }

        fn remembered(&self) -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(self.dir.join("addresses.json")).unwrap())
                .unwrap()
        }

        /// What it remembers once `identity` is written there. The device is
        /// online before its address is written (`learn`, after `ready`): read
        /// at once, the file may still hold the book from before, as it did
        /// now and then on a busy machine.
        async fn remembered_as(&self, identity: &str) -> serde_json::Value {
            for _ in 0..300 {
                let book = self.remembered();
                if book["clim"]["identity"] == identity {
                    return book;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            self.remembered()
        }

        async fn stop(self) {
            self.cancel.cancel();
            self.task.await.unwrap();
            let _ = std::fs::remove_dir_all(self.dir);
        }
    }

    fn book(host: Ipv4Addr) -> serde_json::Value {
        serde_json::json!({ "clim": {
            "configured": CONFIGURED.to_string(),
            "host": host.to_string(),
            "mac": MAC,
            "identity": MAC,
        }})
    }

    #[tokio::test]
    async fn a_device_that_changed_address_is_found_and_followed() {
        let port = adapter(Ipv4Addr::LOCALHOST, 0, MAC).await;
        let house = House::new("moved", port, LOOPBACK, None);
        assert!(house.online(MAC).await, "found at its new address");
        assert_eq!(
            house.moves(),
            [("127.0.0.2".to_owned(), "127.0.0.1".to_owned())],
            "journaled once"
        );
        assert_eq!(house.remembered(), book(Ipv4Addr::LOCALHOST));
        let remembered = house.remembered();
        house.stop().await;

        // Next start: straight to where it was found, nothing to look for.
        let house = House::new("moved-again", port, NOBODY, Some(remembered));
        assert!(house.online(MAC).await, "reached at the address remembered");
        assert!(house.moves().is_empty());
        house.stop().await;
    }

    #[tokio::test]
    async fn another_device_with_its_mac_is_not_followed() {
        let port = adapter(Ipv4Addr::LOCALHOST, 0, OTHER).await;
        let house = House::new("impostor", port, LOOPBACK, None);
        assert!(!house.online(OTHER).await, "not the same identity");
        assert!(house.moves().is_empty(), "nothing journaled");
        assert!(
            !house.dir.join("addresses.json").exists(),
            "nothing remembered"
        );
        house.stop().await;
    }

    #[tokio::test]
    async fn another_device_at_the_address_found_earlier_is_never_taken() {
        let port = adapter(Ipv4Addr::LOCALHOST, 0, OTHER).await;
        let house = House::new("taken", port, NOBODY, Some(book(Ipv4Addr::LOCALHOST)));
        assert!(!house.online(OTHER).await, "not adopted");
        assert!(!house.online(MAC).await, "and the adapter is nowhere");
        assert_eq!(
            house.remembered(),
            book(Ipv4Addr::LOCALHOST),
            "nothing learned about the other one"
        );
        house.stop().await;
    }

    #[tokio::test]
    async fn the_device_whose_address_another_one_took_is_found_elsewhere() {
        // Another adapter got the configured address; ours is at .3.
        let port = adapter(ELSEWHERE, 0, MAC).await;
        adapter(CONFIGURED, port, OTHER).await;
        let house = House::new("twin", port, TWIN, Some(book(CONFIGURED)));
        assert!(house.online(MAC).await, "followed to its new address");
        assert!(
            house
                .hub
                .device(&DeviceId::from(format!("clim:{OTHER}")))
                .is_none()
        );
        assert_eq!(
            house.moves(),
            [("127.0.0.2".to_owned(), "127.0.0.3".to_owned())]
        );
        assert_eq!(house.remembered(), book(ELSEWHERE));
        house.stop().await;
    }

    #[tokio::test]
    async fn another_device_at_its_configured_address_is_not_taken_before_a_long_search() {
        // Ours is off (nowhere); another one got its address.
        let port = adapter(CONFIGURED, 0, OTHER).await;
        let house = House::new("off", port, NOBODY, Some(book(CONFIGURED)));
        assert!(!house.online(OTHER).await, "not taken at once");
        assert_eq!(house.remembered(), book(CONFIGURED));
        house.stop().await;
    }

    #[tokio::test]
    async fn an_address_a_person_changed_is_obeyed() {
        // Learned for another configured address: the person's word wins.
        let port = adapter(CONFIGURED, 0, OTHER).await;
        let mut before = book(Ipv4Addr::LOCALHOST);
        before["clim"]["configured"] = "127.0.0.9".into();
        let house = House::new("edited", port, LOOPBACK, Some(before));
        assert!(house.online(OTHER).await, "what answers there is taken");
        assert!(house.moves().is_empty(), "nothing was looked for");
        assert_eq!(house.remembered_as(OTHER).await["clim"]["identity"], OTHER);
        house.stop().await;
    }

    #[test]
    fn only_builtin_profiles_of_home_devices_are_followed() {
        let (profile, _) = open("open-meteo").unwrap();
        let web = Config {
            host: Some("10.0.0.30".into()),
            ..config("open-meteo")
        };
        assert!(!followable(&web, &profile, Lan::REAL), "its own host");
        let (daikin, _) = open("daikin-brp069").unwrap();
        let mut home = Config {
            host: Some("10.0.0.21".into()),
            ..config("daikin-brp069")
        };
        assert!(followable(&home, &daikin, Lan::REAL));
        let file = Profile::parse(builtin("daikin-brp069").unwrap())
            .unwrap()
            .compile(Source::File)
            .unwrap();
        assert!(
            !followable(&home, &file, Lan::REAL),
            "a file reaches only the host a person gave"
        );
        home.host = Some("8.8.8.8".into());
        assert!(
            !followable(&home, &daikin, Lan::REAL),
            "not the home network"
        );
        home.host = Some("clim.local".into());
        assert!(
            !followable(&home, &daikin, Lan::REAL),
            "a name, not an address"
        );
    }
}

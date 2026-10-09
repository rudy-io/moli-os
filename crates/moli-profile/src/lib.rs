//! Declarative device profiles.
//!
//! A profile (TOML) describes an HTTP device: which requests to make, how
//! often, in which format, and how each answer maps to typed points. One
//! generic driver runs any profile: adding a brand is adding a file, which
//! an agent can write and `moli-os profile check` validates before use. A
//! file runs only once a human approved it (`moli-os profile trust`), and it
//! changes a device only through its declared writes, which are commands.

mod profile;
mod trust;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use moli_core::{Device, DeviceId, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
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
/// A failed request is retried after at most this long.
const RETRY_SOON: Duration = Duration::from_secs(15);

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
        })
    }
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

/// One HTTP(S) exchange (`GET`, or `POST` of a JSON body), bounded in time
/// and size. `label` names the request in errors: the path *template*, so a
/// `{secret}` never ends up in a log.
async fn exchange(
    target: &Target,
    path: &str,
    headers: &[(String, String)],
    label: &str,
    json_body: Option<Vec<u8>>,
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
        let body = http_body_util::Limited::new(response.into_body(), MAX_BODY)
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
        Call::Get => exchange(target, &path, &headers, &request.path, None).await?,
        Call::Meross { namespace, payload } => {
            let key = secret.context("meross request without its key")?;
            let message = meross_message(namespace, payload, key);
            exchange(
                target,
                &path,
                &headers,
                &request.path,
                Some(message.to_string().into_bytes()),
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
        }
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
                self.due[i] = Instant::now() + request.every;
                publish(ctx, profile, device, &request.id, &answer);
                self.answers.insert(request.id.clone(), answer);
                Ok(())
            }
            Err(e) => {
                self.failures[i] += 1;
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
        let body = exchange(self.target, &path, &[], &write.path, None)
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
    let target = Target::of(&driver.config, profile)?;
    if let Some(file) = &driver.file
        && !approved(ctx, driver, file).await
    {
        return Ok(()); // waiting for a human, then shut down
    }
    let Some(secret) = required_secret(profile, ctx).await else {
        return Ok(()); // waiting for a human, then shut down
    };
    let mut session = Session::new(&target, profile, secret.as_deref());
    session.first_round().await?;
    let lookup = |reference: &Option<String>| {
        let reference = reference.as_deref()?;
        let (request, path) = reference.split_once('.')?;
        let path: Vec<String> = path.split('.').map(str::to_owned).collect();
        session.answers.get(request)?.text(&path)
    };
    // A declared identity is mandatory: falling back to the address would
    // split one device in two the day the identity comes back.
    let native = match &profile.meta.identity {
        Some(reference) => lookup(&profile.meta.identity)
            .with_context(|| format!("identity {reference} missing from the device's answer"))?,
        None => format!("{}:{}", target.host, target.port),
    };
    let id = ctx.device_id(&native);
    let name = lookup(&profile.meta.name_from).unwrap_or_else(|| profile.meta.name.clone());
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
    for (request, answer) in &session.answers {
        publish(ctx, profile, &id, request, answer);
    }
    ctx.set_availability(&id, true);
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), profile = %profile.meta.id, device = %id, "profile device ready");

    // Then each request on its own schedule, orders as they come.
    loop {
        let Some((next, at)) = session.next_due() else {
            bail!("profile without requests");
        };
        let command = tokio::select! {
            () = tokio::time::sleep_until(at) => None,
            command = ctx.next_command() => match command {
                Some(command) => Some(command),
                None => return Ok(()),
            },
        };
        match command {
            Some(command) => session.command(ctx, &id, command).await,
            None => {
                let _ = session.poll(ctx, &id, next).await;
            }
        }
    }
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
}

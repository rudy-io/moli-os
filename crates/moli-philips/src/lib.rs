//! Philips Android TVs, through the JointSpace API (HTTPS 1926, digest
//! authentication with the pairing credentials): power, volume, mute,
//! current app. Turning on from standby also sends a Wake-on-LAN packet.
//! Their Android side ([`android`]) opens apps and says what plays.

mod android;

use std::time::Duration;

use anyhow::{Context as _, bail};
use md5::{Digest as _, Md5};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::{Method, PIN_MISMATCH, PinnedHttps};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::sync::Mutex;

const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const CERT: &str = "cert";
const PORT: u16 = 1926;
const POLL: Duration = Duration::from_secs(5);
/// After « switch on » from deep standby: how long the packet is sent
/// again (each poll) while waiting for the TV to answer.
const WAKING: Duration = Duration::from_secs(45);

/// Remote keys the JointSpace API accepts (a press each, never repeated).
pub const KEYS: [&str; 24] = [
    "CursorUp",
    "CursorDown",
    "CursorLeft",
    "CursorRight",
    "Confirm",
    "Back",
    "Home",
    "Options",
    "Info",
    "PlayPause",
    "Play",
    "Pause",
    "Stop",
    "Next",
    "Previous",
    "FastForward",
    "Rewind",
    "VolumeUp",
    "VolumeDown",
    "Mute",
    "ChannelStepUp",
    "ChannelStepDown",
    "Source",
    "WatchTV",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    /// For Wake-on-LAN, `02:00:00:00:00:02`.
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default = "default_api")]
    pub api_version: u8,
}

fn default_api() -> u8 {
    6
}

#[derive(Debug)]
pub struct Philips {
    config: Config,
}

impl Philips {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Philips {
    fn kind(&self) -> &'static str {
        "philips"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn hex_md5(input: &str) -> String {
    use std::fmt::Write as _;
    Md5::digest(input.as_bytes())
        .iter()
        .fold(String::with_capacity(32), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// A digest challenge (`WWW-Authenticate: Digest …`).
#[derive(Clone, Debug, Default, PartialEq)]
struct Challenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    qop: bool,
}

fn parse_challenge(header: &str) -> Option<Challenge> {
    let params = header.trim().strip_prefix("Digest")?.trim();
    let mut challenge = Challenge::default();
    // key="value" or key=value, comma separated (values may hold commas
    // only inside quotes).
    let mut rest = params;
    while !rest.is_empty() {
        let (key, after) = rest.split_once('=')?;
        let key = key.trim().trim_start_matches(',').trim();
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"')?;
            (&quoted[..end], &quoted[end + 1..])
        } else {
            after.split_once(',').unwrap_or((after, ""))
        };
        match key {
            "realm" => value.clone_into(&mut challenge.realm),
            "nonce" => value.clone_into(&mut challenge.nonce),
            "opaque" => challenge.opaque = Some(value.to_owned()),
            "qop" => challenge.qop = value.split(',').any(|q| q.trim() == "auth"),
            _ => {}
        }
        rest = next.trim_start_matches(',').trim();
    }
    (!challenge.nonce.is_empty()).then_some(challenge)
}

fn authorization(
    challenge: &Challenge,
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    nc: u32,
    cnonce: &str,
) -> String {
    let ha1 = hex_md5(&format!("{user}:{}:{password}", challenge.realm));
    let ha2 = hex_md5(&format!("{method}:{uri}"));
    let mut header = if challenge.qop {
        let response = hex_md5(&format!(
            "{ha1}:{}:{nc:08x}:{cnonce}:auth:{ha2}",
            challenge.nonce
        ));
        format!(
            r#"Digest username="{user}", realm="{}", nonce="{}", uri="{uri}", qop=auth, nc={nc:08x}, cnonce="{cnonce}", response="{response}", algorithm=MD5"#,
            challenge.realm, challenge.nonce
        )
    } else {
        let response = hex_md5(&format!("{ha1}:{}:{ha2}", challenge.nonce));
        format!(
            r#"Digest username="{user}", realm="{}", nonce="{}", uri="{uri}", response="{response}", algorithm=MD5"#,
            challenge.realm, challenge.nonce
        )
    };
    if let Some(opaque) = &challenge.opaque {
        let _ = std::fmt::Write::write_fmt(&mut header, format_args!(r#", opaque="{opaque}""#));
    }
    header
}

struct Tv {
    http: PinnedHttps,
    api: u8,
    username: String,
    password: String,
    /// Last challenge and request counter.
    auth: Mutex<(Option<Challenge>, u32)>,
}

impl Tv {
    /// A request safe to repeat (reads, absolute settings).
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
    ) -> anyhow::Result<Json> {
        self.call(method, path, body, false).await
    }

    /// A key press: sent at most once (a replay would toggle back).
    async fn press(&self, key: &str) -> anyhow::Result<Json> {
        self.call(
            Method::POST,
            "input/key",
            Some(&json!({ "key": key })),
            true,
        )
        .await
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
        once: bool,
    ) -> anyhow::Result<Json> {
        let uri = format!("/{}/{path}", self.api);
        for attempt in 0..2 {
            let mut builder = self.http.request(method.clone(), &uri);
            {
                let mut auth = self.auth.lock().await;
                if let (Some(challenge), _) = &*auth {
                    let challenge = challenge.clone();
                    auth.1 += 1;
                    let cnonce = format!(
                        "{:016x}",
                        moli_core::now_ms() ^ u64::from(auth.1).rotate_left(32)
                    );
                    builder = builder.header(
                        "authorization",
                        authorization(
                            &challenge,
                            &self.username,
                            &self.password,
                            method.as_str(),
                            &uri,
                            auth.1,
                            &cnonce,
                        ),
                    );
                }
            }
            let request = match body {
                Some(body) => moli_net::json_body(builder, body)?,
                None => moli_net::empty(builder)?,
            };
            let (status, bytes, challenge) = if once {
                self.http.send_once_with_challenge(request).await?
            } else {
                self.http.send_with_challenge(request).await?
            };
            if status.as_u16() == 401 && attempt == 0 {
                let challenge = challenge
                    .as_deref()
                    .and_then(parse_challenge)
                    .context("TV refused without a digest challenge")?;
                *self.auth.lock().await = (Some(challenge), 0);
                continue;
            }
            if !status.is_success() {
                bail!(moli_i18n::tr!(
                    "pilotes.philips.tele_repond",
                    status = status
                ));
            }
            if bytes.is_empty() {
                return Ok(Json::Null);
            }
            return serde_json::from_slice(&bytes).context("TV answer");
        }
        bail!(moli_i18n::tr!("pilotes.philips.identifiants_refuses"))
    }
}

fn spec(key: &str, label: &str, kind: Kind, write: bool, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit: None,
        semantic,
    }
}

#[allow(clippy::too_many_lines)] // one entry per point
fn device(ctx: &DriverCtx, id: &DeviceId, name: &str, max_volume: f64) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some("Philips".into()),
        model: Some("Android TV".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "power",
                &moli_i18n::tr!("pilotes.philips.allumee"),
                Kind::Binary,
                true,
                Semantic::OnOff,
            ),
            spec(
                "volume",
                &moli_i18n::tr!("pilotes.philips.volume"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(max_volume),
                    step: Some(1.0),
                },
                true,
                Semantic::infer("volume", None),
            ),
            spec(
                "mute",
                &moli_i18n::tr!("pilotes.philips.sourdine"),
                Kind::Binary,
                true,
                Semantic::infer("mute", None),
            ),
            // Written: opens that app (Android package) or link.
            spec(
                "app",
                &moli_i18n::tr!("pilotes.philips.application"),
                Kind::Text,
                true,
                Semantic::Other,
            ),
            spec(
                "ambilight",
                &moli_i18n::tr!("pilotes.philips.ambilight"),
                Kind::Binary,
                true,
                Semantic::OnOff,
            ),
            PointSpec {
                key: "key".into(),
                label: moli_i18n::tr!("pilotes.philips.touche").into(),
                kind: Kind::Enum {
                    values: KEYS.iter().map(|k| (*k).into()).collect(),
                },
                // A press, not a state: nothing to read back.
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            spec(
                "media_app",
                &moli_i18n::tr!("pilotes.philips.appli_en_lecture"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            spec(
                "media_state",
                &moli_i18n::tr!("pilotes.philips.lecture"),
                enumeration(&["playing", "paused", "buffering", "idle"]),
                false,
                Semantic::Other,
            ),
            spec(
                "media_title",
                &moli_i18n::tr!("pilotes.philips.titre"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            spec(
                "media_subtitle",
                &moli_i18n::tr!("pilotes.philips.serie_artiste"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            spec(
                "media_image",
                &moli_i18n::tr!("pilotes.philips.image"),
                Kind::Text,
                false,
                Semantic::Other,
            ),
            seconds("media_duration", &moli_i18n::tr!("pilotes.philips.duree")),
            seconds(
                "media_position",
                &moli_i18n::tr!("pilotes.philips.position"),
            ),
            PointSpec {
                key: "media_control".into(),
                label: moli_i18n::tr!("pilotes.philips.lecture_commande").into(),
                kind: enumeration(&["play", "pause", "stop"]),
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
        ],
    }
}

fn enumeration(values: &[&str]) -> Kind {
    Kind::Enum {
        values: values.iter().map(|v| (*v).into()).collect(),
    }
}

fn seconds(key: &str, label: &str) -> PointSpec {
    PointSpec {
        unit: Some(Unit::Second),
        ..spec(
            key,
            label,
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: None,
            },
            false,
            Semantic::Other,
        )
    }
}

/// « Switch on » from deep standby: the TV's API is off with it, so the
/// Wake-on-LAN packet goes, the order is answered at once (the hub waits
/// 5 s, a TV takes longer), and the packet goes again at each poll until
/// the TV answers or [`WAKING`] has passed.
#[derive(Debug, Default)]
struct Waking(Option<tokio::time::Instant>);

impl Waking {
    async fn ask(&mut self, config: &Config, ctx: &DriverCtx) -> Result<(), String> {
        let Some(mac) = &config.mac else {
            return Err(moli_i18n::tr!("pilotes.philips.sans_mac"));
        };
        wake(mac, &config.host)
            .await
            .map_err(|e| moli_i18n::tr!("pilotes.philips.reveil", error = format!("{e:#}")))?;
        tracing::info!(instance = %ctx.instance(), "tv asleep: wake-on-lan sent, waiting for it");
        self.0 = Some(tokio::time::Instant::now() + WAKING);
        Ok(())
    }

    fn active(&self) -> bool {
        self.0.is_some()
    }

    async fn tick(&mut self, config: &Config, ctx: &DriverCtx) {
        let Some(until) = self.0 else { return };
        if tokio::time::Instant::now() >= until {
            self.0 = None;
            tracing::warn!(instance = %ctx.instance(), "tv did not wake: is « wake on LAN / WoWLAN » enabled on it?");
        } else if let Some(mac) = &config.mac {
            let _ = wake(mac, &config.host).await;
        }
    }

    /// The TV answers again: switch the screen on if it is still off.
    async fn woke(&mut self, tv: &Tv, values: &[(&str, Value)], ctx: &DriverCtx) {
        if self.0.take().is_none() {
            return;
        }
        tracing::info!(instance = %ctx.instance(), "tv woke up");
        let on = values
            .iter()
            .any(|(k, v)| *k == "power" && *v == Value::Bool(true));
        if !on {
            let _ = tv
                .request(
                    Method::POST,
                    "powerstate",
                    Some(&json!({"powerstate": "On"})),
                )
                .await;
        }
    }
}

/// Wake-on-LAN: six 0xFF then the MAC sixteen times, broadcast on the
/// whole network and on the TV's own (/24): a host with several interfaces
/// (Docker bridges) may not route the first one to the TV.
async fn wake(mac: &str, host: &str) -> anyhow::Result<()> {
    let bytes: Vec<u8> = mac
        .split([':', '-'])
        .map(|b| u8::from_str_radix(b, 16))
        .collect::<Result<_, _>>()
        .context("MAC address")?;
    if bytes.len() != 6 {
        bail!("MAC address must have 6 bytes");
    }
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;
    let mut targets = vec![std::net::Ipv4Addr::BROADCAST];
    if let Ok(std::net::IpAddr::V4(ip)) = host.parse::<std::net::IpAddr>() {
        let [a, b, c, _] = ip.octets();
        targets.push(std::net::Ipv4Addr::new(a, b, c, 255));
    }
    // A few times, on the two usual ports: a sleeping Wi-Fi card can miss
    // one packet.
    for round in 0..3 {
        if round > 0 {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        for target in &targets {
            for port in [9, 7] {
                socket.send_to(&packet, (*target, port)).await?;
            }
        }
    }
    Ok(())
}

/// One order. « Switch on » with the TV unreachable wakes it; an app asked
/// for with the TV asleep wakes it too (kept until the remote is there), and
/// an app is answered when the TV takes it or refuses it.
async fn order(
    command: CommandRequest,
    tv: &Tv,
    android: &mut android::Android,
    waking: &mut Waking,
    online: Option<bool>,
    config: &Config,
    ctx: &DriverCtx,
) {
    if matches!(
        (&*command.key, &command.value),
        ("power", Value::Bool(true))
    ) && online != Some(true)
    {
        command.reply(waking.ask(config, ctx).await);
        return;
    }
    // Only a real app with a paired remote: never the TV on for nothing.
    let real_app = matches!(&command.value, Value::Text(t) if !t.trim().is_empty());
    if &*command.key == "app"
        && real_app
        && android.can_launch()
        && online != Some(true)
        && !waking.active()
        && let Err(e) = waking.ask(config, ctx).await
    {
        command.reply(Err(e));
        return;
    }
    if let ("app", Value::Text(target)) = (&*command.key, &command.value) {
        let target = target.to_string();
        android.launch(&target, android::Waiting::order(command));
        return;
    }
    let result = apply(tv, android, &command)
        .await
        .map_err(|e| format!("{e:#}"));
    command.reply(result);
}

/// Until `at`; forever without one (a `select!` branch that never fires).
async fn until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn apply(
    tv: &Tv,
    android: &mut android::Android,
    command: &CommandRequest,
) -> anyhow::Result<()> {
    match (&*command.key, &command.value) {
        ("media_control", Value::Text(word)) => android.control(word)?,
        ("power", Value::Bool(true)) => {
            // Awake enough to answer (network standby): its API switches
            // the screen on. Deep standby is the run loop's (Wake-on-LAN).
            tv.request(
                Method::POST,
                "powerstate",
                Some(&json!({"powerstate": "On"})),
            )
            .await?;
        }
        ("power", Value::Bool(false)) => {
            // The Standby key toggles: only press it when the TV is on.
            let state = tv.request(Method::GET, "powerstate", None).await?;
            if state["powerstate"].as_str() == Some("On") {
                let absolute = tv
                    .request(
                        Method::POST,
                        "powerstate",
                        Some(&json!({"powerstate": "Standby"})),
                    )
                    .await;
                if absolute.is_err() {
                    tv.press("Standby").await?;
                }
            }
        }
        ("volume", v) => {
            #[allow(clippy::cast_possible_truncation)]
            let level = v.as_f64().context("volume")?.round() as i64;
            tv.request(
                Method::POST,
                "audio/volume",
                Some(&json!({"muted": false, "current": level})),
            )
            .await?;
        }
        ("key", Value::Text(key)) if KEYS.contains(&&**key) => {
            tv.press(key).await?;
        }
        ("ambilight", Value::Bool(on)) => {
            tv.request(
                Method::POST,
                "ambilight/power",
                Some(&json!({"power": if *on { "On" } else { "Off" }})),
            )
            .await?;
        }
        ("mute", Value::Bool(muted)) => {
            let current = tv.request(Method::GET, "audio/volume", None).await?["current"]
                .as_i64()
                .unwrap_or(0);
            tv.request(
                Method::POST,
                "audio/volume",
                Some(&json!({"muted": muted, "current": current})),
            )
            .await?;
        }
        (key, _) => bail!(moli_i18n::tr!(
            "pilotes.philips.ne_se_regle_pas",
            point = key
        )),
    }
    Ok(())
}

async fn poll(tv: &Tv) -> anyhow::Result<(Vec<(&'static str, Value)>, f64)> {
    let power = tv.request(Method::GET, "powerstate", None).await?;
    let on = power["powerstate"].as_str() == Some("On");
    let mut values = vec![("power", Value::Bool(on))];
    let mut max = 60.0;
    if on {
        let volume = tv.request(Method::GET, "audio/volume", None).await?;
        max = volume["max"].as_f64().unwrap_or(max);
        if let Some(v) = volume["current"].as_i64() {
            values.push(("volume", Value::Int(v)));
        }
        values.push((
            "mute",
            Value::Bool(volume["muted"].as_bool().unwrap_or(false)),
        ));
        if let Ok(ambilight) = tv.request(Method::GET, "ambilight/power", None).await
            && let Some(power) = ambilight["power"].as_str()
        {
            values.push(("ambilight", Value::Bool(power == "On")));
        }
        let current = tv.request(Method::GET, "activities/current", None).await?;
        let app = current["component"]["packageName"]
            .as_str()
            .filter(|p| *p != "NA");
        values.push(("app", app.map_or(Value::Null, |a| Value::Text(a.into()))));
    }
    Ok((values, max))
}

async fn first_pin(config: &Config, ctx: &DriverCtx) -> anyhow::Result<String> {
    if !ctx.can_store_secrets() {
        bail!("cannot pin the TV's certificate: the secret store is unavailable");
    }
    let probe = PinnedHttps::new(&config.host, PORT, None)?;
    probe.handshake().await.context("first contact")?;
    let seen = probe.seen_fingerprint().context("no certificate seen")?;
    ctx.store_secret(CERT, &seen).await?;
    Ok(seen)
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (Some(username), Some(password)) = (ctx.secret(USERNAME), ctx.secret(PASSWORD)) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.philips.identifiants_manquants",
            instance = ctx.instance(),
            username = USERNAME,
            password = PASSWORD
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let pin = match ctx.secret(CERT) {
        Some(pin) => pin,
        None => first_pin(config, ctx).await?,
    };
    let tv = Tv {
        http: PinnedHttps::new(&config.host, PORT, Some(pin))?,
        api: config.api_version,
        username,
        password,
        auth: Mutex::new((None, 0)),
    };
    let id = ctx.device_id(&config.host);
    let system = tv.request(Method::GET, "system", None).await.ok();
    let name = system
        .as_ref()
        .and_then(|s| s["name"].as_str())
        .map_or_else(|| moli_i18n::tr!("pilotes.philips.tele"), str::to_owned);
    let mut max = 60.0;
    ctx.upsert_device(device(ctx, &id, &name, max));
    let (mut android, mut ups) = android::Android::start(ctx, &config.host);
    ctx.ready();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut online = None;
    // « Switch on » from deep standby: until when the packet is sent again.
    let mut waking = Waking::default();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                order(command, &tv, &mut android, &mut waking, online, config, ctx).await;
            }
            Some(up) = ups.recv() => android.on(up, ctx, &id).await,
            () = until(android.due()) => android.settle(),
            _ = tick.tick() => {
                waking.tick(config, ctx).await;
                let polled = poll(&tv).await;
                android.tick(polled.is_ok());
                match polled {
                Ok((values, seen_max)) => {
                    waking.woke(&tv, &values, ctx).await;
                    if (seen_max - max).abs() > f64::EPSILON {
                        max = seen_max;
                        ctx.upsert_device(device(ctx, &id, &name, max));
                    }
                    if online != Some(true) {
                        online = Some(true);
                        ctx.set_availability(&id, true);
                    }
                    for (key, value) in values {
                        // On a Google TV, JointSpace never knows the app.
                        if key == "app" && android.knows_the_app() {
                            continue;
                        }
                        ctx.set_state(&id, key, value);
                    }
                }
                Err(e) => {
                    if format!("{e:#}").contains(PIN_MISMATCH) {
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.philips.certificat_change",
                            instance = ctx.instance(),
                            key = CERT
                        ));
                        ctx.cancelled().await;
                        return Ok(());
                    }
                    if online != Some(false) {
                        online = Some(false);
                        // Deep standby: the API sleeps with the TV.
                        ctx.set_availability(&id, false);
                        ctx.set_state(&id, "power", Value::Bool(false));
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "tv unreachable");
                    }
                }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_rfc_2617_example() {
        // RFC 2617 §3.5.
        let challenge = parse_challenge(
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
        )
        .unwrap();
        assert!(challenge.qop);
        let header = authorization(
            &challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            1,
            "0a4f113b",
        );
        assert!(
            header.contains(r#"response="6629fae49393a05397450978507c4ef1""#),
            "{header}"
        );
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
    }
}

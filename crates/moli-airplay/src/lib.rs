//! The `airplay` kind: spoken announcements on an AirPlay 1 receiver that
//! takes plain PCM with no password nor pairing ([`raop::plain`]), a
//! Freebox Delta Player for one. Announcements only: no music. The queue,
//! the silence, the level and the speech are `moli-announce`'s; the
//! protocol is `moli_net::raop`.
//!
//! At start, the receiver's TXT is asked of it: one that says it will not
//! take plain sound stops the driver, with what to do.
//!
//! Every RTSP session cuts what the receiver plays, reading the volume too:
//! the `volume` point is read when the driver starts and in each
//! announcement's own session (the level put back is the one the receiver
//! says), and followed from what Moli writes in between. A change made
//! elsewhere (a phone, the remote) is seen at the next announcement. Whether
//! the receiver is there is asked by a bare TCP connection, which cuts
//! nothing.

mod sound;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use moli_announce::{Announcements, Announcer, Speaker, Speech};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::raop;
use moli_runtime::{BoxFuture, Driver, DriverCtx, MediaPublisher};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

pub use moli_announce::QUIET_FOR;

/// Whether the receiver is there, asked this often (a TCP connection only).
const REACH_EVERY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The Wyoming speech server (Piper), `host:port`.
    #[serde(default = "moli_announce::default_tts")]
    pub tts: String,
    /// Piper voice (default: the server's).
    #[serde(default)]
    pub voice: Option<String>,
    /// Where `announce_volume` starts, 0–100; once written, the point is kept.
    #[serde(default = "default_volume")]
    pub volume: u8,
}

fn default_port() -> u16 {
    raop::PORT
}

fn default_volume() -> u8 {
    40
}

impl Config {
    fn speech(&self) -> Speech {
        Speech {
            tts: self.tts.clone(),
            voice: self.voice.clone(),
        }
    }
}

#[derive(Debug)]
pub struct Airplay {
    config: Config,
    /// Where the receiver's TXT is asked ([`raop::MDNS_PORT`]).
    mdns_port: u16,
}

impl Airplay {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            mdns_port: raop::MDNS_PORT,
        }
    }
}

impl Driver for Airplay {
    fn kind(&self) -> &'static str {
        "airplay"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, self.mdns_port, ctx))
    }
}

/// The receiver as `moli-announce` sees it, and the volume Moli follows.
#[derive(Clone, Debug)]
struct Receiver {
    host: String,
    port: u16,
    /// One RTSP session at a time: a volume set waits for the announcement.
    line: Arc<tokio::sync::Mutex<()>>,
    /// The receiver's own level, 0–100, as last read or written; unknown
    /// until read.
    level: Arc<Mutex<Option<u8>>>,
    /// The last level written and not sent yet: a burst of writes sends
    /// only the last.
    wanted: Arc<Mutex<Option<u8>>>,
    /// A task is sending `wanted`.
    sending: Arc<AtomicBool>,
    /// Cancelled when the driver stops: nothing it started outlives it.
    stop: CancellationToken,
}

impl Receiver {
    fn level(&self) -> Option<u8> {
        *self.level.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn keep(&self, level: u8) {
        *self.level.lock().unwrap_or_else(PoisonError::into_inner) = Some(level);
    }

    /// The receiver's level now (what it plays is cut: once, at start).
    async fn read(&self) -> anyhow::Result<Option<u8>> {
        let _line = self.line.lock().await;
        let level = raop::read_volume(&self.host, self.port)
            .await
            .map_err(|e| explained(e, &self.host))?
            .map(|db| percent(raop::percent_of_db(db)));
        if let Some(level) = level {
            self.keep(level);
        }
        Ok(level)
    }

    /// Sets the receiver's level once any announcement is over, and answers
    /// at once: the last level written wins, the ones before it are never
    /// sent. A failure is told to the house.
    fn set(&self, level: u8, publisher: MediaPublisher) {
        self.keep(level);
        *self.wanted.lock().unwrap_or_else(PoisonError::into_inner) = Some(level);
        if self.sending.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = this.stop.cancelled() => {}
                () = this.send_wanted(&publisher) => {}
            }
        });
    }

    /// Sends the level wanted, then the one written meanwhile, until none.
    async fn send_wanted(&self, publisher: &MediaPublisher) {
        loop {
            // Taken once the line is ours: writes made while an
            // announcement holds it collapse into the last one.
            let line = self.line.lock().await;
            let next = self
                .wanted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let Some(level) = next else {
                drop(line);
                self.sending.store(false, Ordering::SeqCst);
                // Written between the take and now: this task sends it.
                if self
                    .wanted
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some()
                    && !self.sending.swap(true, Ordering::SeqCst)
                {
                    continue;
                }
                return;
            };
            let set = raop::set_volume(&self.host, self.port, db(level))
                .await
                .map_err(|e| explained(e, &self.host));
            drop(line);
            if let Err(e) = set {
                tracing::warn!(host = %self.host, error = %format!("{e:#}"), "airplay volume not set");
                publisher.tell(
                    &moli_i18n::tr!("pilotes.annonces.annonce_vocale"),
                    moli_i18n::tr!("pilotes.airplay.volume_non_regle", error = format!("{e:#}")),
                );
            }
        }
    }
}

impl Speaker for Receiver {
    fn play(&self, wav: Vec<u8>, level: u8) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            let pcm = sound::from_wav(&wav)?;
            let _line = self.line.lock().await;
            let put_back = raop::say(&self.host, self.port, &pcm, db(level), self.level().map(db))
                .await
                .map_err(|e| explained(e, &self.host))?;
            // What the receiver said in the session is its level now,
            // unless one was written meanwhile (it is sent next).
            if let Some(read) = put_back
                && self
                    .wanted
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_none()
            {
                self.keep(percent(raop::percent_of_db(read)));
            }
            Ok(())
        })
    }
}

async fn run(config: &Config, mdns_port: u16, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let host = config.host.as_str();
    let id = ctx.device_id(host);
    let stop = CancellationToken::new();
    // Stopped or failed, this run cancels what it started.
    let _stop = stop.clone().drop_guard();
    let receiver = Receiver {
        host: host.to_owned(),
        port: config.port,
        line: Arc::default(),
        level: Arc::default(),
        wanted: Arc::default(),
        sending: Arc::default(),
        stop,
    };
    let mut announcements = Announcements::new(Some(Announcer::new(
        receiver.clone(),
        config.speech(),
        config.volume,
        ctx.media_publisher(),
    )));
    ctx.upsert_device(device(ctx, &id, host, &announcements));
    announcements.start(ctx, &id);
    // What the receiver says of itself: one that will not take plain
    // sound is refused now, before any session.
    match raop::announced(host, mdns_port, config.port).await {
        Ok(Some(txt)) => {
            if let Err(why) = raop::plain(&txt) {
                return Err(anyhow::Error::new(why)
                    .context(moli_i18n::tr!("pilotes.airplay.refuse", host = host)));
            }
        }
        Ok(None) => tracing::info!(
            host,
            "airplay receiver did not give its TXT: its answers will tell"
        ),
        Err(e) => tracing::info!(host, error = %format!("{e:#}"), "airplay TXT not asked"),
    }
    let mut online = None;
    // Read once: each reading cuts what the receiver plays.
    match receiver.read().await {
        Ok(level) => {
            if level.is_none() {
                tracing::info!(host, "airplay volume unreadable");
            }
            online = Some(true);
            ctx.set_availability(&id, true);
        }
        // It will refuse every announcement too: say so now, and why.
        Err(e) if e.downcast_ref::<raop::Refused>().is_some() => return Err(e),
        Err(e) => {
            tracing::info!(host, error = %format!("{e:#}"), "airplay receiver not reached at start");
        }
    }
    let mut shown = None;
    show_level(ctx, &id, &receiver, &mut shown);
    ctx.ready();
    let mut tick = tokio::time::interval(REACH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if let Some(result) = announcements.command(ctx, &id, &command) {
                    command.reply(result);
                    continue;
                }
                let result = match &*command.key {
                    "volume" => moli_announce::level_of(&command.value)
                        .map(|level| receiver.set(level, ctx.media_publisher()))
                        .ok_or_else(|| moli_i18n::tr!("pilotes.airplay.volume_nombre")),
                    key => Err(moli_i18n::tr!("pilotes.airplay.ne_se_regle_pas", point = key)),
                };
                show_level(ctx, &id, &receiver, &mut shown);
                command.reply(result);
            }
            _ = tick.tick() => {
                announcements.tick(ctx, &id);
                let up = reachable(host, config.port).await;
                if online != Some(up) {
                    online = Some(up);
                    ctx.set_availability(&id, up);
                }
                show_level(ctx, &id, &receiver, &mut shown);
            }
        }
    }
}

/// The level Moli follows, on the `volume` point when it changed (it may
/// have been learnt during an announcement).
fn show_level(ctx: &DriverCtx, id: &DeviceId, receiver: &Receiver, shown: &mut Option<u8>) {
    let level = receiver.level();
    if level.is_some() && level != *shown {
        *shown = level;
        ctx.set_state(id, "volume", Value::Int(level.unwrap_or_default().into()));
    }
}

/// A refusal, in words that say what to do (the receiver's answer follows).
fn explained(e: anyhow::Error, host: &str) -> anyhow::Error {
    if e.downcast_ref::<raop::Refused>().is_some() {
        e.context(moli_i18n::tr!("pilotes.airplay.refuse", host = host))
    } else {
        e
    }
}

/// Whether the receiver listens: a TCP connection opened then dropped,
/// which cuts nothing it plays (an RTSP session would).
async fn reachable(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect((host, port))
        )
        .await,
        Ok(Ok(_))
    )
}

fn db(level: u8) -> f64 {
    raop::db_of_percent(f64::from(level))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent(v: f64) -> u8 {
    v.round().clamp(0.0, 100.0) as u8
}

fn device(ctx: &DriverCtx, id: &DeviceId, host: &str, announcements: &Announcements) -> Device {
    let mut points = vec![PointSpec {
        key: "volume".into(),
        // Read at start and at each announcement, followed in between: the
        // label says so, for a human and for an agent.
        label: moli_i18n::tr!("pilotes.airplay.volume_suivi").into(),
        kind: Kind::Numeric {
            min: Some(0.0),
            max: Some(100.0),
            step: Some(1.0),
        },
        access: Access {
            read: true,
            write: true,
        },
        unit: Some(Unit::Percent),
        semantic: Semantic::infer("volume", None),
    }];
    points.extend(announcements.points());
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: format!("AirPlay {host}").into(),
        manufacturer: None,
        model: Some("AirPlay 1 (RAOP)".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_core::{DriverStatus, InstanceId, Origin, PointId};
    use moli_runtime::{Hub, HubOptions, spawn_driver};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn a_receiver_needs_only_its_address() {
        let config: Config =
            serde_json::from_value(serde_json::json!({ "host": "10.0.0.42" })).unwrap();
        assert_eq!(
            (config.port, config.tts.as_str(), config.volume),
            (5000, "127.0.0.1:10200", 40)
        );
        assert!(
            serde_json::from_value::<Config>(serde_json::json!({ "host": "h", "media_base": "x" }))
                .is_err()
        );
    }

    const DELTA: &[&str] = &[
        "et=0,1,3,5",
        "cn=0,1,2,3",
        "pw=false",
        "sf=0x44",
        "ft=0x12BFFEBB",
    ];
    const HOMEPOD: &[&str] = &[
        "et=0,3,5",
        "cn=0,1,2,3",
        "sf=0xa0c04",
        "ft=0x4A7FCA00,0x3C356BD0",
    ];

    /// A receiver on loopback.
    struct Fake {
        /// RTSP.
        port: u16,
        /// Where it answers its TXT.
        mdns: u16,
        /// The volumes it was given.
        set: Arc<Mutex<Vec<String>>>,
        /// The RTSP connections it took.
        sessions: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// Opens every session (or refuses `ANNOUNCE`, as one that wants a
    /// password), says its volume is -6 dB, takes `slow` to set one, and
    /// gives `txt` when asked.
    async fn receiver(refuse: bool, txt: &'static [&'static str], slow: Duration) -> Fake {
        let rtsp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mdns = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let fake = Fake {
            port: rtsp.local_addr().unwrap().port(),
            mdns: mdns.local_addr().unwrap().port(),
            set: Arc::default(),
            sessions: Arc::default(),
        };
        let rtsp_port = fake.port;
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((_, from)) = mdns.recv_from(&mut buf).await {
                let mut a = vec![0, 0, 0x84, 0, 0, 0, 0, 2, 0, 0, 0, 0];
                for label in ["Salon", "_raop", "_tcp", "local"] {
                    a.push(u8::try_from(label.len()).unwrap());
                    a.extend_from_slice(label.as_bytes());
                }
                a.extend_from_slice(&[0, 0, 16, 0x80, 1, 0, 0, 0x11, 0x94]);
                let data: Vec<u8> = txt
                    .iter()
                    .flat_map(|t| {
                        [vec![u8::try_from(t.len()).unwrap()], t.as_bytes().to_vec()].concat()
                    })
                    .collect();
                a.extend_from_slice(&u16::try_from(data.len()).unwrap().to_be_bytes());
                a.extend_from_slice(&data);
                // Its SRV, under a pointer to the same name: the RTSP port.
                a.extend_from_slice(&[0xC0, 12, 0, 33, 0x80, 1, 0, 0, 0x11, 0x94, 0, 7]);
                a.extend_from_slice(&[0, 0, 0, 0]);
                a.extend_from_slice(&rtsp_port.to_be_bytes());
                a.push(0);
                let _ = mdns.send_to(&a, from).await;
            }
        });
        let (log, sessions) = (Arc::clone(&fake.set), Arc::clone(&fake.sessions));
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = rtsp.accept().await {
                sessions.fetch_add(1, Ordering::SeqCst);
                let log = Arc::clone(&log);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut got = Vec::new();
                    loop {
                        let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") else {
                            let Ok(n @ 1..) = tcp.read(&mut buf).await else {
                                return;
                            };
                            got.extend_from_slice(&buf[..n]);
                            continue;
                        };
                        let head = String::from_utf8_lossy(&got[..end]).into_owned();
                        let length: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .map_or(0, |v| v.parse().unwrap());
                        while got.len() < end + 4 + length {
                            let Ok(n @ 1..) = tcp.read(&mut buf).await else {
                                return;
                            };
                            got.extend_from_slice(&buf[..n]);
                        }
                        let body = String::from_utf8_lossy(&got[end + 4..end + 4 + length])
                            .trim()
                            .to_owned();
                        got.drain(..end + 4 + length);
                        let method = head.split_whitespace().next().unwrap().to_owned();
                        let cseq = head
                            .lines()
                            .find_map(|l| l.strip_prefix("CSeq: "))
                            .unwrap()
                            .to_owned();
                        let answer = match method.as_str() {
                            "ANNOUNCE" if refuse => "RTSP/1.0 403 Forbidden\r\n".to_owned(),
                            "SETUP" => "RTSP/1.0 200 OK\r\nSession: 1\r\nTransport: \
                                        RTP/AVP/UDP;server_port=9;control_port=9\r\n"
                                .to_owned(),
                            "GET_PARAMETER" => {
                                "RTSP/1.0 200 OK\r\nContent-Length: 19\r\n".to_owned()
                            }
                            _ => "RTSP/1.0 200 OK\r\n".to_owned(),
                        };
                        if method == "SET_PARAMETER" {
                            tokio::time::sleep(slow).await;
                            log.lock().unwrap().push(body);
                        }
                        let tail = if method == "GET_PARAMETER" {
                            "volume: -6.000000\r\n"
                        } else {
                            ""
                        };
                        let answer = format!("{answer}CSeq: {cseq}\r\n\r\n{tail}");
                        if tcp.write_all(answer.as_bytes()).await.is_err() || method == "TEARDOWN" {
                            return;
                        }
                    }
                });
            }
        });
        fake
    }

    fn start(hub: &Hub, fake: &Fake) -> CancellationToken {
        let cancel = CancellationToken::new();
        let config = Config {
            host: "127.0.0.1".into(),
            port: fake.port,
            tts: moli_announce::default_tts(),
            voice: None,
            volume: 40,
        };
        let mut driver = Airplay::new(config);
        driver.mdns_port = fake.mdns;
        spawn_driver(
            hub,
            InstanceId::from("salon"),
            Arc::new(driver),
            cancel.clone(),
        );
        cancel
    }

    async fn settled(hub: &Hub, point: &PointId, want: &Value) {
        for _ in 0..300 {
            if hub.state(point).is_some_and(|s| &s.value == want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{point} never became {want:?}: {:?}", hub.state(point));
    }

    /// Why the driver stopped.
    async fn stopped(hub: &Hub) -> String {
        for _ in 0..300 {
            if let Some(DriverStatus::Backoff { error, .. }) = hub
                .snapshot()
                .drivers
                .into_iter()
                .find(|d| d.instance == InstanceId::from("salon"))
                .map(|d| d.status)
            {
                return error;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the driver never stopped");
    }

    /// The receiver's volume read at start, a level written sent to it,
    /// and the announcements' points next to it.
    #[tokio::test]
    async fn the_volume_is_read_at_start_then_followed_from_what_moli_writes() {
        let fake = receiver(false, DELTA, Duration::ZERO).await;
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = start(&hub, &fake);
        let volume = PointId::from("salon:127.0.0.1/volume".to_owned());
        let level = PointId::from("salon:127.0.0.1/announce_volume".to_owned());
        settled(&hub, &volume, &Value::Int(80)).await;
        settled(&hub, &level, &Value::Int(40)).await;
        hub.command(&volume, Value::Int(50), Origin::Ui, None)
            .await
            .unwrap();
        settled(&hub, &volume, &Value::Int(50)).await;
        for _ in 0..300 {
            if !fake.set.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*fake.set.lock().unwrap(), ["volume: -15.000000"]);
        cancel.cancel();
    }

    /// Volumes written faster than the receiver takes them: the first one
    /// goes, then only the last, never those in between.
    #[tokio::test]
    async fn a_burst_of_volumes_sends_only_the_last_one_waiting() {
        let fake = receiver(false, DELTA, Duration::from_millis(300)).await;
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = start(&hub, &fake);
        let volume = PointId::from("salon:127.0.0.1/volume".to_owned());
        settled(&hub, &volume, &Value::Int(80)).await;
        for v in [50, 51, 52] {
            hub.command(&volume, Value::Int(v), Origin::Ui, None)
                .await
                .unwrap();
        }
        settled(&hub, &volume, &Value::Int(52)).await;
        for _ in 0..300 {
            if fake.set.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            *fake.set.lock().unwrap(),
            ["volume: -15.000000", "volume: -14.400000"]
        );
        cancel.cancel();
    }

    /// A receiver that wants a password (or encryption, or pairing) stops
    /// the driver at start, with what to do in its words.
    #[tokio::test]
    async fn a_receiver_that_refuses_plain_sound_is_refused_at_start() {
        let fake = receiver(true, DELTA, Duration::ZERO).await;
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = start(&hub, &fake);
        let error = stopped(&hub).await;
        assert!(error.contains("127.0.0.1"), "{error}");
        assert!(error.contains("403 Forbidden"), "{error}");
        assert!(fake.set.lock().unwrap().is_empty());
        cancel.cancel();
    }

    /// An AirPlay 2 receiver (a HomePod's TXT, as theomarie measured it in
    /// #68) is refused from what it says of itself, before any session.
    #[tokio::test]
    async fn an_airplay_2_receiver_is_refused_from_its_txt_before_any_session() {
        let fake = receiver(false, HOMEPOD, Duration::ZERO).await;
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = start(&hub, &fake);
        let error = stopped(&hub).await;
        assert!(error.contains("127.0.0.1"), "{error}");
        assert!(error.contains("AirPlay 2"), "{error}");
        assert_eq!(fake.sessions.load(Ordering::SeqCst), 0, "no session opened");
        cancel.cancel();
    }
}

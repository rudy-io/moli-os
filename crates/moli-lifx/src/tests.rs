//! The orders a model takes, and the driver entire against fake bulbs on
//! 127.0.0.1 (one port each, so it runs on macOS too): what reaches the
//! hub, and what reaches the bulbs. Serials are made up (02:00:00:00:00:0x).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moli_core::{InstanceId, Origin, PointId, Value};
use moli_runtime::{Hub, HubOptions, spawn_driver};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use super::Lifx;
use crate::bulb::{Change, change, merge};
use crate::proto::{self, Hsbk, Parts, Reply, Serial};

fn config(toml: &str) -> anyhow::Result<Lifx> {
    Lifx::new(toml::from_str(toml)?)
}

#[test]
fn options_are_checked() {
    let lifx = config(r#"hosts = ["192.168.77.10"]"#).unwrap();
    assert_eq!(
        lifx.hosts,
        [SocketAddrV4::new(
            Ipv4Addr::new(192, 168, 77, 10),
            proto::PORT
        )]
    );
    assert!(lifx.broadcast.is_some());
    assert_eq!(lifx.poll, Duration::from_secs(5));
    assert!(config("discovery = false").is_err());
    assert!(config(r#"hosts = ["lampe.local"]"#).is_err());
    assert!(config("poll_secs = 0").is_err());
    assert!(config("transition_ms = 60000").is_ok());
    assert!(config("transition_ms = 60001").is_err());
    assert!(config("colour = true").is_err());
}

const WARM: Hsbk = Hsbk {
    hue: 0,
    saturation: 0,
    brightness: 65_535,
    kelvin: 2700,
};

#[test]
fn a_fixed_white_takes_power_and_brightness_only() {
    let white = (false, None);
    let Ok(Change::Color(parts, wanted)) = change(white, "brightness", &Value::Int(40)) else {
        panic!("brightness refused");
    };
    assert_eq!(
        parts,
        Parts {
            brightness: true,
            ..Parts::default()
        }
    );
    let after = merge(WARM, parts, wanted);
    assert_eq!(after.brightness, proto::level(40.0));
    assert_eq!(after.kelvin, 2700);
    assert_eq!(
        change(white, "on", &Value::Bool(false)),
        Ok(Change::Power(false))
    );
    assert!(change(white, "color_temp", &Value::Int(4000)).is_err());
    assert!(change(white, "color", &Value::from("#ff0000")).is_err());
    assert!(change(white, "rssi", &Value::Int(-50)).is_err());
}

#[test]
fn a_colour_bulb_takes_temperature_and_colour() {
    let colour = (true, Some((1500, 9000)));
    let Ok(Change::Color(parts, wanted)) = change(colour, "color_temp", &Value::Int(12_000)) else {
        panic!("temperature refused");
    };
    assert!(parts.kelvin && parts.saturation && !parts.hue && !parts.brightness);
    assert_eq!(merge(WARM, parts, wanted).kelvin, 9000);
    let Ok(Change::Color(parts, wanted)) = change(colour, "color", &Value::from("#0000ff")) else {
        panic!("colour refused");
    };
    let after = merge(WARM, parts, wanted);
    assert_eq!(proto::hex(after), "#0000ff");
    assert_eq!(after.brightness, 65_535);
    assert!(change(colour, "color", &Value::from("bleu")).is_err());
}

// ---- fake bulbs ----------------------------------------------------------------------

const ONE: Serial = [0x02, 0, 0, 0, 0, 0x01];
const TWO: Serial = [0x02, 0, 0, 0, 0, 0x02];

/// How a fake bulb behaves.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // one switch per behaviour tested
struct Fake {
    pid: u32,
    name: &'static str,
    on: bool,
    /// Knows `LightSetWaveformOptional`.
    waveform: bool,
    /// `GetVersion`s left unanswered first.
    version_silences: u32,
    /// `GetVersion`s answered, then no more (`None`: all).
    version_answers: Option<u32>,
    /// `GetHostFirmware`s left unanswered first.
    firmware_silences: u32,
    /// `GetGroup`s left unanswered first.
    group_silences: u32,
    /// Every answer leaves from another socket (another port).
    elsewhere: bool,
    /// Only the acknowledgements do.
    acks_elsewhere: bool,
    /// More serials announced in its `StateService`s.
    flood: Vec<Serial>,
    /// Answers as whatever serial it is asked as.
    any_serial: bool,
    /// Real answers (by the type asked) instead of made-up ones.
    fixture: Option<&'static str>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            pid: 51, // LIFX Mini W
            name: "Lampe",
            on: true,
            waveform: true,
            version_silences: 0,
            version_answers: None,
            firmware_silences: 0,
            group_silences: 0,
            elsewhere: false,
            acks_elsewhere: false,
            flood: Vec::new(),
            any_serial: false,
            fixture: None,
        }
    }
}

/// A running fake bulb: what it was asked, a switch for its
/// acknowledgements.
struct Running {
    at: SocketAddrV4,
    seen: Arc<Mutex<Vec<u16>>>,
    acks: Arc<AtomicBool>,
    /// Does not answer the search.
    quiet: Arc<AtomicBool>,
    /// `LightGet`s to leave unanswered from now on.
    light_silences: Arc<AtomicU32>,
    stop: CancellationToken,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Running {
    fn seen(&self) -> Vec<u16> {
        self.seen.lock().unwrap().clone()
    }

    /// What it was asked apart from readings.
    fn orders(&self) -> Vec<u16> {
        self.seen()
            .into_iter()
            .filter(|k| {
                ![
                    proto::GET_SERVICE,
                    proto::LIGHT_GET,
                    proto::GET_WIFI_INFO,
                    proto::GET_HOST_FIRMWARE,
                    proto::GET_VERSION,
                    proto::GET_GROUP,
                ]
                .contains(k)
            })
            .collect()
    }

    fn forget(&self) {
        self.seen.lock().unwrap().clear();
    }
}

fn label(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn word(p: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([p[i], p[i + 1]])
}

async fn free() -> SocketAddrV4 {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    match socket.local_addr().unwrap() {
        std::net::SocketAddr::V4(at) => at,
        std::net::SocketAddr::V6(_) => unreachable!(),
    }
}

/// Starts a fake bulb with this serial at `at`.
#[allow(clippy::too_many_lines)] // one fake bulb, every message it knows
async fn bulb(at: SocketAddrV4, serial: Serial, fake: Fake) -> Running {
    let socket = UdpSocket::bind(at).await.unwrap();
    let other = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let acks = Arc::new(AtomicBool::new(true));
    let quiet = Arc::new(AtomicBool::new(false));
    let light_silences = Arc::new(AtomicU32::new(0));
    let stop = CancellationToken::new();
    let (log, ack, hush, done) = (seen.clone(), acks.clone(), quiet.clone(), stop.clone());
    let lights_lost = light_silences.clone();
    let fixture: Option<HashMap<String, String>> =
        fake.fixture.map(|f| serde_json::from_str(f).unwrap());
    tokio::spawn(async move {
        let mut fake = fake;
        let mut color = Hsbk {
            hue: 0,
            saturation: 0,
            brightness: u16::MAX,
            kelvin: 2700,
        };
        let mut buf = [0u8; 1024];
        loop {
            let (n, from) = tokio::select! {
                () = done.cancelled() => return,
                got = socket.recv_from(&mut buf) => got.unwrap(),
            };
            let Some(m) = proto::decode(&buf[..n]) else {
                continue;
            };
            let me = if fake.any_serial && m.target != [0; 6] {
                m.target
            } else {
                serial
            };
            if m.target != [0; 6] && m.target != me {
                continue;
            }
            log.lock().unwrap().push(m.kind);
            let real = |name: &str| fixture.as_ref().map(|f| unhex(&f[name]));
            let mut answers: Vec<(Serial, u16, Vec<u8>)> = Vec::new();
            let acked = || ack.load(Ordering::SeqCst);
            match m.kind {
                proto::GET_SERVICE if hush.load(Ordering::SeqCst) => {}
                proto::GET_SERVICE => {
                    let mut p = vec![1];
                    p.extend_from_slice(&u32::from(at.port()).to_le_bytes());
                    for s in std::iter::once(serial).chain(fake.flood.iter().copied()) {
                        answers.push((s, proto::STATE_SERVICE, p.clone()));
                    }
                }
                proto::LIGHT_GET
                    if lights_lost
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok() => {}
                proto::LIGHT_GET => {
                    let p = real("light_state").unwrap_or_else(|| {
                        let mut p = Vec::new();
                        for v in [
                            color.hue,
                            color.saturation,
                            color.brightness,
                            color.kelvin,
                            0,
                        ] {
                            p.extend_from_slice(&v.to_le_bytes());
                        }
                        p.extend_from_slice(&(if fake.on { u16::MAX } else { 0 }).to_le_bytes());
                        p.extend_from_slice(&label(fake.name));
                        p.extend_from_slice(&[0; 8]);
                        p
                    });
                    answers.push((me, proto::LIGHT_STATE, p));
                }
                proto::GET_VERSION if fake.version_silences > 0 => fake.version_silences -= 1,
                proto::GET_VERSION if fake.version_answers == Some(0) => {}
                proto::GET_HOST_FIRMWARE if fake.firmware_silences > 0 => {
                    fake.firmware_silences -= 1;
                }
                proto::GET_GROUP if fake.group_silences > 0 => fake.group_silences -= 1,
                proto::GET_VERSION => {
                    if let Some(n) = &mut fake.version_answers {
                        *n -= 1;
                    }
                    let p = real("state_version").unwrap_or_else(|| {
                        let mut p = 1u32.to_le_bytes().to_vec();
                        p.extend_from_slice(&fake.pid.to_le_bytes());
                        p.extend_from_slice(&[0; 4]);
                        p
                    });
                    answers.push((me, proto::STATE_VERSION, p));
                }
                proto::GET_HOST_FIRMWARE => {
                    let p = real("state_host_firmware").unwrap_or_else(|| {
                        let mut p = vec![0; 16];
                        p.extend_from_slice(&90u16.to_le_bytes());
                        p.extend_from_slice(&3u16.to_le_bytes());
                        p
                    });
                    answers.push((me, proto::STATE_HOST_FIRMWARE, p));
                }
                proto::GET_GROUP => {
                    let p = real("state_group").unwrap_or_else(|| {
                        let mut p = vec![0; 16];
                        p.extend_from_slice(&label("Bureau"));
                        p.extend_from_slice(&[0; 8]);
                        p
                    });
                    answers.push((me, proto::STATE_GROUP, p));
                }
                proto::GET_WIFI_INFO => {
                    let p = real("state_wifi_info").unwrap_or_else(|| {
                        let mut p = 1e-6f32.to_le_bytes().to_vec();
                        p.extend_from_slice(&[0; 10]);
                        p
                    });
                    answers.push((me, proto::STATE_WIFI_INFO, p));
                }
                proto::LIGHT_SET_POWER if acked() => {
                    fake.on = m.payload[..2] != [0, 0];
                    answers.push((me, proto::ACKNOWLEDGEMENT, Vec::new()));
                }
                proto::LIGHT_SET_COLOR if acked() => {
                    let p = &m.payload;
                    color = Hsbk {
                        hue: word(p, 1),
                        saturation: word(p, 3),
                        brightness: word(p, 5),
                        kelvin: word(p, 7),
                    };
                    answers.push((me, proto::ACKNOWLEDGEMENT, Vec::new()));
                }
                proto::LIGHT_SET_WAVEFORM_OPTIONAL if !fake.waveform => {
                    answers.push((
                        me,
                        proto::STATE_UNHANDLED,
                        proto::LIGHT_SET_WAVEFORM_OPTIONAL.to_le_bytes().to_vec(),
                    ));
                }
                proto::LIGHT_SET_WAVEFORM_OPTIONAL if acked() => {
                    let p = &m.payload;
                    if p[21] == 1 {
                        color.hue = word(p, 2);
                    }
                    if p[22] == 1 {
                        color.saturation = word(p, 4);
                    }
                    if p[23] == 1 {
                        color.brightness = word(p, 6);
                    }
                    if p[24] == 1 {
                        color.kelvin = word(p, 8);
                    }
                    answers.push((me, proto::ACKNOWLEDGEMENT, Vec::new()));
                }
                _ => {}
            }
            for (who, kind, payload) in answers {
                let frame = proto::encode(
                    m.source,
                    Some(who),
                    m.sequence,
                    kind,
                    &payload,
                    Reply::Response,
                );
                let away =
                    fake.elsewhere || (fake.acks_elsewhere && kind == proto::ACKNOWLEDGEMENT);
                let out = if away { &other } else { &socket };
                let _ = out.send_to(&frame, from).await;
            }
        }
    });
    Running {
        at,
        seen,
        acks,
        quiet,
        light_silences,
        stop,
    }
}

/// How the driver under test runs.
struct Setup {
    hosts: Vec<SocketAddrV4>,
    poll: Duration,
    transition: u32,
    search_missing: Duration,
    search_again: Duration,
    max_bulbs: usize,
}

impl Setup {
    fn new(hosts: &[&Running]) -> Self {
        Self {
            hosts: hosts.iter().map(|r| r.at).collect(),
            poll: Duration::from_millis(300),
            transition: 0,
            search_missing: Duration::from_secs(60),
            search_again: Duration::from_secs(60),
            max_bulbs: 100,
        }
    }

    fn searching(mut self, every: Duration) -> Self {
        self.search_missing = every;
        self.search_again = every;
        self
    }

    fn start(self) -> Driven {
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let lifx = Lifx {
            hosts: self.hosts,
            broadcast: None,
            poll: self.poll,
            transition: self.transition,
            search_missing: self.search_missing,
            search_again: self.search_again,
            max_bulbs: self.max_bulbs,
        };
        spawn_driver(
            &hub,
            InstanceId::from("lifx"),
            Arc::new(lifx),
            cancel.clone(),
        );
        Driven {
            hub,
            _cancel: cancel,
        }
    }
}

struct Driven {
    hub: Hub,
    _cancel: CancellationToken,
}

const FIRST: &str = "lifx:020000000001";
const SECOND: &str = "lifx:020000000002";

impl Driven {
    fn online(&self, id: &str) -> Option<bool> {
        self.hub
            .snapshot()
            .devices
            .iter()
            .find(|d| d.device.id.as_str() == id)
            .and_then(|d| d.online)
    }

    fn keys(&self, id: &str) -> Vec<String> {
        self.hub
            .snapshot()
            .devices
            .iter()
            .find(|d| d.device.id.as_str() == id)
            .map(|d| d.device.points.iter().map(|p| p.key.to_string()).collect())
            .unwrap_or_default()
    }

    fn value(&self, id: &str, key: &str) -> Value {
        self.hub
            .state(&PointId::from(format!("{id}/{key}")))
            .unwrap()
            .value
    }

    async fn set(&self, id: &str, key: &str, value: Value) -> Result<(), String> {
        self.hub
            .command(
                &PointId::from(format!("{id}/{key}")),
                value,
                Origin::Api,
                None,
            )
            .await
            .map(drop)
            .map_err(|e| format!("{e:?}"))
    }

    async fn until(&self, what: &str, ok: impl Fn(&Self) -> bool) {
        for _ in 0..400 {
            if ok(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("never: {what}");
    }
}

// ---- the driver against them ------------------------------------------------------------

#[tokio::test]
async fn an_order_is_published_only_once_acknowledged() {
    let fake = bulb(free().await, ONE, Fake::default()).await;
    let driven = Setup::new(&[&fake]).start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    let view = driven.hub.snapshot().devices.pop().unwrap();
    assert_eq!(&*view.device.native_name, "Lampe");
    assert_eq!(view.device.native_room.as_deref(), Some("Bureau"));
    assert_eq!(view.device.model.as_deref(), Some("LIFX Mini W"));
    assert_eq!(driven.keys(FIRST), ["on", "brightness", "rssi"]);
    assert_eq!(driven.value(FIRST, "rssi"), Value::Int(-60));

    driven
        .set(FIRST, "brightness", Value::Int(40))
        .await
        .unwrap();
    assert_eq!(driven.value(FIRST, "brightness"), Value::Int(40));

    fake.acks.store(false, Ordering::SeqCst);
    let refused = driven.set(FIRST, "brightness", Value::Int(10)).await;
    assert!(refused.is_err(), "no acknowledgement, no success");
    assert_eq!(driven.value(FIRST, "brightness"), Value::Int(40));
}

#[tokio::test]
async fn an_acknowledgement_from_another_address_does_not_count() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            acks_elsewhere: true,
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&fake]).start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    let refused = driven.set(FIRST, "on", Value::Bool(false)).await;
    assert!(refused.is_err(), "an ack from elsewhere was believed");
    assert_eq!(driven.value(FIRST, "on"), Value::Bool(true));
}

#[tokio::test]
async fn without_discovery_answers_from_elsewhere_are_ignored() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            elsewhere: true,
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&fake]).start();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(fake.seen().contains(&proto::GET_SERVICE));
    assert_eq!(
        driven.online(FIRST),
        None,
        "a bulb answering from elsewhere was met"
    );
}

#[tokio::test]
async fn a_poll_is_one_light_get_per_bulb() {
    let fake = bulb(free().await, ONE, Fake::default()).await;
    let driven = Setup::new(&[&fake]).start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    fake.forget();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let seen = fake.seen();
    assert!(seen.len() >= 3, "polled: {seen:?}");
    assert!(seen.iter().all(|&k| k == proto::LIGHT_GET), "{seen:?}");
}

#[tokio::test]
async fn a_bulb_that_answers_is_not_moved_by_a_claim() {
    let real = bulb(free().await, ONE, Fake::default()).await;
    let liar = bulb(free().await, ONE, Fake::default()).await;
    liar.quiet.store(true, Ordering::SeqCst);
    let driven = Setup::new(&[&real, &liar])
        .searching(Duration::from_millis(300))
        .start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    // The real bulb no longer answers the search (it still answers its
    // readings); the other one claims its serial at an address of `hosts`.
    real.quiet.store(true, Ordering::SeqCst);
    liar.quiet.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(liar.seen().contains(&proto::GET_SERVICE));
    real.forget();
    liar.forget();
    driven.set(FIRST, "on", Value::Bool(false)).await.unwrap();
    assert!(real.orders().contains(&proto::LIGHT_SET_POWER));
    assert!(!liar.orders().contains(&proto::LIGHT_SET_POWER));
}

#[tokio::test]
async fn a_silent_bulb_moves_and_another_one_never_takes_its_place() {
    let first = free().await;
    let second = free().await;
    let entree = bulb(
        first,
        ONE,
        Fake {
            name: "Entrée",
            ..Fake::default()
        },
    )
    .await;
    // A long search period: only the bulb going offline brings the next
    // search early.
    let mut setup = Setup::new(&[&entree]);
    setup.hosts.push(second);
    setup.search_missing = Duration::from_millis(300);
    setup.search_again = Duration::from_secs(60);
    let driven = setup.start();
    driven
        .until("first published", |d| d.online(FIRST) == Some(true))
        .await;

    // It goes away; another bulb answers at its address.
    drop(entree);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _cuisine = bulb(
        first,
        TWO,
        Fake {
            name: "Cuisine",
            ..Fake::default()
        },
    )
    .await;
    driven
        .until("first offline", |d| d.online(FIRST) == Some(false))
        .await;
    driven
        .until("second published", |d| d.online(SECOND) == Some(true))
        .await;
    let names: Vec<(String, String)> = driven
        .hub
        .snapshot()
        .devices
        .iter()
        .map(|d| (d.device.id.to_string(), d.device.native_name.to_string()))
        .collect();
    assert!(
        names.contains(&(FIRST.into(), "Entrée".into())),
        "{names:?}"
    );
    assert!(
        names.contains(&(SECOND.into(), "Cuisine".into())),
        "{names:?}"
    );

    // The first one comes back elsewhere: same serial, silent where it
    // was: followed.
    let moved = bulb(
        second,
        ONE,
        Fake {
            name: "Entrée",
            ..Fake::default()
        },
    )
    .await;
    driven
        .until("first back", |d| d.online(FIRST) == Some(true))
        .await;
    moved.forget();
    driven.set(FIRST, "on", Value::Bool(false)).await.unwrap();
    assert!(moved.orders().contains(&proto::LIGHT_SET_POWER));
}

#[tokio::test]
async fn a_flood_of_serials_does_not_hold_the_orders() {
    let real = bulb(free().await, ONE, Fake::default()).await;
    // Forty serials that never answer as lights.
    let flood: Vec<Serial> = (10..50).map(|i| [0x02, 0, 0, 0, 1, i]).collect();
    let flooder = bulb(
        free().await,
        TWO,
        Fake {
            flood,
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&real, &flooder])
        .searching(Duration::from_millis(300))
        .start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    for on in [false, true, false] {
        driven.set(FIRST, "on", Value::Bool(on)).await.unwrap();
    }
}

#[tokio::test]
async fn an_instance_takes_a_bounded_number_of_bulbs() {
    let flood: Vec<Serial> = (10..30).map(|i| [0x02, 0, 0, 0, 1, i]).collect();
    let crowd = bulb(
        free().await,
        ONE,
        Fake {
            flood,
            any_serial: true,
            ..Fake::default()
        },
    )
    .await;
    let mut setup = Setup::new(&[&crowd]).searching(Duration::from_millis(300));
    setup.max_bulbs = 3;
    let driven = setup.start();
    driven
        .until("full", |d| {
            d.hub
                .snapshot()
                .drivers
                .iter()
                .any(|v| v.warning.as_deref().is_some_and(|w| w.contains('3')))
        })
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(driven.hub.snapshot().devices.len(), 3);
}

#[tokio::test]
async fn a_model_that_did_not_answer_is_asked_again() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            pid: 1, // LIFX Original: colour, 2500-9000 K
            version_silences: 2,
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&fake])
        .searching(Duration::from_millis(300))
        .start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    driven
        .until("its model known", |d| {
            d.keys(FIRST) == ["on", "brightness", "color_temp", "color", "rssi"]
        })
        .await;
    assert!(
        driven
            .hub
            .snapshot()
            .drivers
            .iter()
            .all(|v| v.warning.is_none())
    );
}

#[tokio::test]
async fn an_off_bulb_takes_the_value_then_fades_on() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            on: false,
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&fake]).start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    fake.forget();
    driven
        .set(FIRST, "brightness", Value::Int(30))
        .await
        .unwrap();
    assert_eq!(
        fake.orders(),
        [proto::LIGHT_SET_WAVEFORM_OPTIONAL, proto::LIGHT_SET_POWER]
    );
    assert_eq!(driven.value(FIRST, "on"), Value::Bool(true));
    assert_eq!(driven.value(FIRST, "brightness"), Value::Int(30));
}

#[tokio::test]
async fn an_old_firmware_gets_the_whole_colour() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            waveform: false,
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&fake]).start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    fake.forget();
    driven
        .set(FIRST, "brightness", Value::Int(30))
        .await
        .unwrap();
    driven
        .set(FIRST, "brightness", Value::Int(60))
        .await
        .unwrap();
    assert_eq!(
        fake.orders(),
        [
            proto::LIGHT_SET_WAVEFORM_OPTIONAL,
            proto::LIGHT_SET_COLOR,
            proto::LIGHT_SET_COLOR
        ]
    );
    assert_eq!(driven.value(FIRST, "brightness"), Value::Int(60));
}

#[tokio::test]
async fn no_reading_during_a_fade() {
    let fake = bulb(free().await, ONE, Fake::default()).await;
    let mut setup = Setup::new(&[&fake]);
    setup.transition = 1000;
    let driven = setup.start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    driven
        .set(FIRST, "brightness", Value::Int(20))
        .await
        .unwrap();
    fake.forget();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !fake.seen().contains(&proto::LIGHT_GET),
        "read while fading"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(fake.seen().contains(&proto::LIGHT_GET), "never read again");
}

#[tokio::test]
async fn real_answers_replay_through_the_driver() {
    let white = bulb(
        free().await,
        ONE,
        Fake {
            fixture: Some(include_str!(
                "../../../integrations/lifx/fixtures/lifx-white.json"
            )),
            ..Fake::default()
        },
    )
    .await;
    let mini = bulb(
        free().await,
        TWO,
        Fake {
            fixture: Some(include_str!(
                "../../../integrations/lifx/fixtures/lifx-mini-w.json"
            )),
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&white, &mini]).start();
    driven
        .until("both published", |d| {
            d.online(FIRST) == Some(true) && d.online(SECOND) == Some(true)
        })
        .await;
    let devices = driven.hub.snapshot().devices;
    for (id, model, on, brightness, rssi) in [
        (FIRST, "LIFX White", true, 92, -53),
        (SECOND, "LIFX Mini W", false, 83, -55),
    ] {
        let device = &devices
            .iter()
            .find(|d| d.device.id.as_str() == id)
            .unwrap()
            .device;
        assert_eq!(device.model.as_deref(), Some(model));
        assert_eq!(&*device.native_name, "Lampe");
        assert_eq!(device.native_room.as_deref(), Some("Pièce"));
        // Fixed whites (2700 K): no temperature, no colour, even though
        // the White says 3500 K in its state.
        assert_eq!(driven.keys(id), ["on", "brightness", "rssi"]);
        assert_eq!(driven.value(id, "on"), Value::Bool(on));
        assert_eq!(driven.value(id, "brightness"), Value::Int(brightness));
        assert_eq!(driven.value(id, "rssi"), Value::Int(rssi));
    }
}

#[tokio::test]
async fn silent_bulbs_do_not_hold_the_orders() {
    let live = bulb(free().await, ONE, Fake::default()).await;
    let mut gone = Vec::new();
    for i in 0..10u8 {
        gone.push(bulb(free().await, [0x02, 0, 0, 0, 2, i], Fake::default()).await);
    }
    let mut hosts: Vec<&Running> = vec![&live];
    hosts.extend(gone.iter());
    // Eight meetings per search: the last three at the next one.
    let driven = Setup::new(&hosts).searching(Duration::from_secs(1)).start();
    driven
        .until("all published", |d| d.hub.snapshot().devices.len() == 11)
        .await;
    // Ten bulbs switched off at the wall: 500 ms each per reading, a
    // whole poll of 5 s; the orders to the live one still pass.
    gone.clear();
    for i in 0..12 {
        driven
            .set(FIRST, "on", Value::Bool(i % 2 == 0))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn the_group_is_asked_again_when_meeting() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            group_silences: 1,
            ..Fake::default()
        },
    )
    .await;
    // No search before a minute: only the resends can bring the group.
    let driven = Setup::new(&[&fake]).start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    let room = driven.hub.snapshot().devices[0].device.native_room.clone();
    assert_eq!(room.as_deref(), Some("Bureau"));
}

#[tokio::test]
async fn one_lost_reading_does_not_hand_a_bulb_over() {
    let real = bulb(free().await, ONE, Fake::default()).await;
    let liar = bulb(free().await, ONE, Fake::default()).await;
    liar.quiet.store(true, Ordering::SeqCst);
    let mut setup = Setup::new(&[&real, &liar]).searching(Duration::from_millis(300));
    // No poll in the way: only the check of the old address asks it.
    setup.poll = Duration::from_secs(60);
    let driven = setup.start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    real.light_silences.store(1, Ordering::SeqCst);
    real.quiet.store(true, Ordering::SeqCst);
    liar.quiet.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        real.light_silences.load(Ordering::SeqCst),
        0,
        "never checked"
    );
    real.forget();
    liar.forget();
    driven.set(FIRST, "on", Value::Bool(false)).await.unwrap();
    assert!(real.orders().contains(&proto::LIGHT_SET_POWER));
    assert!(!liar.orders().contains(&proto::LIGHT_SET_POWER));
}

#[tokio::test]
async fn a_firmware_found_late_keeps_the_model() {
    let fake = bulb(
        free().await,
        ONE,
        Fake {
            pid: 1, // LIFX Original: colour, 2500-9000 K
            firmware_silences: 1,
            version_answers: Some(1),
            ..Fake::default()
        },
    )
    .await;
    let driven = Setup::new(&[&fake])
        .searching(Duration::from_millis(300))
        .start();
    driven
        .until("published", |d| d.online(FIRST) == Some(true))
        .await;
    let colour = ["on", "brightness", "color_temp", "color", "rssi"];
    assert_eq!(driven.keys(FIRST), colour);
    // The firmware answers at the next searches; the model must stay.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let firmwares = fake
        .seen()
        .iter()
        .filter(|&&k| k == proto::GET_HOST_FIRMWARE)
        .count();
    assert!(firmwares >= 2, "asked {firmwares} times");
    assert_eq!(driven.keys(FIRST), colour);
}

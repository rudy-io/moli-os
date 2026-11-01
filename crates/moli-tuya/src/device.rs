//! One Tuya device: a TCP connection kept open (6668), its session key,
//! status pushes, heartbeats, orders. Reconnects on its own.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use moli_core::Value;
use serde_json::{Map, Value as Json, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, timeout};

use crate::connections::Connections;
use crate::model::TuyaDevice;
use crate::protocol::{Codec, Frame, Version, cmd};

const PORT: u16 = 6668;
const CONNECT: Duration = Duration::from_secs(5);
const REPLY: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(10);
/// Silent this long: the connection is dead.
const SILENCE: Duration = Duration::from_secs(35);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Plugs only report power when asked (or when it changes a lot).
const REFRESH_POWER: Duration = Duration::from_secs(30);
/// How long a sub-device's first message may still be its answer to our
/// query at connection. Past it, even a first message is a press: a switch
/// that never answers (asleep, or a gateway that does not answer for it)
/// would otherwise stay mute for good, worse than a replayed press. Do not
/// remove this bound.
#[cfg(not(test))]
const QUIET_CAP: Duration = Duration::from_secs(30);
#[cfg(test)]
const QUIET_CAP: Duration = Duration::from_secs(7);

/// Tells, frame by frame, a press pushed by a device from the answers to
/// our queries at connection, which a gateway sends alike (`STATUS`).
struct Answers {
    since: Instant,
    /// Sub-devices (`cid`) that answered since the connection.
    answered: HashSet<String>,
}

impl Answers {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            answered: HashSet::new(),
        }
    }

    /// Whether this frame is pushed by the device on its own (a press in it
    /// is then an event). Within a reply's time, everything answers; after
    /// it, a sub-device's first message is still its answer, a slow
    /// gateway's, until [`QUIET_CAP`].
    fn pushed(&mut self, command: u32, cid: Option<&str>) -> bool {
        let elapsed = self.since.elapsed();
        let first = cid.is_some_and(|cid| self.answered.insert(cid.to_owned()));
        command == cmd::STATUS && elapsed >= REPLY && (!first || elapsed >= QUIET_CAP)
    }
}
const POWER_DPS: [u32; 3] = [18, 19, 20];

/// Where the device is, from the device file or discovery.
pub type Address = Option<(String, Version)>;

/// Shared with discovery: a TCP connection alone proves neither presence
/// nor identity. Only the link marks a device online; key searches may
/// also raise the minimum protocol version after a successful proof.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reachability {
    pub online: bool,
    pub connected_ip: Option<std::net::Ipv4Addr>,
    pub version: Option<Version>,
}

/// What a device task reports to the driver.
#[derive(Debug)]
pub enum Up {
    State(usize, Vec<(String, Value)>),
    /// Presses the device pushed (scene switches): each one is an event,
    /// even when it repeats the last.
    Pressed(usize, Vec<(String, Value)>),
    Online(usize, bool),
}

/// An order: DP values to set, and who waits for the outcome.
#[derive(Debug)]
pub struct Order {
    pub dps: Map<String, Json>,
    /// For a sub-device behind the gateway this link talks to.
    pub cid: Option<String>,
    pub done: oneshot::Sender<Result<(), String>>,
    pub at: Instant,
}

/// A sub-device (Zigbee/BLE) behind the gateway a link talks to: its
/// states come through the gateway's connection, tagged with its `cid`.
#[derive(Debug)]
pub struct Child {
    pub cid: String,
    pub index: usize,
    pub device: Arc<TuyaDevice>,
}

/// An order older than this is never sent: the caller gave up, and a
/// heater switched on hours later is worse than a refusal.
const ORDER_TTL: Duration = Duration::from_secs(5);

impl Order {
    fn stale(&self) -> bool {
        self.at.elapsed() > ORDER_TTL || self.done.is_closed()
    }

    fn refuse(self, why: String) {
        let _ = self.done.send(Err(why));
    }
}

pub struct Link {
    pub index: usize,
    pub device: Arc<TuyaDevice>,
    pub key: [u8; 16],
    pub address: watch::Receiver<Address>,
    pub reachability: Arc<watch::Sender<Reachability>>,
    pub connections: Arc<Connections>,
    pub orders: mpsc::Receiver<Order>,
    pub up: mpsc::Sender<Up>,
    /// Empty unless the device is a gateway with sub-devices.
    pub children: Vec<Child>,
}

#[derive(Debug, thiserror::Error)]
enum LinkError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Protocol(#[from] crate::protocol::ProtocolError),
    #[error("no answer")]
    Timeout,
}

enum Wake {
    Retry,
    Address,
    Stopped,
}

/// The oldest protocol version a device is spoken to in: what its file
/// says (3.4+), then what it proved by negotiating a session. 3.3 has no
/// device proof: a broadcast, which anyone on the network can send, may
/// move a device's address, never bring it back to an older version.
#[derive(Debug)]
struct Floor {
    version: Option<Version>,
    /// An older announcement is said once in the log.
    said: bool,
}

impl Floor {
    fn new(file: Option<Version>) -> Self {
        Self {
            version: file.filter(|v| v.negotiates()),
            said: false,
        }
    }

    /// The device negotiated a session in `version`: it holds its key.
    fn proved(&mut self, version: Version) {
        self.version = self.version.max(Some(version));
    }

    /// The version to speak to `device`, announced in `announced`.
    fn version(&mut self, device: &str, announced: Version) -> Version {
        match self.version {
            Some(floor) if announced < floor => {
                if !self.said {
                    self.said = true;
                    tracing::warn!(
                        device,
                        ?announced,
                        kept = ?floor,
                        "tuya device announced with an older protocol version: ignored"
                    );
                }
                floor
            }
            _ => announced,
        }
    }
}

impl Link {
    pub async fn run(mut self) {
        let mut backoff = Duration::from_secs(2);
        let mut online = None;
        // Discovery gone (its ports taken): the address only comes from
        // the device file; never a reason to stop.
        let mut watching = true;
        let mut floor = Floor::new(self.device.version);
        loop {
            if let Some(version) = self.reachability.borrow().version {
                floor.proved(version);
            }
            let Some((ip, announced)) = self.address.borrow().clone() else {
                if online.is_none() {
                    online = Some(false);
                    self.announce(false).await;
                }
                if matches!(self.idle(None, &mut watching).await, Wake::Stopped) {
                    return;
                }
                continue;
            };
            let version = floor.version(&self.device.name, announced);
            let started = Instant::now();
            let result = self.session(&ip, version, &mut online, &mut floor).await;
            self.reachability.send_modify(|state| state.online = false);
            if self.up.is_closed() {
                return;
            }
            if online != Some(false) {
                online = Some(false);
                self.announce(false).await;
            }
            if let Err(e) = result {
                tracing::debug!(device = self.device.name, ip, error = %e, "tuya link down");
            }
            // A connection that lived a while was not the device's fault.
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(2);
            }
            match self.idle(Some(backoff), &mut watching).await {
                Wake::Stopped => return,
                Wake::Address => backoff = Duration::from_secs(2),
                Wake::Retry => backoff = (backoff * 2).min(BACKOFF_MAX),
            }
        }
    }

    /// Away from the device: refuse orders until the backoff ends (if any)
    /// or an address proof arrives, including a proof of the same IP.
    async fn idle(&mut self, backoff: Option<Duration>, watching: &mut bool) -> Wake {
        let wake = tokio::time::sleep(backoff.unwrap_or(Duration::from_secs(86_400)));
        tokio::pin!(wake);
        loop {
            tokio::select! {
                () = &mut wake, if backoff.is_some() => return Wake::Retry,
                changed = self.address.changed(), if *watching => {
                    if changed.is_err() {
                        *watching = false;
                        continue;
                    }
                    return Wake::Address;
                }
                order = self.orders.recv() => match order {
                    Some(order) => order.refuse(moli_i18n::tr!(
                        "pilotes.tuya.injoignable",
                        name = self.device.name.trim()
                    )),
                    None => return Wake::Stopped,
                },
            }
        }
    }

    /// One connection, until it fails.
    async fn session(
        &mut self,
        ip: &str,
        version: Version,
        online: &mut Option<bool>,
        floor: &mut Floor,
    ) -> Result<(), LinkError> {
        let (stream, _reservation) = timeout(CONNECT, self.connections.connect(ip, PORT))
            .await
            .map_err(|_| LinkError::Timeout)??;
        stream.set_nodelay(true)?;
        let connected_ip = match stream.peer_addr()?.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            std::net::IpAddr::V6(_) => None,
        };
        let (mut reader, mut writer) = stream.into_split();
        let mut codec = Codec::new(version, self.key);
        let mut buf = Vec::with_capacity(1024);

        let id = self.device.id.as_str();
        let cids: Vec<&str> = self.children.iter().map(|c| c.cid.as_str()).collect();
        // Bound the entire proof, including a peer that sends heartbeats
        // forever instead of completing its handshake or encrypted status.
        let first = timeout(
            REPLY,
            authenticate(&mut codec, &mut reader, &mut writer, &mut buf, id, &cids),
        )
        .await
        .map_err(|_| LinkError::Timeout)??;
        // What answers our queries is a state, even pushed like a press (a
        // gateway answers for its sub-devices so).
        let mut answers = Answers::new();
        floor.proved(version);
        // Orders received before identity was established are not replayed
        // after recovery, even if their caller has not timed out yet.
        while let Ok(order) = self.orders.try_recv() {
            order.refuse(moli_i18n::tr!(
                "pilotes.tuya.injoignable",
                name = self.device.name.trim()
            ));
        }
        self.reachability.send_modify(|state| {
            state.online = true;
            state.connected_ip = connected_ip;
            state.version = state.version.max(Some(version));
        });
        *online = Some(true);
        self.announce(true).await;
        if let Some(frame) = first {
            self.report(&codec, &frame, &mut answers).await;
        }

        let wants_power = self.device.dps.iter().any(|d| POWER_DPS.contains(&d.id));
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.tick().await;
        let mut refresh = tokio::time::interval(REFRESH_POWER);
        refresh.tick().await;
        let mut heard = Instant::now();
        // Orders sent, waiting for the device's acknowledgement.
        let mut pending: Vec<(Instant, oneshot::Sender<Result<(), String>>)> = Vec::new();
        loop {
            let mut chunk = [0u8; 2048];
            tokio::select! {
                read = reader.read(&mut chunk) => {
                    let n = read?;
                    if n == 0 {
                        return Err(LinkError::Io(std::io::ErrorKind::UnexpectedEof.into()));
                    }
                    heard = Instant::now();
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(frame) = codec.decode(&mut buf)? {
                        if matches!(frame.cmd, cmd::CONTROL | cmd::CONTROL_NEW) {
                            // The device took the order, or refused it.
                            let outcome = match frame.retcode {
                                Some(0) | None => Ok(()),
                                Some(code) => Err(moli_i18n::tr!("pilotes.tuya.refuse", code = code)),
                            };
                            for (_, done) in pending.drain(..) {
                                let _ = done.send(outcome.clone());
                            }
                        }
                        self.report(&codec, &frame, &mut answers).await;
                    }
                }
                _ = heartbeat.tick() => {
                    if heard.elapsed() > SILENCE {
                        return Err(LinkError::Timeout);
                    }
                    let id = self.device.id.as_str();
                    writer.write_all(&codec.encode(cmd::HEART_BEAT, json!({"gwId": id, "devId": id}).to_string().as_bytes())).await?;
                }
                _ = refresh.tick(), if wants_power => {
                    writer.write_all(&codec.encode(cmd::UPDATEDPS, br#"{"dpId":[18,19,20]}"#)).await?;
                }
                () = tokio::time::sleep_until(pending.first().map_or_else(Instant::now, |(t, _)| *t + REPLY)), if !pending.is_empty() => {
                    let (_, done) = pending.remove(0);
                    let _ = done.send(Err(moli_i18n::tr!("pilotes.tuya.sans_accuse")));
                }
                order = self.orders.recv() => {
                    let Some(order) = order else { return Ok(()) };
                    if order.stale() {
                        order.refuse(moli_i18n::tr!("pilotes.tuya.expire"));
                        continue;
                    }
                    send_order(&mut codec, &mut writer, id, order.cid.as_deref(), &order.dps).await?;
                    pending.push((Instant::now(), order.done));
                }
            }
        }
    }

    /// The device, and the sub-devices it carries, are reachable or not.
    async fn announce(&self, online: bool) {
        let _ = self.up.send(Up::Online(self.index, online)).await;
        for child in &self.children {
            let _ = self.up.send(Up::Online(child.index, online)).await;
        }
    }

    /// A frame's state, or presses when the device pushed it on its own
    /// (`answers` tells which).
    async fn report(&self, codec: &Codec, frame: &Frame, answers: &mut Answers) {
        let (cid, dps) = match status(codec, frame) {
            Ok(Some(status)) => status,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(device = self.device.name, error = %e, "unreadable tuya payload");
                return;
            }
        };
        let pushed = answers.pushed(frame.cmd, cid.as_deref());
        // A sub-device's state: never applied to the gateway itself.
        let (index, device) = match cid {
            None => (self.index, &self.device),
            Some(cid) => {
                let Some(child) = self.children.iter().find(|c| c.cid == cid) else {
                    // Said plainly: how a person finds the `node_id` to file.
                    tracing::info!(
                        gateway = self.device.name,
                        cid,
                        dps = %serde_json::Value::Object(dps),
                        "tuya sub-device not in the device file"
                    );
                    return;
                };
                (child.index, &child.device)
            }
        };
        let mut values = device.decode_dps(&dps);
        // Pushed by the device, not an answer to our queries: a press is an
        // event of its own. The answers at each connection never replay it.
        if pushed {
            let (presses, rest) = values
                .into_iter()
                .partition::<Vec<_>, _>(|(code, _)| device.is_press(code));
            values = rest;
            if !presses.is_empty() {
                let _ = self.up.send(Up::Pressed(index, presses)).await;
            }
        }
        if !values.is_empty() {
            let _ = self.up.send(Up::State(index, values)).await;
        }
    }
}

/// A successful, decrypted state is the only key proof available in 3.3.
/// Empty acknowledgements, heartbeats and error replies never qualify.
pub(crate) fn status_dps(
    codec: &Codec,
    frame: &Frame,
) -> Result<Option<Map<String, Json>>, crate::protocol::ProtocolError> {
    Ok(status(codec, frame)?.map(|(_, dps)| dps))
}

/// A state and, from a gateway, the `cid` of the sub-device it is about.
type Status = (Option<String>, Map<String, Json>);

fn status(codec: &Codec, frame: &Frame) -> Result<Option<Status>, crate::protocol::ProtocolError> {
    if frame.retcode.is_some_and(|code| code != 0)
        || !matches!(
            frame.cmd,
            cmd::STATUS
                | cmd::DP_QUERY
                | cmd::DP_QUERY_NEW
                | cmd::CONTROL
                | cmd::CONTROL_NEW
                | cmd::UPDATEDPS
        )
    {
        return Ok(None);
    }
    let Some(json) = codec.open_json(&frame.payload)? else {
        return Ok(None);
    };
    let data = json.get("data");
    let cid = json
        .get("cid")
        .or_else(|| data.and_then(|data| data.get("cid")))
        .and_then(Json::as_str)
        .map(str::to_owned);
    Ok(json
        .get("dps")
        .or_else(|| data.and_then(|data| data.get("dps")))
        .and_then(Json::as_object)
        .map(|dps| (cid, dps.clone())))
}

/// Asks for a device's state, or for a sub-device's through its gateway.
fn query(codec: &mut Codec, id: &str, cid: Option<&str>) -> Vec<u8> {
    match (codec.version, cid) {
        (Version::V33, None) => codec.encode(
            cmd::DP_QUERY,
            json!({"gwId": id, "devId": id, "uid": id, "t": now()})
                .to_string()
                .as_bytes(),
        ),
        (_, None) => codec.encode(cmd::DP_QUERY_NEW, b"{}"),
        (Version::V33, Some(cid)) => {
            codec.encode(cmd::DP_QUERY, json!({"cid": cid}).to_string().as_bytes())
        }
        (_, Some(cid)) => codec.encode(
            cmd::DP_QUERY_NEW,
            json!({"cid": cid}).to_string().as_bytes(),
        ),
    }
}

async fn authenticate(
    codec: &mut Codec,
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    writer: &mut OwnedWriteHalf,
    buf: &mut Vec<u8>,
    id: &str,
    children: &[&str],
) -> Result<Option<Frame>, LinkError> {
    if codec.version.negotiates() {
        let (start, local) = codec.session_start();
        writer.write_all(&start).await?;
        loop {
            let frame = next_frame(codec, reader, buf).await?;
            if frame.cmd == cmd::SESS_KEY_NEG_RESP {
                let finish = codec.session_finish(local, &frame)?;
                writer.write_all(&finish).await?;
                break;
            }
        }
    }
    let frame = query(codec, id, None);
    writer.write_all(&frame).await?;
    // A gateway may answer for itself with « data unvalid »: in 3.3, a
    // sub-device's state, decrypted with the gateway's key, is the proof.
    for cid in children {
        let frame = query(codec, id, Some(cid));
        writer.write_all(&frame).await?;
    }
    if codec.version.negotiates() {
        return Ok(None);
    }
    loop {
        let frame = next_frame(codec, reader, buf).await?;
        if status_dps(codec, &frame)?.is_some() {
            return Ok(Some(frame));
        }
    }
}

async fn send_order(
    codec: &mut Codec,
    writer: &mut OwnedWriteHalf,
    id: &str,
    cid: Option<&str>,
    dps: &Map<String, Json>,
) -> Result<(), LinkError> {
    let t = now().parse::<u64>().unwrap_or(0);
    let payload = match (codec.version, cid) {
        (Version::V33, None) => json!({"devId": id, "uid": id, "t": now(), "dps": dps}),
        (Version::V33, Some(cid)) => json!({"t": t, "cid": cid, "dps": dps}),
        (_, None) => json!({"protocol": 5, "t": t, "data": {"dps": dps}}),
        (_, Some(cid)) => {
            json!({"protocol": 5, "t": t, "data": {"cid": cid, "ctype": 0, "dps": dps}})
        }
    };
    let command = if codec.version == Version::V33 {
        cmd::CONTROL
    } else {
        cmd::CONTROL_NEW
    };
    let frame = codec.encode(command, payload.to_string().as_bytes());
    writer.write_all(&frame).await?;
    Ok(())
}

async fn next_frame(
    codec: &Codec,
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut Vec<u8>,
) -> Result<Frame, LinkError> {
    let deadline = Instant::now() + REPLY;
    loop {
        if let Some(frame) = codec.decode(buf)? {
            return Ok(frame);
        }
        let mut chunk = [0u8; 1024];
        let n = tokio::time::timeout_at(deadline, reader.read(&mut chunk))
            .await
            .map_err(|_| LinkError::Timeout)??;
        if n == 0 {
            return Err(LinkError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn now() -> String {
    (moli_core::now_ms() / 1000).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    fn response(version: Version, key: [u8; 16], command: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = Codec::new(version, key).encode(command, payload);
        frame.splice(16..16, [0, 0, 0, 0]);
        let total = frame.len();
        let trailer = if version == Version::V33 { 8 } else { 36 };
        frame[12..16].copy_from_slice(&u32::try_from(total - 16).unwrap().to_be_bytes());
        let checksum = if version == Version::V33 {
            crc32fast::hash(&frame[..total - trailer])
                .to_be_bytes()
                .to_vec()
        } else {
            crate::protocol::hmac_sha256(&key, &frame[..total - trailer]).to_vec()
        };
        frame[total - trailer..total - 4].copy_from_slice(&checksum);
        frame
    }

    fn order(value: bool) -> (Order, oneshot::Receiver<Result<(), String>>) {
        let (done, result) = oneshot::channel();
        (
            Order {
                dps: serde_json::from_value(json!({"1": value})).unwrap(),
                cid: None,
                done,
                at: Instant::now(),
            },
            result,
        )
    }

    async fn request(stream: &mut TcpStream) -> (u32, Vec<u8>) {
        let mut header = [0; 16];
        stream.read_exact(&mut header).await.unwrap();
        let size = u32::from_be_bytes(header[12..16].try_into().unwrap()) as usize;
        let mut body = vec![0; size];
        stream.read_exact(&mut body).await.unwrap();
        (
            u32::from_be_bytes(header[8..12].try_into().unwrap()),
            body[..size - 8].to_vec(),
        )
    }

    #[tokio::test]
    async fn unverified_connections_never_receive_an_order() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.7", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(request(&mut stream).await.0, cmd::DP_QUERY);
            let reply = response(Version::V33, *b"0123456789abcdef", cmd::HEART_BEAT, b"{}");
            stream.write_all(&reply).await.unwrap();
            assert!(
                timeout(Duration::from_millis(100), stream.read(&mut [0; 1024]))
                    .await
                    .is_err(),
                "no CONTROL may be sent before a decrypted state proves the key"
            );
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, orders) = test_link(up);
        let (queued, _done) = order(true);
        orders.send(queued).await.unwrap();
        let mut online = Some(false);
        let _ = link
            .session(
                "127.0.0.7",
                Version::V33,
                &mut online,
                &mut Floor::new(None),
            )
            .await;
        server.await.unwrap();
        assert_eq!(online, Some(false));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_status_encrypted_with_another_key_does_not_mark_online() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.8", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            stream
                .write_all(&response(
                    Version::V33,
                    *b"fedcba9876543210",
                    cmd::STATUS,
                    br#"{"dps":{"1":true}}"#,
                ))
                .await
                .unwrap();
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, _orders) = test_link(up);
        let mut online = Some(false);
        assert!(
            link.session(
                "127.0.0.8",
                Version::V33,
                &mut online,
                &mut Floor::new(None)
            )
            .await
            .is_err()
        );
        server.await.unwrap();
        assert_eq!(online, Some(false));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn verified_status_allows_fresh_orders_but_discards_the_recovery_queue() {
        let key = *b"0123456789abcdef";
        let listener = tokio::net::TcpListener::bind(("127.0.0.9", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            stream
                .write_all(&response(
                    Version::V33,
                    key,
                    cmd::STATUS,
                    br#"{"dps":{"1":false}}"#,
                ))
                .await
                .unwrap();
            let (command, body) = request(&mut stream).await;
            assert_eq!(command, cmd::CONTROL);
            let json = Codec::new(Version::V33, key)
                .open_json(&body)
                .unwrap()
                .unwrap();
            assert_eq!(
                json["dps"]["1"], true,
                "only the fresh order crosses the wire"
            );
            stream
                .write_all(&response(Version::V33, key, cmd::CONTROL, b"{}"))
                .await
                .unwrap();
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, orders) = test_link(up);
        let (queued, refused) = order(false);
        orders.send(queued).await.unwrap();
        let session = tokio::spawn(async move {
            link.session(
                "127.0.0.9",
                Version::V33,
                &mut Some(false),
                &mut Floor::new(None),
            )
            .await
        });
        assert!(matches!(events.recv().await, Some(Up::Online(0, true))));
        assert!(
            matches!(events.recv().await, Some(Up::State(0, values)) if values == vec![("switch_1".into(), Value::Bool(false))])
        );
        assert!(refused.await.unwrap().is_err());
        let (fresh, done) = order(true);
        orders.send(fresh).await.unwrap();
        assert!(done.await.unwrap().is_ok());
        server.await.unwrap();
        let _ = session.await.unwrap();
    }

    #[tokio::test]
    async fn endless_heartbeats_do_not_extend_the_handshake_deadline() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.10", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = stream.read(&mut [0; 1024]).await.unwrap();
            let heartbeat = response(Version::V34, *b"0123456789abcdef", cmd::HEART_BEAT, b"{}");
            loop {
                if stream.write_all(&heartbeat).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, _orders) = test_link(up);
        let result = timeout(
            Duration::from_secs(6),
            link.session(
                "127.0.0.10",
                Version::V34,
                &mut Some(false),
                &mut Floor::new(None),
            ),
        )
        .await;
        server.abort();
        assert!(matches!(result, Ok(Err(LinkError::Timeout))));
        assert!(
            link.connections
                .reserve("127.0.0.10".parse().unwrap())
                .is_some(),
            "authentication timeout releases the address for a later retry"
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn an_announcement_during_authentication_does_not_replace_the_connected_address() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.13", PORT))
            .await
            .unwrap();
        let (queried, query) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            queried.send(()).unwrap();
            released.await.unwrap();
            stream
                .write_all(&response(
                    Version::V33,
                    *b"0123456789abcdef",
                    cmd::STATUS,
                    br#"{"dps":{"1":false}}"#,
                ))
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, _orders) = test_link(up);
        let (address, receiver) = watch::channel(Some(("127.0.0.13".into(), Version::V33)));
        link.address = receiver;
        let health = Arc::clone(&link.reachability);
        let session = tokio::spawn(async move {
            link.session(
                "127.0.0.13",
                Version::V33,
                &mut Some(false),
                &mut Floor::new(None),
            )
            .await
        });
        query.await.unwrap();
        address
            .send(Some(("127.0.0.14".into(), Version::V33)))
            .unwrap();
        release.send(()).unwrap();
        assert!(matches!(events.recv().await, Some(Up::Online(0, true))));
        let connected_ip = health.borrow().connected_ip;
        server.abort();
        session.abort();
        assert_eq!(
            connected_ip,
            Some(std::net::Ipv4Addr::new(127, 0, 0, 13)),
            "the protected address belongs to the verified socket, not the latest broadcast"
        );
    }

    fn test_link(up: mpsc::Sender<Up>) -> (Link, mpsc::Sender<Order>) {
        let (_, address) = watch::channel(None);
        let (sender, orders) = mpsc::channel(1);
        (
            Link {
                index: 0,
                device: Arc::new(
                    serde_json::from_value(json!({
                        "id": "test-device", "name": "Test switch", "version": "3.3",
                        "dps": [{"id": 1, "code": "switch_1", "type": "Boolean"}]
                    }))
                    .unwrap(),
                ),
                key: *b"0123456789abcdef",
                address,
                reachability: Arc::new(watch::channel(Reachability::default()).0),
                connections: Arc::new(Connections::default()),
                orders,
                up,
                children: Vec::new(),
            },
            sender,
        )
    }

    fn thermometer() -> Child {
        Child {
            cid: "a4c138ff".into(),
            index: 1,
            device: Arc::new(
                serde_json::from_value(json!({
                    "id": "thermo", "name": "Salon", "category": "wsdcg", "gateway": "tuya",
                    "node_id": "a4c138ff", "parent": "test-device",
                    "dps": [
                        {"id": 1, "code": "temp_current", "type": "Integer", "scale": 1, "unit": "℃"},
                        {"id": 2, "code": "humidity_value", "type": "Integer"},
                        {"id": 9, "code": "switch", "type": "Boolean", "writable": true}
                    ]
                }))
                .unwrap(),
            ),
        }
    }

    fn scene_switch() -> Child {
        Child {
            cid: "b5d249ff".into(),
            index: 1,
            device: Arc::new(
                serde_json::from_value(json!({
                    "id": "button", "name": "Bureau", "category": "wxkg", "gateway": "tuya",
                    "node_id": "b5d249ff", "parent": "test-device",
                    "dps": [
                        {"id": 1, "code": "switch_mode1", "type": "Enum",
                         "range": ["click", "double_click", "press"]},
                        {"id": 10, "code": "battery_percentage", "type": "Integer", "unit": "%"}
                    ]
                }))
                .unwrap(),
            ),
        }
    }

    /// The same press twice is two presses: what the switch pushes is an
    /// event each time. What comes at connection, within a reply's time, is
    /// only its state, even pushed alike: a reconnection never replays the
    /// last press.
    #[tokio::test]
    async fn each_press_of_a_scene_switch_is_an_event() {
        let key = *b"0123456789abcdef";
        let listener = tokio::net::TcpListener::bind(("127.0.0.24", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(request(&mut stream).await.0, cmd::DP_QUERY);
            assert_eq!(request(&mut stream).await.0, cmd::DP_QUERY);
            let switch = |payload: &[u8]| response(Version::V33, key, cmd::STATUS, payload);
            // The gateway answers for its sub-device as it would push a press.
            for payload in [
                &br#"{"cid":"b5d249ff","dps":{"1":"single_click","10":100}}"#[..],
                br#"{"cid":"b5d249ff","dps":{"1":"single_click"}}"#,
            ] {
                stream.write_all(&switch(payload)).await.unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            // Presses, later than the answers.
            tokio::time::sleep(REPLY).await;
            for _ in 0..2 {
                stream
                    .write_all(&switch(br#"{"cid":"b5d249ff","dps":{"1":"single_click"}}"#))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            std::future::pending::<()>().await;
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, _orders) = test_link(up);
        link.children = vec![scene_switch()];
        let session = tokio::spawn(async move {
            link.session(
                "127.0.0.24",
                Version::V33,
                &mut Some(false),
                &mut Floor::new(None),
            )
            .await
        });
        assert!(matches!(events.recv().await, Some(Up::Online(0, true))));
        assert!(matches!(events.recv().await, Some(Up::Online(1, true))));
        let press = (
            "switch_mode1".to_owned(),
            Value::Text("single_click".into()),
        );
        for _ in 0..2 {
            let event = events.recv().await;
            let Some(Up::State(1, values)) = event else {
                panic!("an answer at connection is a state, not a press: {event:?}");
            };
            assert!(values.contains(&press));
        }
        for _ in 0..2 {
            let event = events.recv().await;
            let Some(Up::Pressed(1, presses)) = event else {
                panic!("each pushed press is an event: {event:?}");
            };
            assert_eq!(presses, vec![press.clone()]);
        }
        server.abort();
        session.abort();
    }

    /// A gateway carrying a probe (index 1, it answers at once, the proof)
    /// and a scene switch (index 2), whose frames the gateway sends after
    /// the given delays. Returns the events once those frames are read.
    async fn switch_behind(ip: &'static str, frames: Vec<(Duration, &'static [u8])>) -> Vec<Up> {
        let key = *b"0123456789abcdef";
        let listener = tokio::net::TcpListener::bind((ip, PORT)).await.unwrap();
        let count = frames.len();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for _ in 0..3 {
                assert_eq!(request(&mut stream).await.0, cmd::DP_QUERY);
            }
            let status = |payload: &[u8]| response(Version::V33, key, cmd::STATUS, payload);
            stream
                .write_all(&status(br#"{"cid":"a4c138ff","dps":{"1":215,"2":64}}"#))
                .await
                .unwrap();
            for (after, payload) in frames {
                tokio::time::sleep(after).await;
                stream.write_all(&status(payload)).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        let (up, mut events) = mpsc::channel(16);
        let (mut link, _orders) = test_link(up);
        let mut button = scene_switch();
        button.index = 2;
        link.children = vec![thermometer(), button];
        let session = tokio::spawn(async move {
            link.session(ip, Version::V33, &mut Some(false), &mut Floor::new(None))
                .await
        });
        let mut seen = Vec::new();
        // Online ×3, the probe's state, then one event per frame.
        while seen.len() < 4 + count {
            seen.push(events.recv().await.expect("the session goes on"));
        }
        server.abort();
        session.abort();
        seen
    }

    const SINGLE: &[u8] = br#"{"cid":"b5d249ff","dps":{"1":"single_click"}}"#;

    /// A slow gateway answers for the switch after a reply's time: still its
    /// answer, a state, not a press replayed; what follows is a press.
    #[tokio::test]
    async fn a_slow_gateways_answer_is_still_a_state() {
        let seen = switch_behind(
            "127.0.0.25",
            vec![
                (REPLY + Duration::from_millis(500), SINGLE),
                (Duration::from_millis(300), SINGLE),
            ],
        )
        .await;
        assert!(matches!(seen[4], Up::State(2, _)), "{seen:?}");
        assert!(matches!(seen[5], Up::Pressed(2, _)), "{seen:?}");
    }

    /// A switch that never answers is not mute for good: past the cap, its
    /// first message is a press.
    #[tokio::test]
    async fn a_switch_that_never_answers_is_not_mute() {
        let seen = switch_behind(
            "127.0.0.26",
            vec![(QUIET_CAP + Duration::from_millis(500), SINGLE)],
        )
        .await;
        assert!(matches!(seen[4], Up::Pressed(2, _)), "{seen:?}");
    }

    #[tokio::test]
    async fn a_gateway_carries_its_sub_devices_states_and_orders() {
        let key = *b"0123456789abcdef";
        let listener = tokio::net::TcpListener::bind(("127.0.0.23", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let codec = Codec::new(Version::V33, key);
            // The gateway's own query, then the sub-device's, by its cid.
            assert_eq!(request(&mut stream).await.0, cmd::DP_QUERY);
            let (command, body) = request(&mut stream).await;
            assert_eq!(command, cmd::DP_QUERY);
            let asked = codec.open_json(&body).unwrap().unwrap();
            assert_eq!(asked["cid"], "a4c138ff");
            // A gateway has no state of its own to give.
            stream
                .write_all(&response(
                    Version::V33,
                    key,
                    cmd::DP_QUERY,
                    b"json obj data unvalid",
                ))
                .await
                .unwrap();
            stream
                .write_all(&response(
                    Version::V33,
                    key,
                    cmd::STATUS,
                    br#"{"cid":"a4c138ff","dps":{"1":215,"2":64},"t":1700000000}"#,
                ))
                .await
                .unwrap();
            let (command, body) = request(&mut stream).await;
            assert_eq!(command, cmd::CONTROL);
            let order = codec.open_json(&body).unwrap().unwrap();
            assert_eq!(order["cid"], "a4c138ff");
            assert_eq!(order["dps"]["9"], true);
            stream
                .write_all(&response(Version::V33, key, cmd::CONTROL, b"{}"))
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, orders) = test_link(up);
        link.children = vec![thermometer()];
        let session = tokio::spawn(async move {
            link.session(
                "127.0.0.23",
                Version::V33,
                &mut Some(false),
                &mut Floor::new(None),
            )
            .await
        });
        assert!(matches!(events.recv().await, Some(Up::Online(0, true))));
        assert!(matches!(events.recv().await, Some(Up::Online(1, true))));
        let Some(Up::State(1, values)) = events.recv().await else {
            panic!("the sub-device's state belongs to the sub-device");
        };
        assert!(values.contains(&("temp_current".into(), Value::Float(21.5))));
        assert!(values.contains(&("humidity_value".into(), Value::Int(64))));
        let (done, result) = oneshot::channel();
        orders
            .send(Order {
                dps: serde_json::from_value(json!({"9": true})).unwrap(),
                cid: Some("a4c138ff".into()),
                done,
                at: Instant::now(),
            })
            .await
            .unwrap();
        assert!(result.await.unwrap().is_ok());
        assert!(
            events.try_recv().is_err(),
            "nothing reaches the gateway itself"
        );
        server.abort();
        session.abort();
    }

    #[test]
    fn a_newer_gateway_names_the_sub_device_inside_its_data() {
        let key = *b"0123456789abcdef";
        let codec = Codec::new(Version::V35, key);
        let frame = Frame {
            seqno: 1,
            cmd: cmd::STATUS,
            retcode: None,
            payload: br#"{"protocol":4,"t":1700000000,"data":{"cid":"a4c138ff","dps":{"1":198}}}"#
                .to_vec(),
        };
        let (cid, dps) = status(&codec, &frame).unwrap().unwrap();
        assert_eq!(cid.as_deref(), Some("a4c138ff"));
        assert_eq!(dps["1"], 198);
    }

    #[tokio::test]
    async fn a_reused_address_cannot_claim_to_be_online_with_a_heartbeat() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.2", PORT))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            let mut response = Vec::new();
            for word in [0x55aa_u32, 1, cmd::HEART_BEAT, 12, 0] {
                response.extend_from_slice(&word.to_be_bytes());
            }
            response.extend_from_slice(&crc32fast::hash(&response).to_be_bytes());
            response.extend_from_slice(&0xaa55_u32.to_be_bytes());
            stream.write_all(&response).await.unwrap();
        });
        let (up, mut events) = mpsc::channel(8);
        let (mut link, _orders) = test_link(up);
        let mut online = Some(false);
        let _ = link
            .session(
                "127.0.0.2",
                Version::V33,
                &mut online,
                &mut Floor::new(None),
            )
            .await;
        server.await.unwrap();
        assert_ne!(online, Some(true), "a heartbeat proves no device identity");
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, Up::Online(_, true)));
        }
    }

    #[tokio::test]
    async fn an_old_owner_cannot_open_an_already_verified_peers_socket() {
        exclusive_connection("127.0.0.16", true).await;
    }

    #[tokio::test]
    async fn concurrent_authentication_cannot_interrupt_the_first_socket() {
        exclusive_connection("127.0.0.17", false).await;
    }

    #[tokio::test]
    async fn a_new_proved_address_recovers_quickly_after_its_first_connection_fails() {
        quick_retry_after_proof("127.0.0.19", "127.0.0.20").await;
    }

    #[tokio::test]
    async fn proving_the_same_address_again_also_resets_a_long_retry_delay() {
        quick_retry_after_proof("127.0.0.21", "127.0.0.21").await;
    }

    async fn quick_retry_after_proof(old_ip: &str, found_ip: &str) {
        let old = tokio::net::TcpListener::bind((old_ip, PORT)).await.unwrap();
        let found = if old_ip == found_ip {
            None
        } else {
            Some(
                tokio::net::TcpListener::bind((found_ip, PORT))
                    .await
                    .unwrap(),
            )
        };
        let (up, mut events) = mpsc::channel(16);
        let (mut link, _orders) = test_link(up);
        let (address, receiver) = watch::channel(Some((old_ip.to_owned(), Version::V33)));
        link.address = receiver;
        let task = tokio::spawn(link.run());
        // Real refusals at t=0, t=2 and t=6 build the normal exponential
        // delay. We observe sockets, not a private backoff variable.
        for _ in 0..3 {
            let (mut socket, _) = timeout(Duration::from_secs(8), old.accept())
                .await
                .unwrap()
                .unwrap();
            request(&mut socket).await;
            socket
                .write_all(&response(
                    Version::V33,
                    *b"fedcba9876543210",
                    cmd::STATUS,
                    br#"{"dps":{"1":false}}"#,
                ))
                .await
                .unwrap();
        }
        address.send_replace(Some((found_ip.to_owned(), Version::V33)));
        let listener = found.as_ref().unwrap_or(&old);
        let (mut transient, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        request(&mut transient).await;
        // Some devices briefly refuse the first connection after a search.
        drop(transient);
        let recovered = timeout(Duration::from_secs(3), listener.accept()).await;
        if recovered.is_err() {
            task.abort();
        }
        let (mut socket, _) = recovered
            .expect("a proved address must reset the retry delay before the first attempt")
            .unwrap();
        request(&mut socket).await;
        socket
            .write_all(&response(
                Version::V33,
                *b"0123456789abcdef",
                cmd::STATUS,
                br#"{"dps":{"1":true}}"#,
            ))
            .await
            .unwrap();
        timeout(Duration::from_secs(1), async {
            while let Some(event) = events.recv().await {
                if matches!(event, Up::State(0, values) if values == vec![("switch_1".into(), Value::Bool(true))]) {
                    return;
                }
            }
            panic!("the recovered connection must publish its authenticated state");
        }).await.unwrap();
        task.abort();
        let _ = task.await;
    }

    async fn exclusive_connection(ip: &'static str, authenticated: bool) {
        let listener = tokio::net::TcpListener::bind((ip, PORT)).await.unwrap();
        let (up, mut events) = mpsc::channel(8);
        let (mut owner, _orders) = test_link(up.clone());
        let (mut old_owner, _old_orders) = test_link(up);
        old_owner.key = *b"fedcba9876543210";
        old_owner.device = Arc::new(
            serde_json::from_value(json!({
                "id": "old-owner", "name": "Old address owner", "version": "3.3", "dps": []
            }))
            .unwrap(),
        );
        let connections = Arc::clone(&owner.connections);
        old_owner.connections = Arc::clone(&connections);
        let connected = tokio::spawn(async move {
            owner
                .session(ip, Version::V33, &mut Some(false), &mut Floor::new(None))
                .await
        });
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        if authenticated {
            socket
                .write_all(&response(
                    Version::V33,
                    *b"0123456789abcdef",
                    cmd::STATUS,
                    br#"{"dps":{"1":false}}"#,
                ))
                .await
                .unwrap();
            assert!(matches!(events.recv().await, Some(Up::Online(0, true))));
        }
        let retry = tokio::spawn(async move {
            old_owner
                .session(ip, Version::V33, &mut Some(false), &mut Floor::new(None))
                .await
        });
        let second_connection = timeout(Duration::from_millis(100), listener.accept()).await;
        connected.abort();
        retry.abort();
        let _ = connected.await;
        let _ = retry.await;
        assert!(
            second_connection.is_err(),
            "an obsolete configured IP must not evict a healthy peer from a single-client device"
        );
        assert!(
            connections.reserve(ip.parse().unwrap()).is_some(),
            "cancelling a connection releases its slot, before or after authentication"
        );
    }

    #[test]
    fn a_broadcast_never_lowers_the_version_a_device_proved() {
        // Nothing known yet: the announcement decides.
        let mut floor = Floor::new(None);
        assert_eq!(floor.version("prise", Version::V33), Version::V33);
        // A 3.5 session negotiated: 3.3 or 3.4 announced, 3.5 spoken.
        floor.proved(Version::V35);
        assert_eq!(floor.version("prise", Version::V33), Version::V35);
        assert_eq!(floor.version("prise", Version::V34), Version::V35);
        // The device file says 3.4: never 3.3, a newer one is taken.
        let mut floor = Floor::new(Some(Version::V34));
        assert_eq!(floor.version("prise", Version::V33), Version::V34);
        assert_eq!(floor.version("prise", Version::V35), Version::V35);
        // 3.3 in the file proves nothing.
        let mut floor = Floor::new(Some(Version::V33));
        assert_eq!(floor.version("prise", Version::V33), Version::V33);
    }
}

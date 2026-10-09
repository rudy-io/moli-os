//! One Tuya device: a TCP connection kept open (6668), its session key,
//! status pushes, heartbeats, orders. Reconnects on its own.

use std::sync::Arc;
use std::time::Duration;

use moli_core::Value;
use serde_json::{Map, Value as Json, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, timeout};

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
const POWER_DPS: [u32; 3] = [18, 19, 20];

/// Where the device is, from the device file or discovery.
pub type Address = Option<(String, Version)>;

/// What a device task reports to the driver.
#[derive(Debug)]
pub enum Up {
    State(usize, Vec<(String, Value)>),
    Online(usize, bool),
}

/// An order: DP values to set, and who waits for the outcome.
#[derive(Debug)]
pub struct Order {
    pub dps: Map<String, Json>,
    pub done: oneshot::Sender<Result<(), String>>,
    pub at: Instant,
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
    pub orders: mpsc::Receiver<Order>,
    pub up: mpsc::Sender<Up>,
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

impl Link {
    pub async fn run(mut self) {
        let mut backoff = Duration::from_secs(2);
        let mut online = None;
        // Discovery gone (its ports taken): the address only comes from
        // the device file; never a reason to stop.
        let mut watching = true;
        loop {
            let Some((ip, version)) = self.address.borrow().clone() else {
                if online.is_none() {
                    online = Some(false);
                    let _ = self.up.send(Up::Online(self.index, false)).await;
                }
                if !self.idle(None, &mut watching).await {
                    return;
                }
                continue;
            };
            let started = Instant::now();
            let result = self.session(&ip, version, &mut online).await;
            if self.up.is_closed() {
                return;
            }
            if online != Some(false) {
                online = Some(false);
                let _ = self.up.send(Up::Online(self.index, false)).await;
            }
            if let Err(e) = result {
                tracing::debug!(device = self.device.name, ip, error = %e, "tuya link down");
            }
            // A connection that lived a while was not the device's fault.
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(2);
            }
            if !self.idle(Some(backoff), &mut watching).await {
                return;
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Away from the device: refuse orders until the backoff ends (if any)
    /// or the address changes. `false`: the driver is gone.
    async fn idle(&mut self, backoff: Option<Duration>, watching: &mut bool) -> bool {
        let wake = tokio::time::sleep(backoff.unwrap_or(Duration::from_secs(86_400)));
        tokio::pin!(wake);
        loop {
            tokio::select! {
                () = &mut wake, if backoff.is_some() => return true,
                changed = self.address.changed(), if *watching => {
                    if changed.is_err() {
                        *watching = false;
                        continue;
                    }
                    return true;
                }
                order = self.orders.recv() => match order {
                    Some(order) => order.refuse(moli_i18n::tr!(
                        "pilotes.tuya.injoignable",
                        name = self.device.name.trim()
                    )),
                    None => return false,
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
    ) -> Result<(), LinkError> {
        let stream = timeout(CONNECT, TcpStream::connect((ip, PORT)))
            .await
            .map_err(|_| LinkError::Timeout)??;
        stream.set_nodelay(true)?;
        let (mut reader, mut writer) = stream.into_split();
        let mut codec = Codec::new(version, self.key);
        let mut buf = Vec::with_capacity(1024);

        if version.negotiates() {
            let (start, local) = codec.session_start();
            writer.write_all(&start).await?;
            let answer = loop {
                let frame = next_frame(&codec, &mut reader, &mut buf).await?;
                if frame.cmd == cmd::SESS_KEY_NEG_RESP {
                    break frame;
                }
            };
            let finish = codec.session_finish(local, &answer)?;
            writer.write_all(&finish).await?;
        }

        let id = self.device.id.as_str();
        let query = if version == Version::V33 {
            codec.encode(
                cmd::DP_QUERY,
                json!({"gwId": id, "devId": id, "uid": id, "t": now()})
                    .to_string()
                    .as_bytes(),
            )
        } else {
            codec.encode(cmd::DP_QUERY_NEW, b"{}")
        };
        writer.write_all(&query).await?;

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
                        if *online != Some(true) {
                            *online = Some(true);
                            let _ = self.up.send(Up::Online(self.index, true)).await;
                        }
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
                        self.report(&codec, &frame).await;
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
                    send_order(&mut codec, &mut writer, id, &order.dps).await?;
                    pending.push((Instant::now(), order.done));
                }
            }
        }
    }

    async fn report(&self, codec: &Codec, frame: &Frame) {
        if !matches!(
            frame.cmd,
            cmd::STATUS
                | cmd::DP_QUERY
                | cmd::DP_QUERY_NEW
                | cmd::CONTROL
                | cmd::CONTROL_NEW
                | cmd::UPDATEDPS
        ) {
            return;
        }
        let json = match codec.open_json(&frame.payload) {
            Ok(Some(json)) => json,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(device = self.device.name, error = %e, "unreadable tuya payload");
                return;
            }
        };
        // 3.4+ nest the DPs under `data`.
        let dps = json
            .get("dps")
            .or_else(|| json.get("data").and_then(|d| d.get("dps")));
        if let Some(Json::Object(dps)) = dps {
            let values = self.device.decode_dps(dps);
            if !values.is_empty() {
                let _ = self.up.send(Up::State(self.index, values)).await;
            }
        }
    }
}

async fn send_order(
    codec: &mut Codec,
    writer: &mut OwnedWriteHalf,
    id: &str,
    dps: &Map<String, Json>,
) -> Result<(), LinkError> {
    let frame = if codec.version == Version::V33 {
        codec.encode(
            cmd::CONTROL,
            json!({"devId": id, "uid": id, "t": now(), "dps": dps})
                .to_string()
                .as_bytes(),
        )
    } else {
        codec.encode(
            cmd::CONTROL_NEW,
            json!({"protocol": 5, "t": now().parse::<u64>().unwrap_or(0), "data": {"dps": dps}})
                .to_string()
                .as_bytes(),
        )
    };
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

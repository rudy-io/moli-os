//! The HAP server: HTTP/1.1 over TCP, in clear until pair-verify, then in
//! ChaCha20-Poly1305 frames (two length bytes as AAD, at most 1024 bytes
//! of text each, one counter per direction). Endpoints: `/pair-setup`,
//! `/pair-verify`, `/pairings`, `/accessories`, `/characteristics`,
//! `/identify`; events go out as `EVENT/1.0`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use moli_core::{Origin, PointId, Value};
use moli_runtime::Hub;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};

use crate::bridge::{self, Accessories, Binding};
use crate::pairing::{self, Identity, Keys, Outcome};

/// HAP status codes.
const READ_ONLY: i64 = -70404;
const UNREACHABLE: i64 = -70402;
const NO_SUCH: i64 = -70409;
const INVALID: i64 = -70410;
const NO_EVENTS: i64 = -70406;

/// A request, at most this big.
const MAX_REQUEST: usize = 64 << 10;

/// What every connection shares.
pub struct Shared {
    pub hub: Hub,
    pub identity: Mutex<Identity>,
    /// The person's setup code (`XXX-XX-XXX`), none: not pairable.
    pub code: Option<String>,
    pub accessories: RwLock<Accessories>,
    /// `(aid, iid, value)` as Moli's states change.
    pub events: broadcast::Sender<(u64, u64, Json)>,
    /// The identity changed: to keep in the vault.
    pub keep: mpsc::Sender<Identity>,
    /// One pair-setup at a time, and a limit on attempts.
    pub setup: Mutex<SetupGate>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared")
    }
}

#[derive(Debug, Default)]
pub struct SetupGate {
    busy: bool,
    attempts: u32,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One connection's crypto, after pair-verify.
struct Session {
    keys: Keys,
    sent: u64,
    received: u64,
}

impl Session {
    fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + 64);
        for chunk in plain.chunks(1024) {
            let len = u16::try_from(chunk.len()).unwrap_or(1024).to_le_bytes();
            let mut data = chunk.to_vec();
            pairing::seal(
                &self.keys.read,
                pairing::counter_nonce(self.sent),
                &len,
                &mut data,
            );
            self.sent += 1;
            out.extend_from_slice(&len);
            out.extend_from_slice(&data);
        }
        out
    }

    /// Frames out of `raw` into `plain`; `None` if one does not open.
    fn open(&mut self, raw: &mut Vec<u8>, plain: &mut Vec<u8>) -> Option<()> {
        loop {
            if raw.len() < 2 {
                return Some(());
            }
            let len = usize::from(u16::from_le_bytes([raw[0], raw[1]]));
            if len > 1024 {
                return None;
            }
            if raw.len() < 2 + len + 16 {
                return Some(());
            }
            let aad = [raw[0], raw[1]];
            let mut data = raw[2..2 + len + 16].to_vec();
            let n = pairing::open(
                &self.keys.write,
                pairing::counter_nonce(self.received),
                &aad,
                &mut data,
            )?;
            self.received += 1;
            plain.extend_from_slice(&data[..n]);
            raw.drain(..2 + len + 16);
        }
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// One request off the front of `buf`, if whole.
fn request(buf: &mut Vec<u8>) -> Option<Request> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if buf.len() < end + 4 + length {
        return None;
    }
    let body = buf[end + 4..end + 4 + length].to_vec();
    buf.drain(..end + 4 + length);
    Some(Request { method, path, body })
}

fn response(status: &str, kind: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\n").into_bytes();
    if body.is_empty() {
        out.extend_from_slice(b"Content-Length: 0\r\n");
    } else {
        out.extend_from_slice(
            format!("Content-Type: {kind}\r\nContent-Length: {}\r\n", body.len()).as_bytes(),
        );
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

fn tlv(body: &[u8]) -> Vec<u8> {
    response("200 OK", "application/pairing+tlv8", body)
}

fn hap(status: &str, body: &Json) -> Vec<u8> {
    response(status, "application/hap+json", body.to_string().as_bytes())
}

fn event(body: &Json) -> Vec<u8> {
    let body = body.to_string();
    format!(
        "EVENT/1.0 200 OK\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// The current value HomeKit reads for a characteristic.
pub fn value(hub: &Hub, binding: &Binding) -> Json {
    bridge::read(binding, |p| hub.state(p).map(|s| s.value))
}

/// One connection, until it closes.
pub async fn connection(shared: Arc<Shared>, mut stream: TcpStream) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let mut raw = Vec::new();
    let mut plain = Vec::new();
    let mut session: Option<Session> = None;
    let mut setup = pairing::Setup::default();
    let mut verify = pairing::Verify::default();
    let mut setting_up = false;
    let mut watched: BTreeSet<(u64, u64)> = BTreeSet::new();
    // The write announced by `/prepare`: its id and when it lapses.
    let mut timed: Option<(u64, tokio::time::Instant)> = None;
    let mut events = shared.events.subscribe();
    let mut buf = vec![0u8; 8192];
    let result = async {
        loop {
            tokio::select! {
                read = stream.read(&mut buf) => {
                    let n = read?;
                    if n == 0 {
                        return anyhow::Ok(());
                    }
                    if raw.len() + n > MAX_REQUEST * 2 {
                        anyhow::bail!("request too large");
                    }
                    raw.extend_from_slice(&buf[..n]);
                    match session.as_mut() {
                        Some(s) => s.open(&mut raw, &mut plain).ok_or_else(|| anyhow::anyhow!("frame does not open"))?,
                        None => plain.append(&mut raw),
                    }
                    while let Some(req) = request(&mut plain) {
                        let mut keys = None;
                        let asked = format!("{} {}", req.method, req.path);
                        if req.method == "PUT" {
                            tracing::debug!(asked, body = %String::from_utf8_lossy(&req.body), "homekit: request");
                        }
                        let answer = handle(&shared, req, session.as_ref().map(|s| s.keys.controller.as_str()), &mut setup, &mut setting_up, &mut verify, &mut keys, &mut watched, &mut timed).await;
                        tracing::debug!(asked, answer = %String::from_utf8_lossy(answer.split(|&c| c == b'\r').next().unwrap_or_default()), "homekit");
                        let bytes = match session.as_mut() {
                            Some(s) => s.seal(&answer),
                            None => answer,
                        };
                        stream.write_all(&bytes).await?;
                        if let Some(keys) = keys {
                            session = Some(Session { keys, sent: 0, received: 0 });
                        }
                    }
                }
                changed = events.recv(), if session.is_some() && !watched.is_empty() => {
                    let Ok((aid, iid, value)) = changed else { continue };
                    if watched.contains(&(aid, iid)) && let Some(s) = session.as_mut() {
                        let body = json!({ "characteristics": [{ "aid": aid, "iid": iid, "value": value }] });
                        stream.write_all(&s.seal(&event(&body))).await?;
                    }
                }
            }
        }
    }
    .await;
    if setting_up {
        lock(&shared.setup).busy = false;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn handle(
    shared: &Shared,
    req: Request,
    controller: Option<&str>,
    setup: &mut pairing::Setup,
    setting_up: &mut bool,
    verify: &mut pairing::Verify,
    keys: &mut Option<Keys>,
    watched: &mut BTreeSet<(u64, u64)>,
    timed: &mut Option<(u64, tokio::time::Instant)>,
) -> Vec<u8> {
    let path = req.path.split('?').next().unwrap_or_default();
    match (req.method.as_str(), path, controller) {
        ("POST", "/pair-setup", None) => pair_setup(shared, setup, setting_up, &req.body).await,
        ("POST", "/pair-verify", None) => {
            let identity = lock(&shared.identity).clone();
            let (answer, verified) = verify.step(&identity, &req.body);
            *keys = verified;
            tlv(&answer)
        }
        ("POST", "/identify", None) => {
            if lock(&shared.identity).paired() {
                hap("400 Bad Request", &json!({ "status": -70401 }))
            } else {
                response("204 No Content", "", &[])
            }
        }
        (_, _, None) => hap(
            "470 Connection Authorization Required",
            &json!({ "status": -70401 }),
        ),
        ("POST", "/pairings", Some(from)) => {
            let (answer, changed) = {
                let mut identity = lock(&shared.identity);
                let (answer, changed) = pairing::manage(&mut identity, from, &req.body);
                (answer, changed.then(|| identity.clone()))
            };
            if let Some(identity) = changed {
                let _ = shared.keep.send(identity).await;
            }
            tlv(&answer)
        }
        ("GET", "/accessories", Some(_)) => {
            let body = {
                let accessories = shared
                    .accessories
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                with_values(&shared.hub, &accessories)
            };
            tracing::debug!(accessories = %summary(&body), "homekit: announced");
            hap("200 OK", &body)
        }
        ("GET", "/characteristics", Some(_)) => read_characteristics(shared, &req.path),
        // A timed write (the Home app asks for it on a security system): the
        // write that follows must name this id, within its time.
        ("PUT", "/prepare", Some(_)) => {
            let asked = serde_json::from_slice::<Json>(&req.body).ok();
            let ids = asked
                .as_ref()
                .and_then(|a| Some((a["pid"].as_u64()?, a["ttl"].as_u64()?)));
            if let Some((pid, ttl)) = ids {
                *timed = Some((
                    pid,
                    tokio::time::Instant::now() + Duration::from_millis(ttl.min(10_000)),
                ));
                hap("200 OK", &json!({ "status": 0 }))
            } else {
                hap("400 Bad Request", &json!({ "status": INVALID }))
            }
        }
        ("PUT", "/characteristics", Some(_)) => {
            write_characteristics(shared, &req.body, watched, timed).await
        }
        _ => hap("404 Not Found", &json!({ "status": NO_SUCH })),
    }
}

async fn pair_setup(
    shared: &Shared,
    setup: &mut pairing::Setup,
    setting_up: &mut bool,
    body: &[u8],
) -> Vec<u8> {
    use crate::tlv8::{self, Items, Writer};
    let Some(code) = shared.code.clone() else {
        return tlv(&Writer::new()
            .byte(tlv8::STATE, 2)
            .byte(tlv8::ERROR, tlv8::ERR_UNAVAILABLE)
            .finish());
    };
    if Items::parse(body).and_then(|i| i.byte(tlv8::STATE)) == Some(1) {
        let mut gate = lock(&shared.setup);
        if gate.attempts >= 100 {
            return tlv(&Writer::new()
                .byte(tlv8::STATE, 2)
                .byte(tlv8::ERROR, tlv8::ERR_BACKOFF)
                .finish());
        }
        if gate.busy && !*setting_up {
            return tlv(&Writer::new()
                .byte(tlv8::STATE, 2)
                .byte(tlv8::ERROR, tlv8::ERR_BUSY)
                .finish());
        }
        gate.busy = true;
        gate.attempts += 1;
        *setting_up = true;
    }
    let identity = lock(&shared.identity).clone();
    let (answer, outcome) = setup.step(&identity, &code, body);
    if let Outcome::Paired(controller) = outcome {
        let identity = {
            let mut identity = lock(&shared.identity);
            identity.controllers.push(controller);
            identity.clone()
        };
        {
            let mut gate = lock(&shared.setup);
            gate.busy = false;
            gate.attempts = 0;
        }
        *setting_up = false;
        let _ = shared.keep.send(identity).await;
    }
    tlv(&answer)
}

/// `2:Jardin[3E,85,86] 3:…`, for the log.
fn summary(body: &Json) -> String {
    body["accessories"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let services: Vec<&str> = a["services"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["type"].as_str())
                .collect();
            let name = a["services"][0]["characteristics"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| c["type"] == "23")
                .and_then(|c| c["value"].as_str())
                .unwrap_or("?");
            format!("{}:{name}[{}]", a["aid"], services.join(","))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `/accessories` with each readable characteristic's current value.
fn with_values(hub: &Hub, accessories: &Accessories) -> Json {
    let mut body = accessories.json.clone();
    for accessory in body["accessories"].as_array_mut().into_iter().flatten() {
        let aid = accessory["aid"].as_u64().unwrap_or(0);
        for service in accessory["services"].as_array_mut().into_iter().flatten() {
            for c in service["characteristics"]
                .as_array_mut()
                .into_iter()
                .flatten()
            {
                let iid = c["iid"].as_u64().unwrap_or(0);
                if let Some(ch) = accessories.chars.get(&(aid, iid))
                    && !matches!(ch.binding, Binding::Fixed(_))
                {
                    c["value"] = value(hub, &ch.binding);
                }
            }
        }
    }
    body
}

/// `?id=2.9,2.10`.
fn ids(path: &str) -> Vec<(u64, u64)> {
    let query = path.split_once('?').map_or("", |(_, q)| q);
    query
        .split('&')
        .filter_map(|kv| kv.strip_prefix("id="))
        .flat_map(|list| list.split(','))
        .filter_map(|pair| {
            let (a, i) = pair.split_once('.')?;
            Some((a.parse().ok()?, i.parse().ok()?))
        })
        .collect()
}

fn read_characteristics(shared: &Shared, path: &str) -> Vec<u8> {
    let accessories = shared
        .accessories
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    let mut failed = false;
    let items: Vec<Json> = ids(path)
        .into_iter()
        .map(|(aid, iid)| {
            if let Some(c) = accessories.chars.get(&(aid, iid)) {
                json!({ "aid": aid, "iid": iid, "value": value(&shared.hub, &c.binding) })
            } else {
                failed = true;
                json!({ "aid": aid, "iid": iid, "status": NO_SUCH })
            }
        })
        .collect();
    if failed {
        let items: Vec<Json> = items
            .into_iter()
            .map(|mut i| {
                if i.get("status").is_none() {
                    i["status"] = json!(0);
                }
                i
            })
            .collect();
        hap("207 Multi-Status", &json!({ "characteristics": items }))
    } else {
        hap("200 OK", &json!({ "characteristics": items }))
    }
}

/// One order from the Home app, through the hub like any other (guard, journal).
async fn order(shared: &Shared, point: &PointId, value: Value) -> i64 {
    match shared
        .hub
        .command(point, value, Origin::HomeKit, Some("HomeKit".into()))
        .await
    {
        Ok(()) => 0,
        Err(e) => {
            tracing::info!(error = %e, "homekit: order refused");
            UNREACHABLE
        }
    }
}

async fn write_characteristics(
    shared: &Shared,
    body: &[u8],
    watched: &mut BTreeSet<(u64, u64)>,
    timed: &mut Option<(u64, tokio::time::Instant)>,
) -> Vec<u8> {
    let Ok(request) = serde_json::from_slice::<Json>(body) else {
        return hap("400 Bad Request", &json!({ "status": INVALID }));
    };
    // A write that names a prepared id counts only once, in time.
    if let Some(pid) = request["pid"].as_u64() {
        let prepared = timed.take();
        if !prepared.is_some_and(|(id, until)| id == pid && tokio::time::Instant::now() <= until) {
            return hap("400 Bad Request", &json!({ "status": INVALID }));
        }
    }
    let mut results = Vec::new();
    for item in request["characteristics"].as_array().into_iter().flatten() {
        let (aid, iid) = (
            item["aid"].as_u64().unwrap_or(0),
            item["iid"].as_u64().unwrap_or(0),
        );
        let found = {
            let accessories = shared
                .accessories
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            accessories
                .chars
                .get(&(aid, iid))
                .map(|c| (c.binding.clone(), c.json["perms"].clone()))
        };
        let Some((binding, perms)) = found else {
            results.push((aid, iid, NO_SUCH));
            continue;
        };
        let mut status = 0;
        if let Some(ev) = item["ev"].as_bool() {
            if perms
                .as_array()
                .is_some_and(|p| p.iter().any(|x| x == "ev"))
            {
                if ev {
                    watched.insert((aid, iid));
                } else {
                    watched.remove(&(aid, iid));
                }
            } else {
                status = NO_EVENTS;
            }
        }
        if let Some(value) = item.get("value").filter(|v| !v.is_null()) {
            status = match &binding {
                Binding::Target(point) => match value.as_u64().and_then(bridge::security_mode) {
                    Some(mode) => match shared
                        .hub
                        .command(
                            point,
                            Value::Text(mode.into()),
                            Origin::HomeKit,
                            Some("HomeKit".into()),
                        )
                        .await
                    {
                        Ok(()) => 0,
                        Err(e) => {
                            tracing::info!(error = %e, "homekit: order refused");
                            UNREACHABLE
                        }
                    },
                    None => INVALID,
                },
                Binding::Position(point) => match value.as_u64().filter(|n| *n <= 100) {
                    Some(n) => {
                        order(shared, point, Value::Int(i64::try_from(n).unwrap_or(0))).await
                    }
                    None => INVALID,
                },
                Binding::DoorTarget { control, .. } => match value.as_u64() {
                    Some(0) => order(shared, control, Value::Text("open".into())).await,
                    Some(1) => order(shared, control, Value::Text("close".into())).await,
                    _ => INVALID,
                },
                // « On » pulses the gate; « off » is only the switch falling back.
                Binding::Pulse(point) => match value.as_bool() {
                    Some(true) => order(shared, point, Value::Bool(true)).await,
                    Some(false) => 0,
                    None => INVALID,
                },
                // Identify: nothing to blink.
                Binding::Fixed(Json::Null) => 0,
                _ => READ_ONLY,
            };
        }
        results.push((aid, iid, status));
    }
    if results.iter().all(|(_, _, s)| *s == 0) {
        response("204 No Content", "", &[])
    } else {
        let items: Vec<Json> = results
            .iter()
            .map(|(aid, iid, status)| json!({ "aid": aid, "iid": iid, "status": status }))
            .collect();
        hap("207 Multi-Status", &json!({ "characteristics": items }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_cut_whole_from_the_stream() {
        let mut buf =
            b"PUT /characteristics HTTP/1.1\r\nHost: x\r\ncontent-length: 4\r\n\r\nabc".to_vec();
        assert!(request(&mut buf).is_none());
        buf.extend_from_slice(b"dGET /accessories HTTP/1.1\r\n\r\n");
        let first = request(&mut buf).unwrap();
        assert_eq!(
            (
                first.method.as_str(),
                first.path.as_str(),
                first.body.as_slice()
            ),
            ("PUT", "/characteristics", &b"abcd"[..])
        );
        let second = request(&mut buf).unwrap();
        assert_eq!(second.path, "/accessories");
        assert!(buf.is_empty());
    }

    #[test]
    fn frames_round_trip_with_their_counters() {
        let keys = Keys {
            read: [1; 32],
            write: [1; 32],
            controller: "c".into(),
        };
        let mut ours = Session {
            keys: keys.clone(),
            sent: 0,
            received: 0,
        };
        let mut theirs = Session {
            keys,
            sent: 0,
            received: 0,
        };
        let text = vec![b'x'; 2500];
        let mut raw = ours.seal(&text);
        raw.extend(ours.seal(b"second"));
        let mut plain = Vec::new();
        let mut half = raw.split_off(700);
        theirs.open(&mut raw, &mut plain).unwrap();
        raw.append(&mut half);
        theirs.open(&mut raw, &mut plain).unwrap();
        assert!(raw.is_empty());
        assert_eq!(plain.len(), 2506);
        assert_eq!(theirs.received, 4);
        // Out of order: the counter no longer matches.
        let mut replay = ours.seal(b"x");
        let _ = ours.seal(b"y");
        theirs.received += 1;
        assert!(theirs.open(&mut replay, &mut Vec::new()).is_none());
    }

    #[test]
    fn the_announced_accessories_are_summed_up() {
        let body = json!({ "accessories": [
            { "aid": 1, "services": [{ "type": "3E", "characteristics": [{ "type": "23", "value": "Moli" }] }] },
            { "aid": 2, "services": [
                { "type": "3E", "characteristics": [{ "type": "23", "value": "Jardin" }] },
                { "type": "85", "characteristics": [] },
            ] },
        ] });
        assert_eq!(summary(&body), "1:Moli[3E] 2:Jardin[3E,85]");
    }

    #[test]
    fn ids_are_read_from_the_query() {
        assert_eq!(ids("/characteristics?id=2.9,3.10&ev=1"), [(2, 9), (3, 10)]);
        assert!(ids("/characteristics").is_empty());
    }
}

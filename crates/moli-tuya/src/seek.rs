//! Finding the devices broadcasts do not reveal (their ports held by another
//! program, as Home Assistant's localtuya does, or a device that keeps
//! quiet): who listens on 6668 on the home network, then which device each
//! one is. Only a device's own local key opens a conversation with it: the
//! key is the proof, no address is ever guessed. What `tinytuya scan
//! --force` does.
//!
//! One connection at a time per address, a few seconds each; done at start,
//! then every 15 minutes for the devices still missing (a battery sensor
//! never listens: it is never found, and costs nothing once tried).

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::device::Address;
use crate::protocol::{Codec, Frame, Version, cmd};

const PORT: u16 = 6668;
/// Long enough for a device on the home Wi-Fi to accept a connection.
const KNOCK: Duration = Duration::from_millis(800);
/// One identification attempt, all included.
const ATTEMPT: Duration = Duration::from_secs(3);
/// A new look for the devices still missing.
const AGAIN: Duration = Duration::from_secs(15 * 60);
/// Addresses knocked on at once, and addresses questioned at once.
const KNOCKS_AT_ONCE: usize = 64;
const QUESTIONS_AT_ONCE: usize = 16;

/// A device without an address, and where to put the one found.
#[derive(Clone)]
pub struct Wanted {
    pub id: String,
    pub name: String,
    pub key: [u8; 16],
    pub address: Arc<watch::Sender<Address>>,
}

impl std::fmt::Debug for Wanted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("Wanted")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// `192.168.0.17` → the 254 host addresses of `192.168.0.0/24`.
#[must_use]
pub fn neighbours(ip: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = ip.octets();
    (1..=254).map(|d| Ipv4Addr::new(a, b, c, d)).collect()
}

/// Runs until the driver stops. `taken`: addresses already known to be
/// some device's (never questioned).
pub async fn seek(network: Vec<Ipv4Addr>, wanted: Vec<Wanted>, taken: HashSet<Ipv4Addr>) {
    // (address, device) pairs that did not match: never asked twice.
    let mut tried: HashSet<(Ipv4Addr, String)> = HashSet::new();
    loop {
        let missing: Vec<Wanted> = wanted
            .iter()
            .filter(|w| w.address.borrow().is_none())
            .cloned()
            .collect();
        if !missing.is_empty() {
            let mut busy = taken.clone();
            busy.extend(wanted.iter().filter_map(|w| {
                w.address
                    .borrow()
                    .as_ref()
                    .and_then(|(ip, _)| ip.parse::<Ipv4Addr>().ok())
            }));
            let found = round(&network, &missing, &busy, &mut tried).await;
            let still = missing.len() - found;
            tracing::info!(
                found,
                still_missing = still,
                "tuya devices looked for by their key"
            );
        }
        tokio::time::sleep(AGAIN).await;
    }
}

/// One look: knock, then question each listener with the missing devices'
/// keys. Returns how many were found.
async fn round(
    network: &[Ipv4Addr],
    missing: &[Wanted],
    busy: &HashSet<Ipv4Addr>,
    tried: &mut HashSet<(Ipv4Addr, String)>,
) -> usize {
    let listening = listeners(network.iter().filter(|ip| !busy.contains(ip)).copied()).await;
    let mut questions = JoinSet::new();
    let mut found = 0;
    let mut claimed: HashSet<String> = HashSet::new();
    let mut queue = listening
        .into_iter()
        .filter_map(|ip| {
            let candidates: Vec<Wanted> = missing
                .iter()
                .filter(|w| !tried.contains(&(ip, w.id.clone())))
                .cloned()
                .collect();
            (!candidates.is_empty()).then_some((ip, candidates))
        })
        .collect::<Vec<_>>()
        .into_iter();
    let mut collect = |result: (Ipv4Addr, Vec<String>, Option<(Wanted, Version)>),
                       tried: &mut HashSet<(Ipv4Addr, String)>| {
        let (ip, asked, matched) = result;
        for id in asked {
            tried.insert((ip, id));
        }
        if let Some((w, version)) = matched
            && claimed.insert(w.id.clone())
        {
            found += 1;
            tracing::info!(device = w.name, %ip, ?version, "tuya device found by its key");
            let ip = ip.to_string();
            w.address.send_if_modified(|current| {
                let next = Some((ip.clone(), version));
                let changed = *current != next;
                *current = next;
                changed
            });
        }
    };
    loop {
        while questions.len() < QUESTIONS_AT_ONCE {
            let Some((ip, candidates)) = queue.next() else {
                break;
            };
            questions.spawn(question(ip, candidates));
        }
        let Some(done) = questions.join_next().await else {
            break;
        };
        if let Ok(result) = done {
            collect(result, &mut *tried);
        }
    }
    found
}

/// The addresses accepting a connection on 6668.
async fn listeners(addresses: impl Iterator<Item = Ipv4Addr>) -> Vec<Ipv4Addr> {
    let addresses: Vec<Ipv4Addr> = addresses.collect();
    let mut open = Vec::new();
    for chunk in addresses.chunks(KNOCKS_AT_ONCE) {
        let mut knocks = JoinSet::new();
        for &ip in chunk {
            knocks.spawn(async move {
                matches!(
                    timeout(KNOCK, TcpStream::connect((ip, PORT))).await,
                    Ok(Ok(_))
                )
                .then_some(ip)
            });
        }
        while let Some(knock) = knocks.join_next().await {
            if let Ok(Some(ip)) = knock {
                open.push(ip);
            }
        }
    }
    open.sort_unstable();
    open
}

/// Which of `candidates` answers at `ip`, and in which version. Returns the
/// devices asked, for the record.
async fn question(
    ip: Ipv4Addr,
    candidates: Vec<Wanted>,
) -> (Ipv4Addr, Vec<String>, Option<(Wanted, Version)>) {
    let mut asked = Vec::new();
    for w in candidates {
        for version in [Version::V33, Version::V34, Version::V35] {
            if identify((ip, PORT).into(), &w.id, w.key, version).await {
                return (ip, asked, Some((w, version)));
            }
        }
        asked.push(w.id);
    }
    (ip, asked, None)
}

/// Whether the device at `ip` is `id`: it answers sensibly only to its own
/// key (3.3: a status it encrypted; 3.4/3.5: the proof of the handshake).
async fn identify(at: SocketAddr, id: &str, key: [u8; 16], version: Version) -> bool {
    let attempt = async {
        let stream = TcpStream::connect(at).await.ok()?;
        let (mut reader, mut writer) = stream.into_split();
        let mut codec = Codec::new(version, key);
        let mut buf = Vec::with_capacity(512);
        if version.negotiates() {
            let (start, local) = codec.session_start();
            writer.write_all(&start).await.ok()?;
            let answer = next_frame(&codec, &mut reader, &mut buf).await?;
            return Some(codec.session_finish(local, &answer).is_ok());
        }
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .to_string();
        let query = codec.encode(
            cmd::DP_QUERY,
            json!({"gwId": id, "devId": id, "uid": id, "t": t})
                .to_string()
                .as_bytes(),
        );
        writer.write_all(&query).await.ok()?;
        loop {
            let frame = next_frame(&codec, &mut reader, &mut buf).await?;
            match codec.open_json(&frame.payload) {
                // Only a status decrypted with this key counts.
                Ok(Some(json)) => return Some(json.get("dps").is_some()),
                Ok(None) => {}
                Err(_) => return Some(false),
            }
        }
    };
    timeout(ATTEMPT, attempt)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// The next whole frame; `None` on a closed connection or a corrupt one.
async fn next_frame(codec: &Codec, reader: &mut OwnedReadHalf, buf: &mut Vec<u8>) -> Option<Frame> {
    loop {
        if let Some(frame) = codec.decode(buf).ok()? {
            return Some(frame);
        }
        let mut chunk = [0u8; 1024];
        let n = reader.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn the_home_network_is_a_slash_24() {
        let n = neighbours(Ipv4Addr::new(192, 168, 0, 17));
        assert_eq!(n.len(), 254);
        assert_eq!(n[0], Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(n[253], Ipv4Addr::new(192, 168, 0, 254));
    }

    /// A fake 3.3 device on loopback: answers a DP query with its status,
    /// encrypted with its own key (as a real one does).
    async fn fake_device(key: [u8; 16]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut chunk = [0u8; 1024];
                let _ = stream.read(&mut chunk).await;
                let mut codec = Codec::new(Version::V33, key);
                let mut reply =
                    codec.encode(cmd::DP_QUERY, br#"{"devId":"bf01","dps":{"1":true}}"#);
                // A device's answer carries a return code after the header.
                reply.splice(16..16, [0, 0, 0, 0]);
                let total = reply.len();
                let len = u32::try_from(total - 16).unwrap().to_be_bytes();
                reply[12..16].copy_from_slice(&len);
                let crc = crc32fast::hash(&reply[..total - 8]).to_be_bytes();
                reply[total - 8..total - 4].copy_from_slice(&crc);
                let _ = stream.write_all(&reply).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn only_the_right_key_identifies_a_device() {
        let key = *b"0123456789abcdef";
        let port = fake_device(key).await;
        let at: SocketAddr = ([127, 0, 0, 1], port).into();
        assert!(
            identify(at, "bf01", key, Version::V33).await,
            "its own key opens it"
        );
        assert!(
            !identify(at, "bf01", *b"fedcba9876543210", Version::V33).await,
            "another key does not"
        );
        // A 3.4 handshake to a device that does not speak it: no answer.
        assert!(!identify(at, "bf01", key, Version::V34).await);
    }
}

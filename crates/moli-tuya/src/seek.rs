//! Finding the devices broadcasts do not reveal (their ports held by another
//! program, as Home Assistant's localtuya does, or a device that keeps
//! quiet): who listens on 6668 on the home network, then which device each
//! one is. Only a device's own local key opens a conversation with it: the
//! key is the proof, no address is ever guessed. What `tinytuya scan
//! --force` does.
//!
//! One connection at a time per address, a few seconds each; done at start,
//! then once a minute for local devices whose connection is not verified.
//! Battery sensors and gateway children are never included.

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

use crate::connections::Connections;
use crate::device::{Address, Reachability};
use crate::protocol::{Codec, Frame, Version, cmd};

const PORT: u16 = 6668;
/// Long enough for a device on the home Wi-Fi to accept a connection.
const KNOCK: Duration = Duration::from_millis(800);
/// One identification attempt, all included.
const ATTEMPT: Duration = Duration::from_secs(3);
/// A new look for the devices still missing.
const AGAIN: Duration = Duration::from_secs(60);
/// Let the links finish their first connect + identity proof before scanning.
const INITIAL: Duration = Duration::from_secs(12);
/// Addresses knocked on at once, and addresses questioned at once.
const KNOCKS_AT_ONCE: usize = 64;
const QUESTIONS_AT_ONCE: usize = 16;

/// A local device, and where to put the address verified by its key.
#[derive(Clone)]
pub struct Wanted {
    pub id: String,
    pub name: String,
    pub key: [u8; 16],
    pub address: Arc<watch::Sender<Address>>,
    pub reachability: Arc<watch::Sender<Reachability>>,
    pub connections: Arc<Connections>,
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

/// Runs until the driver stops. A configured address is a starting point,
/// not a reservation: DHCP may have assigned it to another offline device.
pub async fn seek(network: Vec<Ipv4Addr>, wanted: Vec<Wanted>) {
    tokio::time::sleep(INITIAL).await;
    loop {
        if wanted.iter().any(|w| !w.reachability.borrow().online) {
            let found = round(&network, &wanted).await;
            tracing::debug!(found, "tuya offline devices looked for by their key");
        }
        // Failures only belong to this round. A device may have rebooted,
        // changed lease, or briefly refused a connection during the probe.
        tokio::time::sleep(AGAIN).await;
    }
}

fn busy(ip: Ipv4Addr, devices: &[Wanted]) -> bool {
    devices.iter().any(|w| {
        let state = w.reachability.borrow();
        state.online && state.connected_ip == Some(ip)
    })
}

/// One look: knock, then question each listener with the missing devices'
/// keys. Returns how many were found.
async fn round(network: &[Ipv4Addr], devices: &[Wanted]) -> usize {
    let missing: Vec<Wanted> = devices
        .iter()
        .filter(|w| !w.reachability.borrow().online)
        .cloned()
        .collect();
    if missing.is_empty() {
        return 0;
    }
    let listening = listeners(
        network.iter().filter(|ip| !busy(**ip, devices)).copied(),
        devices,
    )
    .await;
    let mut questions = JoinSet::new();
    let mut found = 0;
    let mut claimed: HashSet<String> = HashSet::new();
    let mut queue = listening.into_iter();
    loop {
        while questions.len() < QUESTIONS_AT_ONCE {
            let Some(ip) = queue.next() else {
                break;
            };
            questions.spawn(question(ip, missing.clone(), devices.to_vec()));
        }
        let Some(done) = questions.join_next().await else {
            break;
        };
        if let Ok((ip, Some((w, version)))) = done
            && !w.reachability.borrow().online
            && version >= w.reachability.borrow().version.unwrap_or(Version::V33)
            && !busy(ip, devices)
            && claimed.insert(w.id.clone())
        {
            found += 1;
            tracing::info!(device = w.name, %ip, ?version, "tuya device found by its key");
            w.reachability
                .send_modify(|state| state.version = state.version.max(Some(version)));
            // Wake the link even if the same address recovered: its retry
            // backoff may otherwise still last several minutes.
            w.address.send_replace(Some((ip.to_string(), version)));
        }
    }
    found
}

/// The addresses accepting a connection on 6668.
async fn listeners(addresses: impl Iterator<Item = Ipv4Addr>, devices: &[Wanted]) -> Vec<Ipv4Addr> {
    let Some(first) = devices.first() else {
        return Vec::new();
    };
    let connections = Arc::clone(&first.connections);
    let addresses: Vec<Ipv4Addr> = addresses.collect();
    let devices = Arc::new(devices.to_vec());
    let mut open = Vec::new();
    for chunk in addresses.chunks(KNOCKS_AT_ONCE) {
        let mut knocks = JoinSet::new();
        for &ip in chunk {
            let devices = Arc::clone(&devices);
            let connections = Arc::clone(&connections);
            knocks.spawn(async move {
                // A previous batch may have given the link enough time to
                // recover. Never disturb that now-healthy TCP connection.
                if busy(ip, &devices) {
                    return None;
                }
                let _reservation = connections.reserve(ip.into())?;
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

/// Which of `candidates` answers at `ip`, and in which version.
async fn question(
    ip: Ipv4Addr,
    candidates: Vec<Wanted>,
    devices: Vec<Wanted>,
) -> (Ipv4Addr, Option<(Wanted, Version)>) {
    for w in candidates {
        for version in [Version::V33, Version::V34, Version::V35] {
            if busy(ip, &devices) {
                return (ip, None);
            }
            if w.reachability.borrow().online {
                break;
            }
            if version < w.reachability.borrow().version.unwrap_or(Version::V33) {
                continue;
            }
            let Some(_reservation) = w.connections.reserve(ip.into()) else {
                return (ip, None);
            };
            if identify((ip, PORT).into(), &w.id, w.key, version).await {
                return (ip, Some((w, version)));
            }
        }
    }
    (ip, None)
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
            match crate::device::status_dps(&codec, &frame) {
                // Only a status decrypted with this key counts.
                Ok(Some(_)) => return Some(true),
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

    fn wanted(id: &str, key: [u8; 16], ip: Option<&str>, version: Version) -> Wanted {
        Wanted {
            id: id.into(),
            name: id.into(),
            key,
            connections: Arc::new(Connections::default()),
            address: Arc::new(watch::channel(ip.map(|ip| (ip.into(), version))).0),
            reachability: Arc::new(
                watch::channel(Reachability {
                    online: false,
                    connected_ip: None,
                    version: Some(version),
                })
                .0,
            ),
        }
    }

    async fn changing_device(ip: Ipv4Addr, key: [u8; 16]) -> watch::Sender<[u8; 16]> {
        let listener = TcpListener::bind((ip, PORT)).await.unwrap();
        let (keys, current) = watch::channel(key);
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 1024];
                if stream.read(&mut request).await.unwrap_or(0) == 0 {
                    continue;
                }
                let mut codec = Codec::new(Version::V33, *current.borrow());
                let mut reply = codec.encode(cmd::DP_QUERY, br#"{"dps":{"1":true}}"#);
                reply.splice(16..16, [0, 0, 0, 0]);
                let total = reply.len();
                reply[12..16].copy_from_slice(&u32::try_from(total - 16).unwrap().to_be_bytes());
                let crc = crc32fast::hash(&reply[..total - 8]).to_be_bytes();
                reply[total - 8..total - 4].copy_from_slice(&crc);
                let _ = stream.write_all(&reply).await;
            }
        });
        keys
    }

    #[tokio::test]
    async fn configured_offline_devices_are_found_after_a_dhcp_swap() {
        let (a, b) = (Ipv4Addr::new(127, 0, 0, 3), Ipv4Addr::new(127, 0, 0, 4));
        let (ka, kb) = (*b"0123456789abcdef", *b"fedcba9876543210");
        let _a = changing_device(a, kb).await;
        let _b = changing_device(b, ka).await;
        let wa = wanted("a", ka, Some("127.0.0.3"), Version::V33);
        let wb = wanted("b", kb, Some("127.0.0.4"), Version::V33);
        let mut aa = wa.address.subscribe();
        let mut ab = wb.address.subscribe();
        let task = tokio::spawn(seek(vec![a, b], vec![wa, wb]));
        let result = timeout(Duration::from_secs(20), async {
            aa.changed().await.unwrap();
            ab.changed().await.unwrap();
        })
        .await;
        task.abort();
        assert!(
            result.is_ok(),
            "configured addresses must not hide offline devices from recovery"
        );
        assert_eq!(aa.borrow().as_ref().unwrap().0, "127.0.0.4");
        assert_eq!(ab.borrow().as_ref().unwrap().0, "127.0.0.3");
    }

    #[tokio::test]
    async fn a_failed_key_address_pair_is_retried_on_the_next_round() {
        let ip = Ipv4Addr::new(127, 0, 0, 5);
        let key = *b"0123456789abcdef";
        let keys = changing_device(ip, *b"fedcba9876543210").await;
        let candidates = [wanted("a", key, None, Version::V33)];
        assert_eq!(round(&[ip], &candidates).await, 0);
        keys.send(key).unwrap();
        assert_eq!(
            round(&[ip], &candidates).await,
            1,
            "a transient mismatch or DHCP change must not poison future searches"
        );
    }

    #[tokio::test]
    async fn key_search_does_not_downgrade_a_configured_protocol() {
        let ip = Ipv4Addr::new(127, 0, 0, 6);
        let key = *b"0123456789abcdef";
        let _keys = changing_device(ip, key).await;
        let candidate = wanted("a", key, None, Version::V34);
        let (_, found) = question(ip, vec![candidate], vec![]).await;
        assert!(
            found.is_none(),
            "3.3 must never be probed after a 3.4 minimum is known"
        );
    }

    #[tokio::test]
    async fn a_healthy_devices_address_is_never_probed_for_a_missing_peer() {
        let ip = Ipv4Addr::new(127, 0, 0, 11);
        let listener = TcpListener::bind((ip, PORT)).await.unwrap();
        let healthy = wanted(
            "healthy",
            *b"0123456789abcdef",
            Some("127.0.0.15"),
            Version::V33,
        );
        healthy.reachability.send_modify(|state| {
            state.online = true;
            state.connected_ip = Some(ip);
        });
        let missing = wanted("missing", *b"fedcba9876543210", None, Version::V33);
        assert_eq!(round(&[ip], &[healthy, missing]).await, 0);
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "even a port probe can interrupt a device limited to one TCP client"
        );
    }

    #[tokio::test]
    async fn a_link_recovered_after_candidate_selection_is_not_knocked() {
        let ip = Ipv4Addr::new(127, 0, 0, 12);
        let listener = TcpListener::bind((ip, PORT)).await.unwrap();
        let device = wanted(
            "recovering",
            *b"0123456789abcdef",
            Some("127.0.0.12"),
            Version::V33,
        );
        let candidates = std::iter::once(ip).inspect(|_| {
            device.reachability.send_modify(|state| {
                state.online = true;
                state.connected_ip = Some(ip);
            });
        });
        let result = listeners(candidates, std::slice::from_ref(&device)).await;
        assert!(
            result.is_empty(),
            "a connection recovered since the scan snapshot must be skipped"
        );
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn searches_do_not_connect_during_a_links_unfinished_authentication() {
        let ip = Ipv4Addr::new(127, 0, 0, 18);
        let listener = TcpListener::bind((ip, PORT)).await.unwrap();
        let device = wanted("a", *b"0123456789abcdef", None, Version::V33);
        let reservation = device.connections.reserve(ip.into()).unwrap();
        assert!(
            listeners(std::iter::once(ip), std::slice::from_ref(&device))
                .await
                .is_empty()
        );
        assert!(
            question(ip, vec![device.clone()], vec![device.clone()])
                .await
                .1
                .is_none()
        );
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        drop(reservation);
        assert_eq!(
            listeners(std::iter::once(ip), std::slice::from_ref(&device)).await,
            vec![ip],
            "a finished or cancelled authentication must not block the next search"
        );
    }

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

//! Finding a device whose address changed (the box gave it a new lease, it
//! was unplugged and plugged back): who listens on the API port on the home
//! network, then which one names the expected MAC address. An encrypted
//! device says it in its hello, before any key; one in clear in its device
//! info. Nothing is trusted on that word alone: the key handshake (or, for a
//! device in clear, the same MAC check as before giving a key) still decides.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::timeout;

/// Long enough for a device on the home Wi-Fi to accept a connection.
const KNOCK: Duration = Duration::from_millis(800);
/// Addresses knocked on at once.
const AT_ONCE: usize = 64;

/// `10.0.0.17` → the other 253 host addresses of `10.0.0.0/24`.
#[must_use]
pub(crate) fn neighbours(ip: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = ip.octets();
    (1..=254)
        .map(|d| Ipv4Addr::new(a, b, c, d))
        .filter(|n| *n != ip)
        .collect()
}

/// The MAC address (12 lowercase hex digits) of the device at `at`, if it
/// speaks the ESPHome API there.
async fn mac_at(at: SocketAddr) -> Option<String> {
    let mut tcp = timeout(KNOCK, TcpStream::connect(at)).await.ok()?.ok()?;
    let said = timeout(
        crate::frame::HANDSHAKE_LIMIT,
        crate::frame::hello_of(&mut tcp),
    )
    .await;
    match said {
        Ok(Ok(hello)) => Some(crate::mac_hex(&hello.mac)),
        Ok(Err(e)) if format!("{e:#}").contains(crate::IN_CLEAR) => {
            // In clear: a fresh connection, the hello and the device info.
            let mut tcp = timeout(KNOCK, TcpStream::connect(at)).await.ok()?.ok()?;
            let info = crate::plain_info(&mut tcp).await.ok()?;
            Some(crate::mac_hex(&info.mac))
        }
        _ => None,
    }
}

/// The address among `network` where the device with `mac` answers on `port`.
pub(crate) async fn find(network: &[Ipv4Addr], port: u16, mac: &str) -> Option<Ipv4Addr> {
    let wanted = crate::mac_hex(mac);
    for chunk in network.chunks(AT_ONCE) {
        let mut knocks = JoinSet::new();
        for &ip in chunk {
            knocks.spawn(async move { (ip, mac_at(SocketAddr::from((ip, port))).await) });
        }
        while let Some(knock) = knocks.join_next().await {
            if let Ok((ip, Some(found))) = knock
                && found == wanted
            {
                return Some(ip);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn the_home_network_is_a_slash_24_without_the_old_address() {
        let n = neighbours(Ipv4Addr::new(10, 0, 0, 17));
        assert_eq!(n.len(), 253);
        assert_eq!(n[0], Ipv4Addr::new(10, 0, 0, 1));
        assert!(!n.contains(&Ipv4Addr::new(10, 0, 0, 17)));
    }

    /// A device that answers our hello with its own: protocol, name, MAC.
    async fn device(mac: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let mut hello = [0u8; 3];
                if tcp.read_exact(&mut hello).await.is_err() {
                    continue;
                }
                let mut body = b"\x01voice-pe\x00".to_vec();
                body.extend_from_slice(mac.as_bytes());
                body.push(0);
                let mut out = vec![0x01];
                out.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
                out.extend_from_slice(&body);
                let _ = tcp.write_all(&out).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn the_device_is_found_by_its_mac() {
        let port = device("aabbccddeeff").await;
        let home = [Ipv4Addr::LOCALHOST];
        assert_eq!(
            find(&home, port, "AA:BB:CC:DD:EE:FF").await,
            Some(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(find(&home, port, "aabbccddee00").await, None);
    }

    #[tokio::test]
    async fn nobody_listening_is_nobody_found() {
        // A port just freed: nothing answers there.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        assert_eq!(
            find(&[Ipv4Addr::LOCALHOST], port, "aabbccddeeff").await,
            None
        );
    }
}

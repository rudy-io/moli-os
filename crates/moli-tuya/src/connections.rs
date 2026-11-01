//! A Tuya device may accept only one TCP client. Reserve an address before
//! connecting, for both links and searches, including identity negotiation.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use tokio::net::{TcpStream, lookup_host};

#[derive(Debug, Default)]
pub struct Connections(Mutex<HashSet<IpAddr>>);

#[derive(Debug)]
pub struct Reservation {
    connections: Arc<Connections>,
    ip: IpAddr,
}

impl Connections {
    /// No lock is held during network I/O; different addresses are independent.
    pub fn reserve(self: &Arc<Self>, ip: IpAddr) -> Option<Reservation> {
        let ip = ip.to_canonical();
        self.0
            .lock()
            .expect("connection reservations")
            .insert(ip)
            .then(|| Reservation {
                connections: Arc::clone(self),
                ip,
            })
    }

    /// Resolve first so a hostname and its numeric IP share the same slot.
    pub async fn connect(
        self: &Arc<Self>,
        host: &str,
        port: u16,
    ) -> std::io::Result<(TcpStream, Reservation)> {
        let mut last = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        for address in lookup_host((host, port)).await? {
            let Some(reservation) = self.reserve(address.ip()) else {
                continue;
            };
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok((stream, reservation)),
                Err(error) => last = error,
            }
        }
        Err(last)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.connections
            .0
            .lock()
            .expect("connection reservations")
            .remove(&self.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_reserved_address_is_excluded_until_its_guard_drops() {
        let connections = Arc::new(Connections::default());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let guard = connections.reserve(ip).unwrap();
        assert!(connections.reserve(ip).is_none());
        assert!(
            connections
                .reserve("::ffff:127.0.0.1".parse().unwrap())
                .is_none()
        );
        assert!(connections.reserve("127.0.0.2".parse().unwrap()).is_some());
        drop(guard);
        assert!(connections.reserve(ip).is_some());
    }

    #[tokio::test]
    async fn a_hostname_and_its_numeric_ip_share_the_same_connection_slot() {
        let connections = Arc::new(Connections::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_socket, _reservation) = connections.connect("127.0.0.1", port).await.unwrap();
        let _first = listener.accept().await.unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            connections.connect("localhost", port),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }
}

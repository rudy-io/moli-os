//! Where the printer is: it says so itself. A Bambu printer broadcasts a
//! `NOTIFY` to UDP 2021 every few seconds (what Bambu Studio listens to),
//! with its serial (`USN`) and its address (`Location`). A box that gives it
//! another address (a new lease) is followed that way; the caller checks the
//! printer's pinned certificate there before sending it anything.

use std::net::{Ipv4Addr, SocketAddr};

use tokio::net::UdpSocket;
use tokio::sync::watch;

pub const PORT: u16 = 2021;

/// The address a datagram gives for the printer with `serial`, sent from
/// `from`: its `Location`, else the sender. Only a private IPv4 address.
#[must_use]
pub fn announced(datagram: &str, serial: &str, from: Ipv4Addr) -> Option<Ipv4Addr> {
    if !datagram.starts_with("NOTIFY") {
        return None;
    }
    let header = |name: &str| {
        datagram.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    };
    if !header("USN").is_some_and(|usn| usn.eq_ignore_ascii_case(serial.trim())) {
        return None;
    }
    let ip = match header("Location") {
        Some(location) => location.parse().ok()?,
        None => from,
    };
    ip.is_private().then_some(ip)
}

/// Shared with anything else listening (Bambu Studio on the same machine):
/// broadcasts reach every socket bound with SO_REUSEPORT.
fn socket() -> std::io::Result<UdpSocket> {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    UdpSocket::from_std(socket.into())
}

/// Listens while the returned task lives: the last address the printer
/// announced goes to `last`. Nothing to listen with (the port refused): the
/// printer is reached at its known address only.
pub fn listen(
    serial: String,
    last: watch::Sender<Option<Ipv4Addr>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let socket = match socket() {
            Ok(socket) => socket,
            Err(e) => {
                tracing::warn!(port = PORT, error = %e, "printer announcements not heard");
                return;
            }
        };
        let mut buf = [0u8; 2048];
        loop {
            let Ok((len, from)) = socket.recv_from(&mut buf).await else {
                continue;
            };
            let SocketAddr::V4(from) = from else { continue };
            let Some(ip) = std::str::from_utf8(&buf[..len])
                .ok()
                .and_then(|text| announced(text, &serial, *from.ip()))
            else {
                continue;
            };
            last.send_if_modified(|seen| {
                let changed = *seen != Some(ip);
                *seen = Some(ip);
                changed
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTIFY: &str = "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nServer: UPnP/1.0\r\n\
        Location: 10.0.0.28\r\nNT: urn:bambulab-com:device:3dprinter:1\r\nUSN: 01P00A000000000\r\n\
        Cache-Control: max-age=1800\r\nDevModel.bambu.com: N2S\r\nDevConnect.bambu.com: cloud\r\n\r\n";
    const FROM: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 28);

    #[test]
    fn the_printer_says_where_it_is() {
        assert_eq!(
            announced(NOTIFY, "01P00A000000000", FROM),
            Some(Ipv4Addr::new(10, 0, 0, 28))
        );
        assert_eq!(
            announced(NOTIFY, " 01p00a000000000", FROM),
            Some(Ipv4Addr::new(10, 0, 0, 28)),
            "a serial as typed"
        );
    }

    #[test]
    fn another_printer_or_another_message_is_not_it() {
        assert_eq!(announced(NOTIFY, "01P00A999999999", FROM), None);
        let search = NOTIFY.replacen("NOTIFY *", "M-SEARCH *", 1);
        assert_eq!(announced(&search, "01P00A000000000", FROM), None);
    }

    #[test]
    fn without_a_location_the_sender_is_the_address() {
        let bare = NOTIFY.replace("Location: 10.0.0.28\r\n", "");
        assert_eq!(announced(&bare, "01P00A000000000", FROM), Some(FROM));
        let odd = NOTIFY.replace("10.0.0.28", "http://printer/");
        assert_eq!(announced(&odd, "01P00A000000000", FROM), None);
    }

    #[test]
    fn only_a_home_network_address() {
        let outside = NOTIFY.replace("10.0.0.28", "8.8.8.8");
        assert_eq!(announced(&outside, "01P00A000000000", FROM), None);
    }
}

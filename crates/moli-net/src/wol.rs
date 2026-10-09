//! Wake-on-LAN: the magic packet (six 0xFF, then the MAC sixteen times),
//! broadcast on the whole network and on the target's own /24 (a host with
//! several interfaces, Docker bridges, may not route the first one), on the
//! two usual ports, a few times (a sleeping network card can miss one).

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::{Context as _, bail};

/// The packet for `mac` (`aa:bb:cc:dd:ee:ff` or `aa-bb-…`).
pub fn packet(mac: &str) -> anyhow::Result<Vec<u8>> {
    let bytes: Vec<u8> = mac
        .split([':', '-'])
        .map(|b| u8::from_str_radix(b, 16))
        .collect::<Result<_, _>>()
        .context("MAC address")?;
    if bytes.len() != 6 {
        bail!("MAC address must have 6 bytes");
    }
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    Ok(packet)
}

/// Sends the packet for `mac`; `host` (its last address) adds its /24.
pub async fn wake(mac: &str, host: Option<&str>) -> anyhow::Result<()> {
    let packet = packet(mac)?;
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;
    let mut targets = vec![Ipv4Addr::BROADCAST];
    if let Some(Ok(IpAddr::V4(ip))) = host.map(str::parse::<IpAddr>) {
        let [a, b, c, _] = ip.octets();
        targets.push(Ipv4Addr::new(a, b, c, 255));
    }
    for round in 0..3 {
        if round > 0 {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        for target in &targets {
            for port in [9, 7] {
                socket.send_to(&packet, (*target, port)).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_packet() {
        let p = packet("02-00-00-00-00-01").unwrap();
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[6..12], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&p[96..], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert!(packet("02:00:00").is_err());
        assert!(packet("zz:00:00:00:00:01").is_err());
    }
}

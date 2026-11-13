//! Best-effort discovery of this host's routable local addresses, used when
//! advertising the controller as a discoverable operational service (OTA
//! provider, ICD check-in listener). A socket bound to the wildcard address
//! (`[::]` / `0.0.0.0`) reports an unspecified `local_addr`, which a peer
//! resolves to nothing routable — so we advertise a real source address the OS
//! would use to reach the network instead.

use std::net::{IpAddr, UdpSocket};

/// Return the primary routable local IP addresses (IPv4 and/or IPv6) this host
/// would use to reach off-link peers, for an operational mDNS advertisement.
///
/// Uses the UDP "connect" trick: connecting a datagram socket sends no packets
/// but makes the OS select the source address for a route to a public target.
/// That address is one peers can actually reach — unlike a wildcard bind
/// address. Loopback and unspecified results are dropped.
///
/// Returns an empty vec when no route is available (e.g. fully offline); callers
/// should fall back to their bind address in that case.
#[must_use]
pub fn local_advertise_addrs() -> Vec<IpAddr> {
    let mut addrs = Vec::new();
    // IPv4 primary source.
    if let Some(ip) = source_addr_for("0.0.0.0:0", "8.8.8.8:80") {
        addrs.push(ip);
    }
    // IPv6 primary source (global/ULA), if the host has IPv6 connectivity.
    if let Some(ip) = source_addr_for("[::]:0", "[2001:4860:4860::8888]:80") {
        if !addrs.contains(&ip) {
            addrs.push(ip);
        }
    }
    addrs
}

/// Bind an ephemeral UDP socket and `connect` it (no traffic) to `target` so the
/// OS picks the outbound source address; return it unless it is unspecified or
/// loopback.
fn source_addr_for(bind: &str, target: &str) -> Option<IpAddr> {
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect(target).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::local_advertise_addrs;

    /// Invariant (holds even offline, where the result is empty): every returned
    /// address is a concrete, routable candidate — never loopback or the
    /// wildcard/unspecified address that caused peers to fail to resolve us.
    #[test]
    fn advertised_addrs_are_never_loopback_or_unspecified() {
        for ip in local_advertise_addrs() {
            assert!(!ip.is_loopback(), "advertised a loopback address: {ip}");
            assert!(
                !ip.is_unspecified(),
                "advertised an unspecified address: {ip}"
            );
        }
    }
}

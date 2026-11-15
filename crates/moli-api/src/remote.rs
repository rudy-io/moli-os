//! `[server.remote]`: the ways in from outside the house that the
//! installation declares (docs/SECURITY-MODEL.md, step 2). Nothing declared:
//! Moli behaves as before (the home network, Cloudflare Access).
//!
//! - `vpn`: subnets of a VPN (WireGuard, Tailscale…). Their callers are the
//!   household, but from outside: what stays at home ([`home_only`]) is
//!   refused to them.
//! - `proxies`: the addresses of reverse proxies. What they relay is a
//!   stranger's until a person signs in; the client is the last hop of
//!   `X-Forwarded-For`, the one the proxy added (what the client wrote
//!   before is never believed). Any other relay header stays a stranger's.

use std::net::IpAddr;

use axum::http::HeaderMap;
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    #[serde(default)]
    pub vpn: Vec<Net>,
    #[serde(default)]
    pub proxies: Vec<IpAddr>,
}

/// A subnet, written `10.6.0.0/24` or `fd7a:115c:a1e0::/48`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl TryFrom<String> for Net {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let bad = || format!("{text:?} is not a subnet (like 10.6.0.0/24)");
        let (addr, prefix) = text.split_once('/').ok_or_else(bad)?;
        let addr: IpAddr = addr.parse().map_err(|_| bad())?;
        let prefix: u8 = prefix.parse().map_err(|_| bad())?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(bad());
        }
        Ok(Self {
            addr: addr.to_canonical(),
            prefix,
        })
    }
}

impl Net {
    pub fn contains(&self, ip: IpAddr) -> bool {
        let bits = |ip: IpAddr| match ip.to_canonical() {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)) << 96, true),
            IpAddr::V6(v6) => (u128::from(v6), false),
        };
        let ((net, v4), (ip, ip_v4)) = (bits(self.addr), bits(ip));
        let mask = u128::MAX
            .checked_shl(128 - u32::from(self.prefix))
            .unwrap_or(0);
        v4 == ip_v4 && net & mask == ip & mask
    }

    /// A private address: the home network's ranges or Tailscale's.
    pub fn is_home_address(ip: IpAddr) -> bool {
        block(ip).is_some()
    }

    /// Inside the home network's ranges or Tailscale's (100.64.0.0/10):
    /// never a piece of the public Internet.
    fn is_private(&self) -> bool {
        let last = match self.addr {
            IpAddr::V4(v4) => {
                let span = 1u32.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0);
                IpAddr::V4((u32::from(v4) | span.wrapping_sub(1)).into())
            }
            IpAddr::V6(v6) => {
                let span = 1u128.checked_shl(128 - u32::from(self.prefix)).unwrap_or(0);
                IpAddr::V6((u128::from(v6) | span.wrapping_sub(1)).into())
            }
        };
        block(self.addr).is_some() && block(self.addr) == block(last)
    }
}

/// Which private range `ip` is in: the home network's (RFC 1918, link-local,
/// unique-local) or Tailscale's shared range (100.64.0.0/10).
fn block(ip: IpAddr) -> Option<u8> {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            match (a, b) {
                (10, _) => Some(1),
                (172, 16..=31) => Some(2),
                (192, 168) => Some(3),
                (169, 254) => Some(4),
                (100, 64..=127) => Some(5),
                _ => None,
            }
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            if first & 0xfe00 == 0xfc00 {
                Some(6)
            } else if first & 0xffc0 == 0xfe80 {
                Some(7)
            } else {
                None
            }
        }
    }
}

impl RemoteConfig {
    /// What would open the house wider than it says (I6): refused at start
    /// and by `moli-os check-config`.
    pub fn check(&self) -> Result<(), String> {
        for net in &self.vpn {
            if !net.is_private() {
                return Err(format!(
                    "[server.remote] vpn {}/{}: a VPN subnet is private (home ranges or 100.64.0.0/10), never the Internet",
                    net.addr, net.prefix
                ));
            }
        }
        for proxy in &self.proxies {
            let ip = proxy.to_canonical();
            if !(ip.is_loopback() || block(ip).is_some()) {
                return Err(format!(
                    "[server.remote] proxies {proxy}: a proxy is on this machine or the home network"
                ));
            }
            if self.vpn.iter().any(|n| n.contains(ip)) {
                return Err(format!(
                    "[server.remote] proxies {proxy} is inside a vpn subnet: one or the other"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn in_vpn(&self, ip: IpAddr) -> bool {
        self.vpn.iter().any(|n| n.contains(ip))
    }

    /// What a client behind a declared proxy is known by: the last hop of
    /// `X-Forwarded-For`, or `proxy:<address>` when that hop is not an
    /// address. `None`: not a declared proxy, or nothing relayed (a request
    /// of the proxy's machine itself).
    pub(crate) fn proxied(&self, peer: IpAddr, headers: &HeaderMap) -> Option<String> {
        if !self.proxies.iter().any(|p| p.to_canonical() == peer) {
            return None;
        }
        let last = headers.get_all("x-forwarded-for").iter().next_back()?;
        let client = last
            .to_str()
            .ok()
            .and_then(|v| v.rsplit(',').next())
            .and_then(|hop| hop.trim().parse::<IpAddr>().ok());
        Some(client.map_or_else(
            || format!("proxy:{peer}"),
            |ip| ip.to_canonical().to_string(),
        ))
    }
}

/// What stays at home (I4): agents (MCP), the house's machines and
/// satellites, the installation and the integrations' setup, and what
/// writes secrets or the house's code. Refused to whoever comes in through `[server.remote]`; mirrored by
/// the `distant` column of `tests/routes.txt`.
#[doc(hidden)]
#[must_use]
pub fn home_only(path: &str) -> bool {
    let under = |prefix: &str| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    under("/mcp")
        || under("/api/integrations")
        || path.starts_with("/api/machines/")
        || path.starts_with("/api/voice/")
        || matches!(
            path,
            "/api/infra"
                | "/api/session/setup"
                | "/api/session/pin"
                | "/api/assistant/listen"
                | "/api/assistant/key"
                | "/api/automations/import"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Net {
        Net::try_from(s.to_owned()).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_subnet_holds_its_addresses_only() {
        let wg = net("10.6.0.0/24");
        assert!(wg.contains(ip("10.6.0.7")));
        assert!(wg.contains(ip("::ffff:10.6.0.7")));
        assert!(!wg.contains(ip("10.6.1.7")));
        assert!(!wg.contains(ip("fd00::1")));
        let ts = net("fd7a:115c:a1e0::/48");
        assert!(ts.contains(ip("fd7a:115c:a1e0:ab12::1")));
        assert!(!ts.contains(ip("fd7a:115c:a1e1::1")));
        assert!(net("0.0.0.0/0").contains(ip("8.8.8.8")));
        for bad in ["10.6.0.0", "10.6.0.0/33", "fd00::/129", "maison/24"] {
            assert!(Net::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }

    #[test]
    fn what_would_open_the_house_to_the_internet_is_refused() {
        let ok = RemoteConfig {
            vpn: vec![
                net("10.6.0.0/24"),
                net("100.64.0.0/10"),
                net("fd7a:115c:a1e0::/48"),
            ],
            proxies: vec![ip("192.168.1.5"), ip("127.0.0.1")],
        };
        assert_eq!(ok.check(), Ok(()));
        for vpn in [
            "0.0.0.0/0",
            "8.8.8.0/24",
            "10.0.0.0/7",
            "100.0.0.0/8",
            "::/0",
        ] {
            let config = RemoteConfig {
                vpn: vec![net(vpn)],
                proxies: Vec::new(),
            };
            assert!(config.check().is_err(), "{vpn}");
        }
        let public_proxy = RemoteConfig {
            vpn: Vec::new(),
            proxies: vec![ip("203.0.113.9")],
        };
        assert!(public_proxy.check().is_err());
        let proxy_in_vpn = RemoteConfig {
            vpn: vec![net("10.6.0.0/24")],
            proxies: vec![ip("10.6.0.1")],
        };
        assert!(proxy_in_vpn.check().is_err());
    }

    #[test]
    fn only_the_hop_the_proxy_added_is_believed() {
        let config = RemoteConfig {
            vpn: Vec::new(),
            proxies: vec![ip("192.168.1.5")],
        };
        let mut headers = HeaderMap::new();
        // The client wrote the first one; the proxy appended the last.
        headers.insert("x-forwarded-for", "10.0.0.1, 203.0.113.9".parse().unwrap());
        headers.insert("x-real-ip", "192.168.0.20".parse().unwrap());
        assert_eq!(
            config.proxied(ip("192.168.1.5"), &headers),
            Some("203.0.113.9".to_owned())
        );
        assert_eq!(config.proxied(ip("192.168.1.6"), &headers), None);
        assert_eq!(config.proxied(ip("192.168.1.5"), &HeaderMap::new()), None);
        let mut garbled = HeaderMap::new();
        garbled.insert("x-forwarded-for", "nobody".parse().unwrap());
        assert_eq!(
            config.proxied(ip("192.168.1.5"), &garbled),
            Some("proxy:192.168.1.5".to_owned())
        );
    }

    #[test]
    fn agents_machines_and_secrets_stay_at_home() {
        for path in [
            "/mcp",
            "/mcp/",
            "/api/machines/pc",
            "/api/machines/pc/remote",
            "/api/voice/salon",
            "/api/session/pin",
            "/api/session/setup",
            "/api/assistant/key",
            "/api/infra",
            "/api/integrations/tuya-cloud",
            "/api/integrations/airzone/discover",
        ] {
            assert!(home_only(path), "{path}");
        }
        for path in [
            "/",
            "/mcpx",
            "/api/integrationsx",
            "/api/devices",
            "/api/session",
            "/api/command",
            "/api/assistant",
        ] {
            assert!(!home_only(path), "{path}");
        }
    }
}

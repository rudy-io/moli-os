//! DNS-rebinding guard for every surface.
//!
//! A web page on any site can make a browser on the LAN send requests to
//! this server; it cannot forge the `Host` header. Only the names this
//! instance is really reached by are accepted, and any address typed as
//! such: DNS rebinding takes a name the attacker controls, never an IP, so a
//! new installation answers on its own address with nothing to configure.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Clone, Debug)]
pub(crate) struct AllowedHosts(Arc<[String]>);

impl AllowedHosts {
    pub(crate) fn new(extra: &[String]) -> Self {
        let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
            .into_iter()
            .map(String::from)
            // Entries go through the same normalization as the header, so
            // "moli:8790" or "[fe80::1]" in the config behave as expected.
            .chain(
                extra
                    .iter()
                    .map(|h| strip_port(h.trim()).to_ascii_lowercase()),
            )
            .collect();
        hosts.dedup();
        Self(hosts.into())
    }

    fn allows(&self, host_header: &str) -> bool {
        let host = strip_port(host_header).to_ascii_lowercase();
        host.parse::<std::net::IpAddr>().is_ok() || self.0.contains(&host)
    }
}

/// `host:port`, `[v6]:port`, `host` → host.
fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !host.contains(':') => {
            host
        }
        _ => authority,
    }
}

pub(crate) async fn check(
    State(allowed): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    match host {
        Some(host) if allowed.allows(host) => next.run(request).await,
        _ => (
            StatusCode::FORBIDDEN,
            "host not allowed (add it to server.allowed_hosts)",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        let allowed = AllowedHosts::new(&["192.168.0.62".into(), "Moli".into()]);
        assert!(allowed.allows("localhost:8790"));
        assert!(allowed.allows("127.0.0.1"));
        assert!(allowed.allows("[::1]:8790"));
        assert!(allowed.allows("192.168.0.62:8790"));
        assert!(allowed.allows("moli:8790"));
        assert!(!allowed.allows("evil.example:8790"));
        assert!(!allowed.allows("192.168.0.62.evil.example"));
        // Any address typed as such: rebinding needs a name.
        assert!(allowed.allows("10.0.0.5:8790"));
        assert!(allowed.allows("[fe80::2]:8790"));
        assert!(!allowed.allows("moli.evil.example:8790"));

        let with_ports = AllowedHosts::new(&["moli:8790".into(), "[fe80::1]".into()]);
        assert!(with_ports.allows("moli:8790"));
        assert!(with_ports.allows("[fe80::1]:8790"));
    }
}

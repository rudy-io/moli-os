//! Who is calling, established once per request before any handler.
//!
//! - the home network or this very machine: the household (and, for this
//!   machine only, the CLI);
//! - through Cloudflare (the tunnel) with a valid Access assertion: the
//!   person Access let in, by e-mail;
//! - anything else (Internet without proof, public address): a stranger.
//!
//! Human sessions (PIN) and failure counters are bound to [`Caller::key`]:
//! the address on the home network, the e-mail through Access (a phone's
//! public address changes all the time).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::access::Verifier;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Via {
    /// This very machine (a script, the CLI).
    Loopback,
    /// A device on the home network.
    Lan,
    /// From the Internet, a person Cloudflare Access let in.
    Access { email: String },
    /// From the Internet without valid proof, or a public address.
    Outside,
    /// The Moli app on a phone, reporting with its own token: it reaches
    /// one route only (its reports), where the token is checked.
    Phone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caller {
    pub(crate) via: Via,
    /// What sessions and failure counters are bound to.
    pub(crate) key: String,
}

impl Caller {
    /// The household: home network, this machine, or a person Access let in.
    pub(crate) fn is_household(&self) -> bool {
        matches!(self.via, Via::Lan | Via::Loopback | Via::Access { .. })
    }

    /// This very machine, not through the tunnel (which also ends here).
    pub(crate) fn is_local_machine(&self) -> bool {
        self.via == Via::Loopback
    }

    /// For the journal: who clicked.
    pub(crate) fn describe(&self) -> String {
        match &self.via {
            Via::Access { email } => format!("tableau de bord ({email})"),
            _ => format!("tableau de bord ({})", self.key),
        }
    }

    pub(crate) fn via_name(&self) -> &'static str {
        match self.via {
            Via::Loopback => "local",
            Via::Lan => "lan",
            Via::Access { .. } => "access",
            Via::Outside => "outside",
            Via::Phone => "phone",
        }
    }
}

/// Requests from Cloudflare carry these. They mean something only when the
/// request comes out of the tunnel, i.e. from this very machine; a device of
/// the network that adds them makes itself a stranger (and is refused).
fn through_cloudflare(headers: &HeaderMap) -> bool {
    headers.contains_key("cf-ray") || headers.contains_key("cf-connecting-ip")
}

/// A proxy on this very machine (a reverse proxy, `tailscale serve`) would
/// make everyone look local, hence trusted: a request that says it was
/// relayed is a stranger's.
fn relayed(headers: &HeaderMap) -> bool {
    [
        "forwarded",
        "x-forwarded-for",
        "x-real-ip",
        "tailscale-user-login",
    ]
    .iter()
    .any(|h| headers.contains_key(*h))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

/// Private, link-local or unique-local: the home network. (`ip` is
/// canonical: an IPv4-mapped address arrives as IPv4.)
fn is_home(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// `access`: the e-mail of a verified Access assertion, if any.
pub(crate) fn classify(peer: IpAddr, headers: &HeaderMap, access: Option<String>) -> Caller {
    // `::ffff:192.168.0.20` is the home network too.
    let mapped = peer.to_canonical();
    if through_cloudflare(headers) {
        // The tunnel ends on this very machine: only from there are
        // Cloudflare's headers Cloudflare's. Keys never come from a header
        // a client could choose, except through the tunnel.
        return match (mapped.is_loopback(), access) {
            (true, Some(email)) => Caller {
                key: format!("access:{email}"),
                via: Via::Access { email },
            },
            (true, None) => Caller {
                key: format!(
                    "cf:{}",
                    header(headers, "cf-connecting-ip")
                        .and_then(|ip| ip.parse::<IpAddr>().ok())
                        .map_or_else(|| "?".to_owned(), |ip| ip.to_string())
                ),
                via: Via::Outside,
            },
            (false, _) => Caller {
                key: mapped.to_string(),
                via: Via::Outside,
            },
        };
    }
    let via = if mapped.is_loopback() && relayed(headers) {
        Via::Outside
    } else if mapped.is_loopback() {
        Via::Loopback
    } else if is_home(mapped) {
        Via::Lan
    } else {
        Via::Outside
    };
    Caller {
        via,
        key: mapped.to_string(),
    }
}

/// `/api/phones/<id>/report` → the phone's id.
fn phone_report(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/api/phones/")?.strip_suffix("/report")?;
    (!id.is_empty() && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some(id)
}

/// Middleware: identifies the caller and leaves it in the request.
pub(crate) async fn identify(
    State(access): State<Option<Arc<Verifier>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip());
    let headers = request.headers();
    let from_tunnel = through_cloudflare(headers) && peer.to_canonical().is_loopback();
    let email = match (&access, from_tunnel) {
        (Some(verifier), true) => match header(headers, "cf-access-jwt-assertion") {
            Some(token) => verifier.identity(token).await,
            None => None,
        },
        _ => None,
    };
    let mut caller = classify(peer, request.headers(), email);
    // A phone's report carries its own token (checked by the route): the
    // one way in without the home network or Cloudflare Access.
    if caller.via == Via::Outside
        && let Some(id) = phone_report(request.uri().path())
        && request
            .headers()
            .contains_key(axum::http::header::AUTHORIZATION)
    {
        caller = Caller {
            key: format!("phone:{id}"),
            via: Via::Phone,
        };
    }
    // Strangers get nothing: not a reading, not a PIN attempt. Moli is the
    // home network's, and Cloudflare Access's for the people it lets in.
    if caller.via == Via::Outside {
        tracing::warn!(client = caller.key, path = %request.uri().path(), "stranger refused");
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": moli_i18n::tr!("serveur.acces.refuse") })),
        )
            .into_response();
    }
    request.extensions_mut().insert(caller);
    next.run(request).await
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = (StatusCode, &'static str);

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Caller>()
                .cloned()
                .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "caller not identified")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn the_home_network_is_the_household() {
        let lan = classify("192.168.0.20".parse().unwrap(), &HeaderMap::new(), None);
        assert_eq!(lan.via, Via::Lan);
        assert_eq!(lan.key, "192.168.0.20");
        assert!(lan.is_household() && !lan.is_local_machine());
        let local = classify("127.0.0.1".parse().unwrap(), &HeaderMap::new(), None);
        assert!(local.is_local_machine());
        let mapped = classify(
            "::ffff:192.168.0.20".parse().unwrap(),
            &HeaderMap::new(),
            None,
        );
        assert_eq!(mapped.via, Via::Lan);
        assert_eq!(mapped.key, "192.168.0.20");
        let ula = classify("fd00::1".parse().unwrap(), &HeaderMap::new(), None);
        assert_eq!(ula.via, Via::Lan);
        let public = classify("8.8.8.8".parse().unwrap(), &HeaderMap::new(), None);
        assert_eq!(public.via, Via::Outside);
        assert!(!public.is_household());
        // A proxy on this machine would make everyone local: relayed is a stranger.
        for relay in ["x-forwarded-for", "forwarded", "tailscale-user-login"] {
            let mut headers = HeaderMap::new();
            headers.insert(relay, "100.64.0.7".parse().unwrap());
            let relayed = classify("127.0.0.1".parse().unwrap(), &headers, None);
            assert_eq!(relayed.via, Via::Outside, "{relay}");
            assert!(!relayed.is_household() && !relayed.is_local_machine());
        }
    }

    #[test]
    fn only_a_report_path_names_a_phone() {
        assert_eq!(
            phone_report("/api/phones/pab12cd34/report"),
            Some("pab12cd34")
        );
        assert_eq!(phone_report("/api/phones/../devices/report"), None);
        assert_eq!(phone_report("/api/phones/p1/report/x"), None);
        assert_eq!(phone_report("/api/devices"), None);
    }

    #[test]
    fn the_network_cannot_pose_as_the_tunnel() {
        let cf = headers(&[("cf-ray", "x"), ("cf-connecting-ip", "203.0.113.9")]);
        let posing = classify(
            "192.168.0.30".parse().unwrap(),
            &cf,
            Some("moi@example.org".into()),
        );
        assert_eq!(
            posing.via,
            Via::Outside,
            "headers from the LAN are not Cloudflare's"
        );
        assert_eq!(
            posing.key, "192.168.0.30",
            "keyed by its real address, not a header"
        );
    }

    #[test]
    fn the_tunnel_is_trusted_only_with_a_verified_person() {
        // The tunnel ends on this machine: without proof, a stranger, and
        // never the CLI.
        let cf = headers(&[("cf-ray", "8c…-CDG"), ("cf-connecting-ip", "203.0.113.7")]);
        let stranger = classify("127.0.0.1".parse().unwrap(), &cf, None);
        assert_eq!(stranger.via, Via::Outside);
        assert_eq!(stranger.key, "cf:203.0.113.7");
        assert!(!stranger.is_household() && !stranger.is_local_machine());
        let owner = classify(
            "127.0.0.1".parse().unwrap(),
            &cf,
            Some("moi@example.org".into()),
        );
        assert_eq!(
            owner.via,
            Via::Access {
                email: "moi@example.org".into()
            }
        );
        assert_eq!(owner.key, "access:moi@example.org");
        assert!(owner.is_household() && !owner.is_local_machine());
        assert_eq!(owner.describe(), "tableau de bord (moi@example.org)");
    }
}

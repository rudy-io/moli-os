//! Who is calling, established once per request before any handler.
//!
//! - the home network or this very machine: the household (and, for this
//!   machine only, the CLI);
//! - through Cloudflare (the tunnel) with a valid Access assertion: the
//!   person Access let in, by e-mail, unless that e-mail is of someone no
//!   longer of the house (then a stranger);
//! - anything else (Internet without proof, public address): a stranger;
//! - except on a demo house (`[server] demo = true`, a house without real
//!   devices): there, a stranger is a visitor, let in like the household,
//!   but nothing that would change the house for the next visitors (its
//!   code, names, plan, assistant key) is open to anyone.
//!
//! Human sessions (PIN) and failure counters are bound to [`Caller::key`]:
//! the address on the home network, the e-mail through Access (a phone's
//! public address changes all the time).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::access::Verifier;
use crate::people::{People, PersonRef, Vouched};
use crate::remote::RemoteConfig;

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
    /// Someone trying a demo house: anyone, anywhere.
    Visitor,
    /// From the Internet, a person signed in with their password.
    Person,
}

/// What [`identify`] needs: the Access verifier, and whether this house is
/// a demo.
#[derive(Clone)]
pub(crate) struct Gate {
    pub(crate) access: Option<Arc<Verifier>>,
    pub(crate) demo: bool,
    pub(crate) people: Arc<People>,
    /// `[server.access] known_only`: an e-mail none of the people has is a
    /// stranger's.
    pub(crate) known_only: bool,
    /// `[server.remote]`: the declared VPNs and proxies.
    pub(crate) remote: Arc<RemoteConfig>,
}

/// What a guest may reach: the dashboard, what it reads of its rooms (the
/// routes keep only those: devices, their history, the live stream, the
/// orders held for them), itself among the people, the house's layout,
/// plan and sky; command in its rooms (checked by the routes), sign in or
/// out, its own password. Nothing else of the house: no camera, no journal,
/// no setting, nobody's whereabouts, no phone of its own (the house's
/// notifications are the household's).
fn guest_allowed(method: &Method, path: &str) -> bool {
    if method == Method::GET || method == Method::HEAD {
        let api = path.starts_with("/api/") || path == "/api" || path.starts_with("/mcp");
        return !api
            || matches!(
                path,
                "/api/session"
                    | "/api/health"
                    | "/api/home"
                    | "/api/ambiances"
                    | "/api/devices"
                    | "/api/history"
                    | "/api/approvals"
                    | "/api/events"
                    | "/api/people"
                    | "/api/plan"
            )
            || (path.starts_with("/api/devices/") && !path.ends_with("/snapshot"))
            || path.starts_with("/api/plan/images/")
            || path.starts_with("/api/weather/");
    }
    matches!(path, "/api/command" | "/api/ambiance" | "/api/session")
        || (path.starts_with("/api/people/") && path.ends_with("/password"))
}

/// What a stranger may reach: the dashboard's files (its sign-in page) and
/// signing in. Nothing that reads or changes the house.
fn public(method: &Method, path: &str) -> bool {
    let api = path.starts_with("/api/") || path == "/api" || path.starts_with("/mcp");
    (!api && (method == Method::GET || method == Method::HEAD))
        || path == "/api/session"
        || (path == "/api/invitation" && method == Method::POST)
}

/// What a visitor may not change in a demo house: everything but playing
/// with it (orders, looks, the published code, approving what was held,
/// trying or running its routines, agents over MCP, whose lasting tools
/// refuse there). Anything else would change the house for every visitor
/// after them (its code, the names, the plan, the routines, the people). A
/// demo comes back as it was at its next start anyway.
fn demo_locked(method: &Method, path: &str) -> bool {
    if *method == Method::GET || *method == Method::HEAD {
        return false;
    }
    let one = |prefix: &str, suffix: &str| {
        path.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
    };
    let plays = matches!(
        path,
        "/api/command" | "/api/ambiance" | "/api/session" | "/api/automations/check" | "/mcp"
    ) || one("/api/approvals/", "")
        || one("/api/automations/", "/test")
        || one("/api/automations/", "/run");
    !plays
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caller {
    pub(crate) via: Via,
    /// What sessions and failure counters are bound to.
    pub(crate) key: String,
    /// The person, when Moli knows who it is (Access e-mail, password).
    pub(crate) person: Option<PersonRef>,
    /// Came in through `[server.remote]` (a declared VPN or proxy): what
    /// stays at home is refused ([`crate::remote::home_only`]).
    pub(crate) remote: bool,
}

impl Caller {
    /// The household: home network, this machine, or a person Access let in.
    pub(crate) fn is_household(&self) -> bool {
        matches!(
            self.via,
            Via::Lan | Via::Loopback | Via::Access { .. } | Via::Visitor | Via::Person
        )
    }

    /// This very machine, not through the tunnel (which also ends here).
    pub(crate) fn is_local_machine(&self) -> bool {
        self.via == Via::Loopback
    }

    /// Who asks, for what is kept per person: the person when the house
    /// knows them (`person:<id>`), else this caller's own key.
    pub(crate) fn asker(&self) -> String {
        self.person
            .as_ref()
            .map_or_else(|| self.key.clone(), |p| format!("person:{}", p.id))
    }

    /// For the journal: who clicked.
    pub(crate) fn describe(&self) -> String {
        if let Some(p) = &self.person {
            return format!("tableau de bord ({})", p.name);
        }
        match &self.via {
            Via::Access { email } => format!("tableau de bord ({email})"),
            Via::Visitor => "tableau de bord (visiteur de la démo)".to_owned(),
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
            Via::Visitor => "visitor",
            Via::Person => "person",
        }
    }
}

/// Requests from Cloudflare carry these. They mean something only when the
/// request comes out of the tunnel, i.e. from this very machine; a device of
/// the network that adds them makes itself a stranger (and is refused).
fn through_cloudflare(headers: &HeaderMap) -> bool {
    headers.contains_key("cf-ray") || headers.contains_key("cf-connecting-ip")
}

/// A proxy (a reverse proxy, `tailscale serve`), on this very machine or on
/// another of the home network, would make everyone look local or of the
/// house, hence trusted: a request that says it was relayed is a stranger's.
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
pub(crate) fn classify(
    peer: IpAddr,
    headers: &HeaderMap,
    access: Option<String>,
    remote: &RemoteConfig,
) -> Caller {
    // `::ffff:192.168.0.20` is the home network too.
    let mapped = peer.to_canonical();
    if through_cloudflare(headers) {
        // The tunnel ends on this very machine: only from there are
        // Cloudflare's headers Cloudflare's. Keys never come from a header
        // a client could choose, except through the tunnel.
        return match (mapped.is_loopback(), access) {
            (true, Some(email)) => Caller {
                remote: false,
                key: format!("access:{email}"),
                via: Via::Access { email },
                person: None,
            },
            (true, None) => Caller {
                remote: false,
                key: format!(
                    "cf:{}",
                    header(headers, "cf-connecting-ip")
                        .and_then(|ip| ip.parse::<IpAddr>().ok())
                        .map_or_else(|| "?".to_owned(), |ip| ip.to_string())
                ),
                via: Via::Outside,
                person: None,
            },
            (false, _) => Caller {
                remote: false,
                key: mapped.to_string(),
                via: Via::Outside,
                person: None,
            },
        };
    }
    // A declared proxy relaying: a stranger until a person signs in, known
    // by the client address the proxy added (never one the client wrote).
    if let Some(key) = remote.proxied(mapped, headers) {
        return Caller {
            via: Via::Outside,
            key,
            person: None,
            remote: true,
        };
    }
    let in_vpn = !mapped.is_loopback() && remote.in_vpn(mapped);
    let via = if relayed(headers) {
        Via::Outside
    } else if in_vpn {
        Via::Lan
    } else if mapped.is_loopback() {
        Via::Loopback
    } else if is_home(mapped) {
        Via::Lan
    } else {
        Via::Outside
    };
    Caller {
        remote: in_vpn && via == Via::Lan,
        via,
        key: mapped.to_string(),
        person: None,
    }
}

/// Who an Access assertion names. Access's rule may still let in someone
/// no longer of the house (a guest whose stay is over, a person removed):
/// a stranger then, never the household.
fn vouch(gate: &Gate, caller: &mut Caller) {
    let Via::Access { email } = &caller.via else {
        return;
    };
    match gate.people.vouched(email, gate.known_only) {
        Vouched::Person(person) => caller.person = Some(person),
        Vouched::Household => {}
        Vouched::Stranger => caller.via = Via::Outside,
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
    State(gate): State<Gate>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip());
    let headers = request.headers();
    let from_tunnel = through_cloudflare(headers) && peer.to_canonical().is_loopback();
    let email = match (&gate.access, from_tunnel) {
        (Some(verifier), true) => match header(headers, "cf-access-jwt-assertion") {
            Some(token) => verifier.identity(token).await,
            None => None,
        },
        _ => None,
    };
    let mut caller = classify(peer, request.headers(), email, &gate.remote);
    // The person Access vouched for, or a stranger if no longer of the house.
    vouch(&gate, &mut caller);
    // A phone's report carries its own token (checked by the route): the
    // one way in without the home network or Cloudflare Access.
    if caller.via == Via::Outside
        && let Some(id) = phone_report(request.uri().path())
        && request
            .headers()
            .contains_key(axum::http::header::AUTHORIZATION)
    {
        caller = Caller {
            remote: caller.remote,
            key: format!("phone:{id}"),
            via: Via::Phone,
            person: None,
        };
    }
    // Who it is, when Access named nobody: the person signed in on this
    // browser (from anywhere: a phone moves).
    if caller.person.is_none()
        && let Some(person) = gate.people.signed_in(request.headers())
    {
        if caller.via == Via::Outside {
            caller.via = Via::Person;
            caller.key = format!("person:{}", person.id);
        }
        caller.person = Some(person);
    }
    if caller
        .person
        .as_ref()
        .is_some_and(crate::people::PersonRef::is_guest)
        && !guest_allowed(request.method(), request.uri().path())
    {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(
                serde_json::json!({ "error": moli_i18n::tr!("serveur.personnes.invite_limite") }),
            ),
        )
            .into_response();
    }
    // A demo house lets anyone in, as a visitor, but keeps itself as it is.
    if gate.demo {
        if demo_locked(request.method(), request.uri().path()) {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({ "error": moli_i18n::tr!("serveur.demo.reserve") })),
            )
                .into_response();
        }
        if caller.via == Via::Outside {
            caller.via = Via::Visitor;
        }
    }
    // What stays at home is refused to whoever came in from outside (I4).
    if caller.remote && crate::remote::home_only(request.uri().path()) {
        tracing::warn!(client = caller.key, path = %request.uri().path(), "remote refused");
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": moli_i18n::tr!("serveur.acces.a_la_maison") })),
        )
            .into_response();
    }
    // Strangers get nothing: not a reading, not a PIN attempt. Moli is the
    // home network's, and Cloudflare Access's for the people it lets in.
    if caller.via == Via::Outside && !public(request.method(), request.uri().path()) {
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
    fn a_demo_visitor_plays_but_changes_nothing_that_lasts() {
        for (method, path) in [
            (Method::POST, "/api/command"),
            (Method::POST, "/api/ambiance"),
            (Method::POST, "/api/session"),
            (Method::DELETE, "/api/session"),
            (Method::POST, "/api/approvals/42"),
            (Method::POST, "/api/automations/check"),
            (Method::POST, "/api/automations/soir/test"),
            (Method::POST, "/api/automations/soir/run"),
            (Method::POST, "/mcp"),
            (Method::GET, "/api/devices"),
        ] {
            assert!(!demo_locked(&method, path), "{method} {path}");
        }
        for (method, path) in [
            (Method::PUT, "/api/labels/hue:lampe"),
            (Method::POST, "/api/automations"),
            (Method::PUT, "/api/automations/soir"),
            (Method::POST, "/api/automations/soir/approve"),
            (Method::POST, "/api/automations/draft"),
            (Method::PUT, "/api/assistant/recording"),
            (Method::POST, "/api/assistant/recap"),
            (Method::POST, "/api/assistant/proposals/3"),
            (Method::POST, "/api/assistant/turns/3/undo"),
            (Method::POST, "/api/assistant/turns/3/correct"),
            (Method::PUT, "/api/assistant/rules/3"),
            (Method::DELETE, "/api/assistant/rules/3"),
            (Method::PUT, "/api/session/pin"),
            (Method::PUT, "/api/energy/price"),
            (Method::POST, "/api/session/setup"),
            (Method::POST, "/api/machines/pc/remote"),
            (Method::POST, "/api/automations/soir/run/again"),
            (Method::POST, "/api/approvals/"),
        ] {
            assert!(demo_locked(&method, path), "{method} {path}");
        }
    }

    #[test]
    fn the_home_network_is_the_household() {
        let lan = classify(
            "192.168.0.20".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &RemoteConfig::default(),
        );
        assert_eq!(lan.via, Via::Lan);
        assert_eq!(lan.key, "192.168.0.20");
        assert!(lan.is_household() && !lan.is_local_machine());
        let local = classify(
            "127.0.0.1".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &RemoteConfig::default(),
        );
        assert!(local.is_local_machine());
        let mapped = classify(
            "::ffff:192.168.0.20".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &RemoteConfig::default(),
        );
        assert_eq!(mapped.via, Via::Lan);
        assert_eq!(mapped.key, "192.168.0.20");
        let ula = classify(
            "fd00::1".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &RemoteConfig::default(),
        );
        assert_eq!(ula.via, Via::Lan);
        let public = classify(
            "8.8.8.8".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &RemoteConfig::default(),
        );
        assert_eq!(public.via, Via::Outside);
        assert!(!public.is_household());
        // A proxy on this machine would make everyone local: relayed is a stranger.
        for relay in ["x-forwarded-for", "forwarded", "tailscale-user-login"] {
            let mut headers = HeaderMap::new();
            headers.insert(relay, "100.64.0.7".parse().unwrap());
            let relayed = classify(
                "127.0.0.1".parse().unwrap(),
                &headers,
                None,
                &RemoteConfig::default(),
            );
            assert_eq!(relayed.via, Via::Outside, "{relay}");
            assert!(!relayed.is_household() && !relayed.is_local_machine());
        }
    }

    #[test]
    fn a_proxy_elsewhere_on_the_home_network_does_not_make_the_internet_the_household() {
        // A reverse proxy on another machine of the house relays the Internet
        // from a private address: what it relays is a stranger's, like on
        // this machine (the people still sign in with their password).
        for peer in ["192.168.0.5", "10.0.0.2", "fd00::5"] {
            for relay in [
                "x-forwarded-for",
                "forwarded",
                "x-real-ip",
                "tailscale-user-login",
            ] {
                let mut headers = HeaderMap::new();
                headers.insert(relay, "203.0.113.9".parse().unwrap());
                let relayed = classify(
                    peer.parse().unwrap(),
                    &headers,
                    None,
                    &RemoteConfig::default(),
                );
                assert_eq!(relayed.via, Via::Outside, "{peer} {relay}");
                assert!(!relayed.is_household(), "{peer} {relay}");
            }
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
    fn a_guest_reaches_its_rooms_and_the_dashboard_only() {
        for path in [
            "/",
            "/assets/index.js",
            "/api/devices",
            "/api/devices/hue:1",
            "/api/events",
            "/api/history",
            "/api/home",
            "/api/plan",
            "/api/weather/forecast",
            "/api/session",
        ] {
            assert!(guest_allowed(&Method::GET, path), "{path}");
        }
        for path in [
            "/api/devices/hue:1/snapshot",
            "/api/journal",
            "/api/assistant",
            "/api/assistant/exchanges",
            "/api/assistant/recordings/1-1.wav",
            "/api/assistant/rules",
            "/api/assistant/turns/3/choices",
            "/api/machines/machines:pc",
            "/api/energy",
            "/api/energy/series",
            "/api/bench",
            "/api/system",
            "/api/automations",
            "/api/mobile/phones",
            "/mcp",
        ] {
            assert!(!guest_allowed(&Method::GET, path), "{path}");
        }
        assert!(guest_allowed(&Method::POST, "/api/command"));
        assert!(!guest_allowed(&Method::POST, "/api/mobile/register"));
        assert!(!guest_allowed(&Method::PUT, "/api/zones"));
        assert!(!guest_allowed(&Method::POST, "/api/people"));
    }

    #[test]
    fn the_network_cannot_pose_as_the_tunnel() {
        let cf = headers(&[("cf-ray", "x"), ("cf-connecting-ip", "203.0.113.9")]);
        let posing = classify(
            "192.168.0.30".parse().unwrap(),
            &cf,
            Some("moi@example.org".into()),
            &RemoteConfig::default(),
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
        let stranger = classify(
            "127.0.0.1".parse().unwrap(),
            &cf,
            None,
            &RemoteConfig::default(),
        );
        assert_eq!(stranger.via, Via::Outside);
        assert_eq!(stranger.key, "cf:203.0.113.7");
        assert!(!stranger.is_household() && !stranger.is_local_machine());
        let owner = classify(
            "127.0.0.1".parse().unwrap(),
            &cf,
            Some("moi@example.org".into()),
            &RemoteConfig::default(),
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

    fn remote() -> RemoteConfig {
        toml_like(&["10.6.0.0/24"], &["192.168.1.5"])
    }

    fn toml_like(vpn: &[&str], proxies: &[&str]) -> RemoteConfig {
        RemoteConfig {
            vpn: vpn
                .iter()
                .map(|n| crate::remote::Net::try_from((*n).to_owned()).unwrap())
                .collect(),
            proxies: proxies.iter().map(|p| p.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn a_declared_vpn_is_the_household_from_outside() {
        let remote = remote();
        let phone = classify(
            "10.6.0.7".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &remote,
        );
        assert_eq!(phone.via, Via::Lan);
        assert!(phone.is_household() && phone.remote);
        // Nothing declared: as before, the home network, not from outside.
        let home = classify(
            "192.168.0.20".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &remote,
        );
        assert!(home.is_household() && !home.remote);
        let undeclared = classify(
            "10.6.0.7".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &RemoteConfig::default(),
        );
        assert!(undeclared.is_household() && !undeclared.remote);
        // A VPN caller that says it was relayed is a stranger, like anyone.
        let mut relayed = HeaderMap::new();
        relayed.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        let posing = classify("10.6.0.7".parse().unwrap(), &relayed, None, &remote);
        assert_eq!(posing.via, Via::Outside);
    }

    #[test]
    fn a_declared_proxy_relays_strangers_known_by_the_hop_it_added() {
        let remote = remote();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "192.168.0.20, 203.0.113.9".parse().unwrap(),
        );
        let client = classify("192.168.1.5".parse().unwrap(), &headers, None, &remote);
        assert_eq!(client.via, Via::Outside);
        assert_eq!(client.key, "203.0.113.9", "the hop the proxy added");
        assert!(client.remote && !client.is_household());
        // Two clients behind the proxy are counted apart.
        let mut other = HeaderMap::new();
        other.insert("x-forwarded-for", "198.51.100.4".parse().unwrap());
        let other = classify("192.168.1.5".parse().unwrap(), &other, None, &remote);
        assert_ne!(other.key, client.key);
        // The proxy's machine itself, relaying nothing: the home network.
        let itself = classify(
            "192.168.1.5".parse().unwrap(),
            &HeaderMap::new(),
            None,
            &remote,
        );
        assert!(itself.is_household() && !itself.remote);
        // Another address of the network relaying: a stranger, not remote.
        let undeclared = classify("192.168.1.6".parse().unwrap(), &headers, None, &remote);
        assert_eq!(undeclared.via, Via::Outside);
        assert_eq!(undeclared.key, "192.168.1.6");
    }
}

//! Signing in to the GRDF customer area as its web page does, then asking its
//! API. GRDF signs in through Okta (Identity Engine): the home page leads to a
//! sign-in page holding a state token; the e-mail address, then the password,
//! are answered to `connexion.grdf.fr`; the success address, followed, leaves
//! the session cookies on `monespace.grdf.fr`.
//!
//! Interface facts (addresses, fields, steps) from the open library PyGazpar,
//! read as a reference; no code of theirs. Nothing here leaves `grdf.fr`: a
//! redirection anywhere else stops the sign-in, cookies and all.

use std::time::Duration;

use bytes::Bytes;
use http::header::{
    ACCEPT, CONTENT_TYPE, COOKIE, HOST, HeaderMap, HeaderValue, LOCATION, USER_AGENT,
};
use http::{Method, StatusCode};
use http_body_util::Full;
use jiff::civil::Date;
use moli_runtime::BoxFuture;
use serde_json::Value as Json;

const HOME: &str = "monespace.grdf.fr";
const IDENTIFY: &str = "https://connexion.grdf.fr/idp/idx/identify";
const ANSWER: &str = "https://connexion.grdf.fr/idp/idx/challenge/answer";
/// What the sign-in pages accept: a browser.
const AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
const OKTA: &str = "application/json; okta-version=1.0.0";
const LIMIT: Duration = Duration::from_secs(45);
/// A year of days is about 250 kB; the sign-in pages are smaller.
const MAX_BODY: usize = 4 << 20;
const MAX_REDIRECTS: usize = 10;

/// One HTTPS exchange with `host` on port 443: the network, or a test's fake.
pub(crate) trait Web: Send + Sync {
    fn send<'a>(
        &'a self,
        host: &'a str,
        request: http::Request<Full<Bytes>>,
    ) -> BoxFuture<'a, anyhow::Result<(StatusCode, HeaderMap, Bytes)>>;
}

/// The real network, checked against the public CAs.
#[derive(Debug)]
pub(crate) struct Internet;

impl Web for Internet {
    fn send<'a>(
        &'a self,
        host: &'a str,
        request: http::Request<Full<Bytes>>,
    ) -> BoxFuture<'a, anyhow::Result<(StatusCode, HeaderMap, Bytes)>> {
        Box::pin(moli_net::web_with_headers(
            host, 443, true, request, LIMIT, MAX_BODY,
        ))
    }
}

/// Why GRDF gave nothing. Never a detail of the request nor of the answer:
/// they carry the address, the meter and the session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The e-mail address or the password is refused.
    Refused,
    /// GRDF asks for another step (a code, another way to sign in).
    MoreSteps,
    /// GRDF cannot be reached.
    Network,
    /// GRDF asks to wait (429).
    Throttled,
    /// The session ended right after signing in.
    SessionLost,
    /// A redirection outside `grdf.fr`: its host only.
    Elsewhere(String),
    /// Any other answer: its status.
    Status(u16),
    /// An answer that does not read as expected.
    Unreadable,
}

/// An `https://` address on `grdf.fr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub host: String,
    /// Path and query, `/` at least.
    pub path: String,
}

fn ours(host: &str) -> bool {
    host == "grdf.fr" || host.ends_with(".grdf.fr")
}

impl Target {
    fn https(text: &str) -> Result<Self, Failure> {
        let rest = text.strip_prefix("https://").ok_or(Failure::Unreadable)?;
        let rest = rest.split('#').next().unwrap_or_default();
        let (host, path) = match rest.find(['/', '?']) {
            Some(at) if rest[at..].starts_with('/') => (&rest[..at], rest[at..].to_owned()),
            Some(at) => (&rest[..at], format!("/{}", &rest[at..])),
            None => (rest, "/".to_owned()),
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty() || host.contains([':', '@']) {
            return Err(Failure::Unreadable);
        }
        if !ours(&host) {
            return Err(Failure::Elsewhere(host));
        }
        Ok(Self { host, path })
    }

    /// Where a `Location` header leads from here.
    fn follow(&self, location: &str) -> Result<Self, Failure> {
        let location = location.trim();
        if location.starts_with("https://") {
            return Self::https(location);
        }
        if location.contains("://") || location.starts_with("//") {
            // Plain HTTP, or another scheme: never with the session.
            return Err(Failure::Unreadable);
        }
        let location = location.split('#').next().unwrap_or_default();
        let path = if location.starts_with('/') {
            location.to_owned()
        } else {
            let dir = self.path.split('?').next().unwrap_or("/");
            let dir = &dir[..=dir.rfind('/').unwrap_or(0)];
            format!("{dir}{location}")
        };
        Ok(Self {
            host: self.host.clone(),
            path,
        })
    }
}

/// One cookie, as the answers set it.
#[derive(Clone)]
struct Cookie {
    /// The host that set it, or the `Domain` it names (without the dot).
    domain: String,
    /// Sent to `domain` only, not to its subdomains (no `Domain` attribute).
    host_only: bool,
    name: String,
    value: String,
}

/// The session's cookies, sent only where they belong.
#[derive(Clone, Default)]
pub(crate) struct Jar(Vec<Cookie>);

impl std::fmt::Debug for Jar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Names only: the values are the session.
        f.debug_list()
            .entries(self.0.iter().map(|c| &c.name))
            .finish()
    }
}

fn matches(cookie: &Cookie, host: &str) -> bool {
    host == cookie.domain || (!cookie.host_only && host.ends_with(&format!(".{}", cookie.domain)))
}

impl Jar {
    fn put(&mut self, cookie: Cookie, gone: bool) {
        self.0
            .retain(|c| !(c.name == cookie.name && c.domain == cookie.domain));
        if !gone {
            self.0.push(cookie);
        }
    }

    /// The cookies an answer from `host` sets.
    fn absorb(&mut self, host: &str, headers: &HeaderMap) {
        for header in headers.get_all(http::header::SET_COOKIE) {
            let Ok(text) = header.to_str() else { continue };
            let mut parts = text.split(';');
            let Some((name, value)) = parts.next().and_then(|first| first.split_once('=')) else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let mut domain = None;
            let mut gone = false;
            for attribute in parts {
                let (key, val) = attribute.split_once('=').unwrap_or((attribute, ""));
                let (key, val) = (key.trim().to_ascii_lowercase(), val.trim());
                match key.as_str() {
                    "domain" => domain = Some(val.trim_start_matches('.').to_ascii_lowercase()),
                    "max-age" => gone |= val.parse::<i64>().is_ok_and(|s| s <= 0),
                    "expires" => gone |= val.contains("1970"),
                    _ => {}
                }
            }
            let domain = domain.filter(|d| !d.is_empty());
            // A cookie for another site than the one answering is refused.
            if domain
                .as_ref()
                .is_some_and(|d| host != d && !host.ends_with(&format!(".{d}")))
            {
                continue;
            }
            let cookie = Cookie {
                host_only: domain.is_none(),
                domain: domain.unwrap_or_else(|| host.to_owned()),
                name: name.to_owned(),
                value: value.trim().trim_matches('"').to_owned(),
            };
            self.put(cookie, gone);
        }
    }

    fn header(&self, host: &str) -> Option<HeaderValue> {
        let text = self
            .0
            .iter()
            .filter(|c| matches(c, host))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        let mut value = HeaderValue::from_str(&text)
            .ok()
            .filter(|_| !text.is_empty())?;
        value.set_sensitive(true);
        Some(value)
    }
}

/// `\x2D` and `-` written by the page's script, read back.
fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits = match chars.next()? {
            'x' => 2,
            'u' => 4,
            other => {
                out.push(other);
                continue;
            }
        };
        let hex: String = chars.by_ref().take(digits).collect();
        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
    }
    Some(out)
}

/// The state token the sign-in page hands its script.
pub(crate) fn state_token(page: &str) -> Option<String> {
    let after = &page[page.find("\"stateToken\"")? + "\"stateToken\"".len()..];
    let after = after
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let raw = &after[..after.find('"')?];
    unescape(raw).filter(|t| !t.is_empty())
}

/// The address of a step Okta offers next (`challenge-authenticator`).
pub(crate) fn remediation(answer: &Json, name: &str) -> Option<String> {
    answer["remediation"]["value"]
        .as_array()?
        .iter()
        .find(|step| step["name"] == name)?["href"]
        .as_str()
        .map(str::to_owned)
}

/// A signed-in session with the GRDF customer area.
pub(crate) struct Session<'w> {
    web: &'w dyn Web,
    jar: Jar,
}

impl std::fmt::Debug for Session<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("jar", &self.jar)
            .finish_non_exhaustive()
    }
}

/// A 401 or 403 is the e-mail address or the password; a 400 is a request
/// GRDF no longer understands (its sign-in changed), not a refusal.
fn refused_or(status: StatusCode) -> Failure {
    match status.as_u16() {
        401 | 403 => Failure::Refused,
        429 => Failure::Throttled,
        other => Failure::Status(other),
    }
}

fn is_html(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"))
}

impl<'w> Session<'w> {
    /// Signs in with this e-mail address and password.
    pub async fn open(web: &'w dyn Web, email: &str, password: &str) -> Result<Self, Failure> {
        let mut session = Self {
            web,
            jar: Jar::default(),
        };
        let home = Target {
            host: HOME.to_owned(),
            path: "/".to_owned(),
        };
        let (status, _, page) = session.get(home, "text/html,*/*").await?;
        if !status.is_success() {
            return Err(refused_or(status).not_refused());
        }
        let page = String::from_utf8_lossy(&page);
        let token = state_token(&page).ok_or(Failure::Unreadable)?;
        // The page's own script leaves the address in this cookie.
        session.jar.put(
            Cookie {
                domain: "grdf.fr".to_owned(),
                host_only: false,
                name: "ln".to_owned(),
                value: email.to_owned(),
            },
            false,
        );
        let identify = Target::https(IDENTIFY)?;
        let body = serde_json::json!({ "identifier": email, "stateHandle": token });
        let answer = session.okta(&identify, &body).await?;
        let handle = answer["stateHandle"].as_str().ok_or(Failure::Unreadable)?;
        let Some(href) = remediation(&answer, "challenge-authenticator") else {
            return Err(Failure::MoreSteps);
        };
        let challenge = Target::https(&href).or_else(|_| Target::https(ANSWER))?;
        let body =
            serde_json::json!({ "credentials": { "passcode": password }, "stateHandle": handle });
        let answer = session.okta(&challenge, &body).await?;
        let Some(success) = answer["success"]["href"].as_str() else {
            // Okta asks for something else after the password: a code.
            return Err(Failure::MoreSteps);
        };
        let (status, _, _) = session
            .get(Target::https(success)?, "text/html,*/*")
            .await?;
        if !status.is_success() {
            return Err(refused_or(status).not_refused());
        }
        Ok(session)
    }

    /// One exchange, cookies sent and kept.
    async fn exchange(
        &mut self,
        method: Method,
        target: &Target,
        accept: &str,
        body: Option<&Json>,
    ) -> Result<(StatusCode, HeaderMap, Bytes), Failure> {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(target.path.as_str())
            .header(HOST, target.host.as_str())
            .header(USER_AGENT, AGENT)
            .header(ACCEPT, accept);
        if let Some(cookies) = self.jar.header(&target.host) {
            builder = builder.header(COOKIE, cookies);
        }
        let request = match body {
            Some(json) => moli_net::json_body(builder, json),
            None => moli_net::empty(builder),
        }
        .map_err(|_| Failure::Unreadable)?;
        // Underlying errors are dropped on purpose: no request detail in a status.
        let (status, headers, body) = self
            .web
            .send(&target.host, request)
            .await
            .map_err(|_| Failure::Network)?;
        self.jar.absorb(&target.host, &headers);
        Ok((status, headers, body))
    }

    /// A page, redirections followed (on `grdf.fr` only).
    async fn get(
        &mut self,
        mut target: Target,
        accept: &str,
    ) -> Result<(StatusCode, HeaderMap, Bytes), Failure> {
        for _ in 0..=MAX_REDIRECTS {
            let (status, headers, body) = self.exchange(Method::GET, &target, accept, None).await?;
            if !status.is_redirection() {
                return Ok((status, headers, body));
            }
            let location = headers
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(Failure::Unreadable)?;
            target = target.follow(location)?;
        }
        Err(Failure::Unreadable)
    }

    /// One step of the sign-in, its JSON answer.
    async fn okta(&mut self, target: &Target, body: &Json) -> Result<Json, Failure> {
        let (status, _, answer) = self
            .exchange(Method::POST, target, OKTA, Some(body))
            .await?;
        if !status.is_success() {
            return Err(refused_or(status));
        }
        serde_json::from_slice(&answer).map_err(|_| Failure::Unreadable)
    }

    /// The meter's daily readings (« données informatives ») between two days.
    pub async fn informative(
        &mut self,
        pce: &str,
        first: Date,
        last: Date,
    ) -> Result<Bytes, Failure> {
        let target = Target {
            host: HOME.to_owned(),
            path: format!(
                "/api/e-conso/pce/consommation/informatives?dateDebut={first}&dateFin={last}&pceList%5B%5D={pce}"
            ),
        };
        let (status, headers, body) = self
            .exchange(Method::GET, &target, "application/json", None)
            .await?;
        // A redirection to the sign-in page, or that page itself: no session.
        if status.as_u16() == 401 || status.is_redirection() {
            return Err(Failure::SessionLost);
        }
        if !status.is_success() {
            return Err(refused_or(status).not_refused());
        }
        if is_html(&headers) {
            return Err(Failure::SessionLost);
        }
        Ok(body)
    }
}

impl Failure {
    /// Outside the sign-in steps, a 400 or 403 says nothing of the password.
    fn not_refused(self) -> Self {
        match self {
            Self::Refused => Self::Status(403),
            other => other,
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A GRDF that answers from a script, for the tests.

    use std::sync::Mutex;

    use super::{Bytes, Full, HeaderMap, StatusCode, Web};
    use moli_runtime::BoxFuture;

    #[derive(Clone, Debug)]
    pub struct Reply {
        pub status: u16,
        pub headers: Vec<(&'static str, String)>,
        pub body: String,
    }

    pub fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    impl Reply {
        pub fn with(mut self, name: &'static str, value: &str) -> Self {
            self.headers.push((name, value.to_owned()));
            self
        }
    }

    /// What was asked: method, host, path, cookies, body.
    #[derive(Clone, Debug)]
    pub struct Asked {
        pub method: String,
        pub host: String,
        pub path: String,
        pub cookie: Option<String>,
        pub body: String,
    }

    #[derive(Debug, Default)]
    pub struct Fake {
        /// Answers by `host + path` prefix, taken in order.
        script: Mutex<Vec<(String, Reply)>>,
        pub asked: Mutex<Vec<Asked>>,
    }

    impl Fake {
        pub fn on(self, url: &str, reply: Reply) -> Self {
            self.script.lock().unwrap().push((url.to_owned(), reply));
            self
        }

        pub fn asked(&self) -> Vec<Asked> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Web for Fake {
        fn send<'a>(
            &'a self,
            host: &'a str,
            request: http::Request<Full<Bytes>>,
        ) -> BoxFuture<'a, anyhow::Result<(StatusCode, HeaderMap, Bytes)>> {
            Box::pin(async move {
                use http_body_util::BodyExt as _;
                let (head, body) = request.into_parts();
                let body = body.collect().await?.to_bytes();
                let path = head.uri.to_string();
                self.asked.lock().unwrap().push(Asked {
                    method: head.method.to_string(),
                    host: host.to_owned(),
                    path: path.clone(),
                    cookie: head
                        .headers
                        .get(http::header::COOKIE)
                        .map(|v| v.to_str().unwrap().to_owned()),
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let url = format!("{host}{path}");
                let mut script = self.script.lock().unwrap();
                let at = script
                    .iter()
                    .position(|(prefix, _)| url.starts_with(prefix.as_str()))
                    .ok_or_else(|| anyhow::anyhow!("unscripted {url}"))?;
                let (_, reply) = script.remove(at);
                let mut headers = HeaderMap::new();
                for (name, value) in reply.headers {
                    headers.append(name, value.parse()?);
                }
                Ok((
                    StatusCode::from_u16(reply.status)?,
                    headers,
                    Bytes::from(reply.body),
                ))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, reply};
    use super::*;

    const START: &str = "monespace.grdf.fr/";

    fn identify() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../integrations/grdf/fixtures/identify.json"
        ))
        .unwrap()
    }

    /// The home page leads to Okta's sign-in page, as GRDF's does.
    fn signing_in() -> Fake {
        Fake::default()
            .on(
                START,
                reply(302, "")
                    .with("location", "https://connexion.grdf.fr/oauth2/default/v1/authorize?client_id=x")
                    .with("set-cookie", "home=1; Path=/; Secure; HttpOnly"),
            )
            .on(
                "connexion.grdf.fr/oauth2/",
                reply(302, "").with("location", "/signin?from=authorize"),
            )
            .on(
                "connexion.grdf.fr/signin",
                reply(200, r#"<script>var oktaData = {"signIn":{"stateToken":"02.id.ab\x2Dcd"}};</script>"#)
                    .with("set-cookie", "DT=okta; Domain=.grdf.fr; Path=/"),
            )
            .on("connexion.grdf.fr/idp/idx/identify", reply(200, &identify()))
    }

    fn signed_in() -> Fake {
        signing_in()
            .on(
                "connexion.grdf.fr/idp/idx/challenge/answer",
                reply(200, r#"{"success":{"href":"https://connexion.grdf.fr/login/token/redirect?stateToken=s"}}"#),
            )
            .on(
                "connexion.grdf.fr/login/token/redirect",
                reply(302, "").with("location", "https://monespace.grdf.fr/callback?code=c"),
            )
            .on(
                "monespace.grdf.fr/callback",
                reply(302, "")
                    .with("location", "/client/particulier/accueil")
                    .with("set-cookie", "auth_token=t0k; Path=/; Secure; HttpOnly"),
            )
            .on("monespace.grdf.fr/client/", reply(200, "<html></html>"))
    }

    #[tokio::test]
    async fn signs_in_like_the_page_then_asks_with_its_cookies() {
        let fake = signed_in().on(
            "monespace.grdf.fr/api/",
            reply(200, "[]").with("content-type", "application/json"),
        );
        let mut session = Session::open(&fake, "user@example.com", "secret")
            .await
            .unwrap();
        let first = jiff::civil::date(2026, 11, 1);
        let last = jiff::civil::date(2026, 11, 8);
        let body = session
            .informative("00000000000000", first, last)
            .await
            .unwrap();
        assert_eq!(&body[..], b"[]");
        let asked = fake.asked();
        let identify = asked
            .iter()
            .find(|a| a.path == "/idp/idx/identify")
            .unwrap();
        assert_eq!(
            (identify.method.as_str(), identify.host.as_str()),
            ("POST", "connexion.grdf.fr")
        );
        assert!(
            identify.body.contains(r#""stateHandle":"02.id.ab-cd""#),
            "{}",
            identify.body
        );
        assert!(identify.cookie.as_deref().unwrap().contains("DT=okta"));
        let answer = asked
            .iter()
            .find(|a| a.path == "/idp/idx/challenge/answer")
            .unwrap();
        assert!(answer.body.contains(r#""passcode":"secret""#));
        assert!(answer.body.contains("02.id.FIXTURE-STATE-HANDLE"));
        let api = asked.last().unwrap();
        assert_eq!(
            api.path,
            "/api/e-conso/pce/consommation/informatives?dateDebut=2026-11-01&dateFin=2026-11-08&pceList%5B%5D=00000000000000"
        );
        let cookies = api.cookie.as_deref().unwrap();
        assert!(
            cookies.contains("auth_token=t0k") && cookies.contains("home=1"),
            "{cookies}"
        );
        // The Okta host's own cookies stay with it; the shared domain's go everywhere.
        assert!(cookies.contains("DT=okta"), "{cookies}");
        // Debug shows names, never values.
        let shown = format!("{session:?}");
        assert!(
            shown.contains("auth_token") && !shown.contains("t0k"),
            "{shown}"
        );
    }

    #[tokio::test]
    async fn a_refused_password_says_so() {
        let fake = signing_in().on(
            "connexion.grdf.fr/idp/idx/challenge/answer",
            reply(
                401,
                r#"{"errorCode":"E0000004","errorSummary":"Authentication failed"}"#,
            ),
        );
        let failure = Session::open(&fake, "user@example.com", "wrong")
            .await
            .unwrap_err();
        assert_eq!(failure, Failure::Refused);
    }

    #[tokio::test]
    async fn a_request_grdf_no_longer_understands_is_not_a_refusal() {
        let fake = Fake::default()
            .on(START, reply(200, r#"{"stateToken":"02.id.x"}"#))
            .on(
                "connexion.grdf.fr/idp/idx/identify",
                reply(400, r#"{"errorCode":"E0000003"}"#),
            );
        let failure = Session::open(&fake, "user@example.com", "pw")
            .await
            .unwrap_err();
        assert_eq!(failure, Failure::Status(400));
    }

    #[tokio::test]
    async fn another_step_after_the_password_is_named() {
        let fake = signing_in().on(
            "connexion.grdf.fr/idp/idx/challenge/answer",
            reply(200, r#"{"remediation":{"value":[{"name":"challenge-authenticator","href":"https://connexion.grdf.fr/idp/idx/challenge/answer"}]}}"#),
        );
        let failure = Session::open(&fake, "user@example.com", "pw")
            .await
            .unwrap_err();
        assert_eq!(failure, Failure::MoreSteps);
    }

    #[tokio::test]
    async fn a_redirection_away_from_grdf_stops_everything() {
        let fake = Fake::default().on(
            START,
            reply(302, "").with("location", "https://example.com/phish"),
        );
        let failure = Session::open(&fake, "user@example.com", "pw")
            .await
            .unwrap_err();
        assert_eq!(failure, Failure::Elsewhere("example.com".into()));
        assert_eq!(fake.asked().len(), 1);
    }

    #[tokio::test]
    async fn the_sign_in_page_sent_instead_of_data_is_a_lost_session() {
        let fake = signed_in().on(
            "monespace.grdf.fr/api/",
            reply(200, "<html>login</html>").with("content-type", "text/html; charset=utf-8"),
        );
        let mut session = Session::open(&fake, "user@example.com", "pw")
            .await
            .unwrap();
        let day = jiff::civil::date(2026, 11, 1);
        assert_eq!(
            session
                .informative("00000000000000", day, day)
                .await
                .unwrap_err(),
            Failure::SessionLost
        );
    }

    #[test]
    fn reads_the_state_token_escaped_by_the_page() {
        assert_eq!(
            state_token(r#"x = {"stateToken" : "02.id.a\x2Db-c"}"#).as_deref(),
            Some("02.id.a-b-c")
        );
        assert_eq!(state_token("no token here"), None);
        assert_eq!(state_token(r#""stateToken":"""#), None);
    }

    #[test]
    fn redirections_stay_on_grdf_over_https() {
        let here = Target::https("https://monespace.grdf.fr/a/b?c=d").unwrap();
        assert_eq!(here.path, "/a/b?c=d");
        assert_eq!(here.follow("/x").unwrap().path, "/x");
        assert_eq!(here.follow("y?z").unwrap().path, "/a/y?z");
        assert_eq!(
            here.follow("https://connexion.grdf.fr").unwrap(),
            Target {
                host: "connexion.grdf.fr".into(),
                path: "/".into()
            }
        );
        assert_eq!(
            here.follow("http://monespace.grdf.fr/"),
            Err(Failure::Unreadable)
        );
        assert_eq!(here.follow("//evil.example/"), Err(Failure::Unreadable));
        assert_eq!(
            here.follow("https://grdf.fr.evil.example/"),
            Err(Failure::Elsewhere("grdf.fr.evil.example".into()))
        );
        assert_eq!(
            here.follow("https://monespace.grdf.fr:8443/"),
            Err(Failure::Unreadable)
        );
    }

    #[test]
    fn cookies_go_where_they_belong_and_leave_when_told() {
        let mut jar = Jar::default();
        let mut headers = HeaderMap::new();
        headers.append("set-cookie", "a=1; Path=/".parse().unwrap());
        headers.append("set-cookie", "b=2; Domain=.grdf.fr".parse().unwrap());
        headers.append("set-cookie", "c=3; Domain=example.com".parse().unwrap());
        jar.absorb("connexion.grdf.fr", &headers);
        let to = |jar: &Jar, host: &str| jar.header(host).map(|v| v.to_str().unwrap().to_owned());
        assert_eq!(to(&jar, "connexion.grdf.fr").as_deref(), Some("a=1; b=2"));
        assert_eq!(to(&jar, "monespace.grdf.fr").as_deref(), Some("b=2"));
        let mut gone = HeaderMap::new();
        gone.append("set-cookie", "a=; Max-Age=0".parse().unwrap());
        jar.absorb("connexion.grdf.fr", &gone);
        assert_eq!(to(&jar, "connexion.grdf.fr").as_deref(), Some("b=2"));
    }

    #[test]
    fn the_password_step_comes_from_the_identify_answer() {
        let answer: Json = serde_json::from_str(&identify()).unwrap();
        assert_eq!(
            remediation(&answer, "challenge-authenticator").as_deref(),
            Some(ANSWER)
        );
        assert_eq!(remediation(&answer, "nothing"), None);
    }
}

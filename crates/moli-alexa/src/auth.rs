//! Signing in to Amazon as the Alexa app does, without a proxy: Moli makes
//! the address of Amazon's own sign-in page (PKCE), the person signs in there
//! (password, one-time code, captcha: all on Amazon's page) and pastes the
//! address the page ends on; its code registers Moli as a device of the
//! account, which gives the durable refresh token. Afterwards: refresh token
//! → the account site's cookies → the Alexa API's CSRF cookie.
//!
//! Interface facts from the open projects that use the same API (alexapy,
//! alexa-cookie2, aioamazondevices); no code of theirs.

use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{HeaderMap, Method, Request};
use http_body_util::Full;
use moli_net::Body as Bytes;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value as Json, json};

use crate::encode;

/// The Alexa iOS app, as which Moli registers.
pub(crate) const DEVICE_TYPE: &str = "A2IVLV5VM2W81";
const APP_VERSION: &str = "2.2.651540.0";
const OS_VERSION: &str = "18.3.1";
pub(crate) const USER_AGENT: &str = "AmazonWebView/Amazon Alexa/2.2.651540.0/iOS/18.3.1/iPhone";
/// Every Alexa sign-in goes through the US site, whatever the account's.
const SIGN_IN_HOST: &str = "www.amazon.com";
const AUTH_HOST: &str = "api.amazon.com";
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 2 * 1024 * 1024;

fn random(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(bytes)
}

fn sha256(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .to_vec()
}

/// A sign-in in progress: what Amazon must see again once the person is back.
#[derive(Clone)]
pub(crate) struct SignIn {
    /// Moli's device serial for this account: 32 uppercase hex digits.
    pub serial: String,
    verifier: String,
    /// Amazon's sign-in page, for the person.
    pub url: String,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the verifier.
        f.debug_struct("SignIn")
            .field("serial", &self.serial)
            .finish_non_exhaustive()
    }
}

impl SignIn {
    /// A new sign-in; `language` like `fr_FR`.
    pub(crate) fn new(language: &str) -> anyhow::Result<Self> {
        let serial = encode::hex(&random(16)?).to_uppercase();
        let verifier = encode::base64url(&random(32)?);
        let url = sign_in_url(&client_id(&serial), &challenge(&verifier), language);
        Ok(Self {
            serial,
            verifier,
            url,
        })
    }
}

/// `hex(serial + "#" + device type)`: what Amazon calls the client.
fn client_id(serial: &str) -> String {
    encode::hex(format!("{serial}#{DEVICE_TYPE}").as_bytes())
}

/// The PKCE challenge of `verifier` (S256).
fn challenge(verifier: &str) -> String {
    encode::base64url(&sha256(verifier.as_bytes()))
}

fn sign_in_url(client_id: &str, challenge: &str, language: &str) -> String {
    let params = [
        (
            "openid.return_to",
            format!("https://{SIGN_IN_HOST}/ap/maplanding"),
        ),
        ("openid.assoc_handle", "amzn_dp_project_dee_ios".into()),
        (
            "openid.identity",
            "http://specs.openid.net/auth/2.0/identifier_select".into(),
        ),
        ("pageId", "amzn_dp_project_dee_ios".into()),
        ("accountStatusPolicy", "P1".into()),
        (
            "openid.claimed_id",
            "http://specs.openid.net/auth/2.0/identifier_select".into(),
        ),
        ("openid.mode", "checkid_setup".into()),
        (
            "openid.ns.oa2",
            "http://www.amazon.com/ap/ext/oauth/2".into(),
        ),
        ("openid.oa2.client_id", format!("device:{client_id}")),
        (
            "openid.ns.pape",
            "http://specs.openid.net/extensions/pape/1.0".into(),
        ),
        ("openid.oa2.response_type", "code".into()),
        ("openid.ns", "http://specs.openid.net/auth/2.0".into()),
        ("openid.pape.max_auth_age", "0".into()),
        ("openid.oa2.scope", "device_auth_access".into()),
        ("openid.oa2.code_challenge_method", "S256".into()),
        ("openid.oa2.code_challenge", challenge.into()),
        ("language", language.into()),
    ];
    let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    format!("https://{SIGN_IN_HOST}/ap/signin?{}", encode::form(&pairs))
}

/// Why a pasted address is not the one expected (a catalogue key).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pasted {
    NotTheEnd,
    NoCode,
}

/// The authorization code in the address Amazon's page ended on
/// (`https://www.amazon.com/ap/maplanding?…&openid.oa2.authorization_code=…`).
pub(crate) fn code_of(pasted: &str) -> Result<String, Pasted> {
    let pasted = pasted.trim();
    let rest = pasted.strip_prefix("https://").ok_or(Pasted::NotTheEnd)?;
    let (host, path) = rest.split_once('/').ok_or(Pasted::NotTheEnd)?;
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let amazon = host.starts_with("www.amazon.") || host == "amazon.com";
    if !amazon || path != "ap/maplanding" {
        return Err(Pasted::NotTheEnd);
    }
    encode::query_param(pasted, "openid.oa2.authorization_code")
        .filter(|c| !c.is_empty())
        .ok_or(Pasted::NoCode)
}

fn request(
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> anyhow::Result<Request<Full<Bytes>>> {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    Ok(builder.body(Full::new(Bytes::from(body)))?)
}

/// What registering gives.
#[derive(Clone)]
pub(crate) struct Registered {
    pub refresh_token: String,
}

impl std::fmt::Debug for Registered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registered").finish_non_exhaustive()
    }
}

/// Registers Moli as a device of the account with the pasted code.
pub(crate) async fn register(sign_in: &SignIn, code: &str) -> anyhow::Result<Registered> {
    let frc = encode::base64(&random(313)?)
        .trim_end_matches('=')
        .to_owned();
    let map_md = encode::base64(
        json!({
            "device_user_dictionary": [],
            "device_registration_data": { "software_version": "1" },
            "app_identifier": { "app_version": APP_VERSION, "bundle_id": "com.amazon.echo" },
        })
        .to_string()
        .as_bytes(),
    );
    let body = json!({
        "requested_extensions": ["device_info", "customer_info"],
        "cookies": { "website_cookies": [], "domain": ".amazon.com" },
        "registration_data": {
            "domain": "Device",
            "app_version": APP_VERSION,
            "device_type": DEVICE_TYPE,
            "device_name": "%FIRST_NAME%'s%DUPE_STRATEGY_1ST%Moli",
            "os_version": OS_VERSION,
            "device_serial": sign_in.serial,
            "device_model": "iPhone",
            "app_name": "Moli",
            "software_version": "1",
        },
        "auth_data": {
            "client_id": client_id(&sign_in.serial),
            "authorization_code": code,
            "code_verifier": sign_in.verifier,
            "code_algorithm": "SHA-256",
            "client_domain": "DeviceLegacy",
        },
        "user_context_map": { "frc": frc },
        "requested_token_type": ["bearer", "mac_dms", "website_cookies"],
    });
    let cookie = format!("frc={frc}; map-md={map_md}");
    let request = request(
        Method::POST,
        "/auth/register",
        &[
            ("content-type", "application/json"),
            ("accept", "application/json"),
            ("accept-charset", "utf-8"),
            ("user-agent", USER_AGENT),
            ("x-amzn-identity-auth-domain", AUTH_HOST),
            ("cookie", &cookie),
        ],
        body.to_string().into_bytes(),
    )?;
    let (status, answer) = moli_net::web(AUTH_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
    let answer: Json = serde_json::from_slice(&answer).unwrap_or(Json::Null);
    if let Some(message) = answer["response"]["error"]["message"].as_str() {
        bail!("Amazon refused the registration: {message}");
    }
    let refresh = answer["response"]["success"]["tokens"]["bearer"]["refresh_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .with_context(|| format!("no refresh token in Amazon's answer (HTTP {status})"))?;
    Ok(Registered {
        refresh_token: refresh.to_owned(),
    })
}

/// The cookies the Alexa API reads, for one site.
#[derive(Clone, Default)]
pub(crate) struct Jar(Vec<(String, String)>);

impl std::fmt::Debug for Jar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Names only: the values are the session.
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

impl Jar {
    pub(crate) fn set(&mut self, name: &str, value: &str) {
        let value = value.trim_matches('"');
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => value.clone_into(&mut slot.1),
            None => self.0.push((name.to_owned(), value.to_owned())),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// `a=1; b=2`.
    pub(crate) fn header(&self) -> String {
        self.0
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The cookies an answer sets.
    pub(crate) fn absorb(&mut self, headers: &HeaderMap) {
        for value in headers.get_all(http::header::SET_COOKIE) {
            let Ok(text) = value.to_str() else { continue };
            let first = text.split(';').next().unwrap_or_default();
            if let Some((name, value)) = first.split_once('=')
                && !name.trim().is_empty()
            {
                self.set(name.trim(), value.trim());
            }
        }
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

/// The form fields Amazon's token exchanges share.
fn token_fields<'a>(refresh: &'a str, requested: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("app_name", "Moli"),
        ("app_version", APP_VERSION),
        ("di.sdk.version", "6.12.4"),
        ("source_token", refresh),
        ("package_name", "com.amazon.echo"),
        ("di.hw.version", "iPhone"),
        ("platform", "iOS"),
        ("requested_token_type", requested),
        ("source_token_type", "refresh_token"),
        ("di.os.name", "iOS"),
        ("di.os.version", OS_VERSION),
        ("current_version", "6.12.4"),
        ("previous_version", "6.12.4"),
    ]
}

/// Why the refresh token no longer works: the person must sign in again.
pub(crate) const SIGN_IN_AGAIN: &str = "Amazon no longer accepts this sign-in";

/// The account site's cookies, from the refresh token.
pub(crate) async fn cookies(refresh: &str, domain: &str) -> anyhow::Result<Jar> {
    let site = format!(".{domain}");
    let mut fields = token_fields(refresh, "auth_cookies");
    fields.push(("domain", &site));
    let auth_domain = format!("api.{domain}");
    let request = request(
        Method::POST,
        "/ap/exchangetoken/cookies",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("accept", "application/json"),
            ("user-agent", USER_AGENT),
            ("x-amzn-identity-auth-domain", &auth_domain),
        ],
        encode::form(&fields).into_bytes(),
    )?;
    let host = format!("www.{domain}");
    let (status, answer) = moli_net::web(&host, 443, true, request, LIMIT, MAX_BODY).await?;
    let answer: Json = serde_json::from_slice(&answer).unwrap_or(Json::Null);
    if matches!(status.as_u16(), 400 | 401) {
        let error = answer["response"]["error"]["code"]
            .as_str()
            .or_else(|| answer["error"].as_str())
            .unwrap_or_default();
        if matches!(
            error,
            "InvalidToken" | "invalid_grant" | "invalid_token" | "unauthorized_client"
        ) {
            bail!("{SIGN_IN_AGAIN} ({error})");
        }
    }
    let mut jar = Jar::default();
    for cookie in answer["response"]["tokens"]["cookies"][&site]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(value)) = (cookie["Name"].as_str(), cookie["Value"].as_str()) {
            jar.set(name, value);
        }
    }
    if jar.len() == 0 {
        bail!("no {site} cookies from Amazon (HTTP {status})");
    }
    Ok(jar)
}

/// The pages that set the Alexa API's `csrf` cookie, tried in turn.
const CSRF_PAGES: [&str; 5] = [
    "/api/language",
    "/spa/index.html",
    "/api/devices-v2/device?cached=false",
    "/templates/oobe/d-device-pick.handlebars",
    "/api/strings",
];

/// The `csrf` cookie of the Alexa API on `host` (`alexa.amazon.fr`), added to `jar`.
pub(crate) async fn csrf(jar: &mut Jar, host: &str) -> anyhow::Result<String> {
    let referer = format!("https://{host}/spa/index.html");
    let origin = format!("https://{host}");
    for page in CSRF_PAGES {
        let cookie = jar.header();
        let request = request(
            Method::GET,
            page,
            &[
                ("dnt", "1"),
                ("user-agent", USER_AGENT),
                ("referer", &referer),
                ("origin", &origin),
                ("accept", "*/*"),
                ("cookie", &cookie),
            ],
            Vec::new(),
        )?;
        let (_, headers, _) =
            moli_net::web_with_headers(host, 443, true, request, LIMIT, MAX_BODY).await?;
        jar.absorb(&headers);
        if let Some(csrf) = jar.get("csrf") {
            return Ok(csrf.to_owned());
        }
    }
    bail!("{host} gave no csrf cookie")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_id_is_the_serial_and_the_app_in_hex() {
        let id = client_id("0123456789ABCDEF0123456789ABCDEF");
        assert_eq!(id.len(), 92);
        assert!(id.ends_with("23413249564c5635564d32573831"), "{id}");
    }

    #[test]
    fn the_challenge_is_the_rfc_7636_one() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_sign_in_has_its_own_serial_and_challenge() {
        let a = SignIn::new("fr_FR").unwrap();
        let b = SignIn::new("fr_FR").unwrap();
        assert_ne!(a.serial, b.serial);
        assert_eq!(a.serial.len(), 32);
        assert!(
            a.serial
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );
        assert!(
            a.url.starts_with("https://www.amazon.com/ap/signin?"),
            "{}",
            a.url
        );
        assert_eq!(
            encode::query_param(&a.url, "openid.oa2.code_challenge").as_deref(),
            Some(challenge(&a.verifier).as_str())
        );
        assert_eq!(
            encode::query_param(&a.url, "openid.oa2.client_id"),
            Some(format!("device:{}", client_id(&a.serial)))
        );
        assert_eq!(
            encode::query_param(&a.url, "language").as_deref(),
            Some("fr_FR")
        );
        assert!(!format!("{a:?}").contains(&a.verifier));
    }

    #[test]
    fn only_the_last_page_with_a_code_is_taken() {
        let end = "https://www.amazon.com/ap/maplanding?openid.assoc_handle=x&openid.oa2.authorization_code=ANcode123&openid.mode=id_res";
        assert_eq!(code_of(end).as_deref(), Ok("ANcode123"));
        assert_eq!(code_of(&format!("  {end}\n")).as_deref(), Ok("ANcode123"));
        assert_eq!(
            code_of("https://www.amazon.fr/ap/maplanding?openid.oa2.authorization_code=ANfr")
                .as_deref(),
            Ok("ANfr")
        );
        assert_eq!(
            code_of("https://www.amazon.com/ap/signin?openid.oa2.authorization_code=x"),
            Err(Pasted::NotTheEnd)
        );
        assert_eq!(
            code_of("https://evil.example/ap/maplanding?openid.oa2.authorization_code=x"),
            Err(Pasted::NotTheEnd)
        );
        assert_eq!(
            code_of("https://www.amazon.com/ap/maplanding?openid.mode=cancel"),
            Err(Pasted::NoCode)
        );
        assert_eq!(
            code_of("http://www.amazon.com/ap/maplanding"),
            Err(Pasted::NotTheEnd)
        );
    }

    #[test]
    fn a_jar_reads_set_cookie_headers_and_hides_values() {
        let mut headers = HeaderMap::new();
        headers.append(
            http::header::SET_COOKIE,
            "csrf=\"123\"; Path=/; Secure".parse().unwrap(),
        );
        headers.append(
            http::header::SET_COOKIE,
            "session-id=abc; Domain=.amazon.fr".parse().unwrap(),
        );
        let mut jar = Jar::default();
        jar.set("at-acbfr", "token");
        jar.absorb(&headers);
        assert_eq!(jar.get("csrf"), Some("123"));
        assert_eq!(jar.header(), "at-acbfr=token; csrf=123; session-id=abc");
        let shown = format!("{jar:?}");
        assert!(
            shown.contains("csrf") && !shown.contains("token"),
            "{shown}"
        );
    }
}

//! Signing in to an ENGIE Belgium account as the ENGIE Smart app does, without
//! ever seeing the password: Moli makes the address of ENGIE's own sign-in
//! page (OAuth 2 with PKCE), the person signs in there (password, then the
//! code ENGIE sends by text or e-mail) and pastes the address the page ends on
//! (`be.engie.smart://login-callback/…?code=…`). Its code gives a refresh
//! token, which ENGIE replaces at each use: the new one is filed at once.
//!
//! Interface facts (client id, redirect, scopes) from the open library
//! aioengiebelgium, which Home Assistant's `engie_be` uses; no code of theirs.

use std::time::Duration;

use anyhow::Context as _;
use http::header::{ACCEPT, CONTENT_TYPE, HOST, USER_AGENT};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::Value as Json;

use crate::encode;

/// The ENGIE Smart app, as which Moli signs in.
const CLIENT_ID: &str = "R0PQyUdjO5B2tBaRnltgitVnnUmjGyld";
pub(crate) const REDIRECT_URI: &str = "be.engie.smart://login-callback/nl";
const SCOPES: &str = "openid profile roles offline_access";
const AUDIENCE: &str = "customer";
pub(crate) const AUTH_HOST: &str = "account.engie.be";
/// What the app's own requests say they are.
pub(crate) const APP_AGENT: &str =
    "Dalvik/2.1.0 (Linux; U; Android 16; Pixel 6 Build/BP4A.251205.006) moli-os";
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 64 * 1024;
/// A token is renewed this long before it expires.
const MARGIN: Duration = Duration::from_secs(60);

fn random(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(bytes)
}

/// A sign-in in progress: what ENGIE must see again once the person is back.
#[derive(Clone)]
pub(crate) struct SignIn {
    verifier: String,
    state: String,
    /// What the identity token must carry back.
    nonce: String,
    /// ENGIE's sign-in page, for the person.
    pub url: String,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the verifier.
        f.debug_struct("SignIn")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl SignIn {
    /// A new sign-in; `language` is the page's (`fr`, `nl`, `en`).
    pub fn new(language: &str) -> anyhow::Result<Self> {
        let verifier = encode::base64url(&random(32)?);
        let challenge = encode::base64url(
            ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref(),
        );
        let state = encode::hex(&random(16)?);
        let nonce = encode::hex(&random(16)?);
        let query = encode::form(&[
            ("redirect_uri", REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("ui_locales", language),
            ("state", &state),
            ("nonce", &nonce),
            ("scope", SCOPES),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("audience", AUDIENCE),
            ("app_scheme", "be-engie-smart"),
        ]);
        Ok(Self {
            verifier,
            state,
            nonce,
            url: format!("https://{AUTH_HOST}/authorize?{query}"),
        })
    }

    /// The code in the address the person pasted, if it is the end of this
    /// sign-in (and not of another one, nor any other address).
    pub fn code(&self, pasted: &str) -> Result<String, Pasted> {
        let pasted = pasted.trim();
        if !pasted.starts_with("be.engie.smart://") {
            return Err(Pasted::NotTheEnd);
        }
        if encode::query_param(pasted, "state").as_deref() != Some(self.state.as_str()) {
            return Err(Pasted::Stale);
        }
        encode::query_param(pasted, "code")
            .filter(|c| !c.is_empty())
            .ok_or(Pasted::NoCode)
    }

    /// The code's tokens; an identity token for another sign-in (its nonce
    /// is not this one's) is refused.
    pub async fn tokens(&self, code: &str) -> Result<Tokens, Failure> {
        let tokens = token_request(&[
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", &self.verifier),
            ("client_id", CLIENT_ID),
        ])
        .await?;
        self.check_nonce(&tokens)?;
        Ok(tokens)
    }

    fn check_nonce(&self, tokens: &Tokens) -> Result<(), Failure> {
        match &tokens.nonce {
            Some(n) if *n != self.nonce => Err(Failure::Other(anyhow::anyhow!(
                "the identity token is not this sign-in's"
            ))),
            _ => Ok(()),
        }
    }
}

/// What is wrong with a pasted address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pasted {
    /// Not the address the sign-in ends on.
    NotTheEnd,
    /// The end of an older sign-in (the page was opened before Moli restarted).
    Stale,
    NoCode,
}

/// The pair ENGIE gives: the access token (short-lived) and its refresh token.
#[derive(Clone)]
pub(crate) struct Tokens {
    pub access: String,
    pub refresh: String,
    pub expires: tokio::time::Instant,
    /// The `nonce` of the identity token, when ENGIE gives one.
    pub nonce: Option<String>,
}

/// The `nonce` claim of an identity token (a JWT); its signature is not
/// checked here: the token comes straight from ENGIE over TLS.
fn id_nonce(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let claims: Json = serde_json::from_slice(&encode::from_base64url(payload)?).ok()?;
    claims["nonce"].as_str().map(str::to_owned)
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").finish_non_exhaustive()
    }
}

impl Tokens {
    pub fn stale(&self) -> bool {
        tokio::time::Instant::now() + MARGIN >= self.expires
    }
}

/// Why a request failed.
#[derive(Debug)]
pub(crate) enum Failure {
    /// ENGIE no longer takes the session (signed in elsewhere, revoked): a
    /// person must sign in again.
    Refused,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused => f.write_str("ENGIE refused the session"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

/// A new pair from the refresh token (which ENGIE replaces).
pub(crate) async fn refresh(refresh_token: &str) -> Result<Tokens, Failure> {
    token_request(&[
        ("refresh_token", refresh_token),
        ("audience", AUDIENCE),
        ("grant_type", "refresh_token"),
        ("scope", SCOPES),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", CLIENT_ID),
    ])
    .await
}

async fn token_request(pairs: &[(&str, &str)]) -> Result<Tokens, Failure> {
    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri("/oauth/token")
        .header(HOST, AUTH_HOST)
        .header(USER_AGENT, APP_AGENT)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(http_body_util::Full::new(moli_net::Body::from(
            encode::form(pairs),
        )))
        .context("token request")?;
    // The body may echo what was sent: never in an error.
    let (status, body) = moli_net::web(AUTH_HOST, 443, true, request, LIMIT, MAX_BODY)
        .await
        .map_err(|_| anyhow::anyhow!("{AUTH_HOST} unreachable"))?;
    tokens_from(status.as_u16(), &body)
}

/// The token endpoint's answer: tokens, or why not.
pub(crate) fn tokens_from(status: u16, body: &[u8]) -> Result<Tokens, Failure> {
    let json: Json = serde_json::from_slice(body).unwrap_or(Json::Null);
    if status == 401 || json["error"] == "invalid_grant" {
        return Err(Failure::Refused);
    }
    if !(200..300).contains(&status) {
        return Err(Failure::Other(anyhow::anyhow!(
            "token endpoint: HTTP {status}"
        )));
    }
    let text = |k: &str| {
        json[k]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let (Some(access), Some(refresh)) = (text("access_token"), text("refresh_token")) else {
        return Err(Failure::Other(anyhow::anyhow!(
            "token endpoint: no token in the answer"
        )));
    };
    let lifetime = json["expires_in"]
        .as_u64()
        .unwrap_or(3600)
        .clamp(120, 86_400);
    Ok(Tokens {
        access,
        refresh,
        expires: tokio::time::Instant::now() + Duration::from_secs(lifetime),
        nonce: json["id_token"].as_str().and_then(id_nonce),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_token_must_be_this_sign_ins() {
        let sign_in = SignIn::new("fr").unwrap();
        let jwt = |nonce: &str| {
            let claims = serde_json::json!({ "nonce": nonce }).to_string();
            format!("e30.{}.sig", encode::base64url(claims.as_bytes()))
        };
        let answer = |id: &str| {
            serde_json::json!({"access_token": "a", "refresh_token": "r", "id_token": id})
                .to_string()
        };
        let ok = tokens_from(200, answer(&jwt(&sign_in.nonce)).as_bytes()).unwrap();
        assert!(sign_in.check_nonce(&ok).is_ok());
        let other = tokens_from(200, answer(&jwt("someone-else")).as_bytes()).unwrap();
        assert!(sign_in.check_nonce(&other).is_err());
        // No identity token at all (a renewal): nothing to check.
        let none = tokens_from(200, br#"{"access_token": "a", "refresh_token": "r"}"#).unwrap();
        assert!(sign_in.check_nonce(&none).is_ok());
    }

    #[test]
    fn the_sign_in_page_is_engies_with_pkce() {
        let sign_in = SignIn::new("fr").unwrap();
        assert!(
            sign_in
                .url
                .starts_with("https://account.engie.be/authorize?")
        );
        for part in [
            "client_id=R0PQyUdjO5B2tBaRnltgitVnnUmjGyld",
            "redirect_uri=be.engie.smart%3A%2F%2Flogin-callback%2Fnl",
            "code_challenge_method=S256",
            "scope=openid%20profile%20roles%20offline_access",
            "audience=customer",
            "ui_locales=fr",
        ] {
            assert!(sign_in.url.contains(part), "{part}");
        }
        let challenge = encode::query_param(&sign_in.url, "code_challenge").unwrap();
        let expected = encode::base64url(
            ring::digest::digest(&ring::digest::SHA256, sign_in.verifier.as_bytes()).as_ref(),
        );
        assert_eq!(challenge, expected);
        assert!(!format!("{sign_in:?}").contains(&sign_in.verifier));
    }

    #[test]
    fn only_the_end_of_this_sign_in_gives_a_code() {
        let sign_in = SignIn::new("fr").unwrap();
        let end = format!(
            "be.engie.smart://login-callback/nl?code=abc123&state={}",
            sign_in.state
        );
        assert_eq!(sign_in.code(&format!("  {end}\n")), Ok("abc123".into()));
        assert_eq!(
            sign_in.code("https://account.engie.be/u/login?state=x"),
            Err(Pasted::NotTheEnd)
        );
        assert_eq!(
            sign_in.code("be.engie.smart://login-callback/nl?code=abc&state=other"),
            Err(Pasted::Stale)
        );
        let no_code = format!("be.engie.smart://login-callback/nl?state={}", sign_in.state);
        assert_eq!(sign_in.code(&no_code), Err(Pasted::NoCode));
    }

    #[test]
    fn token_answers_are_read() {
        let ok = br#"{"access_token":"a","refresh_token":"r","expires_in":600}"#;
        let tokens = tokens_from(200, ok).unwrap();
        assert_eq!(
            (tokens.access.as_str(), tokens.refresh.as_str()),
            ("a", "r")
        );
        assert!(!tokens.stale());
        assert!(matches!(
            tokens_from(403, br#"{"error":"invalid_grant"}"#),
            Err(Failure::Refused)
        ));
        assert!(matches!(tokens_from(401, b""), Err(Failure::Refused)));
        assert!(matches!(tokens_from(500, b""), Err(Failure::Other(_))));
        assert!(matches!(
            tokens_from(200, br#"{"access_token":"a"}"#),
            Err(Failure::Other(_))
        ));
        assert_eq!(format!("{tokens:?}"), "Tokens { .. }");
    }
}

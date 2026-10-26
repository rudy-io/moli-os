//! Viessmann heat pumps and boilers, through the Viessmann cloud (ViCare): the
//! appliance's Wi-Fi gateway reports only there. The driver signs in with the
//! account's e-mail and password and the application key (client id) made at
//! the Viessmann developer portal, all three from Moli's encrypted store,
//! then reads the whole appliance in one call and orders what the cloud
//! accepts (circuit modes, set points, domestic hot water).
//!
//! The cloud is rationed (about 1 450 calls a day, 120 every 10 minutes), so
//! the appliance is read every 2 minutes 30 and a refusal for quota waits the
//! time the cloud gives.

mod encode;
mod model;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use http::{HeaderMap, Method, Request};
use http_body_util::Full;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::time::Instant;

pub use model::{Features, Layout, Site};

const CLIENT_ID: &str = "client_id";
const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const IAM_HOST: &str = "iam.viessmann-climatesolutions.com";
const IAM: &str = "/idp/v3";
const API_HOST: &str = "api.viessmann-climatesolutions.com";
const API: &str = "/iot/v2";
const SCOPE: &str = "IoT User offline_access";
/// The address the Viessmann application sends the browser back to: never
/// followed, only read for its authorization code.
const REDIRECT: &str = "vicare://oauth-callback/everest";
const POLL: Duration = Duration::from_secs(150);
/// After an order the appliance is read again, to show what it did.
const AFTER_ORDER: Duration = Duration::from_secs(20);
const LIMIT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Credentials refused: the next try waits this long (no storm of logins).
const REFUSED_WAIT: Duration = Duration::from_secs(30 * 60);
/// Quota refused without a reset time: the next try waits this long.
const QUOTA_WAIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The installation to read, when the account has several (its number
    /// in the cloud); by default the first.
    #[serde(default)]
    pub installation: Option<u64>,
    /// The redirect address declared on the application key at the developer
    /// portal, when it is not the ViCare application's own.
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

#[derive(Debug)]
pub struct Vicare {
    config: Config,
}

impl Vicare {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Vicare {
    fn kind(&self) -> &'static str {
        "vicare"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// The cloud refused the e-mail, the password or the application key.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// The cloud's quota is spent: nothing to ask for this long.
#[derive(Debug)]
struct Quota(Duration);

impl std::fmt::Display for Quota {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&moli_i18n::tr!(
            "pilotes.vicare.quota",
            minutes = self.0.as_secs().div_ceil(60)
        ))
    }
}

impl std::error::Error for Quota {}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// How long the cloud says to wait (`limitReset`, in milliseconds since
/// 1970), within a minute and a day; a quarter of an hour when it says
/// nothing.
fn quota_wait(answer: &Json, now: i64) -> Duration {
    let left = answer["extendedPayload"]["limitReset"]
        .as_i64()
        .map(|reset| reset / 1000 - now);
    left.map(|s| s.clamp(60, 86_400))
        .and_then(|s| u64::try_from(s).ok())
        .map_or(QUOTA_WAIT, Duration::from_secs)
}

fn random(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(bytes)
}

/// The PKCE challenge of `verifier` (S256).
fn challenge(verifier: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    encode::base64url(digest.as_ref())
}

async fn send(
    host: &str,
    request: Request<Full<moli_net::Body>>,
) -> anyhow::Result<(u16, HeaderMap, Json)> {
    // HTTPS only: the password and the tokens never travel in the clear.
    let (status, headers, bytes) =
        moli_net::web_with_headers(host, 443, true, request, LIMIT, MAX_BODY).await?;
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    Ok((status.as_u16(), headers, json))
}

struct Tokens {
    access: String,
    refresh: Option<String>,
    /// Unix seconds: a minute before the access token ends.
    until: i64,
}

struct Cloud {
    client_id: String,
    username: String,
    password: String,
    redirect: String,
    tokens: Option<Tokens>,
}

impl std::fmt::Debug for Cloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cloud").finish_non_exhaustive()
    }
}

impl Cloud {
    /// The identity provider's token endpoint.
    async fn token_call(&mut self, pairs: &[(&str, &str)]) -> anyhow::Result<String> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{IAM}/token"))
            .header("accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Full::new(moli_net::Body::from(encode::form(pairs))))?;
        let (status, _, answer) = send(IAM_HOST, request).await?;
        let Some(access) = answer["access_token"].as_str().filter(|t| !t.is_empty()) else {
            if (400..500).contains(&status) && status != 429 {
                let why = answer["error_description"]
                    .as_str()
                    .or(answer["error"].as_str())
                    .unwrap_or("")
                    .to_owned();
                return Err(Refused(why).into());
            }
            bail!(moli_i18n::tr!("pilotes.vicare.reponse", status = status));
        };
        let lasts = answer["expires_in"].as_i64().unwrap_or(3600);
        self.tokens = Some(Tokens {
            access: access.to_owned(),
            refresh: answer["refresh_token"].as_str().map(str::to_owned),
            until: now() + lasts - 60,
        });
        Ok(access.to_owned())
    }

    /// Signs in the way the ViCare application does, without a browser: the
    /// e-mail and password (Basic) ask for an authorization code, which the
    /// answer's redirect carries.
    async fn login(&mut self) -> anyhow::Result<String> {
        let (client_id, redirect) = (self.client_id.clone(), self.redirect.clone());
        let verifier = encode::base64url(&random(32)?);
        let query = encode::form(&[
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge(&verifier).as_str()),
            ("code_challenge_method", "S256"),
        ]);
        let basic = encode::base64(format!("{}:{}", self.username, self.password).as_bytes());
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{IAM}/authorize?{query}"))
            .header("accept", "application/json")
            .header("authorization", format!("Basic {basic}"))
            .body(Full::new(moli_net::Body::new()))?;
        let (status, headers, answer) = send(IAM_HOST, request).await?;
        let code = headers
            .get("location")
            .and_then(|l| l.to_str().ok())
            .and_then(|l| encode::query_param(l, "code"));
        let Some(code) = code else {
            if (400..500).contains(&status) && status != 429 {
                let why = answer["error_description"]
                    .as_str()
                    .or(answer["error"].as_str())
                    .unwrap_or("")
                    .to_owned();
                return Err(Refused(why).into());
            }
            bail!(moli_i18n::tr!("pilotes.vicare.reponse", status = status));
        };
        self.token_call(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .await
    }

    /// A new access token: from the refresh token when there is one, signing
    /// in again when it is refused (or gone).
    async fn renew(&mut self) -> anyhow::Result<String> {
        let refresh = self.tokens.take().and_then(|t| t.refresh);
        if let Some(refresh) = refresh {
            let client_id = self.client_id.clone();
            let pairs = [
                ("grant_type", "refresh_token"),
                ("client_id", client_id.as_str()),
                ("refresh_token", refresh.as_str()),
            ];
            if let Ok(access) = self.token_call(&pairs).await {
                return Ok(access);
            }
        }
        self.login().await
    }

    async fn access(&mut self) -> anyhow::Result<String> {
        match &self.tokens {
            Some(t) if t.until > now() => Ok(t.access.clone()),
            _ => self.renew().await,
        }
    }

    /// One call to the cloud's API, signing in when needed (and once again on
    /// a refused token). Returns the answer's JSON.
    async fn api(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Json>,
    ) -> anyhow::Result<Json> {
        for attempt in 0..2 {
            let token = if attempt == 0 {
                self.access().await?
            } else {
                self.login().await?
            };
            let mut builder = Request::builder()
                .method(method.clone())
                .uri(format!("{API}{path}"))
                .header("authorization", format!("Bearer {token}"));
            let payload = if let Some(body) = body {
                builder = builder
                    .header("accept", "application/vnd.siren+json")
                    .header("content-type", "application/json");
                moli_net::Body::from(body.to_string())
            } else {
                builder = builder.header("accept", "application/json");
                moli_net::Body::new()
            };
            let (status, _, answer) = send(API_HOST, builder.body(Full::new(payload))?).await?;
            if status == 429 || answer["statusCode"].as_u64() == Some(429) {
                return Err(Quota(quota_wait(&answer, now())).into());
            }
            if status == 401 || answer["error"] == "EXPIRED TOKEN" {
                self.tokens = None;
                continue;
            }
            if !(200..300).contains(&status) {
                let why = answer["message"].as_str().unwrap_or("");
                bail!(moli_i18n::tr!(
                    "pilotes.vicare.reponse_detail",
                    status = status,
                    why = why
                ));
            }
            return Ok(answer);
        }
        bail!(moli_i18n::tr!("pilotes.vicare.jeton_refuse"))
    }
}

/// What the driver knows of the account and of the appliance.
#[derive(Debug)]
struct Session {
    cloud: Cloud,
    installation: Option<u64>,
    site: Option<Site>,
    layout: Layout,
    published: Vec<String>,
    ids: Vec<moli_core::DeviceId>,
    failures: u32,
    /// Until then the cloud has refused us (credentials, quota): orders are
    /// refused at once instead of spending calls.
    quiet_until: Instant,
}

impl Session {
    /// Reads the whole appliance, finding it first when it is not yet known.
    async fn read(&mut self) -> anyhow::Result<Features> {
        if self.site.is_none() {
            let answer = self
                .cloud
                .api(
                    Method::GET,
                    "/equipment/installations?includeGateways=true",
                    None,
                )
                .await?;
            self.site = Some(
                Site::read(&answer, self.installation)
                    .context(moli_i18n::tr!("pilotes.vicare.pas_de_pac"))?,
            );
        }
        let Some(path) = self.site.as_ref().map(Site::features_path) else {
            bail!(moli_i18n::tr!("pilotes.vicare.pas_de_pac"));
        };
        let answer = self.cloud.api(Method::GET, &path, None).await?;
        let features = Features::read(&answer);
        if features.is_empty() {
            bail!(moli_i18n::tr!("pilotes.vicare.illisible"));
        }
        Ok(features)
    }

    /// One read, published; what to wait before the next.
    async fn poll(&mut self, ctx: &DriverCtx) -> Duration {
        match self.read().await {
            Ok(features) => {
                self.failures = 0;
                let model = self.site.as_ref().and_then(|s| s.model.clone());
                let fresh = Layout::from(&features, model.as_deref());
                if fresh.signature() != self.published {
                    let devices = fresh.devices(ctx.instance(), |n| ctx.device_id(n));
                    self.ids = devices.iter().map(|d| d.id.clone()).collect();
                    for device in devices {
                        ctx.upsert_device(device);
                    }
                    self.published = fresh.signature();
                    self.layout = fresh;
                    ctx.ready();
                }
                for id in &self.ids {
                    ctx.set_availability(id, true);
                }
                for (native, key, value) in self.layout.readings(&features) {
                    ctx.set_state(&ctx.device_id(&native), &key, value);
                }
                POLL
            }
            Err(e) if e.downcast_ref::<Refused>().is_some() => {
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.vicare.identifiants_refuses",
                    instance = ctx.instance(),
                    error = format!("{e:#}")
                ));
                self.quiet_until = Instant::now() + REFUSED_WAIT;
                REFUSED_WAIT
            }
            Err(e) if e.downcast_ref::<Quota>().is_some() => {
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "viessmann quota spent");
                let wait = e.downcast_ref::<Quota>().map_or(QUOTA_WAIT, |q| q.0);
                self.quiet_until = Instant::now() + wait;
                if self.ids.is_empty() {
                    ctx.wait_for(format!("{e:#}"));
                }
                wait
            }
            Err(e) => {
                self.failures += 1;
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "viessmann cloud unreachable");
                if self.failures >= 2 {
                    for id in &self.ids {
                        ctx.set_availability(id, false);
                    }
                }
                if self.ids.is_empty() {
                    ctx.wait_for(moli_i18n::tr!(
                        "pilotes.vicare.injoignable",
                        error = format!("{e:#}")
                    ));
                }
                POLL
            }
        }
    }

    /// Hands one order to the cloud.
    async fn order(&mut self, ctx: &DriverCtx, command: &CommandRequest) -> Result<(), String> {
        let Some(site) = self
            .site
            .as_ref()
            .filter(|_| Instant::now() >= self.quiet_until)
        else {
            return Err(moli_i18n::tr!("pilotes.vicare.suspendu"));
        };
        let native = self
            .layout
            .natives()
            .find(|native| ctx.device_id(native) == command.device.id)
            .ok_or_else(|| moli_i18n::tr!("pilotes.vicare.commande_inconnue"))?;
        let request = self
            .layout
            .request(native, &command.key, &command.value)
            .ok_or_else(|| moli_i18n::tr!("pilotes.vicare.commande_inconnue"))?;
        let path = site.command_path(&request);
        let answer = self
            .cloud
            .api(Method::POST, &path, Some(&request.body))
            .await
            .map_err(|e| format!("{e:#}"))?;
        if answer["data"]["success"] == false {
            let why = answer["data"]["reason"].as_str().unwrap_or("");
            return Err(moli_i18n::tr!("pilotes.vicare.commande_refusee", why = why));
        }
        ctx.set_state(&command.device.id, &command.key, command.value.clone());
        Ok(())
    }
}

async fn run(driver: &Vicare, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (Some(client_id), Some(username), Some(password)) = (
        ctx.secret(CLIENT_ID),
        ctx.secret(USERNAME),
        ctx.secret(PASSWORD),
    ) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.vicare.identifiants_manquants",
            instance = ctx.instance(),
            client = CLIENT_ID,
            user = USERNAME,
            password = PASSWORD
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let mut session = Session {
        cloud: Cloud {
            client_id,
            username,
            password,
            redirect: driver
                .config
                .redirect_uri
                .clone()
                .unwrap_or_else(|| REDIRECT.to_owned()),
            tokens: None,
        },
        installation: driver.config.installation,
        site: None,
        layout: Layout::default(),
        published: Vec::new(),
        ids: Vec::new(),
        failures: 0,
        quiet_until: Instant::now(),
    };
    let mut next = Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = session.order(ctx, &command).await;
                if result.is_ok() {
                    next = next.min(Instant::now() + AFTER_ORDER);
                }
                command.reply(result);
            }
            () = tokio::time::sleep_until(next) => {
                let wait = session.poll(ctx).await;
                next = Instant::now() + wait;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_pkce_challenge_matches_the_rfc() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_quota_wait_follows_the_cloud() {
        let reset = json!({ "extendedPayload": { "limitReset": 1_800_000_600_000_i64 } });
        assert_eq!(quota_wait(&reset, 1_800_000_000), Duration::from_secs(600));
        // Already past, or absurdly far: kept within a minute and a day.
        assert_eq!(quota_wait(&reset, 1_800_000_900), Duration::from_secs(60));
        assert_eq!(
            quota_wait(&reset, 1_700_000_000),
            Duration::from_secs(86_400)
        );
        assert_eq!(quota_wait(&Json::Null, 0), QUOTA_WAIT);
    }
}

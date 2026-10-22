//! ChatGPT Codex sign-in and credentials. OAuth tokens stay in Moli's
//! encrypted secret store; the browser only receives the short-lived device
//! code shown by OpenAI.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::llm::{Endpoint, RequestAuth};
use crate::{Assistant, AssistantError};
use moli_core::InstanceId;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_API: &str = "https://auth.openai.com/api/accounts";
const AUTH_OAUTH: &str = "https://auth.openai.com/oauth";
const CODEX_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const CODEX_API: &str = "https://chatgpt.com/backend-api/codex";
const TOKEN_SECRET: &str = "codex_tokens";
const MODE_SECRET: &str = "auth_mode";
const DEVICE_LIFETIME: Duration = Duration::from_secs(15 * 60);
const REQUEST_LIMIT: Duration = Duration::from_secs(10);
const REFRESH_SKEW: i64 = 60;

pub(super) fn endpoint() -> anyhow::Result<Endpoint> {
    Endpoint::parse(CODEX_API)
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CodexTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    #[serde(default)]
    account_id: Option<String>,
}

impl CodexTokens {
    pub(super) fn request_auth(&self) -> RequestAuth<'_> {
        RequestAuth::Codex {
            access_token: &self.access_token,
            account_id: self.account_id.as_deref(),
        }
    }
}

pub(super) enum AssistantAuth {
    ApiKey(Option<String>),
    Codex(CodexTokens),
}

impl AssistantAuth {
    pub(super) fn request_auth(&self) -> RequestAuth<'_> {
        match self {
            Self::ApiKey(key) => RequestAuth::ApiKey(key.as_deref()),
            Self::Codex(tokens) => tokens.request_auth(),
        }
    }

    pub(super) fn is_codex(&self) -> bool {
        matches!(self, Self::Codex(_))
    }

    pub(super) fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(key) => key.as_deref(),
            Self::Codex(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LoginPhase {
    #[default]
    Idle,
    Starting,
    Waiting,
    Connected,
    Failed,
}

#[derive(Default)]
pub(super) struct LoginState {
    pub(super) phase: LoginPhase,
    verification_url: Option<String>,
    user_code: Option<String>,
    expires_at: Option<i64>,
}

impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginState")
            .field("phase", &self.phase)
            .field(
                "device_code",
                &self.user_code.as_ref().map(|_| "<redacted>"),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(
        default = "default_interval",
        deserialize_with = "deserialize_interval"
    )]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumberOrText {
        Number(u64),
        Text(String),
    }

    match Option::<NumberOrText>::deserialize(deserializer)? {
        Some(NumberOrText::Number(value)) => Ok(value),
        Some(NumberOrText::Text(value)) => value.parse().map_err(serde::de::Error::custom),
        None => Ok(5),
    }
}

struct DeviceCode {
    device_auth_id: String,
    user_code: String,
    interval: Duration,
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

impl Assistant {
    /// Request a device code and poll for completion in a background task.
    pub async fn start_codex_login(&self) -> Result<Json, AssistantError> {
        {
            let mut state = self.0.codex_login.lock().await;
            if matches!(state.phase, LoginPhase::Starting | LoginPhase::Waiting) {
                return Err(AssistantError::Busy);
            }
            state.phase = LoginPhase::Starting;
        }

        let Ok(device) = request_device_code().await else {
            self.0.codex_login.lock().await.phase = LoginPhase::Failed;
            return Err(AssistantError::Upstream(moli_i18n::tr!(
                "assistant.settings.codex_login_failed"
            )));
        };

        {
            let mut state = self.0.codex_login.lock().await;
            state.phase = LoginPhase::Waiting;
            state.verification_url = Some("https://auth.openai.com/codex/device".into());
            state.user_code = Some(device.user_code.clone());
            state.expires_at = Some(
                unix_seconds()
                    .saturating_add(i64::try_from(DEVICE_LIFETIME.as_secs()).unwrap_or(i64::MAX)),
            );
        }
        let user_code = device.user_code.clone();
        let interval = device.interval.as_secs();
        let assistant = self.clone();
        tokio::spawn(async move {
            let phase = if assistant.finish_codex_login(device).await.is_ok() {
                LoginPhase::Connected
            } else {
                LoginPhase::Failed
            };
            let mut state = assistant.0.codex_login.lock().await;
            state.phase = phase;
            state.verification_url = None;
            state.user_code = None;
            state.expires_at = None;
        });

        Ok(json!({
            "verification_url": "https://auth.openai.com/codex/device",
            "user_code": user_code,
            "interval": interval,
            "expires_in": DEVICE_LIFETIME.as_secs(),
        }))
    }

    pub fn codex_login_status(&self) -> Json {
        let state = self.0.codex_login.try_lock().ok();
        let phase = state
            .as_ref()
            .map_or(LoginPhase::Waiting, |state| state.phase);
        json!({
            "phase": match phase {
                LoginPhase::Idle => "idle",
                LoginPhase::Starting => "starting",
                LoginPhase::Waiting => "waiting",
                LoginPhase::Connected => "connected",
                LoginPhase::Failed => "failed",
            },
            "connected": self.read_tokens().is_some(),
            "provider": self.provider_mode(),
            "verification_url": state.as_ref().and_then(|state| state.verification_url.as_deref()),
            "user_code": state.as_ref().and_then(|state| state.user_code.as_deref()),
            "expires_at": state.as_ref().and_then(|state| state.expires_at),
        })
    }

    pub async fn use_auth_provider(&self, provider: &str) -> Result<Json, AssistantError> {
        let provider = provider.trim();
        let allowed = match provider {
            "api_key" => self
                .0
                .hub
                .secret(&InstanceId::from("assistant"), "api_key")
                .is_some(),
            "codex" => self.read_tokens().is_some(),
            "local" => self.0.endpoint.is_local(),
            _ => false,
        };
        if !allowed {
            return Err(AssistantError::Invalid(moli_i18n::tr!(
                "assistant.settings.auth_provider_unavailable"
            )));
        }
        self.0
            .hub
            .store_secret(&InstanceId::from("assistant"), MODE_SECRET, provider)
            .await
            .map_err(|e| {
                AssistantError::Upstream(moli_i18n::tr!("assistant.settings.vault", error = e))
            })?;
        Ok(json!({ "provider": provider }))
    }

    pub(super) fn provider_mode(&self) -> &'static str {
        match self
            .0
            .hub
            .secret(&InstanceId::from("assistant"), MODE_SECRET)
            .as_deref()
        {
            Some("codex") => "codex",
            Some("api_key") => "api_key",
            Some("local") if self.0.endpoint.is_local() => "local",
            _ if self
                .0
                .hub
                .secret(&InstanceId::from("assistant"), "api_key")
                .is_some() =>
            {
                "api_key"
            }
            _ if self.read_tokens().is_some() => "codex",
            _ if self.0.endpoint.is_local() => "local",
            _ => "none",
        }
    }

    pub(super) async fn auth_for_request(&self) -> Result<AssistantAuth, AssistantError> {
        match self.provider_mode() {
            "codex" => self.codex_tokens().await.map(AssistantAuth::Codex),
            "api_key" => self
                .0
                .hub
                .secret(&InstanceId::from("assistant"), "api_key")
                .map(|key| AssistantAuth::ApiKey(Some(key)))
                .or_else(|| {
                    self.0
                        .endpoint
                        .is_local()
                        .then_some(AssistantAuth::ApiKey(None))
                })
                .ok_or_else(|| {
                    AssistantError::NotConfigured(moli_i18n::tr!("assistant.settings.key_missing"))
                }),
            "local" => Ok(AssistantAuth::ApiKey(None)),
            _ => Err(AssistantError::NotConfigured(moli_i18n::tr!(
                "assistant.settings.key_missing"
            ))),
        }
    }

    fn read_tokens(&self) -> Option<CodexTokens> {
        let value = self
            .0
            .hub
            .secret(&InstanceId::from("assistant"), TOKEN_SECRET)?;
        serde_json::from_str(&value).ok()
    }

    pub(super) fn codex_connected(&self) -> bool {
        self.read_tokens().is_some()
    }

    async fn codex_tokens(&self) -> Result<CodexTokens, AssistantError> {
        let assistant = self.clone();
        tokio::spawn(async move { assistant.refresh_codex_tokens().await })
            .await
            .map_err(|_| {
                AssistantError::Upstream(moli_i18n::tr!("assistant.settings.codex_reconnect"))
            })?
    }

    async fn refresh_codex_tokens(&self) -> Result<CodexTokens, AssistantError> {
        let _refresh = self.0.codex_refresh.lock().await;
        let mut tokens = self.read_tokens().ok_or_else(|| {
            AssistantError::NotConfigured(moli_i18n::tr!("assistant.settings.codex_reconnect"))
        })?;
        let now = unix_seconds();
        if tokens.expires_at > now + REFRESH_SKEW {
            return Ok(tokens);
        }

        let body = json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": tokens.refresh_token.as_str(),
            "refresh_reason": "proactive_expiry",
        });
        let refreshed = token_request_json("/token", &body).await;
        let Ok(response) = refreshed else {
            if tokens.expires_at > now {
                return Ok(tokens);
            }
            return Err(AssistantError::NotConfigured(moli_i18n::tr!(
                "assistant.settings.codex_reconnect"
            )));
        };
        if response.access_token.trim().is_empty() {
            return Err(AssistantError::NotConfigured(moli_i18n::tr!(
                "assistant.settings.codex_reconnect"
            )));
        }
        tokens.access_token = response.access_token;
        if let Some(refresh_token) = response.refresh_token.filter(|value| !value.is_empty()) {
            tokens.refresh_token = refresh_token;
        }
        if let Some(id_token) = response.id_token.as_deref() {
            tokens.account_id = account_id(id_token).or(tokens.account_id);
        }
        tokens.expires_at = token_expiry(
            response.expires_in,
            Some(&tokens.access_token),
            unix_seconds(),
        );
        self.store_tokens(&tokens).await?;
        Ok(tokens)
    }

    async fn finish_codex_login(&self, device: DeviceCode) -> Result<(), ()> {
        let authorization = poll_device_token(device).await?;
        let body = form_body(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", authorization.authorization_code.as_str()),
            ("redirect_uri", CODEX_REDIRECT_URI),
            ("code_verifier", authorization.code_verifier.as_str()),
        ]);
        let token_endpoint = Endpoint::parse(AUTH_OAUTH).map_err(|_| ())?;
        let (status, answer) = token_endpoint
            .post_raw(
                "/token",
                "application/x-www-form-urlencoded",
                body.into_bytes(),
                REQUEST_LIMIT,
            )
            .await
            .map_err(|_| ())?;
        if !status.is_success() {
            return Err(());
        }
        let response: TokenResponse = serde_json::from_slice(&answer).map_err(|_| ())?;
        let account_id = response.id_token.as_deref().and_then(account_id);
        if response.access_token.trim().is_empty() {
            return Err(());
        }
        let now = unix_seconds();
        let tokens = CodexTokens {
            expires_at: token_expiry(response.expires_in, Some(&response.access_token), now),
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .filter(|value| !value.is_empty())
                .ok_or(())?,
            account_id,
        };
        self.store_tokens(&tokens).await.map_err(|_| ())?;
        self.0
            .hub
            .store_secret(&InstanceId::from("assistant"), MODE_SECRET, "codex")
            .await
            .map_err(|_| ())?;
        Ok(())
    }

    async fn store_tokens(&self, tokens: &CodexTokens) -> Result<(), AssistantError> {
        let value =
            serde_json::to_string(tokens).map_err(|e| AssistantError::Upstream(e.to_string()))?;
        self.0
            .hub
            .store_secret(&InstanceId::from("assistant"), TOKEN_SECRET, &value)
            .await
            .map_err(|e| {
                AssistantError::Upstream(moli_i18n::tr!("assistant.settings.vault", error = e))
            })
    }
}

async fn request_device_code() -> Result<DeviceCode, ()> {
    let endpoint = Endpoint::parse(AUTH_API).map_err(|_| ())?;
    let body = json!({ "client_id": CLIENT_ID }).to_string().into_bytes();
    let (status, answer) = endpoint
        .post_raw(
            "/deviceauth/usercode",
            "application/json",
            body,
            REQUEST_LIMIT,
        )
        .await
        .map_err(|_| ())?;
    if !status.is_success() {
        return Err(());
    }
    let response: UserCodeResponse = serde_json::from_slice(&answer).map_err(|_| ())?;
    if response.device_auth_id.is_empty()
        || response.device_auth_id.len() > 2_048
        || response.user_code.is_empty()
        || response.user_code.len() > 64
        || !response.user_code.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(());
    }
    Ok(DeviceCode {
        device_auth_id: response.device_auth_id,
        user_code: response.user_code,
        interval: Duration::from_secs(response.interval.clamp(1, 30)),
    })
}

async fn poll_device_token(device: DeviceCode) -> Result<DeviceAuthorization, ()> {
    #[derive(Serialize)]
    struct PollRequest<'a> {
        device_auth_id: &'a str,
        user_code: &'a str,
    }

    let endpoint = Endpoint::parse(AUTH_API).map_err(|_| ())?;
    let started = tokio::time::Instant::now();
    loop {
        let body = serde_json::to_vec(&PollRequest {
            device_auth_id: &device.device_auth_id,
            user_code: &device.user_code,
        })
        .map_err(|_| ())?;
        let (status, answer) = endpoint
            .post_raw("/deviceauth/token", "application/json", body, REQUEST_LIMIT)
            .await
            .map_err(|_| ())?;
        if status.is_success() {
            return serde_json::from_slice(&answer).map_err(|_| ());
        }
        if status != http::StatusCode::FORBIDDEN && status != http::StatusCode::NOT_FOUND {
            return Err(());
        }
        let elapsed = started.elapsed();
        if elapsed >= DEVICE_LIFETIME {
            return Err(());
        }
        tokio::time::sleep(device.interval.min(DEVICE_LIFETIME.saturating_sub(elapsed))).await;
    }
}

async fn token_request_json(path: &str, body: &Json) -> Result<TokenResponse, ()> {
    let endpoint = Endpoint::parse(AUTH_OAUTH).map_err(|_| ())?;
    let (status, answer) = endpoint
        .post_raw(
            path,
            "application/json",
            body.to_string().into_bytes(),
            REQUEST_LIMIT,
        )
        .await
        .map_err(|_| ())?;
    if !status.is_success() {
        return Err(());
    }
    serde_json::from_slice(&answer).map_err(|_| ())
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

fn account_id(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Json = serde_json::from_slice(&decoded).ok()?;
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .or_else(|| claims["chatgpt_account_id"].as_str())
        .map(str::to_owned)
}

fn token_expiry(expires_in: Option<u64>, token: Option<&str>, now: i64) -> i64 {
    let from_claim = token
        .and_then(|value| value.split('.').nth(1))
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|payload| serde_json::from_slice::<Json>(&payload).ok())
        .and_then(|claims| claims["exp"].as_i64());
    from_claim
        .or_else(|| {
            expires_in
                .and_then(|duration| i64::try_from(duration).ok())
                .and_then(|duration| now.checked_add(duration))
        })
        .unwrap_or_else(|| now.saturating_add(3_600))
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_local_model_can_be_selected_after_codex_login() {
        let dir = std::env::temp_dir().join(format!("moli-codex-provider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions {
            secrets: Some((
                dir.join("secrets.json"),
                moli_runtime::MasterKey::generate().unwrap(),
            )),
            ..moli_runtime::HubOptions::default()
        })
        .unwrap();
        let tokens = CodexTokens {
            access_token: "test-access-token".into(),
            refresh_token: "test-refresh-token".into(),
            expires_at: unix_seconds().saturating_add(3_600),
            account_id: None,
        };
        let tokens = serde_json::to_string(&tokens).unwrap();
        hub.store_secret(&InstanceId::from("assistant"), MODE_SECRET, "codex")
            .await
            .unwrap();
        hub.store_secret(&InstanceId::from("assistant"), TOKEN_SECRET, &tokens)
            .await
            .unwrap();
        let config: crate::Config = serde_json::from_value(serde_json::json!({
            "base_url": "http://127.0.0.1:11434/v1",
        }))
        .unwrap();
        let assistant = Assistant::new(hub, None, None, None, config).unwrap();

        assert_eq!(assistant.settings()["provider"], "codex");
        assert_eq!(assistant.settings()["codex"], true);
        assert_eq!(assistant.settings()["local_model"], true);
        let selected = assistant.use_auth_provider("local").await.unwrap();
        assert_eq!(selected["provider"], "local");
        assert_eq!(assistant.settings()["provider"], "local");
        assert!(matches!(
            assistant.auth_for_request().await.unwrap(),
            AssistantAuth::ApiKey(None)
        ));

        drop(assistant);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

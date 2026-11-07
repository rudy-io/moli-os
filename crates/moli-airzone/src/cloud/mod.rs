//! Optional Airzone Cloud **Web API** reader, independent of the local driver.
//!
//! Production origin: https://m.airzonecloud.com/api/v1 (official Web API 1.0).
//! Login is documented for a normal Airzone account, without an OAuth app.
//! No device write, schedule route, arbitrary URL, or network logout is exposed.
//! `with_vault` persists rotating sessions in the existing encrypted Hub store.
mod model;
#[cfg(test)]
mod tests;
mod vault;

pub use model::{
    Access, ConfigSection, Coordinates, Device, DeviceDocument, DeviceMetadata, Group,
    Installation, InstallationDetail, Location, Profile, Temperature,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const HOST: &str = "m.airzonecloud.com";
const PREFIX: &str = "/api/v1";
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY: usize = 2 * 1024 * 1024;
const PAGE_SIZE: usize = 10;
const MAX_INSTALLATIONS: usize = 1000;
const SESSION_TTL: u64 = 48 * 60 * 60;
const REFRESH_TTL: u64 = 7 * 24 * 60 * 60;
const REFRESH_EARLY: u64 = 60;

/// Stable internal codes. Product surfaces must map them to translated messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    InvalidInput,
    AuthenticationRequired,
    MigrationPending,
    Forbidden,
    RateLimited,
    Unavailable,
    Transport,
    InvalidResponse,
    IncompleteInventory,
    Storage,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "airzone_cloud_{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthStatus {
    Disconnected,
    Connected { expires_at: u64 },
    Expired,
    AuthenticationRequired,
    MigrationPending,
}

// Secrets deliberately have no Serialize implementation and no readable Debug.
#[derive(Clone)]
struct Secret(String);
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}
struct Session {
    token: Secret,
    refresh: Secret,
    expires_at: u64,
    refresh_expires_at: u64,
}
struct State {
    session: Option<Session>,
    status: AuthStatus,
}

/// A closed set of read/auth requests; there is no caller-supplied URL/method.
enum Endpoint {
    Login {
        email: Secret,
        password: Secret,
    },
    Refresh(Secret),
    Installations(usize),
    Installation(String),
    DeviceStatus {
        installation: String,
        device: String,
    },
    DeviceConfig {
        installation: String,
        device: String,
        section: ConfigSection,
    },
    GatewayStatus {
        installation: String,
        gateway: String,
        include_devices: bool,
    },
    Location(String),
    Profile,
}
impl Endpoint {
    fn path(&self) -> String {
        let suffix = match self {
            Self::Login { .. } => "/auth/login".into(),
            Self::Refresh(token) => format!("/auth/refreshToken/{}", segment(&token.0)),
            Self::Installations(page) => format!("/installations?items={PAGE_SIZE}&page={page}"),
            Self::Installation(id) => format!("/installations/{}", segment(id)),
            Self::DeviceStatus {
                installation,
                device,
            } => format!(
                "/devices/{}/status?installation_id={}",
                segment(device),
                segment(installation)
            ),
            Self::DeviceConfig {
                installation,
                device,
                section,
            } => format!(
                "/devices/{}/config?installation_id={}&type={}",
                segment(device),
                segment(installation),
                section.query()
            ),
            Self::GatewayStatus {
                installation,
                gateway,
                include_devices,
            } => format!(
                "/devices/ws/{}/status?installation_id={}{}",
                segment(gateway),
                segment(installation),
                if *include_devices { "&devices=1" } else { "" }
            ),
            Self::Location(id) => format!("/installations/location/{}", segment(id)),
            Self::Profile => "/user/".into(),
        };
        format!("{PREFIX}{suffix}")
    }
}
struct Reply {
    status: u16,
    body: Value,
}
type Exchange<'a> = Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>>;
trait Transport: Send + Sync {
    fn exchange(&self, endpoint: Endpoint, token: Option<Secret>) -> Exchange<'_>;
}
struct Network;
impl Transport for Network {
    fn exchange(&self, endpoint: Endpoint, token: Option<Secret>) -> Exchange<'_> {
        Box::pin(async move {
            let mut builder = http::Request::builder().uri(endpoint.path());
            if let Some(token) = token {
                let mut header = http::HeaderValue::from_str(&format!("Bearer {}", token.0))
                    .map_err(|_| Error::InvalidInput)?;
                header.set_sensitive(true);
                builder = builder.header(http::header::AUTHORIZATION, header);
            }
            let request = if let Endpoint::Login { email, password } = endpoint {
                moli_net::json_body(
                    builder.method("POST"),
                    &json!({"email":email.0,"password":password.0}),
                )
            } else {
                moli_net::empty(builder.method("GET"))
            }
            .map_err(|_| Error::InvalidInput)?;
            // No redirects, alternate server, unverified TLS, or backend errors
            // escape this boundary (refresh credentials occur in the URI).
            let (status, body) = moli_net::web(HOST, 443, true, request, TIMEOUT, MAX_BODY)
                .await
                .map_err(|_| Error::Transport)?;
            // Error bodies can contain account data. Discard them unread.
            let body = if status.as_u16() == 200 {
                serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse)?
            } else {
                Value::Null
            };
            Ok(Reply {
                status: status.as_u16(),
                body,
            })
        })
    }
}

/// Serializing auth and reads prevents simultaneous use of a one-use refresh
/// token. Requests are bounded; failed/cancelled refreshes require a new login.
pub struct CloudClient {
    transport: Arc<dyn Transport>,
    state: Mutex<State>,
    vault: Option<Arc<dyn vault::Vault>>,
}
impl fmt::Debug for CloudClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudClient").finish_non_exhaustive()
    }
}
impl Default for CloudClient {
    fn default() -> Self {
        Self::new()
    }
}
impl CloudClient {
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(Arc::new(Network))
    }
    /// Restore an encrypted token bundle without making any network request.
    /// Login refuses to contact Airzone if this Hub has no usable secret store.
    #[must_use]
    pub fn with_vault(hub: moli_runtime::Hub) -> Self {
        Self::build(Arc::new(Network), Some(Arc::new(vault::HubVault(hub))))
    }
    fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self::build(transport, None)
    }
    fn build(transport: Arc<dyn Transport>, vault: Option<Arc<dyn vault::Vault>>) -> Self {
        let mut state = State {
            session: None,
            status: AuthStatus::Disconnected,
        };
        if let Some(stored) = vault.as_ref().and_then(|vault| vault.load()) {
            match vault::restore(&stored) {
                Ok(session) => {
                    state.status = AuthStatus::Connected {
                        expires_at: session.expires_at,
                    };
                    state.session = Some(session);
                }
                Err(_) => state.status = AuthStatus::AuthenticationRequired,
            }
        }
        Self {
            transport,
            state: Mutex::new(state),
            vault,
        }
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<AuthStatus, Error> {
        if email.is_empty()
            || email.len() > 320
            || !email.contains('@')
            || email.chars().any(char::is_control)
            || !(8..=64).contains(&password.chars().count())
        {
            return Err(Error::InvalidInput);
        }
        let mut state = self.state.lock().await;
        state.session = None;
        state.status = AuthStatus::AuthenticationRequired;
        self.save(None).await?;
        let result = self
            .transport
            .exchange(
                Endpoint::Login {
                    email: Secret(email.to_owned()),
                    password: Secret(password.to_owned()),
                },
                None,
            )
            .await?;
        if result.status == 400 {
            // The documented login failures include an unconfirmed account.
            // Do not expose the provider's account-specific error body.
            return Err(Error::AuthenticationRequired);
        }
        let value = success(result)?;
        if value
            .get("pendingMigration")
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err(Error::InvalidResponse);
        }
        if value.get("pendingMigration").and_then(Value::as_bool) == Some(true) {
            state.status = AuthStatus::MigrationPending;
            return Err(Error::MigrationPending);
        }
        let session = session(value, unix_now())?;
        self.save(Some(&session)).await?;
        state.status = AuthStatus::Connected {
            expires_at: session.expires_at,
        };
        state.session = Some(session);
        Ok(state.status)
    }

    pub async fn auth_status(&self) -> AuthStatus {
        let state = self.state.lock().await;
        match &state.session {
            Some(session) if session.expires_at <= unix_now() => AuthStatus::Expired,
            _ => state.status,
        }
    }

    /// Forget this reader's session only; never invalidates another app's login.
    pub async fn disconnect(&self) -> Result<(), Error> {
        let mut state = self.state.lock().await;
        state.session = None;
        state.status = AuthStatus::AuthenticationRequired;
        self.save(None).await?;
        state.status = AuthStatus::Disconnected;
        Ok(())
    }

    /// Returns a complete, deduplicated inventory, or an explicit error. A page
    /// limit, repeated page, changed total or early empty page is never success.
    pub async fn installations(&self) -> Result<Vec<Installation>, Error> {
        tokio::time::timeout(Duration::from_secs(30), self.all_installations())
            .await
            .map_err(|_| Error::IncompleteInventory)?
    }
    async fn all_installations(&self) -> Result<Vec<Installation>, Error> {
        #[derive(Deserialize)]
        struct Page {
            total: usize,
            installations: Vec<Installation>,
        }
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        let mut total = None;
        for page in 0..=MAX_INSTALLATIONS / PAGE_SIZE {
            let value = self.read(Endpoint::Installations(page)).await?;
            let page: Page = serde_json::from_value(value).map_err(|_| Error::InvalidResponse)?;
            if page.total > MAX_INSTALLATIONS
                || total.is_some_and(|n| n != page.total)
                || page.installations.len() > PAGE_SIZE
            {
                return Err(Error::IncompleteInventory);
            }
            total = Some(page.total);
            if page.installations.is_empty() && result.len() != page.total {
                return Err(Error::IncompleteInventory);
            }
            for item in page.installations {
                identifier(&item.installation_id)?;
                if !seen.insert(item.installation_id.clone()) {
                    return Err(Error::IncompleteInventory);
                }
                result.push(item);
            }
            if result.len() == page.total {
                return Ok(result);
            }
            if result.len() > page.total {
                return Err(Error::IncompleteInventory);
            }
        }
        Err(Error::IncompleteInventory)
    }

    pub async fn installation(&self, id: &str) -> Result<InstallationDetail, Error> {
        identifier(id)?;
        let value = self.read(Endpoint::Installation(id.to_owned())).await?;
        let result: InstallationDetail =
            serde_json::from_value(value).map_err(|_| Error::InvalidResponse)?;
        if result.installation_id != id {
            return Err(Error::InvalidResponse);
        }
        for group in &result.groups {
            identifier(&group.group_id)?;
            for device in &group.devices {
                identifier(&device.device_id)?;
            }
        }
        Ok(result)
    }

    pub async fn device_status(
        &self,
        installation: &str,
        device: &str,
    ) -> Result<DeviceDocument, Error> {
        identifier(installation)?;
        identifier(device)?;
        DeviceDocument::parse(
            self.read(Endpoint::DeviceStatus {
                installation: installation.to_owned(),
                device: device.to_owned(),
            })
            .await?,
        )
    }
    pub async fn device_config(
        &self,
        installation: &str,
        device: &str,
        section: ConfigSection,
    ) -> Result<DeviceDocument, Error> {
        identifier(installation)?;
        identifier(device)?;
        DeviceDocument::parse(
            self.read(Endpoint::DeviceConfig {
                installation: installation.to_owned(),
                device: device.to_owned(),
                section,
            })
            .await?,
        )
    }
    pub async fn gateway_status(
        &self,
        installation: &str,
        gateway: &str,
        include_devices: bool,
    ) -> Result<DeviceDocument, Error> {
        identifier(installation)?;
        identifier(gateway)?;
        DeviceDocument::parse(
            self.read(Endpoint::GatewayStatus {
                installation: installation.to_owned(),
                gateway: gateway.to_owned(),
                include_devices,
            })
            .await?,
        )
    }
    pub async fn location(&self, id: &str) -> Result<Location, Error> {
        identifier(id)?;
        let result: Location =
            serde_json::from_value(self.read(Endpoint::Location(id.to_owned())).await?)
                .map_err(|_| Error::InvalidResponse)?;
        if result.id != id {
            return Err(Error::InvalidResponse);
        }
        Ok(result)
    }
    pub async fn profile(&self) -> Result<Profile, Error> {
        serde_json::from_value(self.read(Endpoint::Profile).await?)
            .map_err(|_| Error::InvalidResponse)
    }

    async fn read(&self, endpoint: Endpoint) -> Result<Value, Error> {
        let mut state = self.state.lock().await;
        let existing = state
            .session
            .as_ref()
            .ok_or(Error::AuthenticationRequired)?;
        let refreshed = existing.expires_at <= unix_now().saturating_add(REFRESH_EARLY);
        if refreshed {
            self.refresh(&mut state).await?;
        }
        // Rebuild the closed read endpoint only; login and refresh are never retried.
        let retry = endpoint.read_copy()?;
        let token = state
            .session
            .as_ref()
            .ok_or(Error::AuthenticationRequired)?
            .token
            .clone();
        let mut reply = self.transport.exchange(endpoint, Some(token)).await?;
        if reply.status == 401 && !refreshed {
            self.refresh(&mut state).await?;
            let token = state
                .session
                .as_ref()
                .ok_or(Error::AuthenticationRequired)?
                .token
                .clone();
            reply = self.transport.exchange(retry, Some(token)).await?;
        }
        if reply.status == 401 {
            state.session = None;
            state.status = AuthStatus::AuthenticationRequired;
            self.save(None).await?;
        }
        let value = success(reply)?;
        if !model::bounded(&value, 0) {
            return Err(Error::InvalidResponse);
        }
        Ok(value)
    }

    async fn refresh(&self, state: &mut State) -> Result<(), Error> {
        let old = state.session.take().ok_or(Error::AuthenticationRequired)?;
        state.status = AuthStatus::AuthenticationRequired;
        self.save(None).await?;
        if old.refresh_expires_at <= unix_now() {
            return Err(Error::AuthenticationRequired);
        }
        // Consume before I/O: after a timeout/cancellation the provider may have
        // rotated this token. Reusing it could revoke the replacement session.
        let reply = self
            .transport
            .exchange(Endpoint::Refresh(old.refresh), None)
            .await?;
        if matches!(reply.status, 400 | 403) {
            return Err(Error::AuthenticationRequired);
        }
        let value = success(reply)?;
        let session = session(value, unix_now())?;
        self.save(Some(&session)).await?;
        state.status = AuthStatus::Connected {
            expires_at: session.expires_at,
        };
        state.session = Some(session);
        Ok(())
    }
    async fn save(&self, session: Option<&Session>) -> Result<(), Error> {
        if let Some(vault) = &self.vault {
            if !vault.available() {
                return Err(Error::Storage);
            }
            let stored = session.map(vault::encode).transpose()?;
            vault.write(stored).await?;
        }
        Ok(())
    }
}

impl Endpoint {
    fn read_copy(&self) -> Result<Self, Error> {
        Ok(match self {
            Self::Installations(page) => Self::Installations(*page),
            Self::Installation(id) => Self::Installation(id.clone()),
            Self::DeviceStatus {
                installation,
                device,
            } => Self::DeviceStatus {
                installation: installation.clone(),
                device: device.clone(),
            },
            Self::DeviceConfig {
                installation,
                device,
                section,
            } => Self::DeviceConfig {
                installation: installation.clone(),
                device: device.clone(),
                section: *section,
            },
            Self::GatewayStatus {
                installation,
                gateway,
                include_devices,
            } => Self::GatewayStatus {
                installation: installation.clone(),
                gateway: gateway.clone(),
                include_devices: *include_devices,
            },
            Self::Location(id) => Self::Location(id.clone()),
            Self::Profile => Self::Profile,
            Self::Login { .. } | Self::Refresh(_) => return Err(Error::InvalidInput),
        })
    }
}
fn success(reply: Reply) -> Result<Value, Error> {
    match reply.status {
        200 => Ok(reply.body),
        401 => Err(Error::AuthenticationRequired),
        403 => Err(Error::Forbidden),
        429 => Err(Error::RateLimited),
        _ => Err(Error::Unavailable),
    }
}
fn session(value: Value, now: u64) -> Result<Session, Error> {
    #[derive(Deserialize)]
    struct Tokens {
        token: String,
        #[serde(rename = "refreshToken")]
        refresh: String,
    }
    let tokens: Tokens = serde_json::from_value(value).map_err(|_| Error::InvalidResponse)?;
    if tokens.token.len() > 16 * 1024
        || tokens.refresh.is_empty()
        || matches!(tokens.refresh.as_str(), "." | "..")
        || tokens.refresh.len() > 4096
        || tokens.token.chars().any(char::is_control)
        || tokens.refresh.chars().any(char::is_control)
    {
        return Err(Error::InvalidResponse);
    }
    let expires_at = jwt_expiry(&tokens.token)?.min(now.saturating_add(SESSION_TTL));
    if expires_at <= now {
        return Err(Error::AuthenticationRequired);
    }
    Ok(Session {
        token: Secret(tokens.token),
        refresh: Secret(tokens.refresh),
        expires_at,
        refresh_expires_at: now.saturating_add(REFRESH_TTL),
    })
}

fn identifier(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':'))
    {
        return Err(Error::InvalidInput);
    }
    Ok(())
}
fn segment(value: &str) -> String {
    use fmt::Write as _;
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            result.push(char::from(byte));
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
// JWT payload inspection is only an expiry scheduling hint. HTTPS authenticates
// the issuing server; this does not purport to validate JWT signatures locally.
fn jwt_expiry(token: &str) -> Result<u64, Error> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(Error::InvalidResponse);
    }
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0u32);
    for byte in parts[1].bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err(Error::InvalidResponse),
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(((bits >> count) & 255) as u8);
        }
    }
    let payload: Value = serde_json::from_slice(&out).map_err(|_| Error::InvalidResponse)?;
    payload["exp"].as_u64().ok_or(Error::InvalidResponse)
}

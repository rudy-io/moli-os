//! Tokens use the existing encrypted Hub store, never an Airzone config file.
use super::{Error, REFRESH_TTL, SESSION_TTL, Secret, Session, jwt_expiry, unix_now};
use moli_runtime::Hub;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

const KEY: &str = "airzone_cloud_session";
type Write<'a> = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>>;
pub(super) trait Vault: Send + Sync {
    fn available(&self) -> bool;
    fn load(&self) -> Option<String>;
    fn write(&self, value: Option<String>) -> Write<'_>;
}
pub(super) struct HubVault(pub Hub);
impl Vault for HubVault {
    fn available(&self) -> bool {
        self.0.has_secret_store()
    }
    fn load(&self) -> Option<String> {
        self.0.core_secret(KEY)
    }
    fn write(&self, value: Option<String>) -> Write<'_> {
        Box::pin(async move {
            match value {
                Some(value) => self
                    .0
                    .store_core_secret(KEY, &value)
                    .await
                    .map_err(|_| Error::Storage),
                None => self
                    .0
                    .forget_core_secret(KEY)
                    .await
                    .map(|_| ())
                    .map_err(|_| Error::Storage),
            }
        })
    }
}

// Only this private envelope is serializable. Never return it from application
// endpoints, derive Debug, or place it in tracing fields.
#[derive(Deserialize, Serialize)]
struct Stored {
    version: u8,
    token: String,
    refresh: String,
    expires_at: u64,
    refresh_expires_at: u64,
}
pub(super) fn encode(session: &Session) -> Result<String, Error> {
    serde_json::to_string(&Stored {
        version: 1,
        token: session.token.0.clone(),
        refresh: session.refresh.0.clone(),
        expires_at: session.expires_at,
        refresh_expires_at: session.refresh_expires_at,
    })
    .map_err(|_| Error::Storage)
}
pub(super) fn restore(value: &str) -> Result<Session, Error> {
    if value.len() > 32 * 1024 {
        return Err(Error::InvalidResponse);
    }
    let data: Stored = serde_json::from_str(value).map_err(|_| Error::InvalidResponse)?;
    let now = unix_now();
    if data.version != 1
        || data.token.len() > 16 * 1024
        || data.refresh.is_empty()
        || matches!(data.refresh.as_str(), "." | "..")
        || data.refresh.len() > 4096
        || data.refresh.chars().any(char::is_control)
        || data.token.chars().any(char::is_control)
        || data.expires_at > now.saturating_add(SESSION_TTL)
        || data.expires_at > jwt_expiry(&data.token)?
        || data.refresh_expires_at > now.saturating_add(REFRESH_TTL)
        || data.refresh_expires_at <= now
    {
        return Err(Error::AuthenticationRequired);
    }
    Ok(Session {
        token: Secret(data.token),
        refresh: Secret(data.refresh),
        expires_at: data.expires_at,
        refresh_expires_at: data.refresh_expires_at,
    })
}

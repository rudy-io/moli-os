use super::{MAX_PENDING_BYTES, MAX_STATE_BYTES, bounded_text};
use crate::inventory::{Region, Scope};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "poll")]
    pub poll_secs: u64,
    /// Runtime fence copied from the enclosing Overkiz config, never a cloud TOML override.
    #[serde(skip, default = "read_only")]
    pub read_only: bool,
}
const fn read_only() -> bool {
    true
}
const fn poll() -> u64 {
    1800
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_secs: poll(),
            read_only: true,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        if (300..=86_400).contains(&self.poll_secs) {
            Ok(())
        } else {
            Err(Error::InvalidInput)
        }
    }
}

/// Only local owner requests. Login fields are consumed, never copied into session/state.
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PendingRequest {
    Login {
        username: String,
        password: String,
    },
    SelectSite {
        generation: String,
        site_oid: String,
        gateway_id: String,
        region: Option<Region>,
    },
    Refresh {
        generation: String,
    },
    ScenarioPermission {
        generation: String,
        revision: String,
        scenario_id: String,
        enabled: bool,
    },
}
impl fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PendingRequest([redacted])")
    }
}
impl PendingRequest {
    pub fn validate(&self) -> Result<(), Error> {
        let valid = match self {
            Self::Login { username, password } => {
                bounded_text(username, 1024) && !password.is_empty() && password.len() <= 4096
            }
            Self::SelectSite {
                generation,
                site_oid,
                gateway_id,
                ..
            } => {
                bounded_text(generation, 64)
                    && bounded_text(site_oid, 256)
                    && bounded_text(gateway_id, 256)
            }
            Self::Refresh { generation } => bounded_text(generation, 64),
            Self::ScenarioPermission {
                generation,
                revision,
                scenario_id,
                ..
            } => {
                bounded_text(generation, 64)
                    && revision.len() == 64
                    && revision.bytes().all(|b| b.is_ascii_hexdigit())
                    && super::execution::valid_oid(scenario_id)
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::InvalidInput)
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub version: u8,
    pub request_id: String,
    pub submitted_at_ms: i64,
    pub request: PendingRequest,
}
impl fmt::Debug for RequestEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequestEnvelope([redacted])")
    }
}
impl RequestEnvelope {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || !bounded_text(&self.request_id, 64) || self.submitted_at_ms < 0 {
            return Err(Error::InvalidInput);
        }
        self.request.validate()
    }
}
pub fn decode_request(body: &[u8]) -> Result<RequestEnvelope, Error> {
    if body.len() > MAX_PENDING_BYTES {
        return Err(Error::InvalidInput);
    }
    let value: RequestEnvelope = serde_json::from_slice(body).map_err(|_| Error::InvalidInput)?;
    value.validate()?;
    Ok(value)
}
pub fn encode_request(value: &RequestEnvelope) -> Result<Vec<u8>, Error> {
    value.validate()?;
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidInput)?;
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(Error::InvalidInput);
    }
    Ok(bytes)
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SiteCandidate {
    /// BOB sub-site externalOID: selected setup/catalogue identity, not OAuth scope.
    pub site_oid: String,
    /// Parent BOB siteOID, required by Ginaite's OAuth siteOID query parameter.
    pub home_id: String,
    pub label: Option<String>,
    pub country: Option<String>,
    pub gateway_ids: Vec<String>,
    pub region: Option<Region>,
}
impl fmt::Debug for SiteCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SiteCandidate([redacted])")
    }
}
impl SiteCandidate {
    pub(super) fn validate(&self) -> Result<(), Error> {
        if !bounded_text(&self.site_oid, 256)
            || !bounded_text(&self.home_id, 256)
            || self.label.as_ref().is_some_and(|v| !bounded_text(v, 256))
            || self.country.as_ref().is_some_and(|v| !bounded_text(v, 8))
            || self.gateway_ids.is_empty()
            || self.gateway_ids.len() > 200
            || self.gateway_ids.iter().any(|v| !bounded_text(v, 256))
            || self
                .gateway_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.gateway_ids.len()
        {
            return Err(Error::InvalidResponse);
        }
        Ok(())
    }
}
// Reserve enough envelope space so accepted discovery always fits the state DTO.
pub(super) fn validate_candidates(candidates: &[SiteCandidate]) -> Result<(), Error> {
    if candidates.len() > 200
        || candidates
            .iter()
            .map(|c| c.gateway_ids.len())
            .sum::<usize>()
            > 200
        || candidates
            .iter()
            .map(|c| &c.site_oid)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != candidates.len()
        || serde_json::to_vec(candidates)
            .map_err(|_| Error::InvalidResponse)?
            .len()
            > MAX_STATE_BYTES - 8192
    {
        return Err(Error::DiscoveryLimit);
    }
    for candidate in candidates {
        candidate.validate()?;
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionPhase {
    Disconnected,
    SelectingSite,
    Ready,
    ReconnectRequired,
    PersistencePending,
    Unavailable,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionState {
    pub version: u8,
    pub phase: ConnectionPhase,
    pub generation: Option<String>,
    pub request_id: Option<String>,
    pub scope: Option<Scope>,
    pub candidates: Vec<SiteCandidate>,
    pub failure: Option<Error>,
    pub last_attempt_ms: Option<i64>,
    pub last_observation_ms: Option<i64>,
    pub retry_after_secs: Option<u64>,
}
impl fmt::Debug for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionState")
            .field("phase", &self.phase)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}
impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            version: 1,
            phase: ConnectionPhase::Disconnected,
            generation: None,
            request_id: None,
            scope: None,
            candidates: vec![],
            failure: None,
            last_attempt_ms: None,
            last_observation_ms: None,
            retry_after_secs: None,
        }
    }
}
impl ConnectionState {
    fn clamp_retry(&mut self) {
        self.retry_after_secs = self
            .retry_after_secs
            .map(|seconds| seconds.min(super::MAX_RETRY_SECS));
        self.failure = self.failure.map(Error::public_failure);
        if let Some(Error::RateLimited { retry_after_secs }) = &mut self.failure {
            *retry_after_secs = (*retry_after_secs).min(super::MAX_RETRY_SECS);
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || self
                .request_id
                .as_ref()
                .is_some_and(|v| !bounded_text(v, 64))
            || self
                .generation
                .as_ref()
                .is_some_and(|v| !bounded_text(v, 64))
            || self.candidates.len() > 200
            || self
                .candidates
                .iter()
                .map(|c| c.gateway_ids.len())
                .sum::<usize>()
                > 200
            || self
                .candidates
                .iter()
                .map(|c| &c.site_oid)
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.candidates.len()
            || self.last_attempt_ms.is_some_and(|v| v < 0)
            || self.last_observation_ms.is_some_and(|v| v < 0)
        {
            return Err(Error::InvalidInput);
        }
        if let Some(scope) = &self.scope
            && (!bounded_text(&scope.site_oid, 256) || !bounded_text(&scope.gateway_id, 256))
        {
            return Err(Error::InvalidInput);
        }
        validate_candidates(&self.candidates)?;
        if self.scope.as_ref().is_some_and(|s| {
            self.generation.is_none()
                || !self.candidates.iter().any(|c| {
                    c.site_oid == s.site_oid
                        && c.gateway_ids.contains(&s.gateway_id)
                        && c.region.is_none_or(|r| r == s.region)
                })
        }) || (self.phase == ConnectionPhase::Ready && self.scope.is_none())
        {
            return Err(Error::InvalidInput);
        }
        Ok(())
    }
}
pub fn decode_state(body: &[u8]) -> Result<ConnectionState, Error> {
    if body.len() > MAX_STATE_BYTES {
        return Err(Error::InvalidInput);
    }
    let mut value: ConnectionState =
        serde_json::from_slice(body).map_err(|_| Error::InvalidInput)?;
    value.validate()?;
    value.clamp_retry();
    Ok(value)
}
pub fn encode_state(value: &ConnectionState) -> Result<Vec<u8>, Error> {
    let mut value = value.clone();
    value.clamp_retry();
    value.validate()?;
    let bytes = serde_json::to_vec(&value).map_err(|_| Error::InvalidInput)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(Error::InvalidInput);
    }
    Ok(bytes)
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Error {
    InvalidInput,
    InvalidResponse,
    LoginRequired,
    Network,
    NotDispatched,
    Deadline,
    Persistence,
    DiscoveryLimit,
    ScopeMismatch,
    Forbidden,
    ExecutionUncertain,
    RateLimited { retry_after_secs: u64 },
}
impl Error {
    pub(super) const fn public_failure(self) -> Self {
        match self {
            Self::NotDispatched => Self::Network,
            other => other,
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidInput => moli_i18n::tr!("overkiz.cloud_input"),
            Self::InvalidResponse => moli_i18n::tr!("overkiz.cloud_response"),
            Self::LoginRequired => moli_i18n::tr!("overkiz.cloud_login"),
            Self::Network | Self::NotDispatched => moli_i18n::tr!("overkiz.cloud_network"),
            Self::Deadline => moli_i18n::tr!("overkiz.cloud_deadline"),
            Self::Persistence => moli_i18n::tr!("overkiz.cloud_persistence"),
            Self::DiscoveryLimit => moli_i18n::tr!("overkiz.cloud_discovery"),
            Self::ScopeMismatch => moli_i18n::tr!("overkiz.cloud_scope"),
            Self::Forbidden => moli_i18n::tr!("overkiz.cloud_forbidden"),
            Self::ExecutionUncertain => moli_i18n::tr!("overkiz.cloud_execution_uncertain"),
            Self::RateLimited { .. } => moli_i18n::tr!("overkiz.cloud_rate"),
        };
        f.write_str(&message)
    }
}
impl std::error::Error for Error {}

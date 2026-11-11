use super::protocol::valid_token;
use super::{Error, MAX_SESSION_BYTES, SiteCandidate, bounded_text};
use crate::inventory::{CatalogIdentity, Scope};
use moli_runtime::BoxFuture;
use serde::{Deserialize, Serialize};

/// An acknowledgement means the encrypted store has durably accepted the exact bytes.
/// Cancellation/failure is not an acknowledgement. Never retry an old refresh after it.
pub(crate) trait SessionSink: Send {
    fn save<'a>(&'a mut self, encoded: &'a [u8]) -> BoxFuture<'a, Result<(), Error>>;
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Ready,
    Refreshing,
    Incomplete,
    LoginRequired,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Session {
    pub version: u8,
    pub instance: String,
    pub generation: String,
    pub phase: SessionPhase,
    pub scope: Option<Scope>,
    pub token_site_oid: Option<String>,
    pub access: Option<String>,
    pub refresh: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub candidates: Vec<SiteCandidate>,
}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}
pub(super) use SessionPhase as Phase;

impl Session {
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_scope(false)
    }
    fn validate_scope(&self, allow_legacy_setup_scope: bool) -> Result<(), Error> {
        if self.version != 1
            || !bounded_text(&self.instance, 256)
            || !bounded_text(&self.generation, 64)
            || self.issued_at_ms < 0
            || self.expires_at_ms < 0
            || self.expires_at_ms.saturating_sub(self.issued_at_ms) > 86_400_000
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
            || self.access.as_ref().is_some_and(|v| !valid_token(v))
            || self.refresh.as_ref().is_some_and(|v| !valid_token(v))
            || self
                .token_site_oid
                .as_ref()
                .is_some_and(|v| !bounded_text(v, 256))
        {
            return Err(Error::InvalidResponse);
        }
        super::types::validate_candidates(&self.candidates)?;
        let parent = self.oauth_site_oid()?;
        if let Some(site) = &self.token_site_oid {
            let legacy = allow_legacy_setup_scope
                && self.scope.as_ref().is_some_and(|s| s.site_oid == *site);
            if Some(site.as_str()) != parent && !legacy {
                return Err(Error::ScopeMismatch);
            }
        }
        if self.phase == Phase::Ready
            && (self.access.is_none()
                || self.refresh.is_none()
                || self.expires_at_ms <= self.issued_at_ms)
        {
            return Err(Error::InvalidResponse);
        }
        Ok(())
    }
    /// OAuth scopes the parent BOB home; catalogue identity remains the selected setup.
    pub fn oauth_site_oid(&self) -> Result<Option<&str>, Error> {
        let Some(scope) = &self.scope else {
            return Ok(None);
        };
        self.candidates
            .iter()
            .find(|c| {
                c.site_oid == scope.site_oid
                    && c.gateway_ids.contains(&scope.gateway_id)
                    && c.region.is_none_or(|r| r == scope.region)
            })
            .map(|c| Some(c.home_id.as_str()))
            .ok_or(Error::ScopeMismatch)
    }
    pub fn identity(&self) -> Result<CatalogIdentity, Error> {
        Ok(CatalogIdentity {
            instance: self.instance.clone(),
            connection_generation: self.generation.clone(),
            scope: self.scope.clone().ok_or(Error::ScopeMismatch)?,
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| Error::InvalidResponse)?;
        if bytes.len() > MAX_SESSION_BYTES {
            return Err(Error::InvalidResponse);
        }
        Ok(bytes)
    }
    pub fn decode(body: &[u8], instance: &str) -> Result<Self, Error> {
        if body.len() > MAX_SESSION_BYTES {
            return Err(Error::InvalidResponse);
        }
        let mut session: Self = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
        // Older v1 records used externalOID for OAuth. Validate every field before
        // retiring that bearer; only the actor may checkpoint and re-scope its refresh.
        session.validate_scope(true)?;
        if session.instance != instance {
            return Err(Error::ScopeMismatch);
        }
        if session.token_site_oid.is_some()
            && session.token_site_oid.as_deref() != session.oauth_site_oid()?
        {
            session.token_site_oid = None;
            session.access = None;
            session.expires_at_ms = 0;
            if session.phase == Phase::Ready {
                session.phase = Phase::Incomplete;
            }
            // Refreshing/LoginRequired stay terminal: their refresh may be consumed.
        }
        session.validate()?;
        Ok(session)
    }
}

/// Safe binding for owner-only cached APIs. Require exact generation/scope equality
/// with retained observations. Phase qualifies freshness; same-scope stale history may
/// remain visible after auth failure. This alone does not verify gateway membership.
#[derive(Clone, Serialize)]
pub struct SessionBinding {
    pub generation: String,
    pub scope: Option<Scope>,
    pub phase: SessionPhase,
}
impl std::fmt::Debug for SessionBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionBinding")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}
/// Validate the entire bounded private record, but return no authentication material.
pub fn read_session_binding(body: &[u8], instance: &str) -> Result<SessionBinding, Error> {
    let session = Session::decode(body, instance)?;
    Ok(SessionBinding {
        generation: session.generation,
        scope: session.scope,
        phase: session.phase,
    })
}

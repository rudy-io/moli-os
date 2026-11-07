use super::protocol::{
    self, CYCLE_LIMIT, Endpoint, Https, Query, REQUEST_LIMIT, Request, Response, Transport, form,
    valid_token,
};
use super::session::{Phase, Session, SessionSink};
use super::{ConnectionPhase, ConnectionState, Error, SiteCandidate, bounded_text};
use crate::inventory::{
    self, Catalog, CatalogIdentity, CollectionFailure, CollectionKind, Region, Scope,
};
use serde::Deserialize;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

/// Owned by one actor. All mutating futures borrow exclusively, including persistence.
pub(crate) struct CloudClient<T = Https> {
    instance: String,
    execution_read_only: bool,
    transport: T,
    session: Option<Session>,
    dirty: bool,
    access_deadline: Option<Instant>,
    verified: bool,
    failure: Option<Error>,
    next_allowed: Option<Instant>,
    unrepresentable_retry: Option<u64>,
    backoff: u64,
}
impl<T> std::fmt::Debug for CloudClient<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudClient([redacted])")
    }
}
impl CloudClient<Https> {
    pub(crate) fn new(instance: &str) -> Result<Self, Error> {
        Self::with_transport(instance, Https)
    }
}
impl<T: Transport> CloudClient<T> {
    pub(crate) fn with_transport(instance: &str, transport: T) -> Result<Self, Error> {
        if !bounded_text(instance, 256) {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            instance: instance.into(),
            execution_read_only: true,
            transport,
            session: None,
            dirty: false,
            access_deadline: None,
            verified: false,
            failure: None,
            next_allowed: None,
            unrepresentable_retry: None,
            backoff: 60,
        })
    }
    pub(crate) fn state(&self) -> ConnectionState {
        let phase = match self.session.as_ref().map(|s| s.phase) {
            None => ConnectionPhase::Disconnected,
            Some(Phase::LoginRequired | Phase::Refreshing) => ConnectionPhase::ReconnectRequired,
            _ if self.dirty => ConnectionPhase::PersistencePending,
            _ if self.failure.is_some() => ConnectionPhase::Unavailable,
            _ if self.verified => ConnectionPhase::Ready,
            _ => ConnectionPhase::SelectingSite,
        };
        ConnectionState {
            version: 1,
            phase,
            request_id: None,
            generation: self.session.as_ref().map(|s| s.generation.clone()),
            scope: self.session.as_ref().and_then(|s| s.scope.clone()),
            candidates: self
                .session
                .as_ref()
                .map_or_else(Vec::new, |s| s.candidates.clone()),
            failure: self.failure.map(Error::public_failure),
            last_attempt_ms: None,
            last_observation_ms: None,
            retry_after_secs: self.retry_remaining(),
        }
    }
    /// Selection only, for initializing an empty or retained cache. Not live verification.
    pub(crate) fn selected_identity(&self) -> Result<CatalogIdentity, Error> {
        self.session
            .as_ref()
            .ok_or(Error::LoginRequired)?
            .identity()
    }
    /// Resume usable unscoped auth even when the last discovery failure masks its UI phase.
    pub(crate) fn can_resume_discovery(&self) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.scope.is_none()
                && session.refresh.is_some()
                && matches!(session.phase, Phase::Ready | Phase::Incomplete)
        })
    }
    pub(crate) fn identity(&self) -> Result<CatalogIdentity, Error> {
        if !self.verified || self.dirty {
            return Err(Error::ScopeMismatch);
        }
        self.session
            .as_ref()
            .ok_or(Error::LoginRequired)?
            .identity()
    }
    /// Restore only this instance. Refreshing records are terminal even if they hold a token.
    pub(crate) fn restore(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let session = Session::decode(bytes, &self.instance)?;
        let now = now_ms();
        let remaining = if now >= session.issued_at_ms {
            session
                .expires_at_ms
                .saturating_sub(now)
                .saturating_sub(5000)
        } else {
            0
        };
        self.access_deadline =
            Some(Instant::now() + Duration::from_millis(u64::try_from(remaining).unwrap_or(0)));
        self.verified = false;
        self.dirty = false;
        self.failure = None;
        self.next_allowed = None;
        self.unrepresentable_retry = None;
        self.session = Some(session);
        Ok(())
    }
    async fn persist(&mut self, sink: &mut impl SessionSink) -> Result<(), Error> {
        if !self.dirty {
            return Ok(());
        }
        let bytes = self
            .session
            .as_ref()
            .ok_or(Error::LoginRequired)?
            .encode()?;
        tokio::time::timeout(REQUEST_LIMIT, sink.save(&bytes))
            .await
            .map_err(|_| Error::Persistence)?
            .map_err(|_| Error::Persistence)?;
        self.dirty = false;
        Ok(())
    }
    /// Retry the exact in-memory replacement; this never performs a provider request.
    pub(crate) async fn retry_persistence(
        &mut self,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.persist(sink).await
    }
    async fn send(&mut self, request: Request) -> Result<Response, Error> {
        let limit = request.endpoint.body_limit();
        let response = tokio::time::timeout(REQUEST_LIMIT, self.transport.send(request))
            .await
            .map_err(|_| Error::Deadline)??;
        if response.body.len() > limit {
            return Err(Error::InvalidResponse);
        }
        if response.status == 429 {
            let seconds = parse_retry_after(response.retry_after.as_deref(), now_ms())
                .unwrap_or(self.backoff);
            self.set_retry(seconds);
        }
        Ok(response)
    }
    /// Password is moved into a single bootstrap body and dropped after that exchange.
    pub(crate) async fn login(
        &mut self,
        username: String,
        password: String,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.check_rate()?;
        if !bounded_text(&username, 1024) || password.is_empty() || password.len() > 4096 {
            return Err(Error::InvalidInput);
        }
        let mut generation = [0u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut generation)
            .map_err(|_| Error::InvalidResponse)?;
        self.session = Some(Session {
            version: 1,
            instance: self.instance.clone(),
            generation: generation
                .iter()
                .fold(String::with_capacity(32), |mut out, byte| {
                    let _ = write!(out, "{byte:02x}");
                    out
                }),
            phase: Phase::LoginRequired,
            scope: None,
            token_site_oid: None,
            access: None,
            refresh: None,
            issued_at_ms: now_ms(),
            expires_at_ms: 0,
            candidates: vec![],
        });
        self.verified = false;
        self.dirty = true;
        self.failure = None;
        self.access_deadline = None;
        self.next_allowed = None;
        self.unrepresentable_retry = None;
        let result = tokio::time::timeout(CYCLE_LIMIT, self.login_inner(username, password, sink))
            .await
            .unwrap_or(Err(Error::Deadline));
        self.failure = result.as_ref().err().copied();
        result
    }
    async fn login_inner(
        &mut self,
        username: String,
        password: String,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.persist(sink).await?;
        let body = form(&[
            ("grant_type", "password"),
            ("client_id", protocol::CLIENT_ID),
            ("client_secret", protocol::CLIENT_SECRET),
            ("username", &username),
            ("password", &password),
        ]);
        drop(username);
        drop(password);
        let accounts = self
            .send(Request {
                endpoint: Endpoint::Accounts,
                region: Region::Emea,
                query: Query::None,
                bearer: None,
                body,
            })
            .await?;
        auth_status(&accounts, self.backoff)?;
        let value: serde_json::Value =
            serde_json::from_slice(&accounts.body).map_err(|_| Error::InvalidResponse)?;
        let access = value
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|t| valid_token(t))
            .ok_or(Error::InvalidResponse)?;
        let body = form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("client_id", protocol::CLIENT_ID),
            ("subject_token", access),
            ("subject_issuer", "somfy-customer"),
            (
                "subject_token_type",
                "urn:ietf:params:oauth:token-type:access_token",
            ),
        ]);
        let exchange = self
            .send(Request {
                endpoint: Endpoint::Exchange,
                region: Region::Emea,
                query: Query::None,
                bearer: None,
                body,
            })
            .await?;
        auth_status(&exchange, self.backoff)?;
        let outcome = self.accept_token(&exchange.body, None);
        self.dirty = true;
        self.persist(sink).await?;
        outcome?;
        self.discover(sink).await
    }
    /// BOB requires account-wide auth; discovery after selection safely rotates unscoped first.
    pub(crate) async fn discover(&mut self, sink: &mut impl SessionSink) -> Result<(), Error> {
        self.check_rate()?;
        let result = tokio::time::timeout(CYCLE_LIMIT, self.discover_inner(sink))
            .await
            .unwrap_or(Err(Error::Deadline));
        self.record_read_result(&result);
        result
    }
    async fn discover_inner(&mut self, sink: &mut impl SessionSink) -> Result<(), Error> {
        self.persist(sink).await?;
        let account_wide = self
            .session
            .as_ref()
            .is_some_and(|s| s.token_site_oid.is_none());
        if !account_wide || self.expired() {
            self.rotate(None, sink).await?;
        }
        let mut candidates: Vec<SiteCandidate> = vec![];
        let mut seen_sites = std::collections::HashSet::new();
        let mut complete = false;
        let mut reported_total = None;
        for offset in (0..200).step_by(20) {
            let response = self
                .authorized(Endpoint::Directory, Query::Offset(offset), sink)
                .await?;
            auth_status(&response, self.backoff)?;
            let page: BobPage =
                serde_json::from_slice(&response.body).map_err(|_| Error::InvalidResponse)?;
            if page.results.len() > 20 || page.total_count.is_some_and(|t| t > 200) {
                return Err(Error::DiscoveryLimit);
            }
            if let Some(total) = page.total_count {
                if reported_total.is_some_and(|old| old != total) {
                    return Err(Error::InvalidResponse);
                }
                reported_total = Some(total);
            }
            let count = page.results.len();
            for site in page.results {
                if !bounded_text(&site.site_oid, 256) || !seen_sites.insert(site.site_oid.clone()) {
                    return Err(Error::InvalidResponse);
                }
                if site.sub_sites.len() > 200 {
                    return Err(Error::DiscoveryLimit);
                }
                let region = site.country.as_deref().and_then(region_for_country);
                for sub in site.sub_sites {
                    let candidate = SiteCandidate {
                        site_oid: sub.external_oid,
                        home_id: site.site_oid.clone(),
                        label: site.name.clone(),
                        country: site.country.clone(),
                        gateway_ids: sub.gateways.into_iter().map(|g| g.gateway_id).collect(),
                        region,
                    };
                    merge_candidate(&mut candidates, candidate)?;
                }
            }
            if let Some(total) = reported_total {
                if seen_sites.len() > total || (count < 20 && seen_sites.len() < total) {
                    return Err(Error::InvalidResponse);
                }
                if seen_sites.len() == total {
                    complete = true;
                    break;
                }
            } else if count < 20 {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err(Error::DiscoveryLimit);
        }
        super::types::validate_candidates(&candidates)?;
        let session = self.session.as_mut().ok_or(Error::LoginRequired)?;
        let old_scope = session.scope.clone();
        if session.scope.as_ref().is_some_and(|scope| {
            !candidates
                .iter()
                .any(|c| c.site_oid == scope.site_oid && c.gateway_ids.contains(&scope.gateway_id))
        }) {
            session.scope = None;
            self.verified = false;
        }
        self.dirty |= session.candidates != candidates || session.scope != old_scope;
        session.candidates = candidates;
        self.persist(sink).await
    }
    pub(crate) async fn select_site(
        &mut self,
        generation: &str,
        site_oid: &str,
        gateway_id: &str,
        region: Option<Region>,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.check_rate()?;
        let result = tokio::time::timeout(
            CYCLE_LIMIT,
            self.select_inner(generation, site_oid, gateway_id, region, sink),
        )
        .await
        .unwrap_or(Err(Error::Deadline));
        self.failure = result.as_ref().err().copied();
        result
    }
    async fn select_inner(
        &mut self,
        generation: &str,
        site_oid: &str,
        gateway_id: &str,
        region: Option<Region>,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.persist(sink).await?;
        let session = self.session.as_ref().ok_or(Error::LoginRequired)?;
        if matches!(session.phase, Phase::Refreshing | Phase::LoginRequired) {
            return Err(Error::LoginRequired);
        }
        if session.generation != generation {
            return Err(Error::ScopeMismatch);
        }
        let candidate = session
            .candidates
            .iter()
            .find(|c| c.site_oid == site_oid && c.gateway_ids.iter().any(|g| g == gateway_id))
            .ok_or(Error::ScopeMismatch)?;
        let selected = match (candidate.region, region) {
            (Some(known), Some(requested)) if known != requested => {
                return Err(Error::ScopeMismatch);
            }
            (Some(known), _) => known,
            (None, Some(explicit)) => explicit,
            (None, None) => return Err(Error::ScopeMismatch),
        };
        self.session.as_mut().ok_or(Error::LoginRequired)?.scope = Some(Scope {
            site_oid: site_oid.into(),
            gateway_id: gateway_id.into(),
            region: selected,
        });
        self.verified = false;
        self.rotate(self.authentication_scope(Endpoint::Gateways)?, sink)
            .await?;
        self.verify_scope(sink).await
    }
    fn expired(&self) -> bool {
        self.access_deadline.is_none_or(|d| Instant::now() >= d)
            || self
                .session
                .as_ref()
                .is_none_or(|s| s.phase != Phase::Ready)
    }
    #[cfg(test)]
    pub(crate) async fn refresh(&mut self, sink: &mut impl SessionSink) -> Result<(), Error> {
        self.check_rate()?;
        let site = self
            .session
            .as_ref()
            .ok_or(Error::LoginRequired)?
            .oauth_site_oid()?
            .map(str::to_owned);
        let result = self.rotate(site, sink).await;
        self.failure = result.as_ref().err().copied();
        result
    }
    async fn rotate(
        &mut self,
        site: Option<String>,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.persist(sink).await?;
        let session = self.session.as_mut().ok_or(Error::LoginRequired)?;
        if matches!(session.phase, Phase::Refreshing | Phase::LoginRequired) {
            return Err(Error::LoginRequired);
        }
        let refresh = session.refresh.clone().ok_or(Error::LoginRequired)?;
        let previous = session.clone();
        let previous_deadline = self.access_deadline;
        session.phase = Phase::Refreshing;
        session.token_site_oid.clone_from(&site);
        session.access = None;
        self.access_deadline = None;
        self.dirty = true;
        // Persist BEFORE dispatch; a lost acknowledgement leaves a terminal checkpoint.
        self.persist(sink).await?;
        let body = form(&[
            ("grant_type", "refresh_token"),
            ("client_id", protocol::CLIENT_ID),
            ("refresh_token", &refresh),
        ]);
        let query = site.clone().map_or(Query::None, Query::Site);
        let response = self
            .send(Request {
                endpoint: Endpoint::Refresh,
                region: Region::Emea,
                query,
                bearer: None,
                body,
            })
            .await;
        let response = match response {
            Ok(r) if r.status == 200 => r,
            Err(Error::NotDispatched) => {
                // Only a failed TCP/DNS stage proves that the rotating token was
                // never sent. Restore the exact previous session durably first.
                self.session = Some(previous);
                self.access_deadline = previous_deadline;
                self.dirty = true;
                self.persist(sink).await?;
                return Err(Error::NotDispatched);
            }
            failed => {
                let failure = match failed {
                    Ok(response) if response.status == 429 => Error::RateLimited {
                        retry_after_secs: parse_retry_after(
                            response.retry_after.as_deref(),
                            now_ms(),
                        )
                        .unwrap_or(self.backoff),
                    },
                    _ => Error::LoginRequired,
                };
                self.invalidate();
                let _ = self.persist(sink).await;
                // Retire the ambiguous token, but preserve quota even when its delay is zero.
                return Err(failure);
            }
        };
        let outcome = self.accept_token(&response.body, site);
        self.dirty = true;
        self.persist(sink).await?;
        outcome
    }
    fn invalidate(&mut self) {
        if let Some(s) = self.session.as_mut() {
            s.phase = Phase::LoginRequired;
            s.access = None;
            s.refresh = None;
        }
        self.access_deadline = None;
        self.verified = false;
        self.dirty = true;
    }
    /// Capture a replacement refresh before validating optional expiry/access fields.
    fn accept_token(&mut self, body: &[u8], site: Option<String>) -> Result<(), Error> {
        let parsed: Result<serde_json::Value, _> = serde_json::from_slice(body);
        let Ok(value) = parsed else {
            self.invalidate();
            return Err(Error::InvalidResponse);
        };
        let new_refresh = value
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .filter(|t| valid_token(t))
            .map(str::to_owned);
        let refresh_present = value.get("refresh_token").is_some();
        let access = value
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|t| valid_token(t))
            .map(str::to_owned);
        let expiry = value
            .get("expires_in")
            .map_or(Some(300), serde_json::Value::as_u64)
            .filter(|v| (6..=86400).contains(v));
        let bearer = value
            .get("token_type")
            .is_none_or(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("bearer")));
        let session = self.session.as_mut().ok_or(Error::LoginRequired)?;
        if let Some(replacement) = &new_refresh {
            session.refresh = Some(replacement.clone());
        }
        let valid = access.is_some()
            && expiry.is_some()
            && bearer
            && (!refresh_present || new_refresh.is_some())
            && session.refresh.is_some();
        session.token_site_oid = site;
        session.issued_at_ms = now_ms();
        if valid {
            let lifetime = expiry.ok_or(Error::InvalidResponse)?;
            session.access = access;
            session.phase = Phase::Ready;
            session.expires_at_ms = session
                .issued_at_ms
                .saturating_add(i64::try_from(lifetime * 1000).unwrap_or(0));
            self.access_deadline = Some(Instant::now() + Duration::from_secs(lifetime - 5));
            Ok(())
        } else {
            session.access = None;
            session.expires_at_ms = 0;
            session.phase = if new_refresh.is_some() {
                Phase::Incomplete
            } else {
                session.refresh = None;
                Phase::LoginRequired
            };
            self.access_deadline = None;
            Err(Error::InvalidResponse)
        }
    }
    /// Every regional read uses the selected setup's parent home, never externalOID.
    fn authentication_scope(&self, endpoint: Endpoint) -> Result<Option<String>, Error> {
        match endpoint {
            Endpoint::Directory => Ok(None),
            Endpoint::Collection(_)
            | Endpoint::Location
            | Endpoint::Devices
            | Endpoint::Gateways
            | Endpoint::Scenario => self
                .session
                .as_ref()
                .ok_or(Error::LoginRequired)?
                .oauth_site_oid()?
                .map(|site| Some(site.to_owned()))
                .ok_or(Error::ScopeMismatch),
            _ => Err(Error::InvalidInput),
        }
    }
    async fn authorized(
        &mut self,
        endpoint: Endpoint,
        query: Query,
        sink: &mut impl SessionSink,
    ) -> Result<Response, Error> {
        if endpoint == Endpoint::Scenario {
            return Err(Error::InvalidInput);
        }
        self.check_rate()?;
        self.persist(sink).await?;
        let site = self.authentication_scope(endpoint)?;
        if self.expired()
            || self
                .session
                .as_ref()
                .is_some_and(|s| s.token_site_oid != site)
        {
            self.rotate(site.clone(), sink).await?;
        }
        // Query values are closed. Rebuild only the bounded directory offset on one 401 retry.
        let retry_query = match &query {
            Query::Offset(n) => Query::Offset(*n),
            _ => Query::None,
        };
        let response = self.authenticated_send(endpoint, query).await?;
        if response.status != 401 {
            return Ok(response);
        }
        self.rotate(site, sink).await?;
        self.authenticated_send(endpoint, retry_query).await
    }
    async fn authenticated_send(
        &mut self,
        endpoint: Endpoint,
        query: Query,
    ) -> Result<Response, Error> {
        let session = self.session.as_ref().ok_or(Error::LoginRequired)?;
        if self.dirty || session.phase != Phase::Ready {
            return Err(Error::LoginRequired);
        }
        let bearer = session.access.clone().ok_or(Error::LoginRequired)?;
        let region = session.scope.as_ref().map_or(Region::Emea, |s| s.region);
        self.send(Request {
            endpoint,
            query,
            region,
            bearer: Some(bearer),
            body: String::new(),
        })
        .await
    }
    async fn verify_scope(&mut self, sink: &mut impl SessionSink) -> Result<(), Error> {
        self.verified = false;
        let scope = self
            .session
            .as_ref()
            .and_then(|s| s.scope.clone())
            .ok_or(Error::ScopeMismatch)?;
        let response = self
            .authorized(Endpoint::Gateways, Query::None, sink)
            .await?;
        auth_status(&response, self.backoff)?;
        let gateways: Vec<BobGateway> =
            serde_json::from_slice(&response.body).map_err(|_| Error::InvalidResponse)?;
        if gateways.is_empty()
            || gateways.len() > 200
            || gateways.iter().any(|g| !bounded_text(&g.gateway_id, 256))
        {
            return Err(Error::InvalidResponse);
        }
        if !gateways.iter().any(|g| g.gateway_id == scope.gateway_id) {
            return Err(Error::ScopeMismatch);
        }
        self.verified = true;
        Ok(())
    }
    pub(crate) async fn read_cycle(
        &mut self,
        catalog: &mut Catalog,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.check_rate()?;
        let result = tokio::time::timeout(CYCLE_LIMIT, self.cycle_inner(catalog, sink))
            .await
            .unwrap_or(Err(Error::Deadline));
        if result.is_err() {
            self.verified = false;
        }
        self.record_read_result(&result);
        result
    }
    fn record_read_result(&mut self, result: &Result<(), Error>) {
        self.failure = result.as_ref().err().copied();
        if result.is_err() {
            if !matches!(result, Err(Error::RateLimited { .. })) && self.retry_remaining().is_none()
            {
                self.set_retry(self.backoff);
            }
            self.backoff = (self.backoff * 2).min(super::MAX_RETRY_SECS);
        } else {
            self.backoff = 60;
            self.next_allowed = None;
            self.unrepresentable_retry = None;
        }
    }
    async fn cycle_inner(
        &mut self,
        catalog: &mut Catalog,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        self.persist(sink).await?;
        let session_identity = self
            .session
            .as_ref()
            .ok_or(Error::LoginRequired)?
            .identity()?;
        if catalog.identity() != session_identity {
            return Err(Error::ScopeMismatch);
        }
        self.verify_scope(sink).await?;
        let identity = self.identity()?;
        let mut failed = false;
        for kind in CollectionKind::ALL {
            let attempt = now_ms().max(catalog.collections[&kind].last_attempt_ms.unwrap_or(0));
            let _ = catalog.apply_collection(
                &identity,
                kind,
                attempt,
                Err(CollectionFailure::Unavailable),
            );
            let response = self
                .authorized(Endpoint::Collection(kind), Query::None, sink)
                .await;
            let mut rate_seconds = None;
            let outcome = match response {
                Ok(r) if r.status == 200 => {
                    inventory::parse_collection(kind, &r.body).map_err(CollectionFailure::from)
                }
                Ok(r) if r.status == 429 => {
                    let seconds = parse_retry_after(r.retry_after.as_deref(), now_ms())
                        .unwrap_or(self.backoff);
                    rate_seconds = Some(seconds);
                    Err(CollectionFailure::RateLimited {
                        retry_after_secs: Some(seconds),
                    })
                }
                Ok(r) => Err(CollectionFailure::from_http_status(r.status)),
                Err(Error::RateLimited { retry_after_secs }) => {
                    rate_seconds = Some(retry_after_secs);
                    Err(CollectionFailure::RateLimited {
                        retry_after_secs: Some(retry_after_secs),
                    })
                }
                Err(Error::LoginRequired) => Err(CollectionFailure::Unauthorized),
                Err(Error::InvalidResponse) => Err(CollectionFailure::from(
                    inventory::InventoryError::ByteLimit,
                )),
                Err(_) => Err(CollectionFailure::Unavailable),
            };
            failed |= outcome.is_err();
            // Invalid Ok(data) can update InvalidResponse and return Err; caller still persists catalog.
            if catalog
                .apply_collection(&identity, kind, attempt, outcome)
                .is_err()
            {
                failed = true;
            }
            if let Some(seconds) = rate_seconds.or_else(|| self.retry_remaining()) {
                return Err(Error::RateLimited {
                    retry_after_secs: seconds,
                });
            }
        }
        if self.retry_remaining().is_none() {
            match self.update_metadata(catalog, &identity, sink).await {
                Ok(()) => {}
                Err(error @ Error::RateLimited { .. }) => return Err(error),
                Err(_) => failed = true,
            }
        }
        if failed {
            self.check_rate()?;
            Err(Error::Network)
        } else {
            Ok(())
        }
    }
    async fn update_metadata(
        &mut self,
        catalog: &mut Catalog,
        identity: &CatalogIdentity,
        sink: &mut impl SessionSink,
    ) -> Result<(), Error> {
        let location = self
            .authorized(Endpoint::Location, Query::None, sink)
            .await?;
        auth_status(&location, self.backoff)?;
        let devices = self
            .authorized(Endpoint::Devices, Query::None, sink)
            .await?;
        auth_status(&devices, self.backoff)?;
        let timezone = inventory::parse_location_timezone(&location.body)
            .map_err(|_| Error::InvalidResponse)?;
        let labels =
            inventory::parse_target_labels(&devices.body).map_err(|_| Error::InvalidResponse)?;
        // Commit the pair together, after both bounded metadata parsers succeed.
        catalog
            .apply_metadata(identity, timezone, labels)
            .map_err(|_| Error::InvalidResponse)
    }
    fn set_retry(&mut self, seconds: u64) {
        let seconds = seconds.min(super::MAX_RETRY_SECS);
        self.next_allowed = Instant::now().checked_add(Duration::from_secs(seconds));
        self.unrepresentable_retry = self.next_allowed.is_none().then_some(seconds);
    }
    fn retry_remaining(&self) -> Option<u64> {
        self.unrepresentable_retry.or_else(|| {
            self.next_allowed
                .filter(|t| *t > Instant::now())
                .map(remaining_secs)
        })
    }
    fn check_rate(&self) -> Result<(), Error> {
        if let Some(seconds) = self.retry_remaining() {
            return Err(Error::RateLimited {
                retry_after_secs: seconds,
            });
        }
        Ok(())
    }
}
fn merge_candidate(
    candidates: &mut Vec<SiteCandidate>,
    candidate: SiteCandidate,
) -> Result<(), Error> {
    candidate.validate()?;
    if let Some(old) = candidates
        .iter_mut()
        .find(|c| c.site_oid == candidate.site_oid)
    {
        if old.home_id != candidate.home_id
            || old.label != candidate.label
            || old.country != candidate.country
            || old.region != candidate.region
        {
            return Err(Error::InvalidResponse);
        }
        for gateway in candidate.gateway_ids {
            if !old.gateway_ids.contains(&gateway) {
                old.gateway_ids.push(gateway);
            }
        }
    } else {
        candidates.push(candidate);
    }
    if candidates.len() > 200
        || candidates
            .iter()
            .map(|c| c.gateway_ids.len())
            .sum::<usize>()
            > 200
    {
        return Err(Error::DiscoveryLimit);
    }
    Ok(())
}
fn remaining_secs(deadline: Instant) -> u64 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() != 0))
}
fn auth_status(response: &Response, fallback: u64) -> Result<(), Error> {
    match response.status {
        200 => Ok(()),
        400 | 401 => Err(Error::LoginRequired),
        403 => Err(Error::Forbidden),
        429 => Err(Error::RateLimited {
            retry_after_secs: parse_retry_after(response.retry_after.as_deref(), now_ms())
                .unwrap_or(fallback),
        }),
        300..=399 => Err(Error::InvalidResponse),
        _ => Err(Error::Network),
    }
}
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}
#[cfg(test)]
pub(super) fn retry_after(value: Option<&str>, now: i64) -> u64 {
    parse_retry_after(value, now).unwrap_or(60)
}
fn parse_retry_after(value: Option<&str>, now: i64) -> Option<u64> {
    let value = value.filter(|v| v.len() <= 128)?;
    if value.bytes().all(|b| b.is_ascii_digit()) && !value.is_empty() {
        // Bound hostile or overflowing provider delays as well as ordinary values.
        return Some(
            value
                .parse::<u64>()
                .unwrap_or(u64::MAX)
                .min(super::MAX_RETRY_SECS),
        );
    }
    let date = value
        .strip_suffix(" GMT")
        .and_then(|date| {
            ["%a, %d %b %Y %H:%M:%S %z", "%A, %d-%b-%y %H:%M:%S %z"]
                .iter()
                .find_map(|format| jiff::Timestamp::strptime(format, format!("{date} +0000")).ok())
        })
        .or_else(|| {
            jiff::Timestamp::strptime("%a %b %e %H:%M:%S %Y %z", format!("{value} +0000")).ok()
        })?;
    u64::try_from(
        date.as_millisecond()
            .saturating_sub(now)
            .max(0)
            .saturating_add(999)
            / 1000,
    )
    .ok()
    .map(|seconds| seconds.min(super::MAX_RETRY_SECS))
}

#[derive(Deserialize)]
struct BobPage {
    results: Vec<BobSite>,
    #[serde(rename = "totalCount")]
    total_count: Option<usize>,
}
#[derive(Deserialize)]
struct BobSite {
    #[serde(rename = "siteOID")]
    site_oid: String,
    name: Option<String>,
    country: Option<String>,
    #[serde(rename = "subSites")]
    sub_sites: Vec<BobSubSite>,
}
#[derive(Deserialize)]
struct BobSubSite {
    #[serde(rename = "externalOID")]
    external_oid: String,
    gateways: Vec<BobGateway>,
}
#[derive(Deserialize)]
struct BobGateway {
    #[serde(rename = "gatewayId")]
    gateway_id: String,
}

/// Exact pyoverkiz country table; unlike its default, an absent country stays unknown.
fn region_for_country(country: &str) -> Option<Region> {
    let country = country.to_ascii_uppercase();
    match country.as_str() {
        "CA" | "US" | "MX" => Some(Region::Snaba),
        "AU" | "HK" | "IN" | "ID" | "JP" | "MY" | "NZ" | "PH" | "SG" | "TW" | "TH" | "VN"
        | "KR" | "CN" => Some(Region::Apac),
        "AL" | "AD" | "AT" | "BY" | "BE" | "BG" | "HR" | "CY" | "CZ" | "DK" | "EG" | "EE"
        | "FO" | "FI" | "FR" | "GF" | "PF" | "DE" | "GR" | "GP" | "HU" | "IL" | "IT" | "JE"
        | "JO" | "KZ" | "KW" | "LV" | "LB" | "LT" | "LU" | "MQ" | "YT" | "MC" | "MA" | "NL"
        | "NO" | "NC" | "PS" | "PL" | "PT" | "QA" | "IE" | "RE" | "RO" | "RU" | "BL" | "SA"
        | "RS" | "SK" | "ZA" | "ES" | "SE" | "CH" | "TN" | "TR" | "UA" | "AE" | "GB" | "AM"
        | "AO" | "AZ" | "BA" | "BH" | "CI" | "CM" | "DZ" | "ET" | "GE" | "GG" | "GH" | "GI"
        | "IM" | "IQ" | "IS" | "KE" | "KG" | "LI" | "LY" | "MD" | "ME" | "MG" | "MK" | "MT"
        | "MU" | "NA" | "NG" | "OM" | "SI" | "SM" | "SN" | "TZ" | "UG" | "UZ" | "VA" | "ZM"
        | "ZW" => Some(Region::Emea),
        _ => None,
    }
}

#[path = "client_execution.rs"]
mod scenario_execution;

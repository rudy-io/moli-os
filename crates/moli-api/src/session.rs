//! Human sessions: proof that a request comes from a person at the dashboard.
//!
//! The guard trusts `Origin::Ui` to release held orders. Declaring it is not
//! enough (any LAN client can send a header): it takes a session cookie,
//! obtained by typing a PIN that lives in a secrets manager, or that an
//! owner of the house chose from the dashboard (kept in Moli's encrypted
//! store, it wins: [`Sessions::set_pin`]), never on any surface.
//!
//! - cookies are `HttpOnly` + `SameSite=Strict`, bound to the client that
//!   opened them ([`crate::caller::Caller::key`]: its address on the home
//!   network, its e-mail through Cloudflare Access), and expire on the server
//!   after 30 days;
//! - only their SHA-256 is kept, in the encrypted secret store, with a
//!   fingerprint of the PIN: changing the PIN ends every session;
//! - failures are counted per client, with a lockout that doubles
//!   (15 min, 30 min… up to a day): an agent can neither brute-force the PIN
//!   nor lock the human out from another machine.
//!
//! A new house has no code yet. As long as none is chosen (and the store can
//! keep one), Moli writes a one-time **installation code** in its logs:
//! whoever can read the machine's logs owns the house, types it in the
//! dashboard and chooses the house's code ([`Sessions::claim`]). It lives in
//! memory only, changes at every start, and is worth nothing once used.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use moli_core::now_ms;
use moli_runtime::Hub;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

const COOKIE_NAME: &str = "moli_session";
const MAX_SESSIONS: usize = 20;
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(15 * 60);
const MAX_LOCKOUT: Duration = Duration::from_secs(24 * 3600);
/// Wrong codes from everyone together, per hour: past it, codes are not
/// even checked for an hour, and the house is told (whoever is guessing
/// from many places at once).
const MAX_GLOBAL_FAILURES: u32 = 20;
const GLOBAL_WINDOW: Duration = Duration::from_secs(3600);
/// Failure counters kept at most (a stale one is forgotten first).
const MAX_LIMITERS: usize = 512;
const STORE_KEY: &str = "ui_sessions";
/// The code an owner chose from the dashboard (encrypted store).
const PIN_KEY: &str = "ui_pin";
pub(crate) const COOKIE_MAX_AGE_S: u64 = 30 * 24 * 3600;
/// A PIN shorter than this is refused: human sessions stay disabled. Four
/// digits (the owner's choice): only the household reaches the PIN sheet
/// (home network or Cloudflare Access), and the lockouts below allow at most
/// 20 guesses an hour for the whole house: weeks for 10 000 combinations.
pub const MIN_PIN_LEN: usize = 4;
/// The installation code's letters: none to misread from a log (no 0/O,
/// 1/I/L). Eight of them: about 40 bits, behind the same lockouts.
const SETUP_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const SETUP_LEN: usize = 8;
/// How long an installation code is good for: a fresh one comes with the
/// next start.
const SETUP_TTL: Duration = Duration::from_secs(3600);
/// Without an owner proved by Cloudflare Access, the code is the only
/// lock on the house: six digits at least.
const MIN_PIN_LEN_ALONE: usize = 6;

/// The dashboard PIN. Never printed.
#[derive(Clone)]
pub struct UiPin(String);

impl UiPin {
    #[must_use]
    pub fn new(pin: &str) -> Self {
        Self(pin.trim().to_owned())
    }

    /// Whether it is long enough to resist guessing behind the lockout.
    #[must_use]
    pub fn is_strong(&self) -> bool {
        self.0.chars().count() >= MIN_PIN_LEN
    }
}

impl fmt::Debug for UiPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UiPin(<redacted>)")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LoginError {
    /// No (usable) PIN configured: human sessions are impossible.
    Disabled,
    Wrong,
    Locked,
}

/// Why the installation did not go through.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClaimError {
    /// The house already has its code (or no store to keep one).
    NotNeeded,
    Wrong,
    Locked,
    /// The installation code is too old: a restart gives a fresh one.
    Expired,
    /// The house's new code is refused (too short, not digits, not kept).
    Rejected(String),
}

#[derive(Debug, Default)]
struct Limiter {
    failures: u32,
    lockouts: u32,
    locked_until: Option<Instant>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Session {
    created: u64,
    /// The caller's key (named `ip` when only addresses existed).
    ip: String,
}

/// What is persisted (encrypted): token hashes, and which PIN they belong to.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    pin: String,
    sessions: HashMap<String, Session>,
}

pub(crate) struct Sessions {
    hub: Hub,
    pin: Mutex<Option<UiPin>>,
    /// Who may choose the code from the dashboard (Access e-mails).
    owners: Vec<String>,
    active: Mutex<HashMap<String, Session>>,
    limiters: Mutex<HashMap<String, Limiter>>,
    /// Since when, how many wrong codes in all, and until when codes are off.
    global: Mutex<(Instant, u32, Option<Instant>)>,
    /// The installation code's digest and birth, while the house has no code.
    setup: Mutex<Option<(String, Instant)>>,
    write: tokio::sync::Mutex<()>,
}

impl fmt::Debug for Sessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sessions")
            .field("pin_configured", &self.pin_configured())
            .finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

fn hash(s: &str) -> String {
    hex(digest(&SHA256, s.as_bytes()).as_ref())
}

fn pin_fingerprint(pin: &UiPin) -> String {
    hash(&format!("moli-os ui pin\0{}", pin.0))
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The installation code of a house that never had a code: none chosen
/// here, none given by the environment, no session ever kept. A house that
/// lost its code never reopens to whoever reads the logs: it says so.
fn installation(hub: &Hub, no_code: bool) -> Option<(String, Instant)> {
    if !no_code {
        return None;
    }
    if !hub.has_secret_store() {
        tracing::warn!("no dashboard code and no secret store: guarded orders can only wait");
        return None;
    }
    if hub.core_secret(STORE_KEY).is_some() {
        tracing::error!(
            "this house had a dashboard code and lost it: human sessions disabled (to install it again: moli-os secrets forget core ui_sessions, then restart)"
        );
        return None;
    }
    let code = setup_code()?;
    tracing::warn!(
        code = %code,
        minutes = SETUP_TTL.as_secs() / 60,
        "{}",
        moli_i18n::tr!("serveur.code.journal_installation")
    );
    Some((hash(&normalize_code(&code)), Instant::now()))
}

/// A fresh installation code, `ABCD-EFGH`.
fn setup_code() -> Option<String> {
    let mut raw = [0u8; SETUP_LEN];
    SystemRandom::new().fill(&mut raw).ok()?;
    // 256 is not a multiple of 31: a slight bias, irrelevant behind lockouts.
    let letters: String = raw
        .iter()
        .map(|b| char::from(SETUP_ALPHABET[usize::from(*b) % SETUP_ALPHABET.len()]))
        .collect();
    Some(format!("{}-{}", &letters[..4], &letters[4..]))
}

/// What the person typed, as the code is kept: letters and digits only,
/// upper case (`abcd efgh` is `ABCD-EFGH`).
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A code the house can use: long enough, digits only (the dashboard's pad).
fn valid_pin(new: &str, min: usize) -> Result<UiPin, String> {
    let pin = UiPin::new(new);
    if pin.0.chars().count() < min {
        return Err(moli_i18n::tr!("serveur.code.chiffres_au_moins", min = min));
    }
    if !pin.0.chars().all(|c| c.is_ascii_digit()) {
        return Err(moli_i18n::tr!("serveur.code.chiffres_seulement"));
    }
    Ok(pin)
}

impl Sessions {
    pub(crate) fn new(hub: Hub, pin: Option<UiPin>, owners: &[String]) -> Self {
        // Given by the environment, even empty or too short: this house is
        // set up (its vault is wrong), it never asks for an installation.
        let from_vault = pin.is_some();
        // The code chosen here wins over the vault's, when it is one.
        let chosen = hub
            .core_secret(PIN_KEY)
            .map(|p| UiPin::new(&p))
            .filter(UiPin::is_strong);
        let pin = chosen.or(pin).filter(|p| {
            let strong = p.is_strong();
            if !strong {
                tracing::warn!(
                    "dashboard PIN shorter than {MIN_PIN_LEN} characters: human sessions disabled"
                );
            }
            strong
        });
        // Sessions opened with another PIN do not survive a PIN change.
        let sessions = match (&pin, hub.core_secret(STORE_KEY)) {
            (Some(pin), Some(json)) => serde_json::from_str::<Persisted>(&json)
                .ok()
                .filter(|p| p.pin == pin_fingerprint(pin))
                .map(|p| p.sessions)
                .unwrap_or_default(),
            _ => HashMap::new(),
        };
        let setup = installation(&hub, pin.is_none() && !from_vault);
        Self {
            hub,
            pin: Mutex::new(pin),
            owners: owners.iter().map(|o| o.trim().to_lowercase()).collect(),
            active: Mutex::new(sessions),
            limiters: Mutex::new(HashMap::new()),
            global: Mutex::new((Instant::now(), 0, None)),
            setup: Mutex::new(setup),
            write: tokio::sync::Mutex::new(()),
        }
    }

    /// Whether some owner is proved by Cloudflare Access: then they alone
    /// choose the code, as they always did.
    pub(crate) fn has_owners(&self) -> bool {
        !self.owners.is_empty()
    }

    /// The shortest code this house accepts.
    pub(crate) fn min_pin_len(&self) -> usize {
        if self.has_owners() {
            MIN_PIN_LEN
        } else {
            MIN_PIN_LEN_ALONE
        }
    }

    /// Whether the house waits for its installation (no code chosen yet).
    pub(crate) fn setup_pending(&self) -> bool {
        lock(&self.setup)
            .as_ref()
            .is_some_and(|(_, born)| born.elapsed() < SETUP_TTL)
    }

    pub(crate) fn pin_configured(&self) -> bool {
        lock(&self.pin).is_some()
    }

    /// Whether this e-mail (proved by Access) may choose the code.
    pub(crate) fn is_owner(&self, email: &str) -> bool {
        let email = email.trim().to_lowercase();
        self.owners.contains(&email)
    }

    /// A new code, chosen by an owner (or by whoever knows the current one):
    /// kept encrypted, it replaces the vault's; every session opened with
    /// the old one ends.
    pub(crate) async fn set_pin(&self, new: &str, by: &str) -> Result<(), String> {
        self.keep_pin(new, by).await?;
        self.tell(moli_i18n::tr!("serveur.code.notice_change", by = by));
        Ok(())
    }

    /// The new code kept, every old session ended (no notice: the caller
    /// tells the house what happened).
    async fn keep_pin(&self, new: &str, by: &str) -> Result<(), String> {
        let pin = valid_pin(new, self.min_pin_len())?;
        self.hub
            .store_core_secret(PIN_KEY, &pin.0)
            .await
            .map_err(|e| moli_i18n::tr!("serveur.code.non_enregistre", error = e))?;
        *lock(&self.pin) = Some(pin);
        *lock(&self.setup) = None;
        self.update(HashMap::clear).await;
        tracing::info!(by, "dashboard PIN changed: every session ended");
        Ok(())
    }

    /// The installation: the code from the logs, then the house's own code,
    /// chosen once. Returns the new session's token.
    pub(crate) async fn claim(
        &self,
        code: &str,
        pin: &str,
        key: &str,
    ) -> Result<String, ClaimError> {
        let Some((expected, born)) = lock(&self.setup).clone() else {
            return Err(ClaimError::NotNeeded);
        };
        if born.elapsed() >= SETUP_TTL {
            return Err(ClaimError::Expired);
        }
        // Checked before the code: a refused choice costs no attempt.
        valid_pin(pin, self.min_pin_len()).map_err(ClaimError::Rejected)?;
        let typed = hash(&normalize_code(code));
        self.attempt(key, || typed == expected)
            .map_err(|e| match e {
                LoginError::Locked => ClaimError::Locked,
                LoginError::Wrong | LoginError::Disabled => ClaimError::Wrong,
            })?;
        // One use only, even for two people typing at the same time.
        let Some(taken) = lock(&self.setup).take() else {
            return Err(ClaimError::NotNeeded);
        };
        if let Err(e) = self.keep_pin(pin, key).await {
            *lock(&self.setup) = Some(taken);
            return Err(ClaimError::Rejected(e));
        }
        tracing::info!(client = key, "installation done: the house has its code");
        self.tell(moli_i18n::tr!("serveur.code.notice_installee", key = key));
        self.open(key).await.map_err(|_| ClaimError::NotNeeded)
    }

    pub(crate) fn locked(&self, key: &str) -> bool {
        lock(&self.limiters)
            .get(key)
            .and_then(|l| l.locked_until)
            .is_some_and(|t| Instant::now() < t)
    }

    /// Whether the request carries a valid, unexpired human session opened
    /// from this very client.
    pub(crate) fn is_human(&self, headers: &HeaderMap, key: &str) -> bool {
        let Some(token) = cookie_token(headers).filter(|_| self.pin_configured()) else {
            return false;
        };
        let max_age_ms = COOKIE_MAX_AGE_S * 1000;
        lock(&self.active)
            .get(&hash(token))
            .is_some_and(|s| s.ip == key && now_ms().saturating_sub(s.created) < max_age_ms)
    }

    /// Checks the PIN; on success returns a new session token bound to `key`.
    pub(crate) async fn login(&self, attempt: &str, key: &str) -> Result<String, LoginError> {
        self.verify(attempt, key)?;
        self.open(key).await
    }

    /// Checks the PIN without opening a session (changing the code).
    pub(crate) fn verify(&self, attempt: &str, key: &str) -> Result<(), LoginError> {
        let pin = lock(&self.pin).clone().ok_or(LoginError::Disabled)?;
        // Compare digests, not the PINs: any timing difference reveals
        // something about a hash, nothing about the PIN.
        self.attempt(key, || hash(&pin.0) == hash(attempt.trim()))
    }

    /// One attempt at a code from `key`, behind the per-client and global
    /// lockouts: `matches` is only asked when nobody is locked out.
    pub(crate) fn attempt(
        &self,
        key: &str,
        matches: impl FnOnce() -> bool,
    ) -> Result<(), LoginError> {
        if lock(&self.global)
            .2
            .is_some_and(|until| Instant::now() < until)
        {
            return Err(LoginError::Locked);
        }
        let mut limiters = lock(&self.limiters);
        if limiters.len() >= MAX_LIMITERS {
            let now = Instant::now();
            limiters.retain(|_, l| l.locked_until.is_some_and(|t| now < t));
        }
        let limiter = limiters.entry(key.to_owned()).or_default();
        if limiter.locked_until.is_some_and(|t| Instant::now() < t) {
            return Err(LoginError::Locked);
        }
        if !matches() {
            self.count_global_failure();
            limiter.failures += 1;
            if limiter.failures >= MAX_FAILURES {
                let lockout = LOCKOUT
                    .saturating_mul(1 << limiter.lockouts.min(7))
                    .min(MAX_LOCKOUT);
                limiter.failures = 0;
                limiter.lockouts += 1;
                limiter.locked_until = Some(Instant::now() + lockout);
                tracing::warn!(
                    client = key,
                    minutes = lockout.as_secs() / 60,
                    "dashboard code: too many failures"
                );
            }
            return Err(LoginError::Wrong);
        }
        limiters.remove(key);
        Ok(())
    }

    /// A new session for `key` (the code was just proved).
    async fn open(&self, key: &str) -> Result<String, LoginError> {
        let mut raw = [0u8; 32];
        SystemRandom::new()
            .fill(&mut raw)
            .map_err(|_| LoginError::Disabled)?;
        let token = hex(&raw);
        let session = Session {
            created: now_ms(),
            ip: key.to_owned(),
        };
        self.update(|sessions| {
            let max_age_ms = COOKIE_MAX_AGE_S * 1000;
            sessions.retain(|_, s| now_ms().saturating_sub(s.created) < max_age_ms);
            while sessions.len() >= MAX_SESSIONS {
                let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, s)| s.created)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                sessions.remove(&oldest);
            }
            sessions.insert(hash(&token), session);
        })
        .await;
        Ok(token)
    }

    /// Tells the house (its notices: dashboard, Telegram…).
    fn tell(&self, message: String) {
        self.hub.notice(moli_core::Notice {
            title: Some(moli_i18n::tr!("serveur.code.notice_titre")),
            message,
            from: Some("Moli".into()),
            ts: moli_core::now_ms(),
        });
    }

    /// One more wrong code, from anyone: past the hourly budget, codes are
    /// off for an hour and the house hears about it.
    fn count_global_failure(&self) {
        let now = Instant::now();
        let mut global = lock(&self.global);
        if now.duration_since(global.0) >= GLOBAL_WINDOW {
            *global = (now, 0, global.2);
        }
        global.1 += 1;
        if global.1 == MAX_GLOBAL_FAILURES {
            global.2 = Some(now + GLOBAL_WINDOW);
            drop(global);
            tracing::warn!("too many wrong dashboard codes: codes off for an hour");
            self.hub.notice(moli_core::Notice {
                title: Some(moli_i18n::tr!("serveur.code.notice_titre")),
                message: moli_i18n::tr!("serveur.code.notice_suspendu", max = MAX_GLOBAL_FAILURES),
                from: Some("Moli".into()),
                ts: moli_core::now_ms(),
            });
        }
    }

    pub(crate) async fn logout(&self, headers: &HeaderMap) {
        if let Some(token) = cookie_token(headers) {
            let hashed = hash(token);
            self.update(|sessions| {
                sessions.remove(&hashed);
            })
            .await;
        }
    }

    async fn update(&self, change: impl FnOnce(&mut HashMap<String, Session>)) {
        let _guard = self.write.lock().await;
        let persisted = {
            let mut sessions = lock(&self.active);
            change(&mut sessions);
            Persisted {
                pin: lock(&self.pin)
                    .as_ref()
                    .map(pin_fingerprint)
                    .unwrap_or_default(),
                sessions: sessions.clone(),
            }
        };
        let json = serde_json::to_string(&persisted).unwrap_or_default();
        if let Err(e) = self.hub.store_core_secret(STORE_KEY, &json).await {
            // Without a secret store, sessions simply do not survive restarts.
            tracing::debug!(error = %e, "sessions kept in memory only");
        }
    }

    pub(crate) fn cookie(token: &str) -> String {
        format!(
            "{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE_S}"
        )
    }

    pub(crate) fn clear_cookie() -> String {
        format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
    }
}

/// Route layer for the big uploads (a plan's image, a Home Assistant
/// import): only a person sends one, and that is checked before the first
/// byte of the body is read.
pub(crate) async fn human_first(
    axum::extract::State(humans): axum::extract::State<Arc<Sessions>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let human = request
        .extensions()
        .get::<crate::caller::Caller>()
        .is_some_and(|c| humans.is_human(request.headers(), &c.key));
    if !human {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(
                serde_json::json!({ "error": moli_i18n::tr!("serveur.session.humain_seulement") }),
            ),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_runtime::HubOptions;

    const HOME: &str = "192.168.0.20";
    const AGENT: &str = "192.168.0.62";

    fn headers_with(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            COOKIE,
            format!("theme=dark; {COOKIE_NAME}={token}")
                .parse()
                .unwrap(),
        );
        h
    }

    fn sessions(pin: &str) -> Sessions {
        Sessions::new(
            Hub::new(HubOptions::default()).unwrap(),
            Some(UiPin::new(pin)),
            &["Moi@Example.org".into()],
        )
    }

    #[tokio::test]
    async fn sessions_are_bound_to_their_client() {
        let s = sessions("482913");
        assert!(!s.is_human(&HeaderMap::new(), HOME));
        let token = s.login(" 482913 ", HOME).await.unwrap();
        assert!(s.is_human(&headers_with(&token), HOME));
        assert!(
            !s.is_human(&headers_with(&token), AGENT),
            "a leaked cookie is useless elsewhere"
        );
        assert!(!s.is_human(&headers_with("forged"), HOME));
        s.logout(&headers_with(&token)).await;
        assert!(!s.is_human(&headers_with(&token), HOME));
    }

    #[tokio::test]
    async fn an_agent_can_neither_guess_nor_lock_the_human_out() {
        let s = sessions("482913");
        for _ in 0..MAX_FAILURES {
            assert_eq!(s.login("000000", AGENT).await, Err(LoginError::Wrong));
        }
        assert_eq!(
            s.login("482913", AGENT).await,
            Err(LoginError::Locked),
            "even the right PIN"
        );
        assert!(s.locked(AGENT));
        // The human, from their own device, is not affected.
        assert!(!s.locked(HOME));
        assert!(s.login("482913", HOME).await.is_ok());
        assert!(!format!("{:?}", UiPin::new("482913")).contains("482913"));
    }

    #[tokio::test]
    async fn guessing_from_many_places_is_stopped_too() {
        let s = sessions("482913");
        for i in 0..MAX_GLOBAL_FAILURES {
            let from = format!("192.168.0.{}", 100 + i);
            assert_eq!(s.login("000000", &from).await, Err(LoginError::Wrong));
        }
        assert_eq!(
            s.login("482913", "192.168.0.250").await,
            Err(LoginError::Locked),
            "codes are off for everyone for an hour"
        );
    }

    #[tokio::test]
    async fn weak_or_missing_pins_disable_humans() {
        let weak = sessions("123");
        assert!(!weak.pin_configured());
        assert_eq!(weak.login("123", HOME).await, Err(LoginError::Disabled));
        // Four digits are enough (behind the lockouts).
        assert!(sessions("4821").pin_configured());
        let none = Sessions::new(Hub::new(HubOptions::default()).unwrap(), None, &[]);
        assert_eq!(none.login("", HOME).await, Err(LoginError::Disabled));
        assert!(!none.is_human(&headers_with("anything"), HOME));
    }

    /// A hub with an encrypted store (in a temporary file).
    fn store_hub(file: &str) -> (Hub, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("moli-pin-{}-{file}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let hub = Hub::new(HubOptions {
            secrets: Some((path.clone(), moli_runtime::MasterKey::new(key.into()))),
            ..HubOptions::default()
        })
        .unwrap();
        (hub, path)
    }

    /// With an encrypted store: a chosen code is kept.
    fn sessions_with_store(pin: &str, file: &str) -> (Sessions, std::path::PathBuf) {
        let (hub, path) = store_hub(file);
        (
            Sessions::new(hub, Some(UiPin::new(pin)), &["Moi@Example.org".into()]),
            path,
        )
    }

    #[tokio::test]
    async fn an_owner_chooses_a_new_code_and_old_sessions_end() {
        let (s, path) = sessions_with_store("482913", "owner");
        assert!(s.is_owner("moi@EXAMPLE.org "));
        assert!(!s.is_owner("someone@example.net"));
        let old = s.login("482913", HOME).await.unwrap();
        assert!(s.is_human(&headers_with(&old), HOME));
        assert!(s.set_pin("12", HOME).await.is_err(), "too short");
        assert!(s.set_pin("12ab", HOME).await.is_err(), "digits only");
        s.set_pin("2604", HOME).await.unwrap();
        assert!(
            !s.is_human(&headers_with(&old), HOME),
            "the old code's sessions end"
        );
        assert_eq!(s.login("482913", HOME).await, Err(LoginError::Wrong));
        let new = s.login("2604", HOME).await.unwrap();
        assert!(s.is_human(&headers_with(&new), HOME));
        // Kept: a restart takes the chosen code over the vault's.
        let hub = s.hub.clone();
        let again = Sessions::new(hub, Some(UiPin::new("482913")), &[]);
        assert_eq!(again.login("482913", HOME).await, Err(LoginError::Wrong));
        assert!(again.login("2604", HOME).await.is_ok());
        let _ = std::fs::remove_file(path);
    }

    /// A new house: a store, no code. The installation code is only in the
    /// logs; here it is set to a known one.
    fn new_house(file: &str) -> (Sessions, std::path::PathBuf) {
        let (hub, path) = store_hub(file);
        let s = Sessions::new(hub, None, &["Moi@Example.org".into()]);
        assert!(s.setup_pending(), "no code yet, a store: installation");
        *lock(&s.setup) = Some((hash(&normalize_code("ABCD-EFGH")), Instant::now()));
        (s, path)
    }

    #[tokio::test]
    async fn a_new_house_is_claimed_once_with_the_installation_code() {
        let (s, path) = new_house("claim");
        assert!(!s.pin_configured());
        assert_eq!(
            s.claim("ABCD-EFGX", "4821", HOME).await,
            Err(ClaimError::Wrong)
        );
        assert!(
            matches!(
                s.claim("ABCD-EFGH", "12", HOME).await,
                Err(ClaimError::Rejected(_))
            ),
            "a weak code is refused"
        );
        assert!(s.setup_pending(), "and the installation still waits");
        // Typed as it comes: lower case, a space instead of the dash.
        let token = s.claim(" abcd efgh ", "4821", HOME).await.unwrap();
        assert!(s.is_human(&headers_with(&token), HOME));
        assert!(s.pin_configured() && !s.setup_pending());
        assert_eq!(
            s.claim("ABCD-EFGH", "9999", AGENT).await,
            Err(ClaimError::NotNeeded),
            "once only"
        );
        assert!(s.login("4821", HOME).await.is_ok());
        // A restart keeps the house's code, and asks for no installation.
        let again = Sessions::new(s.hub.clone(), None, &[]);
        assert!(again.pin_configured() && !again.setup_pending());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn the_installation_code_cannot_be_guessed() {
        let (s, path) = new_house("guess");
        for _ in 0..MAX_FAILURES {
            assert_eq!(
                s.claim("AAAA-AAAA", "4821", AGENT).await,
                Err(ClaimError::Wrong)
            );
        }
        assert_eq!(
            s.claim("ABCD-EFGH", "4821", AGENT).await,
            Err(ClaimError::Locked),
            "even the right code, after the lockout"
        );
        assert!(s.claim("ABCD-EFGH", "4821", HOME).await.is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_house_that_had_a_code_never_reopens_to_the_logs() {
        // A code given by the environment, even empty: the vault is wrong,
        // this house is set up all the same.
        let (hub, path) = store_hub("vault-empty");
        assert!(!Sessions::new(hub, Some(UiPin::new("")), &[]).setup_pending());
        let _ = std::fs::remove_file(path);
        // A house whose chosen code was lost: sessions were kept once.
        let (s, path) = new_house("lost");
        s.claim("ABCD-EFGH", "4821", HOME).await.unwrap();
        s.hub.forget_core_secret(PIN_KEY).await.unwrap();
        let again = Sessions::new(s.hub.clone(), None, &[]);
        assert!(!again.pin_configured() && !again.setup_pending());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn an_installation_code_expires_and_a_house_alone_wants_six_digits() {
        let (hub, path) = store_hub("alone");
        let s = Sessions::new(hub, None, &[]);
        assert!(!s.has_owners() && s.min_pin_len() == 6);
        *lock(&s.setup) = Some((hash(&normalize_code("ABCD-EFGH")), Instant::now()));
        assert!(matches!(
            s.claim("ABCD-EFGH", "4821", HOME).await,
            Err(ClaimError::Rejected(_))
        ));
        if let Some(old) = Instant::now().checked_sub(SETUP_TTL * 2) {
            *lock(&s.setup) = Some((hash(&normalize_code("ABCD-EFGH")), old));
            assert!(!s.setup_pending());
            assert_eq!(
                s.claim("ABCD-EFGH", "482193", HOME).await,
                Err(ClaimError::Expired)
            );
        }
        *lock(&s.setup) = Some((hash(&normalize_code("ABCD-EFGH")), Instant::now()));
        assert!(s.claim("ABCD-EFGH", "482193", HOME).await.is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn no_installation_without_a_store_or_with_a_code() {
        let none = Sessions::new(Hub::new(HubOptions::default()).unwrap(), None, &[]);
        assert!(!none.setup_pending(), "no store: nowhere to keep a code");
        let (with_code, path) = sessions_with_store("482913", "configured");
        assert!(!with_code.setup_pending());
        let _ = std::fs::remove_file(path);
        let code = setup_code().unwrap();
        assert_eq!(code.len(), SETUP_LEN + 1);
        assert!(
            normalize_code(&code)
                .bytes()
                .all(|b| SETUP_ALPHABET.contains(&b))
        );
    }

    #[tokio::test]
    async fn the_current_code_proves_the_right_to_change_it() {
        let (s, path) = sessions_with_store("482913", "verify");
        assert_eq!(s.verify("000000", HOME), Err(LoginError::Wrong));
        assert!(s.verify(" 482913", HOME).is_ok());
        let _ = std::fs::remove_file(path);
    }
}

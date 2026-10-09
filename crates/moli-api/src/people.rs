//! The household's people: who lives here, what each may do, how each signs
//! in (« Personnes », like Home Assistant's persons and users).
//!
//! - `data/people.json` (written aside, then renamed): people, zones, pending
//!   invitations. No secret in it: a password lives in the encrypted store
//!   (`core/person/<id>/password`, PBKDF2-HMAC-SHA256), an invitation code and
//!   a session token are kept as SHA-256 only.
//! - Roles: `owner` (everything, manages people and the code), `member`
//!   (commands, approves automations with the code), `guest` (commands in
//!   its rooms only, until it expires).
//! - Signing in: Cloudflare Access's verified e-mail names the person who has
//!   it (no password then); otherwise a password, from anywhere (a session
//!   bound to the person, not to an address: a phone changes address); a new
//!   person sets theirs with an invitation code.
//! - The e-mails of `[server.access] owners` become owners at the first start:
//!   a house that had owners keeps them, nothing to do.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use moli_core::now_ms;
use moli_runtime::Hub;
use ring::digest::{SHA256, digest};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::session::{LoginError, Sessions};

pub(crate) const FILE: &str = "people.json";
const COOKIE_NAME: &str = "moli_person";
const SESSIONS_KEY: &str = "person_sessions";
const SESSION_MAX_AGE_S: u64 = 30 * 24 * 3600;
const MAX_SESSIONS_PER_PERSON: usize = 10;
const MAX_PEOPLE: usize = 50;
const MAX_ZONES: usize = 50;
pub(crate) const MIN_PASSWORD: usize = 10;
const MAX_PASSWORD: usize = 200;
const INVITE_TTL_MS: u64 = 7 * 24 * 3600 * 1000;
/// An invitation code: letters none can misread, eight of them.
const INVITE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const INVITE_LEN: usize = 8;
#[cfg(not(test))]
const PBKDF2_ROUNDS: u32 = 600_000;
#[cfg(test)]
const PBKDF2_ROUNDS: u32 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    Owner,
    Member,
    Guest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Person {
    /// Stable: phones and automations name it. Never renamed.
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) role: Role,
    /// E-mails Cloudflare Access may vouch for (lowercase).
    #[serde(default)]
    pub(crate) emails: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    /// A guest's rooms (the only ones it may command).
    #[serde(default)]
    pub(crate) rooms: Vec<String>,
    /// A guest's end (ms): past it, the person may no longer sign in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expires: Option<u64>,
    /// Wi-Fi addresses of devices that say the person is home.
    #[serde(default)]
    pub(crate) macs: Vec<String>,
    #[serde(default)]
    pub(crate) created: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Zone {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
    /// Metres.
    pub(crate) radius: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) icon: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    code_sha256: String,
    person: String,
    expires: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Book {
    #[serde(default)]
    pub(crate) people: Vec<Person>,
    #[serde(default)]
    pub(crate) zones: Vec<Zone>,
    #[serde(default)]
    invites: Vec<Invite>,
}

/// Who a request comes from, when it is a person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PersonRef {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) role: Role,
    /// A guest's rooms.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) rooms: Vec<String>,
}

impl PersonRef {
    pub(crate) fn is_owner(&self) -> bool {
        self.role == Role::Owner
    }

    pub(crate) fn is_guest(&self) -> bool {
        self.role == Role::Guest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersonSession {
    person: String,
    created: u64,
}

/// Why a change to the people was refused (a sentence for the person).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Invalid(String),
    NotFound,
    LastOwner,
    Store,
}

pub(crate) struct People {
    hub: Hub,
    path: Option<PathBuf>,
    book: Mutex<Book>,
    sessions: Mutex<HashMap<String, PersonSession>>,
    write: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for People {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("People")
            .field("people", &lock(&self.book).people.len())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    s.len()
        .is_multiple_of(2)
        .then(|| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}

fn hash(s: &str) -> String {
    hex(digest(&SHA256, s.as_bytes()).as_ref())
}

fn password_key(id: &str) -> String {
    format!("person/{id}/password")
}

/// `pbkdf2-sha256$<rounds>$<salt>$<hash>` (hex).
fn hash_password(password: &str) -> Option<String> {
    let mut salt = [0u8; 16];
    SystemRandom::new().fill(&mut salt).ok()?;
    let mut out = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF2_ROUNDS)?,
        &salt,
        password.as_bytes(),
        &mut out,
    );
    Some(format!(
        "pbkdf2-sha256${PBKDF2_ROUNDS}${}${}",
        hex(&salt),
        hex(&out)
    ))
}

fn password_matches(stored: &str, attempt: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("pbkdf2-sha256"), Some(rounds), Some(salt), Some(expected)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Some(rounds), Some(salt), Some(expected)) = (
        rounds.parse::<u32>().ok().and_then(NonZeroU32::new),
        unhex(salt),
        unhex(expected),
    ) else {
        return false;
    };
    pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        rounds,
        &salt,
        attempt.as_bytes(),
        &expected,
    )
    .is_ok()
}

fn token() -> Option<String> {
    let mut raw = [0u8; 32];
    SystemRandom::new().fill(&mut raw).ok()?;
    Some(hex(&raw))
}

fn invite_code() -> Option<String> {
    let mut raw = [0u8; INVITE_LEN];
    SystemRandom::new().fill(&mut raw).ok()?;
    let letters: String = raw
        .iter()
        .map(|b| char::from(INVITE_ALPHABET[usize::from(*b) % INVITE_ALPHABET.len()]))
        .collect();
    Some(format!("{}-{}", &letters[..4], &letters[4..]))
}

/// What a person types, as compared: no spaces, no dash, capitals.
fn plain_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

/// An id from a name: `Élodie` → `elodie`.
pub(crate) fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().to_lowercase().chars() {
        let c = match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').chars().take(32).collect::<String>();
    if s.is_empty() { "personne".into() } else { s }
}

fn text_ok(s: &str, max: usize) -> bool {
    let s = s.trim();
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn active(p: &Person) -> bool {
    p.expires.is_none_or(|end| now_ms() < end)
}

impl People {
    /// Reads `people.json` (none: nobody yet) and makes `owners` (Access
    /// e-mails of `[server.access]`) owners if nobody has their e-mail.
    pub(crate) fn open(hub: Hub, data_dir: Option<&Path>, owners: &[String]) -> Self {
        let path = data_dir.map(|d| d.join(FILE));
        let mut book: Book = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|raw| match serde_json::from_slice(&raw) {
                Ok(book) => Some(book),
                Err(e) => {
                    tracing::error!(error = %e, "people.json unreadable: nobody is known until it is fixed");
                    None
                }
            })
            .unwrap_or_default();
        let mut changed = false;
        for email in owners {
            let email = email.trim().to_lowercase();
            if email.is_empty() || book.people.iter().any(|p| p.emails.contains(&email)) {
                continue;
            }
            let local = email.split('@').next().unwrap_or("proprietaire");
            let mut name: String = local.chars().take(30).collect();
            if let Some(first) = name.get(..1) {
                name = first.to_uppercase() + &name[1..];
            }
            let id = unique_id(&book, &slug(local));
            book.people.push(Person {
                id,
                name,
                role: Role::Owner,
                emails: vec![email],
                color: None,
                rooms: Vec::new(),
                expires: None,
                macs: Vec::new(),
                created: now_ms(),
            });
            changed = true;
        }
        let sessions = hub
            .core_secret(SESSIONS_KEY)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let people = Self {
            hub,
            path,
            book: Mutex::new(book),
            sessions: Mutex::new(sessions),
            write: tokio::sync::Mutex::new(()),
        };
        if changed && let Err(e) = people.save_now() {
            tracing::error!(error = %e, "people.json not written");
        }
        people
    }

    fn save_now(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&*lock(&self.book)).unwrap_or_default();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    async fn change<T>(
        &self,
        f: impl FnOnce(&mut Book) -> Result<T, Refused>,
    ) -> Result<T, Refused> {
        let _guard = self.write.lock().await;
        let before = lock(&self.book).clone();
        let out = {
            let mut book = lock(&self.book);
            f(&mut book)?
        };
        if self.save_now().is_err() {
            *lock(&self.book) = before;
            return Err(Refused::Store);
        }
        Ok(out)
    }

    pub(crate) fn book(&self) -> Book {
        lock(&self.book).clone()
    }

    /// Whether an owner can sign in (a password set, or an e-mail Access
    /// vouches for): until then, the house's code manages the people (else
    /// the first owner, created but not yet invited, could never be).
    pub(crate) fn has_owner(&self) -> bool {
        lock(&self.book).people.iter().any(|p| {
            p.role == Role::Owner
                && active(p)
                && (!p.emails.is_empty() || self.hub.core_secret(&password_key(&p.id)).is_some())
        })
    }

    fn reference(p: &Person) -> PersonRef {
        PersonRef {
            id: p.id.clone(),
            name: p.name.clone(),
            role: p.role,
            rooms: p.rooms.clone(),
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<PersonRef> {
        lock(&self.book)
            .people
            .iter()
            .find(|p| p.id == id && active(p))
            .map(Self::reference)
    }

    /// The person Cloudflare Access vouched for, if Moli knows the e-mail.
    pub(crate) fn by_email(&self, email: &str) -> Option<PersonRef> {
        let email = email.trim().to_lowercase();
        lock(&self.book)
            .people
            .iter()
            .find(|p| active(p) && p.emails.contains(&email))
            .map(Self::reference)
    }

    /// The person signed in by this request's cookie.
    pub(crate) fn signed_in(&self, headers: &HeaderMap) -> Option<PersonRef> {
        let token = cookie_token(headers)?;
        let session = lock(&self.sessions).get(&hash(token)).cloned()?;
        (now_ms().saturating_sub(session.created) < SESSION_MAX_AGE_S * 1000)
            .then(|| self.get(&session.person))
            .flatten()
    }

    fn find_login(&self, login: &str) -> Option<Person> {
        let login = login.trim().to_lowercase();
        lock(&self.book)
            .people
            .iter()
            .find(|p| p.id == login || p.name.to_lowercase() == login || p.emails.contains(&login))
            .cloned()
    }

    /// Signs in with a password, behind the same lockouts as the house's
    /// code (per client, and for the whole house).
    pub(crate) async fn login(
        &self,
        guard: &Sessions,
        login: &str,
        password: &str,
        key: &str,
    ) -> Result<(String, PersonRef), LoginError> {
        let person = self.find_login(login).filter(active);
        let stored = person
            .as_ref()
            .and_then(|p| self.hub.core_secret(&password_key(&p.id)));
        guard.attempt(key, || {
            stored
                .as_deref()
                .is_some_and(|s| password_matches(s, password))
        })?;
        let person = person.ok_or(LoginError::Wrong)?;
        let token = self.open_session(&person.id).await?;
        Ok((token, Self::reference(&person)))
    }

    async fn open_session(&self, person: &str) -> Result<String, LoginError> {
        let token = token().ok_or(LoginError::Disabled)?;
        let max_age = SESSION_MAX_AGE_S * 1000;
        let json = {
            let mut sessions = lock(&self.sessions);
            sessions.retain(|_, s| now_ms().saturating_sub(s.created) < max_age);
            let mut mine: Vec<(String, u64)> = sessions
                .iter()
                .filter(|(_, s)| s.person == person)
                .map(|(k, s)| (k.clone(), s.created))
                .collect();
            mine.sort_by_key(|(_, c)| *c);
            while mine.len() >= MAX_SESSIONS_PER_PERSON {
                let (oldest, _) = mine.remove(0);
                sessions.remove(&oldest);
            }
            sessions.insert(
                hash(&token),
                PersonSession {
                    person: person.to_owned(),
                    created: now_ms(),
                },
            );
            serde_json::to_string(&*sessions).unwrap_or_default()
        };
        self.persist_sessions(json).await;
        Ok(token)
    }

    async fn persist_sessions(&self, json: String) {
        if let Err(e) = self.hub.store_core_secret(SESSIONS_KEY, &json).await {
            tracing::debug!(error = %e, "person sessions kept in memory only");
        }
    }

    pub(crate) async fn logout(&self, headers: &HeaderMap) {
        let Some(token) = cookie_token(headers) else {
            return;
        };
        let json = {
            let mut sessions = lock(&self.sessions);
            if sessions.remove(&hash(token)).is_none() {
                return;
            }
            serde_json::to_string(&*sessions).unwrap_or_default()
        };
        self.persist_sessions(json).await;
    }

    /// Ends every session of a person (password changed, person removed).
    async fn end_sessions(&self, person: &str) {
        let json = {
            let mut sessions = lock(&self.sessions);
            sessions.retain(|_, s| s.person != person);
            serde_json::to_string(&*sessions).unwrap_or_default()
        };
        self.persist_sessions(json).await;
    }

    pub(crate) fn cookie(token: &str) -> String {
        format!(
            "{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_MAX_AGE_S}"
        )
    }

    pub(crate) fn clear_cookie() -> String {
        format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
    }

    /// Whether this person has a password (else: Access, or an invitation).
    pub(crate) fn has_password(&self, id: &str) -> bool {
        self.hub.core_secret(&password_key(id)).is_some()
    }

    /// A new password: the person's sessions end, except the one that set it.
    pub(crate) async fn set_password(&self, id: &str, password: &str) -> Result<(), Refused> {
        let n = password.chars().count();
        if !(MIN_PASSWORD..=MAX_PASSWORD).contains(&n) {
            return Err(Refused::Invalid(moli_i18n::tr!(
                "serveur.personnes.mot_de_passe_court",
                min = MIN_PASSWORD
            )));
        }
        if self.get(id).is_none() {
            return Err(Refused::NotFound);
        }
        let stored = hash_password(password).ok_or(Refused::Store)?;
        self.hub
            .store_core_secret(&password_key(id), &stored)
            .await
            .map_err(|_| Refused::Store)?;
        self.end_sessions(id).await;
        Ok(())
    }

    /// An invitation for a person: a code to type, once, within 7 days.
    pub(crate) async fn invite(&self, id: &str) -> Result<(String, u64), Refused> {
        let code = invite_code().ok_or(Refused::Store)?;
        let expires = now_ms() + INVITE_TTL_MS;
        let sha = hash(&plain_code(&code));
        let person = id.to_owned();
        self.change(move |book| {
            if !book.people.iter().any(|p| p.id == person) {
                return Err(Refused::NotFound);
            }
            book.invites
                .retain(|i| i.person != person && i.expires > now_ms());
            book.invites.push(Invite {
                code_sha256: sha,
                person,
                expires,
            });
            Ok(())
        })
        .await?;
        Ok((code, expires))
    }

    /// An invitation used: the person sets a password and is signed in.
    pub(crate) async fn redeem(
        &self,
        guard: &Sessions,
        code: &str,
        password: &str,
        key: &str,
    ) -> Result<(String, PersonRef), Result<LoginError, Refused>> {
        let sha = hash(&plain_code(code));
        let found = lock(&self.book)
            .invites
            .iter()
            .find(|i| i.code_sha256 == sha && i.expires > now_ms())
            .map(|i| i.person.clone());
        guard.attempt(key, || found.is_some()).map_err(Ok)?;
        let person = found.ok_or(Ok(LoginError::Wrong))?;
        self.set_password(&person, password).await.map_err(Err)?;
        let used = sha.clone();
        self.change(move |book| {
            book.invites.retain(|i| i.code_sha256 != used);
            Ok(())
        })
        .await
        .map_err(Err)?;
        let reference = self.get(&person).ok_or(Err(Refused::NotFound))?;
        let token = self.open_session(&person).await.map_err(Ok)?;
        Ok((token, reference))
    }

    /// A new person (an owner adds them).
    pub(crate) async fn create(&self, draft: Draft) -> Result<Person, Refused> {
        let draft = draft.checked()?;
        self.change(move |book| {
            if book.people.len() >= MAX_PEOPLE {
                return Err(Refused::Invalid(moli_i18n::tr!("serveur.personnes.trop")));
            }
            email_free(book, &draft.emails, None)?;
            let person = Person {
                id: unique_id(book, &slug(&draft.name)),
                name: draft.name,
                role: draft.role,
                emails: draft.emails,
                color: draft.color,
                rooms: draft.rooms,
                expires: draft.expires,
                macs: draft.macs,
                created: now_ms(),
            };
            book.people.push(person.clone());
            Ok(person)
        })
        .await
    }

    pub(crate) async fn update(&self, id: &str, draft: Draft) -> Result<Person, Refused> {
        let draft = draft.checked()?;
        let id = id.to_owned();
        let out = self
            .change(move |book| {
                email_free(book, &draft.emails, Some(&id))?;
                let owners_left = book
                    .people
                    .iter()
                    .filter(|p| p.role == Role::Owner && p.id != id)
                    .count();
                let p = book
                    .people
                    .iter_mut()
                    .find(|p| p.id == id)
                    .ok_or(Refused::NotFound)?;
                if p.role == Role::Owner && draft.role != Role::Owner && owners_left == 0 {
                    return Err(Refused::LastOwner);
                }
                p.name = draft.name;
                p.role = draft.role;
                p.emails = draft.emails;
                p.color = draft.color;
                p.rooms = draft.rooms;
                p.expires = draft.expires;
                p.macs = draft.macs;
                Ok(p.clone())
            })
            .await?;
        Ok(out)
    }

    pub(crate) async fn remove(&self, id: &str) -> Result<(), Refused> {
        let gone = id.to_owned();
        self.change(move |book| {
            let p = book
                .people
                .iter()
                .find(|p| p.id == gone)
                .ok_or(Refused::NotFound)?;
            if p.role == Role::Owner
                && !book
                    .people
                    .iter()
                    .any(|o| o.role == Role::Owner && o.id != gone)
            {
                return Err(Refused::LastOwner);
            }
            book.people.retain(|p| p.id != gone);
            book.invites.retain(|i| i.person != gone);
            Ok(())
        })
        .await?;
        self.end_sessions(id).await;
        let _ = self.hub.forget_core_secret(&password_key(id)).await;
        Ok(())
    }

    pub(crate) async fn set_zones(&self, zones: Vec<Zone>) -> Result<Vec<Zone>, Refused> {
        if zones.len() > MAX_ZONES {
            return Err(Refused::Invalid(moli_i18n::tr!(
                "serveur.personnes.zones_trop"
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for z in &zones {
            let ok = !z.id.is_empty()
                && z.id.len() <= 32
                && z.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && seen.insert(z.id.clone())
                && text_ok(&z.name, 40)
                && (-90.0..=90.0).contains(&z.latitude)
                && (-180.0..=180.0).contains(&z.longitude)
                && (20.0..=50_000.0).contains(&z.radius);
            if !ok {
                return Err(Refused::Invalid(moli_i18n::tr!(
                    "serveur.personnes.zone_invalide",
                    zone = z.name.clone()
                )));
            }
        }
        self.change(move |book| {
            book.zones.clone_from(&zones);
            Ok(zones)
        })
        .await
    }
}

fn unique_id(book: &Book, base: &str) -> String {
    let mut id = base.to_owned();
    let mut n = 2;
    while book.people.iter().any(|p| p.id == id) || id == "maison" {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

fn email_free(book: &Book, emails: &[String], except: Option<&str>) -> Result<(), Refused> {
    for e in emails {
        if book
            .people
            .iter()
            .any(|p| Some(p.id.as_str()) != except && p.emails.contains(e))
        {
            return Err(Refused::Invalid(moli_i18n::tr!(
                "serveur.personnes.email_pris",
                email = e.clone()
            )));
        }
    }
    Ok(())
}

/// A person as an owner writes it (`POST`/`PUT /api/people`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Draft {
    pub(crate) name: String,
    pub(crate) role: Role,
    #[serde(default)]
    pub(crate) emails: Vec<String>,
    #[serde(default)]
    pub(crate) color: Option<String>,
    #[serde(default)]
    pub(crate) rooms: Vec<String>,
    #[serde(default)]
    pub(crate) expires: Option<u64>,
    #[serde(default)]
    pub(crate) macs: Vec<String>,
}

impl Draft {
    fn checked(mut self) -> Result<Self, Refused> {
        let bad = |what: &str| {
            Err(Refused::Invalid(moli_i18n::tr!(
                "serveur.personnes.champ",
                champ = what.to_owned()
            )))
        };
        self.name = self.name.trim().to_owned();
        if !text_ok(&self.name, 40) {
            return bad("name");
        }
        self.emails = self
            .emails
            .iter()
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        if self.emails.len() > 5
            || self
                .emails
                .iter()
                .any(|e| e.len() > 120 || !e.contains('@') || e.chars().any(char::is_whitespace))
        {
            return bad("emails");
        }
        if self.color.as_deref().is_some_and(|c| {
            !(c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()))
        }) {
            return bad("color");
        }
        if self.rooms.len() > 30 || self.rooms.iter().any(|r| !text_ok(r, 60)) {
            return bad("rooms");
        }
        self.macs = self.macs.iter().map(|m| m.trim().to_lowercase()).collect();
        if self.macs.len() > 10
            || self.macs.iter().any(|m| {
                m.len() != 17
                    || !m
                        .split(':')
                        .all(|o| o.len() == 2 && o.bytes().all(|b| b.is_ascii_hexdigit()))
            })
        {
            return bad("macs");
        }
        if self.role != Role::Guest {
            self.rooms.clear();
            self.expires = None;
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_runtime::HubOptions;

    fn hub() -> Hub {
        Hub::new(HubOptions::default()).unwrap()
    }

    /// A hub with an encrypted store (passwords need one).
    fn hub_with_store() -> Hub {
        let path = std::env::temp_dir().join(format!(
            "moli-people-secrets-{}-{}.enc",
            std::process::id(),
            now_ms()
        ));
        Hub::new(HubOptions {
            secrets: Some((path, moli_runtime::MasterKey::generate().unwrap())),
            ..HubOptions::default()
        })
        .unwrap()
    }

    fn draft(name: &str, role: Role) -> Draft {
        Draft {
            name: name.into(),
            role,
            emails: Vec::new(),
            color: None,
            rooms: Vec::new(),
            expires: None,
            macs: Vec::new(),
        }
    }

    #[test]
    fn passwords_are_salted_and_checked() {
        let a = hash_password("correct horse battery").unwrap();
        let b = hash_password("correct horse battery").unwrap();
        assert_ne!(a, b, "salted");
        assert!(password_matches(&a, "correct horse battery"));
        assert!(!password_matches(&a, "correct horse batterY"));
        assert!(!password_matches("garbage", "x"));
    }

    #[test]
    fn ids_come_from_names() {
        assert_eq!(slug("Élodie"), "elodie");
        assert_eq!(slug("  Jean-Él ève "), "jean-el-eve");
        assert_eq!(slug("!!!"), "personne");
    }

    #[test]
    fn access_owners_become_owners_once() {
        let dir = std::env::temp_dir().join(format!("moli-people-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let owners = vec!["Moi@Example.org".to_owned()];
        let p = People::open(hub(), Some(&dir), &owners);
        let me = p.by_email("moi@example.org").unwrap();
        assert_eq!((me.id.as_str(), me.role), ("moi", Role::Owner));
        drop(p);
        let p = People::open(hub(), Some(&dir), &owners);
        assert_eq!(p.book().people.len(), 1, "not added twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_last_owner_stays() {
        let p = People::open(hub(), None, &["a@example.org".to_owned()]);
        assert_eq!(p.remove("a").await, Err(Refused::LastOwner));
        assert_eq!(
            p.update("a", draft("A", Role::Member)).await.err(),
            Some(Refused::LastOwner)
        );
        let b = p.create(draft("B", Role::Owner)).await.unwrap();
        p.remove("a").await.unwrap();
        assert!(p.get(&b.id).is_some());
    }

    #[tokio::test]
    async fn an_invitation_sets_a_password_once() {
        let hub = hub_with_store();
        let guard = Sessions::new(hub.clone(), Some(crate::UiPin::new("123456")), &[]);
        let p = People::open(hub, None, &[]);
        let elodie = p.create(draft("Élodie", Role::Member)).await.unwrap();
        let (code, _) = p.invite(&elodie.id).await.unwrap();
        let typed = code.to_lowercase().replace('-', " ");
        let short = p.redeem(&guard, &typed, "court", "k").await;
        assert!(
            matches!(short, Err(Err(Refused::Invalid(_)))),
            "password too short"
        );
        let (token, who) = p
            .redeem(&guard, &typed, "un mot de passe long", "k")
            .await
            .unwrap();
        assert_eq!(who.id, "elodie");
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, format!("{COOKIE_NAME}={token}").parse().unwrap());
        assert_eq!(p.signed_in(&headers).unwrap().id, "elodie");
        assert!(
            p.redeem(&guard, &code, "un mot de passe long", "k")
                .await
                .is_err(),
            "once"
        );
        let (_, again) = p
            .login(&guard, "ELODIE", "un mot de passe long", "k2")
            .await
            .unwrap();
        assert_eq!(again.role, Role::Member);
        assert_eq!(
            p.login(&guard, "elodie", "faux mot de passe", "k3")
                .await
                .err(),
            Some(LoginError::Wrong)
        );
    }

    #[tokio::test]
    async fn an_expired_guest_cannot_sign_in() {
        let hub = hub();
        let guard = Sessions::new(hub.clone(), None, &[]);
        let p = People::open(hub, None, &[]);
        let mut d = draft("Invité", Role::Guest);
        d.expires = Some(1);
        let g = p.create(d).await.unwrap();
        p.set_password(&g.id, "un mot de passe long")
            .await
            .unwrap_err();
        assert!(p.get(&g.id).is_none());
        assert!(
            p.login(&guard, &g.id, "un mot de passe long", "k")
                .await
                .is_err()
        );
    }
}

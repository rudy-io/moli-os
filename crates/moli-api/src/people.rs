//! The household's people: who lives here, what each may do, how each signs
//! in (« Personnes », like Home Assistant's persons and users).
//!
//! - `data/people.json` (written aside, then renamed; readable by Moli's user
//!   only, 0600 on unix): people, zones, pending invitations. Who lives here,
//!   their e-mails and places, but nothing that opens the house: a password
//!   lives in the encrypted store (`core/person/<id>/password`, Argon2id;
//!   an older PBKDF2 one is replaced at the next sign-in), so do the
//!   sessions (their tokens as SHA-256 only);
//!   an invitation code is kept as an HMAC-SHA256 under a key of that store
//!   (`core/invite_key`), so the file alone cannot be tried against every
//!   code.
//! - Roles: `owner` (« admin »: everything, manages people and the code),
//!   `member` (« adult »: commands, approves automations, signed in or with
//!   the code), `guest` (sees and commands its rooms and the devices given
//!   to it only, until it expires: nothing of the rest of the house,
//!   nobody's whereabouts).
//! - Passwords: 15 characters at least, none of the most common ones, none
//!   made of the person's name (NIST SP 800-63B-4: length and a blocklist,
//!   no composition rule). Checked one at a time, off the async workers.
//! - Signing in: Cloudflare Access's verified e-mail names the person who has
//!   it (no password then); otherwise a password, from anywhere (a session
//!   bound to the person, not to an address: a phone changes address); a new
//!   person sets theirs with an invitation code.
//! - The e-mails of `[server.access] owners` become owners at the first start:
//!   a house that had owners keeps them, nothing to do.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use moli_core::{DeviceId, InstanceId, now_ms};
use moli_runtime::{Hub, Snapshot};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{hmac, pbkdf2};
use serde::{Deserialize, Serialize};

use crate::caller::{Caller, Via};
use crate::session::{LoginError, Sessions};

pub(crate) const FILE: &str = "people.json";
const COOKIE_NAME: &str = "moli_person";
const SESSIONS_KEY: &str = "person_sessions";
const SESSION_MAX_AGE_S: u64 = 30 * 24 * 3600;
const MAX_SESSIONS_PER_PERSON: usize = 10;
const MAX_PEOPLE: usize = 50;
const MAX_ZONES: usize = 50;
pub(crate) const MIN_PASSWORD: usize = 15;
const MAX_PASSWORD: usize = 200;
const INVITE_TTL_MS: u64 = 7 * 24 * 3600 * 1000;
/// An invitation code: letters none can misread, eight of them.
const INVITE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const INVITE_LEN: usize = 8;
/// The key invitation codes are kept under (encrypted store).
const INVITE_KEY: &str = "invite_key";
/// Argon2id (RFC 9106) as OWASP sets it: 12 MiB, 3 passes, one lane (its
/// equal of 19 MiB × 2, lighter on a small board). Tests: cheap.
#[cfg(not(test))]
const ARGON2: (u32, u32) = (12 * 1024, 3);
#[cfg(test)]
const ARGON2: (u32, u32) = (64, 1);
/// Sign-ins waiting for their turn at the hash, past which one is refused
/// as too many tries (a burst would otherwise queue without end).
const MAX_WAITING: usize = 8;
/// The most common passwords long enough to be allowed (NCSC, lowercase).
const COMMON: &str = include_str!("common-passwords.txt");
const MAX_DEVICES: usize = 100;
/// Drivers whose devices say where someone is (their phones, where each
/// person is, who is home): never a guest's, whatever their room.
const WHEREABOUTS: [&str; 3] = ["phones", "people", "presence"];

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
    /// Devices given to a guest besides its rooms (`hue:…`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) devices: Vec<String>,
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
    /// HMAC-SHA256 of the code (hex), under the house's invitation key.
    #[serde(default)]
    code_hmac: String,
    /// What older versions kept: a bare SHA-256, which whoever read the
    /// file could try every code against. Such an invitation opens nothing:
    /// dropped when the file is read, the owner invites again.
    #[serde(default, skip_serializing)]
    code_sha256: Option<String>,
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
    /// Devices given to a guest, whatever their room.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) devices: Vec<String>,
}

impl PersonRef {
    pub(crate) fn is_owner(&self) -> bool {
        self.role == Role::Owner
    }

    pub(crate) fn is_guest(&self) -> bool {
        self.role == Role::Guest
    }

    /// What a guest sees of the house, and commands: the devices given to it,
    /// and those whose rooms are all its own (a group's members' count too; a
    /// device without a room is nobody's), never one that says where someone
    /// is.
    pub(crate) fn guest_devices(&self, hub: &Hub, snapshot: &Snapshot) -> HashSet<DeviceId> {
        let hidden = whereabouts(snapshot);
        snapshot
            .devices
            .iter()
            .map(|view| &view.device.id)
            .filter(|id| self.in_rooms(hub, &hidden, id))
            .cloned()
            .collect()
    }

    /// The house as this guest sees it (its devices, the orders held for
    /// them), and the devices it sees.
    pub(crate) fn guest_view(&self, hub: &Hub) -> (Snapshot, HashSet<DeviceId>) {
        let mut snapshot = hub.snapshot();
        let sight = self.guest_devices(hub, &snapshot);
        snapshot
            .devices
            .retain(|view| sight.contains(&view.device.id));
        snapshot
            .approvals
            .retain(|a| a.point.split().is_some_and(|(id, _)| sight.contains(&id)));
        (snapshot, sight)
    }

    /// Whether this guest sees (and commands) `device`.
    pub(crate) fn guest_sees(&self, hub: &Hub, device: &DeviceId) -> bool {
        let snapshot = hub.snapshot();
        snapshot
            .devices
            .iter()
            .any(|view| view.device.id == *device)
            && self.in_rooms(hub, &whereabouts(&snapshot), device)
    }

    fn in_rooms(&self, hub: &Hub, hidden: &HashSet<InstanceId>, device: &DeviceId) -> bool {
        if hidden.contains(&device.instance()) {
            return false;
        }
        if self.devices.iter().any(|d| d == device.as_str()) {
            return true;
        }
        let rooms = hub.rooms_of(device);
        !rooms.is_empty() && rooms.iter().all(|r| self.rooms.contains(r))
    }
}

/// The driver instances whose devices say where someone is.
fn whereabouts(snapshot: &Snapshot) -> HashSet<InstanceId> {
    snapshot
        .drivers
        .iter()
        .filter(|d| WHEREABOUTS.contains(&d.kind))
        .map(|d| d.instance.clone())
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersonSession {
    person: String,
    created: u64,
    /// What it was opened on (« iPhone · Safari »), for the person's list.
    #[serde(default)]
    device: String,
    /// Opened from the Internet rather than the home network.
    #[serde(default)]
    remote: bool,
}

/// A session as its person sees it, in their list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionView {
    /// A short handle (the start of the token's digest), to end it by.
    pub(crate) id: String,
    pub(crate) created: u64,
    pub(crate) device: String,
    pub(crate) remote: bool,
    /// The one this request came with.
    pub(crate) current: bool,
}

/// How long a session's handle is (hex digits of the token's digest).
const HANDLE: usize = 12;

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

fn argon2(memory_kib: u32, passes: u32, password: &str, salt: &[u8]) -> Option<[u8; 32]> {
    let params = argon2::Params::new(memory_kib, passes, 1, Some(32)).ok()?;
    let mut out = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .ok()?;
    Some(out)
}

/// The prefix of a password kept with today's settings.
fn current_scheme() -> String {
    format!("argon2id$v=19$m={},t={},p=1$", ARGON2.0, ARGON2.1)
}

/// `argon2id$v=19$m=<KiB>,t=<passes>,p=1$<salt>$<hash>` (hex). Slow on
/// purpose: call it through [`off_thread`].
fn hash_password(password: &str) -> Option<String> {
    let mut salt = [0u8; 16];
    SystemRandom::new().fill(&mut salt).ok()?;
    let out = argon2(ARGON2.0, ARGON2.1, password, &salt)?;
    Some(format!("{}{}${}", current_scheme(), hex(&salt), hex(&out)))
}

/// Whether a kept password should be hashed again (older scheme, older
/// settings): done at a sign-in, the only time the password is at hand.
fn needs_rehash(stored: &str) -> bool {
    !stored.starts_with(&current_scheme())
}

/// Same bytes, in a time that does not say where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn password_matches(stored: &str, attempt: &str) -> bool {
    if let Some(rest) = stored.strip_prefix("argon2id$v=19$") {
        let mut parts = rest.split('$');
        let (Some(params), Some(salt), Some(expected), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let mut m = None;
        let mut t = None;
        for kv in params.split(',') {
            match kv.split_once('=') {
                Some(("m", v)) => m = v.parse::<u32>().ok(),
                Some(("t", v)) => t = v.parse::<u32>().ok(),
                Some(("p", "1")) => {}
                _ => return false,
            }
        }
        // Never more than a password of this house could have cost.
        let (Some(m), Some(t), Some(salt), Some(expected)) = (
            m.filter(|m| (8..=64 * 1024).contains(m)),
            t.filter(|t| (1..=10).contains(t)),
            unhex(salt),
            unhex(expected),
        ) else {
            return false;
        };
        return argon2(m, t, attempt, &salt).is_some_and(|out| same(&out, &expected));
    }
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

/// A kept password nobody has, checked in place of an unknown login's: a
/// wrong name takes as long as a wrong password, and says nothing more.
fn decoy() -> &'static str {
    static DECOY: OnceLock<String> = OnceLock::new();
    DECOY.get_or_init(|| hash_password("no one has this password").unwrap_or_default())
}

/// Runs a password hash off the async workers, one at a time (each takes
/// its memory and a fraction of a second). `None`: too many waiting.
async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    static TURN: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    static WAITING: AtomicUsize = AtomicUsize::new(0);
    if WAITING.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
        WAITING.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    let permit = TURN.acquire().await;
    WAITING.fetch_sub(1, Ordering::SeqCst);
    let _permit = permit.ok()?;
    tokio::task::spawn_blocking(work).await.ok()
}

/// Why a password is too easy, if it is: too short, one of the most common,
/// a handful of characters over and over, or little more than the person's
/// name or the house's.
fn weak(password: &str, name: &str, id: &str) -> Option<String> {
    let n = password.chars().count();
    if !(MIN_PASSWORD..=MAX_PASSWORD).contains(&n) {
        return Some(moli_i18n::tr!(
            "serveur.personnes.mot_de_passe_court",
            min = MIN_PASSWORD
        ));
    }
    let lower = password.trim().to_lowercase();
    let distinct: HashSet<char> = lower.chars().collect();
    let mut left: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    for word in [
        &name.to_lowercase(),
        id,
        "moli",
        "maison",
        "motdepasse",
        "password",
        "azerty",
        "qwerty",
        "123456789",
    ] {
        let word: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if word.chars().count() >= 3 {
            left = left.replace(&word, "");
        }
    }
    let easy = distinct.len() < 5
        || left.chars().count() < 8
        || COMMON.lines().any(|common| common == lower);
    easy.then(|| moli_i18n::tr!("serveur.personnes.mot_de_passe_facile"))
}

/// What a session was opened on, from the browser's own words: enough for
/// a person to tell their phone from their computer, nothing more kept.
fn device_of(headers: &HeaderMap) -> String {
    let agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let system = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "Android"),
        ("Macintosh", "Mac"),
        ("Windows", "Windows"),
        ("CrOS", "ChromeOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(k, _)| agent.contains(k))
    .map(|(_, v)| *v);
    let browser = [
        ("Edg/", "Edge"),
        ("Firefox/", "Firefox"),
        ("FxiOS/", "Firefox"),
        ("CriOS/", "Chrome"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(k, _)| agent.contains(k))
    .map(|(_, v)| *v);
    match (system, browser) {
        (Some(s), Some(b)) => format!("{s} · {b}"),
        (Some(one), None) | (None, Some(one)) => one.to_owned(),
        (None, None) => String::new(),
    }
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

/// How an invitation code is kept: its HMAC-SHA256 under the house's key.
fn invite_digest(key: &hmac::Key, code: &str) -> String {
    hex(hmac::sign(key, plain_code(code).as_bytes()).as_ref())
}

/// `people.json` says who lives here, their e-mails and places: for Moli's
/// user alone (0600 on unix, like the secret store).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    // A file left there (a crash) kept its own mode.
    private(path)?;
    file.write_all(bytes)
}

#[cfg(unix)]
fn private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn private(_: &Path) -> std::io::Result<()> {
    Ok(())
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
                devices: Vec::new(),
                expires: None,
                macs: Vec::new(),
                created: now_ms(),
            });
            changed = true;
        }
        // Invitations of an older kind (a bare SHA-256) open nothing now.
        let invites = book.invites.len();
        book.invites
            .retain(|i| i.code_sha256.is_none() && !i.code_hmac.is_empty());
        if book.invites.len() < invites {
            tracing::warn!(
                dropped = invites - book.invites.len(),
                "pending invitations of an older kind dropped: invite again"
            );
            changed = true;
        }
        // Written by an older version, readable by all: Moli's user's only.
        if let Some(path) = path.as_deref().filter(|p| p.exists())
            && let Err(e) = private(path)
        {
            tracing::warn!(error = %e, "people.json left readable by others");
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
        write_private(&tmp, &json)?;
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
            devices: p.devices.clone(),
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
    /// code (per client, and for its side of the house: from the Internet,
    /// a budget apart from the household's). An unknown name is checked
    /// against a decoy, as slowly: the answer never says who exists.
    pub(crate) async fn login(
        &self,
        guard: &Sessions,
        login: &str,
        password: &str,
        caller: &Caller,
        headers: &HeaderMap,
    ) -> Result<(String, PersonRef), LoginError> {
        guard.admit(caller)?;
        let person = self.find_login(login).filter(active);
        let stored = person
            .as_ref()
            .and_then(|p| self.hub.core_secret(&password_key(&p.id)));
        let (kept, attempt) = (stored.clone(), password.to_owned());
        let matched = off_thread(move || {
            let against = kept.as_deref().unwrap_or_else(|| decoy());
            password_matches(against, &attempt) && kept.is_some()
        })
        .await
        .ok_or(LoginError::Locked)?;
        let counted = guard.attempt(caller, || matched);
        if counted == Err(LoginError::Wrong)
            && let Some(p) = &person
        {
            self.journal(caller, &p.id, moli_core::AccessChange::Refused);
        }
        counted?;
        let person = person.ok_or(LoginError::Wrong)?;
        if stored.as_deref().is_some_and(needs_rehash) {
            self.rehash(&person.id, password).await;
        }
        let token = self
            .open_session(
                &person.id,
                device_of(headers),
                caller.via != Via::Lan && caller.via != Via::Loopback,
            )
            .await?;
        self.journal(caller, &person.id, moli_core::AccessChange::SignedIn);
        Ok((token, Self::reference(&person)))
    }

    /// A password kept the older way, kept again today's way (it is at hand
    /// only now). A failure leaves the older one, still good.
    async fn rehash(&self, id: &str, password: &str) {
        let password = password.to_owned();
        let Some(Some(stored)) = off_thread(move || hash_password(&password)).await else {
            return;
        };
        match self.hub.store_core_secret(&password_key(id), &stored).await {
            Ok(()) => tracing::info!(person = id, "password kept again with Argon2id"),
            Err(e) => tracing::warn!(person = id, error = %e, "password not kept again"),
        }
    }

    /// Writes in the journal who entered, changed or left (never a secret).
    pub(crate) fn journal(&self, caller: &Caller, person: &str, change: moli_core::AccessChange) {
        self.hub.record_access(
            moli_core::Origin::Ui,
            Some(caller.describe()),
            person,
            change,
        );
    }

    async fn open_session(
        &self,
        person: &str,
        device: String,
        remote: bool,
    ) -> Result<String, LoginError> {
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
                    device,
                    remote,
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

    /// A person's sessions, newest first; `current` marks this request's.
    pub(crate) fn sessions_of(&self, person: &str, headers: &HeaderMap) -> Vec<SessionView> {
        let mine = cookie_token(headers).map(hash);
        let max_age = SESSION_MAX_AGE_S * 1000;
        let mut list: Vec<SessionView> = lock(&self.sessions)
            .iter()
            .filter(|(_, s)| s.person == person && now_ms().saturating_sub(s.created) < max_age)
            .map(|(key, s)| SessionView {
                id: key.chars().take(HANDLE).collect(),
                created: s.created,
                device: s.device.clone(),
                remote: s.remote,
                current: mine.as_deref() == Some(key.as_str()),
            })
            .collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.created));
        list
    }

    /// Ends one of a person's sessions by its handle; `false`: none such.
    pub(crate) async fn end_session(&self, person: &str, handle: &str) -> bool {
        if handle.len() != HANDLE {
            return false;
        }
        let json = {
            let mut sessions = lock(&self.sessions);
            let Some(key) = sessions
                .iter()
                .find(|(k, s)| s.person == person && k.starts_with(handle))
                .map(|(k, _)| k.clone())
            else {
                return false;
            };
            sessions.remove(&key);
            serde_json::to_string(&*sessions).unwrap_or_default()
        };
        self.persist_sessions(json).await;
        true
    }

    /// Ends every session of a person (an owner, for a lost phone).
    pub(crate) async fn end_all_sessions(&self, person: &str) {
        self.end_sessions(person).await;
    }

    /// The session cookie: `Secure` when the page came over HTTPS (on the
    /// home network, Moli is often reached in plain HTTP, where a `Secure`
    /// cookie would never come back).
    pub(crate) fn cookie(token: &str, secure: bool) -> String {
        let secure = if secure { "; Secure" } else { "" };
        format!(
            "{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_MAX_AGE_S}{secure}"
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
        let person = self.get(id);
        let name = person.as_ref().map(|p| p.name.as_str()).unwrap_or_default();
        if let Some(why) = weak(password, name, id) {
            return Err(Refused::Invalid(why));
        }
        if person.is_none() {
            return Err(Refused::NotFound);
        }
        let password = password.to_owned();
        let stored = off_thread(move || hash_password(&password))
            .await
            .flatten()
            .ok_or(Refused::Store)?;
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
        let digest = invite_digest(&self.new_invite_key().await?, &code);
        let person = id.to_owned();
        self.change(move |book| {
            if !book.people.iter().any(|p| p.id == person) {
                return Err(Refused::NotFound);
            }
            book.invites
                .retain(|i| i.person != person && i.expires > now_ms());
            book.invites.push(Invite {
                code_hmac: digest,
                code_sha256: None,
                person,
                expires,
            });
            Ok(())
        })
        .await?;
        Ok((code, expires))
    }

    /// The house's invitation key, from the encrypted store.
    fn invite_key(&self) -> Option<hmac::Key> {
        let key = unhex(&self.hub.core_secret(INVITE_KEY)?)?;
        Some(hmac::Key::new(hmac::HMAC_SHA256, &key))
    }

    /// The invitation key, made at the first invitation. Refused without a
    /// store (where no password could be kept either).
    async fn new_invite_key(&self) -> Result<hmac::Key, Refused> {
        // One at a time: two first invitations make one key.
        let _guard = self.write.lock().await;
        if let Some(key) = self.invite_key() {
            return Ok(key);
        }
        let mut raw = [0u8; 32];
        SystemRandom::new()
            .fill(&mut raw)
            .map_err(|_| Refused::Store)?;
        self.hub
            .store_core_secret(INVITE_KEY, &hex(&raw))
            .await
            .map_err(|_| Refused::Store)?;
        Ok(hmac::Key::new(hmac::HMAC_SHA256, &raw))
    }

    /// An invitation used: the person sets a password and is signed in.
    pub(crate) async fn redeem(
        &self,
        guard: &Sessions,
        code: &str,
        password: &str,
        caller: &Caller,
        headers: &HeaderMap,
    ) -> Result<(String, PersonRef), Result<LoginError, Refused>> {
        // No key yet: no invitation was ever made.
        let typed = self
            .invite_key()
            .map(|house| invite_digest(&house, code))
            .unwrap_or_default();
        let found = lock(&self.book)
            .invites
            .iter()
            .find(|i| !typed.is_empty() && i.code_hmac == typed && i.expires > now_ms())
            .map(|i| i.person.clone());
        guard.attempt(caller, || found.is_some()).map_err(Ok)?;
        let person = found.ok_or(Ok(LoginError::Wrong))?;
        self.set_password(&person, password).await.map_err(Err)?;
        self.change(move |book| {
            book.invites.retain(|i| i.code_hmac != typed);
            Ok(())
        })
        .await
        .map_err(Err)?;
        let reference = self.get(&person).ok_or(Err(Refused::NotFound))?;
        let token = self
            .open_session(
                &person,
                device_of(headers),
                caller.via != Via::Lan && caller.via != Via::Loopback,
            )
            .await
            .map_err(Ok)?;
        self.journal(caller, &person, moli_core::AccessChange::SignedIn);
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
                devices: draft.devices,
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
                p.devices = draft.devices;
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
    pub(crate) devices: Vec<String>,
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
        self.devices.sort();
        self.devices.dedup();
        if self.devices.len() > MAX_DEVICES
            || self
                .devices
                .iter()
                .any(|d| !text_ok(d, 120) || !d.contains(':') || d.chars().any(char::is_whitespace))
        {
            return bad("devices");
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
            self.devices.clear();
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

    /// Someone on the Internet, where invitations and passwords come from.
    fn stranger(key: &str) -> Caller {
        Caller {
            via: crate::caller::Via::Outside,
            key: key.to_owned(),
            person: None,
        }
    }

    /// A hub with an encrypted store (passwords need one), each its own
    /// (two tests in the same millisecond would share a file).
    fn hub_with_store() -> Hub {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "moli-people-secrets-{}-{}-{}.enc",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
            devices: Vec::new(),
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

    /// A password as older versions kept it.
    fn pbkdf2_kept(password: &str) -> String {
        let salt = [7u8; 16];
        let mut out = [0u8; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(1_000).unwrap(),
            &salt,
            password.as_bytes(),
            &mut out,
        );
        format!("pbkdf2-sha256$1000${}${}", hex(&salt), hex(&out))
    }

    #[test]
    fn passwords_are_kept_with_argon2id() {
        let kept = hash_password("une longue phrase de passe").unwrap();
        assert!(kept.starts_with("argon2id$v=19$m="), "{kept}");
        assert!(!needs_rehash(&kept));
        let old = pbkdf2_kept("une longue phrase de passe");
        assert!(password_matches(&old, "une longue phrase de passe"));
        assert!(needs_rehash(&old));
        // A kept string asking for more than any house set: refused, not run.
        let greedy = kept.replacen(&format!("m={}", ARGON2.0), "m=4000000", 1);
        assert!(!password_matches(&greedy, "une longue phrase de passe"));
    }

    #[test]
    fn easy_passwords_are_refused() {
        let easy = |pw: &str| weak(pw, "Élodie", "elodie").is_some();
        assert!(easy("court"), "too short");
        assert!(easy("1q2w3e4r5t6y7u8i9o0p"), "among the most common");
        assert!(easy("aaaaaaaaaaaaaaaaaaaa"), "one letter over and over");
        assert!(easy("elodie2026elodie!"), "her name and a year");
        assert!(easy("MoliMaison123456789"), "the house's name");
        assert!(!easy("cerf-volant sous la pluie"));
        assert!(
            !easy("Élodie aime les cerfs-volants"),
            "a name inside a phrase"
        );
    }

    #[test]
    fn a_device_given_to_a_guest_is_its_own() {
        let hub = hub();
        let guest = PersonRef {
            id: "sam".into(),
            name: "Sam".into(),
            role: Role::Guest,
            rooms: vec!["Chambre d'amis".into()],
            devices: vec!["hue:lampe-salon".into()],
        };
        let hidden = HashSet::new();
        assert!(guest.in_rooms(&hub, &hidden, &DeviceId::from("hue:lampe-salon")));
        assert!(!guest.in_rooms(&hub, &hidden, &DeviceId::from("hue:autre")));
        // Never a device that says where someone is, even given.
        let whereabouts: HashSet<InstanceId> =
            [DeviceId::from("hue:lampe-salon").instance()].into();
        assert!(!guest.in_rooms(&hub, &whereabouts, &DeviceId::from("hue:lampe-salon")));
    }

    #[tokio::test]
    async fn a_person_sees_and_ends_their_sessions() {
        let hub = hub_with_store();
        let guard = Sessions::new(hub.clone(), None, &[]);
        let p = People::open(hub.clone(), None, &[]);
        let sam = p.create(draft("Sam", Role::Member)).await.unwrap();
        let kept = pbkdf2_kept("une longue phrase de passe");
        hub.store_core_secret(&password_key(&sam.id), &kept)
            .await
            .unwrap();
        let mut phone = HeaderMap::new();
        phone.insert(
            axum::http::header::USER_AGENT,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0) Version/18.0 Mobile Safari/604.1"
                .parse()
                .unwrap(),
        );
        let (first, _) = p
            .login(
                &guard,
                "sam",
                "une longue phrase de passe",
                &stranger("k"),
                &phone,
            )
            .await
            .unwrap();
        // The older password, kept again the new way at the sign-in.
        let again = hub.core_secret(&password_key(&sam.id)).unwrap();
        assert!(again.starts_with("argon2id$"), "{again}");
        assert!(password_matches(&again, "une longue phrase de passe"));
        let (second, _) = p
            .login(
                &guard,
                "sam",
                "une longue phrase de passe",
                &stranger("k"),
                &HeaderMap::new(),
            )
            .await
            .unwrap();
        let mut on_phone = HeaderMap::new();
        on_phone.insert(COOKIE, format!("{COOKIE_NAME}={first}").parse().unwrap());
        let list = p.sessions_of(&sam.id, &on_phone);
        assert_eq!(list.len(), 2);
        let phone_session = list.iter().find(|s| s.current).unwrap();
        assert_eq!(phone_session.device, "iPhone · Safari");
        assert!(phone_session.remote);
        // The other one ends; the phone stays signed in.
        let other = list.iter().find(|s| !s.current).unwrap().id.clone();
        assert!(
            !p.end_session("someone-else", &other).await,
            "only one's own"
        );
        assert!(p.end_session(&sam.id, &other).await);
        let mut on_computer = HeaderMap::new();
        on_computer.insert(COOKIE, format!("{COOKIE_NAME}={second}").parse().unwrap());
        assert!(p.signed_in(&on_computer).is_none());
        assert_eq!(p.signed_in(&on_phone).unwrap().id, sam.id);
        // An unknown name: refused like a wrong password.
        assert_eq!(
            p.login(
                &guard,
                "personne",
                "une longue phrase de passe",
                &stranger("k4"),
                &HeaderMap::new()
            )
            .await
            .err(),
            Some(LoginError::Wrong)
        );
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
        let short = p
            .redeem(&guard, &typed, "court", &stranger("k"), &HeaderMap::new())
            .await;
        assert!(
            matches!(short, Err(Err(Refused::Invalid(_)))),
            "password too short"
        );
        let (token, who) = p
            .redeem(
                &guard,
                &typed,
                "une longue phrase de passe",
                &stranger("k"),
                &HeaderMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(who.id, "elodie");
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, format!("{COOKIE_NAME}={token}").parse().unwrap());
        assert_eq!(p.signed_in(&headers).unwrap().id, "elodie");
        assert!(
            p.redeem(
                &guard,
                &code,
                "une longue phrase de passe",
                &stranger("k"),
                &HeaderMap::new()
            )
            .await
            .is_err(),
            "once"
        );
        let (_, again) = p
            .login(
                &guard,
                "ELODIE",
                "une longue phrase de passe",
                &stranger("k2"),
                &HeaderMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(again.role, Role::Member);
        assert_eq!(
            p.login(
                &guard,
                "elodie",
                "faux mot de passe",
                &stranger("k3"),
                &HeaderMap::new()
            )
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
        p.set_password(&g.id, "une longue phrase de passe")
            .await
            .unwrap_err();
        assert!(p.get(&g.id).is_none());
        assert!(
            p.login(
                &guard,
                &g.id,
                "une longue phrase de passe",
                &stranger("k"),
                &HeaderMap::new()
            )
            .await
            .is_err()
        );
    }

    /// A data folder of its own.
    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moli-people-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn an_invitation_code_cannot_be_tried_against_the_file() {
        let dir = folder("hmac");
        let hub = hub_with_store();
        let guard = Sessions::new(hub.clone(), Some(crate::UiPin::new("123456")), &[]);
        let p = People::open(hub.clone(), Some(&dir), &[]);
        let sam = p.create(draft("Sam", Role::Member)).await.unwrap();
        let (code, _) = p.invite(&sam.id).await.unwrap();
        let kept = std::fs::read_to_string(dir.join(FILE)).unwrap();
        // Neither the code nor its bare digest: the key is in the store.
        assert!(!kept.contains(&plain_code(&code)));
        assert!(!kept.contains(&hash(&plain_code(&code))), "{kept}");
        assert!(kept.contains("code_hmac"));
        assert!(hub.core_secret(INVITE_KEY).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "for Moli's user alone");
        }
        // The code itself works, once.
        let long = "une longue phrase de passe";
        assert!(
            p.redeem(&guard, &code, long, &stranger("k"), &HeaderMap::new())
                .await
                .is_ok()
        );
        assert!(
            p.redeem(&guard, &code, long, &stranger("k"), &HeaderMap::new())
                .await
                .is_err()
        );
        // No store, no key: no invitation either.
        let bare = People::open(self::hub(), None, &[]);
        let alone = bare.create(draft("Seul", Role::Member)).await.unwrap();
        assert_eq!(bare.invite(&alone.id).await.err(), Some(Refused::Store));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_invitation_of_an_older_kind_opens_nothing() {
        let dir = folder("old-invite");
        let hub = hub_with_store();
        let guard = Sessions::new(hub.clone(), Some(crate::UiPin::new("123456")), &[]);
        // As an older version kept it: the code's bare SHA-256.
        let old = serde_json::json!({
            "people": [{ "id": "sam", "name": "Sam", "role": "member", "created": 1 }],
            "invites": [{
                "code_sha256": hash("ABCDEFGH"),
                "person": "sam",
                "expires": now_ms() + 3_600_000,
            }],
        });
        std::fs::write(dir.join(FILE), old.to_string()).unwrap();
        let p = People::open(hub, Some(&dir), &[]);
        assert!(p.get("sam").is_some(), "the people are still known");
        assert_eq!(
            p.redeem(
                &guard,
                "ABCD-EFGH",
                "une longue phrase de passe",
                &stranger("k"),
                &HeaderMap::new()
            )
            .await
            .err(),
            Some(Ok(LoginError::Wrong)),
            "never taken unverified"
        );
        let kept = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!kept.contains("code_sha256"), "{kept}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

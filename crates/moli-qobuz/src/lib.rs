//! Music from a Qobuz subscription, through Qobuz's own (unofficial) web API
//! — like the Alexa driver, Qobuz publishes nothing official for this: it
//! may change without notice.
//!
//! The instance is the house's music service ([`DriverCtx::set_music`]): a
//! `play` order on a speaker (Cast, Sonos…) sends it the search phrase and
//! gets back a queue of tracks with their stream addresses.
//!
//! Secrets (the encrypted store, read at start; restart after setting):
//! `app_id` and `app_secret` — the pair the Qobuz web player signs its calls
//! with — plus `email` and `password` of the subscription (the login gives
//! the `user_auth_token`, kept in memory only).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::bail;
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use md5::{Digest as _, Md5};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_net::Body as Bytes;
use moli_runtime::music::{MusicService, Track};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;

const HOST: &str = "www.qobuz.com";
const API: &str = "/api.json/0.2/";
const USER_AGENT: &str = "MoliOS/0.1";
const HTTP_LIMIT: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 4 * 1024 * 1024;

const APP_ID: &str = "app_id";
const APP_SECRET: &str = "app_secret";
const EMAIL: &str = "email";
const PASSWORD: &str = "password";
const TOKEN: &str = "user_auth_token";

fn default_format() -> u32 {
    6
}

fn default_queue() -> usize {
    5
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Stream quality, Qobuz's `format_id`: `5` MP3 320 kb/s, `6` FLAC
    /// 16-bit 44.1 kHz (CD, the default), `7`/`27` hi-res 24-bit.
    #[serde(default = "default_format")]
    pub format_id: u32,
    /// How many tracks a `play` queues at most.
    #[serde(default = "default_queue")]
    pub queue: usize,
}

#[derive(Debug)]
pub struct Qobuz {
    config: Config,
}

impl Qobuz {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Qobuz {
    fn kind(&self) -> &'static str {
        "qobuz"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The account, shown in the house so its state is visible.
fn account_device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.qobuz.compte").into(),
        manufacturer: Some("Qobuz".into()),
        model: Some("Qobuz".into()),
        description: Some(moli_i18n::tr!("pilotes.qobuz.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![PointSpec {
            key: "status".into(),
            label: moli_i18n::tr!("pilotes.qobuz.etat_compte").into(),
            kind: Kind::Text,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Config,
        }],
    }
}

fn set_status(ctx: &DriverCtx, id: &DeviceId, status: &str) {
    ctx.set_state(id, "status", Value::Text(status.into()));
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let account = ctx.device_id("account");
    ctx.upsert_device(account_device(ctx, &account));
    ctx.set_availability(&account, true);
    // `app_id` + `app_secret` are always needed, plus either a ready-made
    // `user_auth_token` (read in the player's devtools) or the
    // `email` + `password` a login gives it from.
    let mut missing: Vec<&str> = [APP_ID, APP_SECRET]
        .iter()
        .filter(|key| ctx.secret(key).is_none_or(|v| v.trim().is_empty()))
        .copied()
        .collect();
    let has_token = ctx.secret(TOKEN).is_some_and(|v| !v.trim().is_empty());
    if !has_token {
        for key in [EMAIL, PASSWORD] {
            if ctx.secret(key).is_none_or(|v| v.trim().is_empty()) {
                missing.push(key);
            }
        }
    }
    if !missing.is_empty() {
        set_status(ctx, &account, "missing");
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.qobuz.secrets_manquants",
            instance = ctx.instance().to_string(),
            keys = missing.join(", ")
        ));
        // Secrets are read at start only: parked until Moli restarts.
        ctx.cancelled().await;
        return Ok(());
    }
    let client = Client {
        app_id: ctx.secret(APP_ID).unwrap_or_default(),
        secret: ctx.secret(APP_SECRET).unwrap_or_default(),
        email: ctx.secret(EMAIL).unwrap_or_default(),
        password: ctx.secret(PASSWORD).unwrap_or_default(),
        format_id: config.format_id,
        queue: config.queue,
        token: Mutex::new(ctx.secret(TOKEN).filter(|t| !t.trim().is_empty())),
    };
    set_status(ctx, &account, "connecting");
    // A stored token goes straight in (a search still re-logins on 401 if
    // email/password are there); otherwise the login proves itself now.
    let usable = if client.token().is_empty() {
        match client.login().await {
            Ok(label) => {
                tracing::info!(instance = %ctx.instance(), label, "qobuz connected");
                true
            }
            Err(e) => {
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "qobuz login failed");
                set_status(ctx, &account, "error");
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.qobuz.connexion_refusee",
                    why = format!("{e:#}")
                ));
                false
            }
        }
    } else {
        tracing::info!(instance = %ctx.instance(), "qobuz token ready");
        true
    };
    if usable {
        set_status(ctx, &account, "connected");
        ctx.set_music(Arc::new(client));
        ctx.ready();
    }
    // No polling: the service answers at each `play`. Commands can only be
    // stale or stray (nothing writable): refused. `None` once Moli stops.
    while let Some(command) = ctx.next_command().await {
        command.reply(Err(moli_i18n::tr!("pilotes.qobuz.lecture_seule")));
    }
    Ok(())
}

/// A search hit before its stream address is known.
#[derive(Clone, Debug)]
struct Hit {
    id: u64,
    title: String,
    artist: String,
    image: Option<String>,
}

/// The subscription's session with the web API.
#[derive(Debug)]
struct Client {
    app_id: String,
    secret: String,
    email: String,
    password: String,
    format_id: u32,
    queue: usize,
    token: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Client {
    fn token(&self) -> String {
        lock(&self.token).clone().unwrap_or_default()
    }

    /// `GET /api.json/0.2/<path>` with the app's headers; the answer's JSON.
    async fn call(&self, path: &str, auth: bool) -> anyhow::Result<(StatusCode, Json)> {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(format!("{API}{path}"))
            .header("user-agent", USER_AGENT)
            .header("x-app-id", &self.app_id)
            .header("accept", "application/json");
        if auth {
            builder = builder.header("x-user-auth-token", self.token());
        }
        let req = builder.body(Full::new(Bytes::new()))?;
        let (status, bytes) = moli_net::web(HOST, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
        Ok((status, serde_json::from_slice(&bytes).unwrap_or(Json::Null)))
    }

    /// `user/login`: the subscription's `user_auth_token`, and the plan's
    /// label for the log (« Studio », « Sublime »…).
    async fn login(&self) -> anyhow::Result<String> {
        let path = format!(
            "user/login?app_id={}&email={}&password={}",
            pct(&self.app_id),
            pct(&self.email),
            md5(&self.password)
        );
        let (status, body) = self.call(&path, false).await?;
        let token = body["user_auth_token"].as_str().unwrap_or_default();
        if !status.is_success() || token.is_empty() {
            let why = body["message"].as_str().unwrap_or("login refused");
            bail!("qobuz user/login: HTTP {} {why}", status.as_u16());
        }
        let label = body["user"]["credential"]["parameters"]["short_label"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        *lock(&self.token) = Some(token.to_owned());
        Ok(label)
    }

    /// The phrase turned into streamable tracks, first first. `album`
    /// searches take the best album's own tracks; anything else, the best
    /// matching tracks.
    async fn tracks(&self, phrase: &str) -> anyhow::Result<Vec<Track>> {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            bail!(moli_i18n::tr!("pilotes.qobuz.recherche_vide"));
        }
        let mut hits = self.search(phrase).await?;
        if hits.is_empty() {
            bail!(moli_i18n::tr!("pilotes.qobuz.rien_trouve", phrase = phrase));
        }
        hits.truncate(self.queue);
        let urls = futures::future::join_all(hits.iter().map(|hit| self.stream_url(hit.id))).await;
        let tracks: Vec<Track> = hits
            .iter()
            .zip(urls)
            .filter_map(|(hit, url)| match url {
                Ok((url, mime)) => Some(Track {
                    id: hit.id,
                    title: hit.title.clone(),
                    artist: hit.artist.clone(),
                    image: hit.image.clone(),
                    url,
                    mime,
                }),
                Err(e) => {
                    tracing::debug!(track = %hit.title, error = %format!("{e:#}"), "qobuz track not streamable");
                    None
                }
            })
            .collect();
        if tracks.is_empty() {
            bail!(moli_i18n::tr!("pilotes.qobuz.rien_trouve", phrase = phrase));
        }
        Ok(tracks)
    }

    /// The tracks a phrase best matches: the album's own tracks when the
    /// phrase names an album, the search's best tracks otherwise.
    async fn search(&self, phrase: &str) -> anyhow::Result<Vec<Hit>> {
        if says_album(phrase)
            && let Ok(album_hits) = self.album_tracks(phrase).await
            && !album_hits.is_empty()
        {
            return Ok(album_hits);
        }
        let path = format!("track/search?query={}&limit={}", pct(phrase), self.queue);
        let (status, body) = self.call(&path, true).await?;
        if status == StatusCode::UNAUTHORIZED {
            self.login().await?;
            let (status, body) = self.call(&path, true).await?;
            return hits(&body, status);
        }
        hits(&body, status)
    }

    /// The best matching album's tracks (`album/search` then `album/get`).
    async fn album_tracks(&self, phrase: &str) -> anyhow::Result<Vec<Hit>> {
        let path = format!("album/search?query={}&limit=1", pct(phrase));
        let (status, body) = self.call(&path, true).await?;
        let album = body["items"]
            .as_array()
            .or_else(|| body["albums"]["items"].as_array())
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or_default();
        let id = album["id"].as_str().unwrap_or_default();
        if !status.is_success() || id.is_empty() {
            return Ok(Vec::new());
        }
        let (status, body) = self.call(&format!("album/get?album_id={id}"), true).await?;
        if !status.is_success() {
            return Ok(Vec::new());
        }
        let artist = album["artist"]["name"]
            .as_str()
            .or_else(|| body["artist"]["name"].as_str())
            .unwrap_or_default();
        let image = album["image"]["large"].as_str().map(str::to_owned);
        Ok(body["tracks"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                t["streamable"].as_bool().unwrap_or(true).then_some(())?;
                Some(Hit {
                    id: t["id"].as_u64()?,
                    title: t["title"].as_str()?.to_owned(),
                    artist: t["performer"]["name"].as_str().unwrap_or(artist).to_owned(),
                    image: image.clone(),
                })
            })
            .collect())
    }

    /// `track/getFileUrl`: the signed stream address of one track, in the
    /// configured quality. A refused signature (rotated `app_secret`) or an
    /// expired token shows here.
    async fn stream_url(&self, track_id: u64) -> anyhow::Result<(String, String)> {
        match self.stream_url_once(track_id).await {
            Err(e) if format!("{e:#}").contains("401") => {
                self.login().await?;
                self.stream_url_once(track_id).await
            }
            other => other,
        }
    }

    async fn stream_url_once(&self, track_id: u64) -> anyhow::Result<(String, String)> {
        let ts = moli_core::now_ms() / 1000;
        let sig = md5(&format!(
            "trackgetFileUrlformat_id{}intentstreamtrack_id{track_id}{ts}{}",
            self.format_id, self.secret
        ));
        let path = format!(
            "track/getFileUrl?track_id={track_id}&format_id={}&intent=stream&request_ts={ts}&request_sig={sig}",
            self.format_id
        );
        let (status, body) = self.call(&path, true).await?;
        let url = body["url"].as_str().unwrap_or_default();
        if !status.is_success() || url.is_empty() {
            let why = body["message"].as_str().unwrap_or("stream refused");
            bail!("qobuz track/getFileUrl: HTTP {} {why}", status.as_u16());
        }
        let mime = body["mime_type"]
            .as_str()
            .unwrap_or("audio/mpeg")
            .to_owned();
        Ok((url.to_owned(), mime))
    }
}

impl MusicService for Client {
    fn resolve(&self, phrase: &str) -> BoxFuture<'_, Result<Vec<Track>, String>> {
        let phrase = phrase.to_owned();
        Box::pin(async move { self.tracks(&phrase).await.map_err(|e| format!("{e:#}")) })
    }
}

/// A search answer's tracks, flattened.
fn hits(body: &Json, status: StatusCode) -> anyhow::Result<Vec<Hit>> {
    if !status.is_success() {
        bail!("qobuz search: HTTP {}", status.as_u16());
    }
    let items = body["items"]
        .as_array()
        .or_else(|| body["tracks"]["items"].as_array());
    Ok(items
        .into_iter()
        .flatten()
        .filter_map(|t| {
            t["streamable"].as_bool().unwrap_or(true).then_some(())?;
            Some(Hit {
                id: t["id"].as_u64()?,
                title: t["title"].as_str()?.to_owned(),
                artist: t["performer"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                image: t["album"]["image"]["large"].as_str().map(str::to_owned),
            })
        })
        .collect())
}

/// The phrase names an album (« l'album Thriller », « mets l'album de… »).
fn says_album(phrase: &str) -> bool {
    let lower = phrase.to_lowercase();
    ["album", "lp", "disque"]
        .iter()
        .any(|w| lower.split(|c: char| !c.is_alphanumeric()).any(|p| p == *w))
}

/// Lowercase hexadecimal.
fn md5(text: &str) -> String {
    Md5::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(32), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Percent-encodes a query value.
fn pct(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_md5_of_the_signed_params() {
        // The signed string Qobuz's player hashes, verbatim.
        let signed = "trackgetFileUrlformat_id6intentstreamtrack_id59667831712345678secret";
        assert_eq!(md5(signed), md5(signed).to_lowercase());
        assert_eq!(md5(signed).len(), 32);
    }

    #[test]
    fn a_track_search_gives_queueable_hits() {
        let body = serde_json::json!({
            "items": [
                { "id": 1, "title": "One", "streamable": true,
                  "performer": {"name": "Artist"},
                  "album": {"image": {"large": "https://i/cover.jpg"}} },
                { "id": 2, "title": "Two", "streamable": false },
                { "id": 3, "title": "Three" }
            ]
        });
        let found = hits(&body, StatusCode::OK).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].artist, "Artist");
        assert_eq!(found[0].image.as_deref(), Some("https://i/cover.jpg"));
        assert_eq!(found[1].artist, "");
    }

    #[test]
    fn album_words_switch_the_search() {
        assert!(says_album("mets l'album Thriller"));
        assert!(says_album("un album de jazz"));
        assert!(!says_album("du jazz"));
        assert!(!says_album("albumsous"));
    }

    #[test]
    fn queries_are_percent_encoded() {
        assert_eq!(pct("a@b.c d"), "a%40b.c%20d");
        assert_eq!(pct("étage"), "%C3%A9tage");
    }

    #[test]
    fn config_defaults() {
        let config: Config = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(config.format_id, 6);
        assert_eq!(config.queue, 5);
        assert!(serde_json::from_value::<Config>(serde_json::json!({"x": 1})).is_err());
    }
}

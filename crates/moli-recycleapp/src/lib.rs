//! A Belgian address's waste collection calendar, from RecycleApp (Fost
//! Plus): its public web API, the one recycleapp.be uses (no account).
//!
//! One device, « Collectes de déchets »: per waste type, the next collection
//! (`residual`, date `2026-11-13`) and the days until it (`residual_days`), and
//! what goes out today and tomorrow (`today`, `tomorrow`: the names, or empty),
//! for an automation that says the evening before what to put out.
//!
//! The calendar is read again every 6 hours (8 weeks ahead); the days are
//! counted again every 15 minutes, in the address's zone.

mod model;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context as _;
use http::header::{ACCEPT, CONTENT_TYPE, HOST, USER_AGENT};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;

const SETTINGS: &str = "https://www.recycleapp.be/config/app.settings.json";
/// The API's address when the settings cannot be read.
const DEFAULT_API: &str = "https://api.fostplus.be/recyclecms";
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 1024 * 1024;
const READ_EVERY: Duration = Duration::from_secs(6 * 3600);
const COUNT_EVERY: Duration = Duration::from_secs(15 * 60);
const RETRY: Duration = Duration::from_secs(15 * 60);
/// Failed reads in a row, or a calendar older than this: it is not shown
/// as if it were current.
const STALE_AFTER_FAILURES: u32 = 3;
const STALE_AFTER: Duration = Duration::from_secs(24 * 3600);
/// The only hosts the API may be on, whatever the settings file says.
const API_DOMAINS: [&str; 2] = ["recycleapp.be", "fostplus.be"];
const LANGUAGES: [&str; 4] = ["fr", "nl", "en", "de"];
/// How far ahead the calendar is read.
const AHEAD_DAYS: i32 = 56;
/// Pages of 100 collections at most (8 weeks hold far fewer).
const MAX_PAGES: u64 = 10;

fn default_language() -> String {
    "fr".into()
}

fn default_timezone() -> String {
    "Europe/Brussels".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The postal code (`1600`).
    pub zipcode: String,
    /// The street, in any of its languages.
    pub street: String,
    pub house_number: u32,
    /// The language of the waste types' names: `fr`, `nl`, `en`, `de`.
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

#[derive(Debug)]
pub struct RecycleApp {
    config: Config,
}

impl RecycleApp {
    /// # Errors
    /// An unknown zone, a postal code that is not 4 digits, an empty street.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        TimeZone::get(&config.timezone)
            .map_err(|e| anyhow::anyhow!("timezone {:?}: {e}", config.timezone))?;
        if config.zipcode.len() != 4 || !config.zipcode.bytes().all(|b| b.is_ascii_digit()) {
            anyhow::bail!(
                "zipcode {:?}: a Belgian postal code, 4 digits",
                config.zipcode
            );
        }
        if config.street.trim().is_empty() || config.house_number == 0 {
            anyhow::bail!("street and house_number are needed");
        }
        if !LANGUAGES.contains(&config.language.as_str()) {
            anyhow::bail!("language {:?}: one of fr, nl, en, de", config.language);
        }
        Ok(Self { config })
    }
}

impl Driver for RecycleApp {
    fn kind(&self) -> &'static str {
        "recycleapp"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The API: its host, and the path its calls start with.
struct Api {
    url: moli_net::WebUrl,
}

impl Api {
    async fn found() -> Self {
        let from_settings = async {
            let url = moli_net::WebUrl::parse(SETTINGS)?;
            let json = call(&url, http::Method::GET, &url.path).await?;
            let api = json["API"].as_str().context("no API in the settings")?;
            moli_net::WebUrl::parse(api)
        };
        let url = match from_settings.await {
            Ok(url) if trusted(&url) => url,
            Ok(_) | Err(_) => {
                moli_net::WebUrl::parse(DEFAULT_API).expect("the default API address parses")
            }
        };
        Self { url }
    }
}

/// The address goes out only over TLS, and only to Fost Plus's own domains.
fn trusted(url: &moli_net::WebUrl) -> bool {
    let host = url.host.to_ascii_lowercase();
    url.tls
        && API_DOMAINS
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

impl Api {
    async fn get(&self, query: &str) -> anyhow::Result<Json> {
        self.call(http::Method::GET, query).await
    }

    async fn call(&self, method: http::Method, query: &str) -> anyhow::Result<Json> {
        let path = format!("{}/public/v1/{query}", self.url.path.trim_end_matches('/'));
        call(&self.url, method, &path).await
    }
}

async fn call(url: &moli_net::WebUrl, method: http::Method, path: &str) -> anyhow::Result<Json> {
    let mut request = http::Request::builder();
    if method == http::Method::POST {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    let request = request
        .method(method)
        .uri(path)
        .header(HOST, url.host.as_str())
        .header(USER_AGENT, "moli-os")
        .header(ACCEPT, "application/json")
        .header("x-consumer", "recycleapp.be")
        .body(http_body_util::Full::new(moli_net::Body::new()))?;
    let (status, body) =
        moli_net::web(&url.host, url.port, url.tls, request, LIMIT, MAX_BODY).await?;
    anyhow::ensure!(
        status.is_success(),
        "{}: HTTP {}",
        url.host,
        status.as_u16()
    );
    Ok(serde_json::from_slice(&body)?)
}

fn percent(text: &str) -> String {
    text.bytes().fold(String::new(), |mut s, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            s.push(char::from(b));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("%{b:02X}"));
        }
        s
    })
}

/// The address's ids: postal code, then street. A postal code several
/// municipalities share: the one where the street is.
async fn address(api: &Api, config: &Config) -> anyhow::Result<(String, String)> {
    let zips = api.get(&format!("zipcodes?q={}", config.zipcode)).await?;
    let zips = model::zipcodes(&zips, &config.zipcode);
    anyhow::ensure!(
        !zips.is_empty(),
        moli_i18n::tr!("pilotes.recycleapp.code_postal", code = config.zipcode)
    );
    for zip in zips {
        let query = format!(
            "streets?q={}&zipcodes={}",
            percent(config.street.trim()),
            percent(&zip)
        );
        let streets = api.call(http::Method::POST, &query).await?;
        if let Some(street) = model::street(&streets, &config.street) {
            return Ok((zip, street));
        }
    }
    anyhow::bail!(moli_i18n::tr!(
        "pilotes.recycleapp.rue",
        rue = config.street
    ))
}

/// Every waste type's dates from `today`, 8 weeks ahead.
async fn calendar(
    api: &Api,
    config: &Config,
    zip: &str,
    street: &str,
    today: Date,
) -> anyhow::Result<BTreeMap<&'static str, model::Kind>> {
    let until = today
        .checked_add(jiff::Span::new().days(AHEAD_DAYS))
        .unwrap_or(today);
    let mut kinds = BTreeMap::new();
    let mut page = 1;
    loop {
        let query = format!(
            "collections?zipcodeId={}&streetId={}&houseNumber={}&fromDate={today}&untilDate={until}&size=100&page={page}",
            percent(zip),
            percent(street),
            config.house_number
        );
        let pages = model::add_collections(&api.get(&query).await?, &config.language, &mut kinds);
        if page >= pages.min(MAX_PAGES) {
            break;
        }
        page += 1;
    }
    Ok(kinds)
}

fn device(ctx: &DriverCtx, id: &DeviceId, kinds: &BTreeMap<&'static str, model::Kind>) -> Device {
    let mut points = vec![
        text("today", &moli_i18n::tr!("pilotes.recycleapp.aujourdhui")),
        text("tomorrow", &moli_i18n::tr!("pilotes.recycleapp.demain")),
    ];
    for (key, kind) in kinds {
        points.push(text(key, &kind.name));
        points.push(PointSpec {
            key: format!("{key}_days").into(),
            label: moli_i18n::tr!("pilotes.recycleapp.dans_jours", nom = kind.name).into(),
            semantic: Semantic::Other,
            kind: Kind::Numeric {
                min: None,
                max: None,
                step: Some(1.0),
            },
            access: Access {
                read: true,
                write: false,
            },
            unit: Some(moli_core::Unit::Other("j".into())),
        });
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.recycleapp.nom").into(),
        manufacturer: Some("Fost Plus".into()),
        model: Some("RecycleApp".into()),
        description: Some(moli_i18n::tr!("pilotes.recycleapp.description").into()),
        native_room: None,
        members: Vec::new(),
        points,
    }
}

fn text(key: &str, label: &str) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::Other,
        kind: Kind::Text,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
    }
}

/// The points' values for `today`.
fn publish(
    ctx: &DriverCtx,
    id: &DeviceId,
    kinds: &BTreeMap<&'static str, model::Kind>,
    today: Date,
) {
    let tomorrow = today.tomorrow().unwrap_or(today);
    let on = |day: Date| {
        kinds
            .values()
            .filter(|k| k.dates.contains(&day))
            .map(|k| k.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    ctx.set_state(id, "today", Value::Text(on(today).into()));
    ctx.set_state(id, "tomorrow", Value::Text(on(tomorrow).into()));
    for (key, kind) in kinds {
        let next = model::next(kind, today);
        ctx.set_state(
            id,
            key,
            next.map_or(Value::Null, |d| Value::Text(d.to_string().into())),
        );
        let days = next.map(|d| (d - today).get_days());
        ctx.set_state(
            id,
            &format!("{key}_days"),
            days.map_or(Value::Null, |d| Value::Int(i64::from(d))),
        );
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let tz = TimeZone::get(&config.timezone)?;
    let today = || Timestamp::now().to_zoned(tz.clone()).date();
    let id = ctx.device_id("collections");
    let api = Api::found().await;
    let mut ids: Option<(String, String)> = None;
    let mut kinds: BTreeMap<&'static str, model::Kind> = BTreeMap::new();
    let mut known_keys: Vec<&'static str> = Vec::new();
    let mut last_error: Option<String> = None;
    let mut read_at: Option<tokio::time::Instant> = None;
    let mut failures: u32 = 0;
    let mut stale = false;
    loop {
        let due = read_at.is_none_or(|at| at.elapsed() >= READ_EVERY);
        if due {
            let read = async {
                if ids.is_none() {
                    ids = Some(address(&api, config).await?);
                }
                let (zip, street) = ids.as_ref().expect("just found");
                calendar(&api, config, zip, street, today()).await
            };
            match read.await {
                Ok(found) => {
                    kinds = found;
                    read_at = Some(tokio::time::Instant::now());
                    last_error = None;
                    failures = 0;
                    if stale {
                        stale = false;
                        ctx.clear_warning();
                    }
                    let keys: Vec<&'static str> = kinds.keys().copied().collect();
                    if keys != known_keys {
                        ctx.upsert_device(device(ctx, &id, &kinds));
                        known_keys = keys;
                    }
                    if kinds.is_empty() {
                        // The address is known, but no collection is: say so
                        // rather than look as if all were well.
                        ctx.wait_for(moli_i18n::tr!("pilotes.recycleapp.aucune_collecte"));
                    } else {
                        ctx.set_availability(&id, true);
                        ctx.ready();
                    }
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    let e = format!("{e:#}");
                    if last_error.as_deref() != Some(e.as_str()) {
                        tracing::warn!(instance = %ctx.instance(), error = %e, "recycleapp: calendar not read");
                        last_error = Some(e);
                    }
                    if known_keys.is_empty() {
                        // Nothing to show yet: say why, try again later.
                        ctx.wait_for(last_error.clone().unwrap_or_default());
                    } else if !stale
                        && (failures >= STALE_AFTER_FAILURES
                            || read_at.is_some_and(|at| at.elapsed() >= STALE_AFTER))
                    {
                        // The dates kept would go blank one by one: the
                        // calendar is no longer current, and it is said.
                        stale = true;
                        ctx.set_availability(&id, false);
                        ctx.warn(moli_i18n::tr!("pilotes.recycleapp.perime"));
                    }
                }
            }
        }
        if !known_keys.is_empty() {
            publish(ctx, &id, &kinds, today());
        }
        let pause = if read_at.is_some() && failures == 0 {
            COUNT_EVERY
        } else {
            RETRY
        };
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            command = ctx.next_command() => {
                if let Some(command) = command {
                    command.reply(Err(moli_i18n::tr!("pilotes.recycleapp.lecture_seule")));
                } else {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(zipcode: &str, street: &str, house_number: u32) -> Config {
        Config {
            zipcode: zipcode.into(),
            street: street.into(),
            house_number,
            language: "fr".into(),
            timezone: "Europe/Brussels".into(),
        }
    }

    #[test]
    fn the_address_is_checked() {
        assert!(RecycleApp::new(config("1000", "Rue Exemple", 1)).is_ok());
        assert!(RecycleApp::new(config("100", "Rue Exemple", 1)).is_err());
        assert!(RecycleApp::new(config("1000", " ", 1)).is_err());
        assert!(RecycleApp::new(config("1000", "Rue Exemple", 0)).is_err());
    }

    #[test]
    fn the_api_is_only_fost_plus() {
        let ok = |u: &str| trusted(&moli_net::WebUrl::parse(u).unwrap());
        assert!(ok(DEFAULT_API));
        assert!(ok("https://api.recycleapp.be/recyclecms"));
        assert!(ok("https://recycleapp.be"));
        assert!(!ok("http://api.fostplus.be/recyclecms"), "TLS only");
        assert!(!ok("https://api.example.org/recyclecms"));
        assert!(!ok("https://fostplus.be.example.org"));
        assert!(!ok("https://evilfostplus.be"));
    }

    #[test]
    fn the_language_is_checked() {
        let mut c = config("1000", "Rue Exemple", 1);
        c.language = "nl".into();
        assert!(RecycleApp::new(c.clone()).is_ok());
        c.language = "xx".into();
        assert!(RecycleApp::new(c).is_err());
    }

    #[test]
    fn queries_are_encoded() {
        assert_eq!(percent("Rue de l'Église"), "Rue%20de%20l%27%C3%89glise");
        assert_eq!(
            percent("https://data.vlaanderen.be/id/straatnaam-1"),
            "https%3A%2F%2Fdata.vlaanderen.be%2Fid%2Fstraatnaam-1"
        );
    }
}

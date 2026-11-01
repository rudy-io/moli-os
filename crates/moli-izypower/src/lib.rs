//! IzyPower micro-inverters, through the IzyPower cloud: they keep no port
//! open on the home network and report only there. Read-only: the driver
//! logs in with the account's e-mail and password (HTTPS only, from Moli's
//! encrypted store), then reads the station and its micro-inverters every
//! three minutes (the cloud is no fresher). Nothing is ever changed on the
//! account.

mod model;

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::Full;
use moli_core::DeviceId;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};

pub use model::{Inverter, Station};

const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const HOST: &str = "application.izypowercloud.fr";
const BASE: &str = "/photo_voltaic/api";
const POLL: Duration = Duration::from_secs(3 * 60);
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Credentials refused: the next try waits this long (no storm of logins).
const REFUSED_WAIT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The station to read, when the account has several (its number in
    /// the cloud); by default the first.
    #[serde(default)]
    pub station: Option<u64>,
}

#[derive(Debug)]
pub struct Izypower {
    config: Config,
}

impl Izypower {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Izypower {
    fn kind(&self) -> &'static str {
        "izypower"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// The cloud refused the e-mail or the password.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

struct Cloud {
    username: String,
    password: String,
    token: Option<(String, i64)>,
}

impl std::fmt::Debug for Cloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cloud").finish_non_exhaustive()
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

async fn send(
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<&Json>,
) -> anyhow::Result<(u16, Json)> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("{BASE}{path}"))
        .header("accept", "application/json")
        .header("accept-language", "fr")
        .header("app-platform", "izy");
    if let Some(token) = token {
        builder = builder.header("x-tts-access-token", token);
    }
    let payload = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            moli_net::Body::from(body.to_string())
        }
        None => moli_net::Body::new(),
    };
    // HTTPS only: the password never travels in the clear.
    let (status, bytes) = moli_net::web(
        HOST,
        443,
        true,
        builder.body(Full::new(payload))?,
        LIMIT,
        MAX_BODY,
    )
    .await?;
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    Ok((status.as_u16(), json))
}

/// The answer's `code` when the cloud says the token is no good.
fn unauthorized(status: u16, answer: &Json) -> bool {
    status == 401 || matches!(answer["code"].as_i64(), Some(401 | 403))
}

impl Cloud {
    async fn login(&mut self) -> anyhow::Result<String> {
        let body = json!({ "username": self.username, "password": self.password });
        let (status, answer) = send(Method::POST, "/login", None, Some(&body)).await?;
        let Some(token) = answer["data"]["token"].as_str().filter(|t| !t.is_empty()) else {
            if (200..500).contains(&status) && status != 429 {
                let why = answer["msg"].as_str().unwrap_or("").to_owned();
                return Err(Refused(why).into());
            }
            bail!(moli_i18n::tr!("pilotes.izypower.reponse", status = status));
        };
        // Logged in again a minute before the token ends (an hour if unknown).
        let until = model::token_expiry(token).map_or(now() + 3600, |exp| exp - 60);
        self.token = Some((token.to_owned(), until));
        Ok(token.to_owned())
    }

    /// `data` of a read, logging in when needed (and once again on a 401).
    async fn get(&mut self, path: &str) -> anyhow::Result<Json> {
        for attempt in 0..2 {
            let token = match &self.token {
                Some((token, until)) if *until > now() && attempt == 0 => token.clone(),
                _ => self.login().await?,
            };
            let (status, answer) = send(Method::GET, path, Some(&token), None).await?;
            if unauthorized(status, &answer) {
                self.token = None;
                continue;
            }
            if !(200..300).contains(&status) || answer["code"].as_i64().is_some_and(|c| c != 200) {
                bail!(moli_i18n::tr!("pilotes.izypower.reponse", status = status));
            }
            return Ok(answer["data"].clone());
        }
        bail!(moli_i18n::tr!("pilotes.izypower.jeton_refuse"))
    }
}

/// Today in the station's time (Paris time: the cloud's `searchTime`),
/// approximated as UTC+1 (+2 in summer does not change the date between 01:00
/// and 23:00; the curve read at night is not used: the station is stale).
fn today(now: i64) -> String {
    let days = (now + 3600).div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// One read of everything.
async fn read(
    cloud: &mut Cloud,
    wanted: Option<u64>,
) -> anyhow::Result<(Station, Vec<Inverter>, HashMap<(String, u32), f64>)> {
    let page = cloud.get("/powerStations/page?page=1&limit=100").await?;
    let records = page["records"].as_array().cloned().unwrap_or_default();
    let id = records
        .iter()
        .filter_map(|r| r["stationsId"].as_u64())
        .find(|id| wanted.is_none_or(|w| w == *id))
        .context(moli_i18n::tr!("pilotes.izypower.pas_de_centrale"))?;
    let info = cloud.get(&format!("/v3/powerStations/info/{id}")).await?;
    let station = Station::read(&info).context(moli_i18n::tr!("pilotes.izypower.illisible"))?;
    let devices = cloud
        .get(&format!(
            "/device/page?powerId={id}&deviceType=all&page=1&limit=100"
        ))
        .await?;
    let inverters = model::inverters(&devices);
    let layout = cloud
        .get(&format!(
            "/report/layoutPower/{id}?searchTime={}&isV2=true",
            today(now())
        ))
        .await
        .unwrap_or(Json::Null);
    Ok((station, inverters, model::latest_inputs(&layout)))
}

/// Sleeps `wait`, refusing orders (there are none to take: read-only).
/// `false`: the driver is stopping.
async fn pause(ctx: &mut DriverCtx, wait: Duration) -> bool {
    let wake = tokio::time::sleep(wait);
    tokio::pin!(wake);
    loop {
        tokio::select! {
            () = &mut wake => return true,
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.izypower.lecture_seule"))),
                None => return false,
            },
        }
    }
}

async fn run(driver: &Izypower, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (Some(username), Some(password)) = (ctx.secret(USERNAME), ctx.secret(PASSWORD)) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.izypower.identifiants_manquants",
            instance = ctx.instance(),
            user = USERNAME,
            password = PASSWORD
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let mut cloud = Cloud {
        username,
        password,
        token: None,
    };
    let mut published: Vec<String> = Vec::new();
    let mut ids: Vec<DeviceId> = Vec::new();
    let mut failures = 0u32;
    loop {
        let wait = match read(&mut cloud, driver.config.station).await {
            Ok((station, inverters, inputs)) => {
                failures = 0;
                let mut natives: Vec<String> = inverters.iter().map(Inverter::native).collect();
                natives.push(station.native());
                if natives != published {
                    let devices =
                        model::devices(&station, &inverters, ctx.instance(), |n| ctx.device_id(n));
                    ids = devices.iter().map(|d| d.id.clone()).collect();
                    for device in devices {
                        ctx.upsert_device(device);
                    }
                    published = natives;
                    ctx.ready();
                }
                ctx.set_availability(&ctx.device_id(&station.native()), true);
                for inverter in &inverters {
                    ctx.set_availability(&ctx.device_id(&inverter.native()), inverter.online);
                }
                for (native, key, value) in model::readings(&station, &inverters, &inputs, now()) {
                    ctx.set_state(&ctx.device_id(&native), &key, value);
                }
                POLL
            }
            Err(e) if e.downcast_ref::<Refused>().is_some() => {
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.izypower.identifiants_refuses",
                    instance = ctx.instance(),
                    error = format!("{e:#}")
                ));
                REFUSED_WAIT
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "izypower cloud unreachable");
                if failures >= 2 {
                    for id in &ids {
                        ctx.set_availability(id, false);
                    }
                }
                if ids.is_empty() {
                    ctx.wait_for(moli_i18n::tr!(
                        "pilotes.izypower.injoignable",
                        error = format!("{e:#}")
                    ));
                }
                POLL
            }
        };
        if !pause(ctx, wait).await {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_date_of_a_day() {
        assert_eq!(today(0), "1970-01-01");
        // 2026-10-26 12:14:45 UTC.
        assert_eq!(today(1_793_016_885), "2026-10-26");
        // 23:30 UTC is already tomorrow in Paris (UTC+1).
        assert_eq!(today(1_793_057_400), "2026-10-27");
    }

    #[test]
    fn a_401_in_the_answer_counts() {
        assert!(unauthorized(200, &json!({"code": 401})));
        assert!(unauthorized(401, &Json::Null));
        assert!(!unauthorized(200, &json!({"code": 200})));
    }
}

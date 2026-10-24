//! Pellet stoves on a Micronova board (MCZ, Red, Extraflame, Ravelli…),
//! through the cloud their Easy Connect app uses (Agua IoT): the household's
//! account, e-mail and password from the vault (`moli-os secrets set
//! <instance> email`, then `password`). Each stove's register map is read
//! once, its values every minute. Read only for now.

mod cloud;
pub mod model;

use std::time::Duration;

use moli_core::Device;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

use cloud::{Cloud, LoggedOut, Stove};
use model::Map;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The app's server (Easy Connect: `remote.mcz.it`).
    #[serde(default = "default_host")]
    pub host: String,
    /// The app's code on that server (Easy Connect: 354924).
    #[serde(default = "default_code")]
    pub customer_code: String,
    /// Seconds between readings.
    #[serde(default = "default_every")]
    pub every: u64,
}

fn default_host() -> String {
    "remote.mcz.it".into()
}

fn default_code() -> String {
    "354924".into()
}

fn default_every() -> u64 {
    60
}

/// Credentials not given yet: looked for again this often.
const LOOK_AGAIN: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Micronova {
    config: Config,
}

impl Micronova {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Micronova {
    fn kind(&self) -> &'static str {
        "micronova"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The account's e-mail and password, once a person has filed them.
async fn credentials(ctx: &DriverCtx) -> Option<(String, String)> {
    loop {
        if let (Some(email), Some(password)) = (ctx.secret("email"), ctx.secret("password")) {
            return Some((email, password));
        }
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.micronova.identifiants",
            instance = ctx.instance()
        ));
        tokio::select! {
            () = tokio::time::sleep(LOOK_AGAIN) => {}
            () = ctx.cancelled() => return None,
        }
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some((email, password)) = credentials(ctx).await else {
        return Ok(());
    };
    let mut cloud = Cloud::new(&config.host, &config.customer_code);
    cloud.login(&email, &password).await?;
    let mut stoves: Vec<(Stove, Map)> = Vec::new();
    for stove in cloud.stoves().await? {
        let map = cloud.map(&stove).await?;
        ctx.upsert_device(Device {
            id: ctx.device_id(&stove.id_device),
            instance: ctx.instance().clone(),
            native_name: stove.name.as_str().into(),
            manufacturer: Some("Micronova".into()),
            model: stove.product.as_deref().map(Into::into),
            description: None,
            native_room: None,
            members: Vec::new(),
            points: map.points(),
        });
        // As the cloud last heard from it, until the first reading.
        ctx.set_availability(&ctx.device_id(&stove.id_device), stove.online);
        stoves.push((stove, map));
    }
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), stoves = stoves.len(), "micronova stoves loaded");

    let every = Duration::from_secs(config.every.max(30));
    let mut next = tokio::time::Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let message = moli_i18n::tr!("pilotes.micronova.lecture_seule", point = command.key);
                command.reply(Err(message));
            }
            () = tokio::time::sleep_until(next) => {
                for (stove, map) in &stoves {
                    let id = ctx.device_id(&stove.id_device);
                    match cloud.read(stove).await {
                        Ok(reading) => {
                            for (key, value) in map.values(&reading) {
                                ctx.set_state(&id, &key, value);
                            }
                            ctx.set_availability(&id, true);
                        }
                        Err(e) if e.is::<LoggedOut>() => {
                            cloud.login(&email, &password).await?;
                        }
                        Err(e) => {
                            tracing::warn!(stove = stove.name, error = %e, "micronova: no reading");
                            ctx.set_availability(&id, false);
                        }
                    }
                }
                next = tokio::time::Instant::now() + every;
            }
        }
    }
}

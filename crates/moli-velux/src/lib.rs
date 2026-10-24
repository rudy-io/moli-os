//! VELUX ACTIVE (KIX 300 gateway): roof windows, blinds and shutters, the
//! indoor climate sensor and the departure switch, through the cloud the
//! VELUX ACTIVE app uses: the household's account, e-mail and password from
//! the vault (`moli-os secrets set <instance> email`, then `password`). The
//! modules are listed once, their state read every minute. Read only for
//! now: no order reaches a window or a blind.

mod cloud;
pub mod model;

use std::time::Duration;

use moli_core::Device;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

use cloud::{Cloud, LoggedOut};
use model::{Home, Role};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The app's server.
    #[serde(default = "default_host")]
    pub host: String,
    /// Seconds between readings.
    #[serde(default = "default_every")]
    pub every: u64,
}

fn default_host() -> String {
    "app.velux-active.com".into()
}

fn default_every() -> u64 {
    60
}

/// Credentials not given yet: looked for again this often.
const LOOK_AGAIN: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Velux {
    config: Config,
}

impl Velux {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Velux {
    fn kind(&self) -> &'static str {
        "velux"
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
            "pilotes.velux.identifiants",
            instance = ctx.instance()
        ));
        tokio::select! {
            () = tokio::time::sleep(LOOK_AGAIN) => {}
            () = ctx.cancelled() => return None,
        }
    }
}

/// A read, logging in again once when the token is gone.
macro_rules! fresh {
    ($cloud:ident, $email:ident, $password:ident, $read:expr) => {{
        let first = $read.await;
        match first {
            Err(e) if e.is::<LoggedOut>() => {
                $cloud.login(&$email, &$password).await?;
                $read.await
            }
            other => other,
        }
    }};
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some((email, password)) = credentials(ctx).await else {
        return Ok(());
    };
    let mut cloud = Cloud::new(&config.host);
    cloud.login(&email, &password).await?;
    let data = fresh!(cloud, email, password, cloud.home())?;
    let home = Home::parse(&data).ok_or_else(|| anyhow::anyhow!("VELUX: unreadable home"))?;
    // The first state says which points each module has (a battery or not).
    let status = fresh!(cloud, email, password, cloud.status(&home.id))?;
    for module in &home.modules {
        ctx.upsert_device(Device {
            id: ctx.device_id(&module.id),
            instance: ctx.instance().clone(),
            native_name: module.name.as_str().into(),
            manufacturer: Some("VELUX".into()),
            model: Some(model_name(module.role, &module.model).into()),
            description: None,
            native_room: module.room.as_deref().map(Into::into),
            members: Vec::new(),
            points: module.points(&status),
        });
    }
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), modules = home.modules.len(), "velux modules loaded");

    let every = Duration::from_secs(config.every.max(30));
    let mut status = Some(status);
    let mut next = tokio::time::Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if !command.abandoned() {
                    command.reply(Err(moli_i18n::tr("pilotes.velux.lecture_seule")));
                }
            }
            () = tokio::time::sleep_until(next) => {
                let read = if let Some(first) = status.take() {
                    Ok(first)
                } else {
                    fresh!(cloud, email, password, cloud.status(&home.id))
                };
                match read {
                    Ok(status) => {
                        for module in &home.modules {
                            let id = ctx.device_id(&module.id);
                            match module.values(&status) {
                                Some((answers, values)) => {
                                    for (key, value) in values {
                                        ctx.set_state(&id, key, value);
                                    }
                                    ctx.set_availability(&id, answers);
                                }
                                None => ctx.set_availability(&id, false),
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "velux: no reading");
                        for module in &home.modules {
                            ctx.set_availability(&ctx.device_id(&module.id), false);
                        }
                    }
                }
                next = tokio::time::Instant::now() + every;
            }
        }
    }
}

/// What the family reads as the model: « Fenêtre de toit », « Store »…
fn model_name(role: Role, velux: &str) -> String {
    let key = match (role, velux) {
        (Role::Gateway, _) => "pilotes.velux.passerelle",
        (Role::Window, _) => "pilotes.velux.fenetre",
        (Role::Cover, "shutter") => "pilotes.velux.volet",
        (Role::Cover, _) => "pilotes.velux.store",
        (Role::Sensor, _) => "pilotes.velux.capteur",
        (Role::Departure, _) => "pilotes.velux.commande_depart",
    };
    moli_i18n::tr(key)
}

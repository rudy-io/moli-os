//! VELUX ACTIVE (KIX 300 gateway): roof windows, blinds and shutters, the
//! indoor climate sensor and the departure switch, through the cloud the
//! VELUX ACTIVE app uses: the household's account, e-mail and password from
//! the vault (`moli-os secrets set <instance> email`, then `password`),
//! used once: the refresh token the cloud then gives is kept in the vault
//! (`refresh_token`) and renews the access, restarts included. The
//! modules are listed once, their state read every minute, and every few
//! seconds after an order while something moves. Blinds and shutters obey
//! as they are; roof windows only with the key the gateway gave when its
//! button was pressed (`hash_sign_key` and `sign_key_id` in the vault).

mod cloud;
pub mod model;
pub mod schedule;
mod sign;

use std::time::{Duration, SystemTime};

use moli_core::Device;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;

use cloud::{Cloud, LoggedOut, Refused};
use model::{Home, Module, Role};
use schedule::Schedule;
use sign::Signer;

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

/// After an order the home is read again this often while something moves,
/// at most this many times (a blind takes up to a minute).
const SOON: Duration = Duration::from_secs(3);
const SOON_READS: u8 = 40;
/// Readings in a row where nothing moves before going back to `every`.
const STILL_READS: u8 = 2;
/// The automations are read again this often (changed in the app too).
const PLAN_EVERY: Duration = Duration::from_secs(10 * 60);

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

/// The gateway's key, if a person filed it (after pressing the gateway's
/// button; read at start, like every secret): windows can then be moved.
fn signer(ctx: &DriverCtx) -> Option<Signer> {
    let (key, id) = (ctx.secret("hash_sign_key")?, ctx.secret("sign_key_id")?);
    let signer = Signer::new(&key, &id);
    if signer.is_none() {
        tracing::warn!(instance = %ctx.instance(), "velux: hash_sign_key is not base64");
    }
    signer
}

/// Lists the modules, with their points (a window's writable with the key;
/// the gateway's automations when the home has them).
fn publish(
    ctx: &DriverCtx,
    home: &Home,
    status: &serde_json::Value,
    signed: bool,
    plan: Option<&Schedule>,
) {
    for module in &home.modules {
        let mut points = module.points(status, signed);
        if module.role == Role::Gateway && plan.is_some() {
            points.push(schedule::spec());
        }
        ctx.upsert_device(Device {
            id: ctx.device_id(&module.id),
            instance: ctx.instance().clone(),
            native_name: module.name.as_str().into(),
            manufacturer: Some("VELUX".into()),
            model: Some(model_name(module.role, &module.model).into()),
            description: None,
            native_room: module.room.as_deref().map(Into::into),
            members: Vec::new(),
            points,
        });
    }
}

/// The automations, shown on the gateway (JSON text, if it fits).
fn show_plan(ctx: &DriverCtx, home: &Home, plan: Option<&Schedule>) {
    let (Some(plan), Some(gateway)) = (plan, home.modules.iter().find(|m| m.role == Role::Gateway))
    else {
        return;
    };
    let cards = plan.cards(home, ctx.instance()).to_string();
    if cards.chars().count() > moli_core::MAX_TEXT {
        tracing::warn!(instance = %ctx.instance(), "velux: too many automations to show");
        return;
    }
    ctx.set_state(
        &ctx.device_id(&gateway.id),
        schedule::POINT,
        moli_core::Value::Text(cards.into()),
    );
}

/// Where the vault keeps the refresh token.
const REFRESH: &str = "refresh_token";

/// A new refresh token, filed in the vault (else the next start logs in
/// with the password, and the account gets an e-mail).
async fn keep_refresh(ctx: &DriverCtx, cloud: &mut Cloud) {
    if let Some(token) = cloud.unsaved_refresh()
        && let Err(e) = ctx.store_secret(REFRESH, &token).await
    {
        tracing::warn!(error = %e, "velux: refresh token not kept");
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
    let mut cloud = Cloud::new(&config.host, ctx.secret(REFRESH));
    cloud.login(&email, &password).await?;
    keep_refresh(ctx, &mut cloud).await;
    let data = fresh!(cloud, email, password, cloud.home())?;
    let home = Home::parse(&data).ok_or_else(|| anyhow::anyhow!("VELUX: unreadable home"))?;
    // The first state says which points each module has (a battery or not).
    let status = fresh!(cloud, email, password, cloud.status(&home.id))?;
    let mut signer = signer(ctx);
    let mut plan = Schedule::find(&data);
    publish(ctx, &home, &status, signer.is_some(), plan.as_ref());
    show_plan(ctx, &home, plan.as_ref());
    ctx.ready();
    tracing::info!(
        instance = %ctx.instance(),
        modules = home.modules.len(),
        windows = signer.is_some(),
        "velux modules loaded"
    );

    let every = Duration::from_secs(config.every.max(30));
    let mut status = Some(status);
    let mut next = tokio::time::Instant::now();
    // Departure mode: the gateway lets nothing move.
    let mut departure = false;
    let (mut soon, mut still) = (0u8, 0u8);
    let mut plan_at = tokio::time::Instant::now() + PLAN_EVERY;
    loop {
        keep_refresh(ctx, &mut cloud).await;
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if command.abandoned() {
                    continue;
                }
                let module = home
                    .modules
                    .iter()
                    .find(|m| ctx.device_id(&m.id) == command.device.id);
                let answer = match module {
                    None => Err(moli_i18n::tr("pilotes.velux.module_inconnu")),
                    Some(m) if m.role == Role::Gateway && &*command.key == schedule::POINT => {
                        let saved = save_plan(&mut cloud, (&email, &password), &home, plan.as_ref(), ctx.instance(), &command).await;
                        if saved.is_ok() {
                            // Read back at once: what the cloud kept.
                            plan_at = tokio::time::Instant::now();
                        }
                        command.reply(saved);
                        continue;
                    }
                    Some(_) if departure => Err(moli_i18n::tr("pilotes.velux.mode_depart")),
                    Some(module) => {
                        let order = Order { home: &home, module, command: &command };
                        order.send(&mut cloud, (&email, &password), signer.as_mut()).await
                    }
                };
                if answer.is_ok() {
                    next = tokio::time::Instant::now();
                    (soon, still) = (SOON_READS, 0);
                }
                command.reply(answer);
            }
            () = tokio::time::sleep_until(next) => {
                let read = if let Some(first) = status.take() {
                    Ok(first)
                } else {
                    fresh!(cloud, email, password, cloud.status(&home.id))
                };
                let moving = take(ctx, &home, read, &mut departure);
                if soon > 0 {
                    soon -= 1;
                    still = if moving { 0 } else { still + 1 };
                    if still >= STILL_READS {
                        soon = 0;
                    }
                }
                next = tokio::time::Instant::now() + if soon > 0 { SOON } else { every };
            }
            () = tokio::time::sleep_until(plan_at) => {
                plan_at = tokio::time::Instant::now() + PLAN_EVERY;
                match fresh!(cloud, email, password, cloud.home()) {
                    Ok(data) => {
                        let read = Schedule::find(&data);
                        if read.is_some() != plan.is_some() {
                            publish(ctx, &home, &serde_json::Value::Null, signer.is_some(), read.as_ref());
                        }
                        plan = read;
                        show_plan(ctx, &home, plan.as_ref());
                    }
                    Err(e) => tracing::warn!(error = %e, "velux: automations not read"),
                }
            }
        }
    }
}

/// One reading of the home's state, published; whether something moves.
/// Unread: every module goes offline.
fn take(
    ctx: &DriverCtx,
    home: &Home,
    read: anyhow::Result<serde_json::Value>,
    departure: &mut bool,
) -> bool {
    let mut moving = false;
    match read {
        Ok(status) => {
            for module in &home.modules {
                let id = ctx.device_id(&module.id);
                match module.values(&status) {
                    Some((answers, values)) => {
                        for (key, value) in values {
                            let on = value == moli_core::Value::Bool(true);
                            match key {
                                "moving" => moving |= on,
                                "departure" => *departure = on,
                                _ => {}
                            }
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
    moving
}

/// The automations a person edited, sent to the cloud (logging in again
/// once if the token is gone). Taken: `Ok`; else why.
async fn save_plan(
    cloud: &mut Cloud,
    (email, password): (&str, &str),
    home: &Home,
    plan: Option<&Schedule>,
    instance: &moli_core::InstanceId,
    command: &CommandRequest,
) -> Result<(), String> {
    let plan = plan.ok_or_else(|| moli_i18n::tr("pilotes.velux.planning.absent"))?;
    let wanted: serde_json::Value = match &command.value {
        moli_core::Value::Text(text) => serde_json::from_str(text).ok(),
        _ => None,
    }
    .ok_or_else(|| moli_i18n::tr("pilotes.velux.planning.illisible"))?;
    let body = Schedule::body(&plan.edited(home, instance, &wanted)?);
    for attempt in 0..2 {
        match cloud
            .sync_schedule(&home.id, plan.id(), plan.name(), &body)
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) if e.is::<LoggedOut>() && attempt == 0 => {
                cloud
                    .login(email, password)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => {
                return Err(match e.downcast::<Refused>() {
                    Ok(Refused(why)) => why,
                    Err(e) => moli_i18n::tr!("pilotes.velux.pas_de_reponse", why = e),
                });
            }
        }
    }
    Err(moli_i18n::tr!(
        "pilotes.velux.pas_de_reponse",
        why = "timeout"
    ))
}

/// One command, on its way to the cloud.
struct Order<'a> {
    home: &'a Home,
    module: &'a Module,
    command: &'a CommandRequest,
}

impl Order<'_> {
    /// Sends it before the hub stops waiting, logging in again once if the
    /// token is gone. Taken: `Ok`; else why, for the family.
    async fn send(
        &self,
        cloud: &mut Cloud,
        (email, password): (&str, &str),
        mut signer: Option<&mut Signer>,
    ) -> Result<(), String> {
        if !self.module.movable(signer.is_some()) {
            return Err(moli_i18n::tr(if self.module.role == Role::Window {
                "pilotes.velux.sans_cle"
            } else {
                "pilotes.velux.lecture_seule"
            }));
        }
        let target = self.module.target(&self.command.key, &self.command.value)?;
        for attempt in 0..2 {
            let left = self
                .command
                .deadline
                .saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            // Signed again on each attempt: a nonce is never sent twice.
            let entry = self
                .module
                .order(target, signer.as_deref_mut(), unix_now())?;
            let result = cloud.set_state(&self.home.setstate(&[entry]), left).await;
            match result {
                Ok(()) => return Ok(()),
                Err(e) if e.is::<LoggedOut>() && attempt == 0 => {
                    if let Err(e) = cloud.login(email, password).await {
                        return Err(e.to_string());
                    }
                }
                Err(e) => {
                    return Err(match e.downcast::<Refused>() {
                        Ok(Refused(why)) => why,
                        Err(e) => moli_i18n::tr!("pilotes.velux.pas_de_reponse", why = e),
                    });
                }
            }
        }
        Err(moli_i18n::tr!(
            "pilotes.velux.pas_de_reponse",
            why = "timeout"
        ))
    }
}

/// Seconds since 1970, what a window's signature carries.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

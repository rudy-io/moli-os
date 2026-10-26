//! Jura coffee machines with a Wi-Fi Connect dongle (the J.O.E. app's), on
//! the house's network only: TCP 51515, no account, no cloud.
//!
//! The dongle serves one client at a time. Moli opens a short session every
//! `every` seconds (20 by default, 5 while the machine works), reads what
//! it needs and leaves: J.O.E. finds it free in between. Status every time;
//! maintenance and drink counters every five minutes and after each drink
//! or program; settings every ten minutes.
//!
//! Pairing, once: Moli asks, the machine shows a request, a person presses
//! OK on it; the token the dongle gives goes to the vault (`token`), with
//! the machine's security code if it has one (`pin`, filed by the person).
//!
//! Orders: a drink (brewed with the recipe the machine keeps for it), a
//! maintenance program (the machine then leads on its display), cancel,
//! and the settings of a known model. Drinks, programs and cancel are a
//! machine's orders (`Semantic::Control`): an agent's waits for a person.

mod model;
mod session;
mod wire;

use std::collections::BTreeMap;
use std::time::Duration;

use moli_core::{Device, DeviceId, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use tokio::time::Instant;

use model::{Model, Order, Status};
use session::{Hello, PORT, Session};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The dongle's address on the house's network.
    pub host: String,
    /// Seconds between two readings (10 at least).
    #[serde(default = "default_every")]
    pub every: u64,
    /// The machine's J.O.E. group (`EF1092` for the E8 EC): its drinks'
    /// names and its settings. Without it, what the whole family shares.
    #[serde(default)]
    pub model: Option<String>,
    /// The name Moli gives itself to the dongle; the token goes with it.
    #[serde(default = "default_name")]
    pub name: String,
}

fn default_every() -> u64 {
    20
}

fn default_name() -> String {
    "Moli".into()
}

const TOKEN: &str = "token";
const PIN: &str = "pin";
/// The dongle refuses a session opened right after the last one (an E8
/// took one again after 1 s; other models want more).
const GAP: Duration = Duration::from_secs(3);
/// While the machine works: how often to look.
const BUSY_EVERY: Duration = Duration::from_secs(5);
const COUNTERS_EVERY: Duration = Duration::from_secs(300);
const SETTINGS_EVERY: Duration = Duration::from_secs(600);
/// `@TF:` comes every 2.05 s.
const STATUS_WAIT: Duration = Duration::from_millis(2600);
/// No one pressed OK: asked again this much later.
const PAIR_AGAIN: Duration = Duration::from_secs(120);
/// An order is not sent this close to its deadline.
const MARGIN: Duration = Duration::from_millis(800);

#[derive(Debug)]
pub struct Jura {
    config: Config,
}

impl Jura {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Jura {
    fn kind(&self) -> &'static str {
        "jura"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId, model: &Model) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: model
            .name()
            .map_or_else(|| "Jura".into(), |n| format!("Jura {n}").into()),
        manufacturer: Some("Jura".into()),
        model: model.name().map(Into::into),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: model.points(),
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let model = Model::new(config.model.as_deref());
    // One machine per instance: its identity is the instance's.
    let id = ctx.device_id("machine");
    ctx.upsert_device(device(ctx, &id, &model));
    if ctx.secret(TOKEN).is_none() && !pair(config, ctx).await? {
        return Ok(());
    }
    ctx.ready();
    let mut machine = Machine {
        config,
        id,
        model,
        status: None,
        last_close: None,
        counters_due: Instant::now(),
        settings_due: Instant::now(),
        reachable: None,
        waiting: false,
    };
    let mut next = Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if command.abandoned() {
                    continue;
                }
                let answer = machine.obey(ctx, &command).await;
                let done = answer.is_ok();
                command.reply(answer);
                if done {
                    // See it start.
                    next = Instant::now() + GAP;
                }
            }
            () = tokio::time::sleep_until(next) => {
                machine.poll(ctx).await?;
                next = Instant::now() + machine.pace();
            }
        }
    }
}

/// Pairs with the dongle (a person presses OK on the machine). `false` on
/// shutdown.
async fn pair(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<bool> {
    if !ctx.can_store_secrets() {
        let problem = ctx.secrets_problem().unwrap_or_default().to_owned();
        ctx.wait_for(moli_i18n::tr!("pilotes.jura.coffre", problem = problem));
        ctx.cancelled().await;
        return Ok(false);
    }
    loop {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.jura.appuyer_ok",
            host = config.host
        ));
        let pin = ctx.secret(PIN).unwrap_or_default();
        match Session::open(&config.host, PORT, &pin, &config.name, "").await {
            Ok((session, hello)) => {
                session.close().await;
                match hello {
                    Hello::Paired(token) => {
                        ctx.store_secret(TOKEN, &token).await?;
                        tracing::info!(instance = %ctx.instance(), "jura machine paired");
                        return Ok(true);
                    }
                    Hello::WrongPin => ctx.wait_for(code_wanted(ctx)),
                    // Paired under this name before, token lost: another name.
                    Hello::Unknown | Hello::Accepted => {
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.jura.nom_deja_pris",
                            name = config.name
                        ));
                    }
                }
            }
            Err(e) => tracing::debug!(instance = %ctx.instance(), error = %e, "jura: not paired"),
        }
        tokio::select! {
            () = tokio::time::sleep(PAIR_AGAIN) => {}
            () = ctx.cancelled() => return Ok(false),
        }
    }
}

fn code_wanted(ctx: &DriverCtx) -> String {
    moli_i18n::tr!("pilotes.jura.code_attendu", instance = ctx.instance())
}

struct Machine<'a> {
    config: &'a Config,
    id: DeviceId,
    model: Model,
    /// The last status, while the machine answers.
    status: Option<Status>,
    last_close: Option<Instant>,
    counters_due: Instant,
    settings_due: Instant,
    reachable: Option<bool>,
    /// Shown waiting for a person (the code).
    waiting: bool,
}

impl Machine<'_> {
    fn pace(&self) -> Duration {
        match &self.status {
            Some(s)
                if s.state(&self.model) != "ready" && s.state(&self.model) != "energy_saving" =>
            {
                BUSY_EVERY.min(Duration::from_secs(self.config.every.max(10)))
            }
            _ => Duration::from_secs(self.config.every.max(10)),
        }
    }

    /// Opens a session, after the gap the dongle wants, if it can start
    /// before `by`. `None`: the machine is off or out of reach (or a code
    /// is awaited), said on the device.
    async fn open(
        &mut self,
        ctx: &mut DriverCtx,
        by: Option<Instant>,
    ) -> anyhow::Result<Option<Session>> {
        if let Some(closed) = self.last_close {
            let from = closed + GAP;
            if by.is_some_and(|by| from + MARGIN > by) {
                return Ok(None);
            }
            tokio::time::sleep_until(from).await;
        }
        let token = ctx.secret(TOKEN).unwrap_or_default();
        let pin = ctx.secret(PIN).unwrap_or_default();
        let opened = Session::open(&self.config.host, PORT, &pin, &self.config.name, &token).await;
        self.last_close = Some(Instant::now());
        match opened {
            Ok((session, Hello::Accepted)) => {
                if self.waiting {
                    ctx.ready();
                    self.waiting = false;
                }
                self.seen(ctx, true);
                Ok(Some(session))
            }
            Ok((session, Hello::Paired(token))) => {
                ctx.store_secret(TOKEN, &token).await?;
                self.seen(ctx, true);
                Ok(Some(session))
            }
            Ok((session, Hello::WrongPin)) => {
                session.close().await;
                ctx.wait_for(code_wanted(ctx));
                self.waiting = true;
                Ok(None)
            }
            Ok((session, Hello::Unknown)) => {
                session.close().await;
                ctx.forget_secret(TOKEN).await?;
                anyhow::bail!("jura: the machine no longer knows Moli, pairing again")
            }
            Err(e) => {
                if self.reachable != Some(false) {
                    tracing::info!(instance = %ctx.instance(), error = %e, "jura machine out of reach");
                }
                self.seen(ctx, false);
                self.status = None;
                Ok(None)
            }
        }
    }

    fn seen(&mut self, ctx: &DriverCtx, reachable: bool) {
        if self.reachable != Some(reachable) {
            if reachable && self.reachable.is_some() {
                tracing::info!(instance = %ctx.instance(), "jura machine back");
            }
            ctx.set_availability(&self.id, reachable);
            self.reachable = Some(reachable);
        }
    }

    async fn close(&mut self, session: Session) {
        session.close().await;
        self.last_close = Some(Instant::now());
    }

    fn set(&self, ctx: &DriverCtx, values: Vec<(String, Value)>) {
        for (key, value) in values {
            ctx.set_state(&self.id, &key, value);
        }
    }

    /// One reading.
    async fn poll(&mut self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let Some(mut session) = self.open(ctx, None).await? else {
            return Ok(());
        };
        let result = self.read(ctx, &mut session).await;
        self.close(session).await;
        result
    }

    async fn read(&mut self, ctx: &mut DriverCtx, session: &mut Session) -> anyhow::Result<()> {
        let was_busy = matches!(self.status, Some(Status::Busy { .. }));
        if let Some(status) = session.status(STATUS_WAIT).await?.cloned() {
            self.set(ctx, status.values(&self.model));
            if was_busy && matches!(status, Status::Idle(_)) {
                // A drink or a program ended: its counters moved.
                self.counters_due = Instant::now();
            }
            self.status = Some(status);
        }
        // A busy machine answers its own stream first; the rest can wait.
        if !matches!(self.status, Some(Status::Idle(_))) {
            return Ok(());
        }
        if Instant::now() >= self.counters_due {
            self.read_counters(ctx, session).await?;
            self.counters_due = Instant::now() + COUNTERS_EVERY;
        }
        if Instant::now() >= self.settings_due {
            for setting in self.model.settings {
                let question = format!("@TM:{}", setting.argument);
                if let Some(answer) = session.ask(&question, "@tm:").await?
                    && let Some(value) = model::setting_value(setting, &answer)
                {
                    ctx.set_state(&self.id, setting.key, value);
                }
            }
            self.settings_due = Instant::now() + SETTINGS_EVERY;
        }
        Ok(())
    }

    async fn read_counters(
        &mut self,
        ctx: &DriverCtx,
        session: &mut Session,
    ) -> anyhow::Result<()> {
        if let Some(answer) = session.ask("@TG:43", "@tg:43").await? {
            self.set(ctx, model::counters(&answer));
        }
        if let Some(answer) = session.ask("@TG:C0", "@tg:C0").await? {
            self.set(ctx, model::percents(&answer));
        }
        let mut counted = BTreeMap::new();
        for page in 0..16u8 {
            if let Some(answer) = session.ask(&format!("@TR:32,{page:02X}"), "@tr:32").await? {
                counted.extend(model::counted(&answer, page));
            }
        }
        if self.model.learn(&counted) {
            ctx.upsert_device(device(ctx, &self.id, &self.model));
        }
        self.set(ctx, model::brews(&self.model, &counted));
        Ok(())
    }

    /// Carries out an order, within its deadline.
    async fn obey(&mut self, ctx: &mut DriverCtx, command: &CommandRequest) -> Result<(), String> {
        let order = model::order(
            &self.model,
            self.status.as_ref(),
            &command.key,
            &command.value,
        )?;
        let by = Instant::from_std(command.deadline);
        let Some(mut session) = self.open(ctx, Some(by)).await.map_err(|e| e.to_string())? else {
            return Err(if self.waiting {
                code_wanted(ctx)
            } else if self.reachable == Some(false) {
                moli_i18n::tr("pilotes.jura.pas_joignable")
            } else {
                moli_i18n::tr("pilotes.jura.pas_le_temps")
            });
        };
        let result = self.carry_out(ctx, &mut session, order, by).await;
        self.close(session).await;
        result.map_err(|e| e.to_string())?
    }

    async fn carry_out(
        &mut self,
        ctx: &DriverCtx,
        session: &mut Session,
        order: Order,
        by: Instant,
    ) -> anyhow::Result<Result<(), String>> {
        let in_time = || Instant::now() + MARGIN < by;
        let refused = || Ok(Err(moli_i18n::tr("pilotes.jura.refuse")));
        match order {
            Order::Brew(drink) => {
                let code = model::recipe_code(&drink);
                let answer = session.ask(&format!("@TM:41,{code:02X}"), "@tm:").await?;
                let Some(recipe) = answer.and_then(|a| model::recipe(&a, code)) else {
                    return Ok(Err(moli_i18n::tr("pilotes.jura.recette_illisible")));
                };
                let command = model::brew_command(&drink, &recipe);
                // Woken from energy saving, a machine may let the first go.
                for _ in 0..2 {
                    if !in_time() {
                        return Ok(Err(moli_i18n::tr("pilotes.jura.pas_le_temps")));
                    }
                    let answer = session.ask(&command, "@tp").await?;
                    if answer
                        .as_deref()
                        .is_some_and(|a| a.starts_with("@tp") && !a.starts_with("@tp:00"))
                    {
                        tracing::info!(instance = %ctx.instance(), drink = drink.slug, "jura: brewing");
                        ctx.set_state(&self.id, "status", Value::from("brewing"));
                        ctx.set_state(&self.id, "activity", Value::from(drink.slug));
                        self.status = Some(Status::Busy {
                            state: 0,
                            code: drink.code,
                            percent: None,
                        });
                        return Ok(Ok(()));
                    }
                }
                refused()
            }
            Order::Program(code) => {
                if !in_time() {
                    return Ok(Err(moli_i18n::tr("pilotes.jura.pas_le_temps")));
                }
                let answer = session.ask(&format!("@TG:{code:02X}"), "@tg").await?;
                let echo = format!("@tg:{code:02x}");
                if answer.is_some_and(|a| a.to_ascii_lowercase().starts_with(&echo)) {
                    tracing::info!(instance = %ctx.instance(), program = code, "jura: maintenance started");
                    self.status = Some(Status::Busy {
                        state: 0,
                        code,
                        percent: None,
                    });
                    Ok(Ok(()))
                } else {
                    refused()
                }
            }
            Order::Cancel => {
                let answer = session.ask("@TG:FF", "@tg").await?;
                if answer.is_some_and(|a| a.to_ascii_lowercase().starts_with("@tg:ff")) {
                    Ok(Ok(()))
                } else {
                    refused()
                }
            }
            Order::Set { setting, value } => {
                if !in_time() {
                    return Ok(Err(moli_i18n::tr("pilotes.jura.pas_le_temps")));
                }
                // The app holds the keys around a write; without it the
                // machine acknowledges and keeps the old value.
                session.ask("@TS:01", "@ts").await?;
                let written = session
                    .ask(&model::set_command(&setting, &value), "@tm:")
                    .await;
                session.ask("@TS:00", "@ts").await?;
                let echo = format!("@tm:{}", setting.argument);
                if written?.as_deref() != Some(echo.as_str()) {
                    return refused();
                }
                let question = format!("@TM:{}", setting.argument);
                if let Some(answer) = session.ask(&question, "@tm:").await?
                    && let Some(read) = model::setting_value(&setting, &answer)
                {
                    ctx.set_state(&self.id, setting.key, read);
                }
                Ok(Ok(()))
            }
        }
    }
}

//! Pellet stoves on a Micronova board (MCZ, Red, Extraflame, Ravelli…),
//! through the cloud their Easy Connect app uses (Agua IoT): the household's
//! account, e-mail and password from the vault (`moli-os secrets set
//! <instance> email`, then `password`). Each stove's register map is read
//! once, its values every minute. Its settings are set from Moli (setpoint,
//! power, fan, eco-stop, programs), one register at a time by a writer of
//! its own, the stove read again once it confirms; never switched on or off.

mod cloud;
pub mod model;

use std::time::Duration;

use moli_core::Device;
use moli_runtime::{BoxFuture, Driver, DriverCtx, MediaPublisher};
use serde::Deserialize;
use tokio::sync::mpsc;

use cloud::{Cloud, LoggedOut, Stove};
use model::{Map, Reading, Write};

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

    let (orders, queue) = mpsc::unbounded_channel();
    let (done, mut written) = mpsc::unbounded_channel();
    let writer = Writer {
        cloud: cloud.clone(),
        email: email.clone(),
        password: password.clone(),
        stoves: stoves.iter().map(|(s, _)| s.clone()).collect(),
        publisher: ctx.media_publisher(),
    };
    let _writer = Abort(tokio::spawn(writer.run(queue, done)));
    // Each stove's last reading: what a command is weighed against.
    let mut last: Vec<Option<Reading>> = vec![None; stoves.len()];

    let every = Duration::from_secs(config.every.max(30));
    let mut next = tokio::time::Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if command.abandoned() {
                    continue;
                }
                let at = stoves
                    .iter()
                    .position(|(s, _)| ctx.device_id(&s.id_device) == command.device.id);
                let answer = match at.map(|i| (i, last[i].as_ref())) {
                    Some((i, Some(reading))) => {
                        match stoves[i].1.writes(&command.key, &command.value, reading) {
                            Ok(registers) if registers.is_empty() => Ok(()),
                            Ok(registers) => {
                                let _ = orders.send((i, registers));
                                Ok(())
                            }
                            Err(why) => Err(why),
                        }
                    }
                    Some((_, None)) => Err(moli_i18n::tr("pilotes.micronova.pas_encore_lu")),
                    None => Err(moli_i18n::tr("pilotes.micronova.reglage_absent")),
                };
                command.reply(answer);
            }
            Some(_) = written.recv() => {
                // Confirmed (or given up): read it again now.
                next = tokio::time::Instant::now();
            }
            () = tokio::time::sleep_until(next) => {
                for (i, (stove, map)) in stoves.iter().enumerate() {
                    let id = ctx.device_id(&stove.id_device);
                    match cloud.read(stove).await {
                        Ok(reading) => {
                            for (key, value) in map.values(&reading) {
                                ctx.set_state(&id, &key, value);
                            }
                            ctx.set_availability(&id, true);
                            last[i] = Some(reading);
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

/// Writes the commands' registers, one after the other, each once the stove
/// confirms the one before; tells the house when one is not taken.
struct Writer {
    cloud: Cloud,
    email: String,
    password: String,
    stoves: Vec<Stove>,
    publisher: MediaPublisher,
}

impl Writer {
    async fn run(
        mut self,
        mut queue: mpsc::UnboundedReceiver<(usize, Vec<Write>)>,
        done: mpsc::UnboundedSender<usize>,
    ) {
        while let Some((i, writes)) = queue.recv().await {
            let stove = &self.stoves[i];
            for write in writes {
                let mut result = self.cloud.write(stove, write).await;
                if result.as_ref().is_err_and(anyhow::Error::is::<LoggedOut>) {
                    result = match self.cloud.login(&self.email, &self.password).await {
                        Ok(()) => self.cloud.write(stove, write).await,
                        Err(e) => Err(e),
                    };
                }
                if let Err(e) = result {
                    tracing::warn!(stove = stove.name, error = %e, "micronova: not written");
                    self.publisher.tell(
                        &stove.name,
                        moli_i18n::tr!("pilotes.micronova.pas_pris", name = stove.name),
                    );
                    break;
                }
            }
            let _ = done.send(i);
        }
    }
}

/// The writer stops with the driver.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

//! 3D printers running Klipper with the Moonraker API (Snapmaker U1, Voron,
//! Prusa with Klipper…), over its local HTTP API (no auth on a trusted LAN).
//!
//! Reads the print (state, progress, time left, layers, every tool head and
//! its filament, bed, chamber), the lifetime totals and the job's thumbnail
//! (the device's image). Writes: the light, and `control` (pause, resume,
//! cancel), which the guard holds for a human when an agent asks.

mod model;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use moli_core::{Device, DeviceId, Value};
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::time::{Instant, sleep_until};

pub use model::{Job, ORDERS, STATES, Shape};

/// While printing: the progress moves.
const ACTIVE: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_secs(15);
/// Off (on a smart plug, often): asked again now and then.
const RETRY: Duration = Duration::from_secs(30);
const TOTALS_EVERY: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Shown name; by default the machine's (« Snapmaker U1 ») or its host name.
    #[serde(default)]
    pub name: Option<String>,
}

fn default_port() -> u16 {
    7125
}

#[derive(Debug)]
pub struct Moonraker {
    config: Config,
}

impl Moonraker {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Moonraker {
    fn kind(&self) -> &'static str {
        "moonraker"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

#[derive(Clone, Debug)]
struct Api {
    host: String,
    port: u16,
}

impl Api {
    async fn call(&self, method: Method, path: &str) -> anyhow::Result<Vec<u8>> {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("accept", "application/json");
        let (status, body) =
            moli_net::plain(&self.host, self.port, moli_net::empty(request)?).await?;
        if !status.is_success() {
            let why: Json = serde_json::from_slice(&body).unwrap_or_default();
            let message = why["error"]["message"].as_str().unwrap_or("").to_owned();
            bail!("Moonraker {}: {message}", status.as_u16());
        }
        Ok(body.to_vec())
    }

    async fn get(&self, path: &str) -> anyhow::Result<Json> {
        let body = self.call(Method::GET, path).await?;
        let json: Json = serde_json::from_slice(&body).context("Moonraker: not JSON")?;
        Ok(json["result"].clone())
    }
}

/// The job's thumbnail, as the printer's image.
#[derive(Debug)]
struct Thumbnail {
    api: Api,
    path: Arc<Mutex<Option<String>>>,
}

impl SnapshotSource for Thumbnail {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            let path = self
                .path
                .lock()
                .map_err(|_| anyhow::anyhow!("thumbnail lock"))?
                .clone()
                .context("no thumbnail for this print")?;
            let bytes = self
                .api
                .call(
                    Method::GET,
                    &format!("/server/files/gcodes/{}", model::encode(&path)),
                )
                .await?;
            let jpeg = std::path::Path::new(&path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
            let content_type = if jpeg { "image/jpeg" } else { "image/png" };
            Ok(Image {
                content_type: content_type.into(),
                bytes,
            })
        })
    }
}

struct Printer {
    id: DeviceId,
    shape: Shape,
    job: Option<Job>,
    totals_at: Option<Instant>,
}

/// Who the printer is and what it has; declares it.
async fn meet(
    api: &Api,
    config: &Config,
    ctx: &DriverCtx,
    thumb: &Arc<Mutex<Option<String>>>,
) -> anyhow::Result<Printer> {
    let info = api.get("/printer/info").await?;
    let host = info["hostname"].as_str().unwrap_or(&config.host).to_owned();
    let objects: Vec<String> =
        serde_json::from_value(api.get("/printer/objects/list").await?["objects"].clone())
            .context("Moonraker: no object list")?;
    let shape = Shape::of(&objects);
    // Snapmaker says what machine it is; others may not.
    let machine = api.get("/machine/system_info").await.ok().and_then(|s| {
        s["system_info"]["product_info"]["machine_type"]
            .as_str()
            .map(str::to_owned)
    });
    let id = ctx.device_id(&host);
    let name = config
        .name
        .clone()
        .or_else(|| machine.clone())
        .unwrap_or_else(|| host.clone());
    ctx.upsert_device(Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some(
            machine
                .as_deref()
                .and_then(|m| m.split_whitespace().next())
                .unwrap_or("Klipper")
                .into(),
        ),
        model: machine.map(Into::into),
        description: Some(moli_i18n::tr!("pilotes.moonraker.imprimante_3d").into()),
        native_room: None,
        members: Vec::new(),
        points: shape.points(),
    });
    ctx.provide_snapshots(
        &id,
        Arc::new(Thumbnail {
            api: api.clone(),
            path: Arc::clone(thumb),
        }),
    );
    Ok(Printer {
        id,
        shape,
        job: None,
        totals_at: None,
    })
}

/// One reading. Returns whether a print is under way.
async fn poll(
    api: &Api,
    ctx: &DriverCtx,
    printer: &mut Printer,
    thumb: &Arc<Mutex<Option<String>>>,
) -> anyhow::Result<bool> {
    let status = api.get(&printer.shape.query()).await?["status"].clone();
    let file = status["print_stats"]["filename"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if printer.job.as_ref().map(|j| j.file.as_str()) != Some(file.as_str()) {
        printer.job = if file.is_empty() {
            None
        } else {
            let meta = api
                .get(&format!(
                    "/server/files/metadata?filename={}",
                    model::encode(&file)
                ))
                .await
                .unwrap_or_default();
            Some(Job::from_metadata(&file, &meta))
        };
        if let Ok(mut path) = thumb.lock() {
            *path = printer.job.as_ref().and_then(|j| j.thumbnail.clone());
        }
    }
    let values = model::values(&printer.shape, &status, printer.job.as_ref());
    let active = values
        .iter()
        .any(|(k, v)| k == "printing" && *v == Value::Bool(true));
    for (key, value) in values {
        ctx.set_state(&printer.id, &key, value);
    }
    if printer
        .totals_at
        .is_none_or(|at| at.elapsed() >= TOTALS_EVERY)
    {
        if let Ok(totals) = api.get("/server/history/totals").await {
            for (key, value) in model::totals(&totals) {
                ctx.set_state(&printer.id, &key, value);
            }
        }
        printer.totals_at = Some(Instant::now());
    }
    Ok(active)
}

async fn obey(api: &Api, printer: Option<&Printer>, command: CommandRequest) {
    let Some(printer) = printer else {
        command.reply(Err(moli_i18n::tr!("pilotes.moonraker.pas_encore_repondu")));
        return;
    };
    let result = match model::order(&printer.shape, &command.key, &command.value) {
        Err(why) => Err(why),
        Ok(path) => api
            .call(Method::POST, &path)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
    };
    command.reply(result);
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let api = Api {
        host: config.host.clone(),
        port: config.port,
    };
    let thumb = Arc::new(Mutex::new(None));
    let mut printer: Option<Printer> = None;
    let mut online: Option<bool> = None;
    let mut next = Instant::now();
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => obey(&api, printer.as_ref(), command).await,
                None => return Ok(()),
            },
            () = sleep_until(next) => {
                if printer.is_none() {
                    match meet(&api, config, ctx, &thumb).await {
                        Ok(p) => {
                            printer = Some(p);
                            ctx.ready();
                        }
                        Err(e) => {
                            ctx.wait_for(moli_i18n::tr!(
                                "pilotes.moonraker.injoignable",
                                cause = e.root_cause()
                            ));
                            next = Instant::now() + RETRY;
                            continue;
                        }
                    }
                }
                let Some(p) = printer.as_mut() else { continue };
                match poll(&api, ctx, p, &thumb).await {
                    Ok(active) => {
                        if online != Some(true) {
                            online = Some(true);
                            ctx.set_availability(&p.id, true);
                        }
                        next = Instant::now() + if active { ACTIVE } else { IDLE };
                    }
                    Err(e) => {
                        if online != Some(false) {
                            online = Some(false);
                            ctx.set_availability(&p.id, false);
                            tracing::info!(error = %e.root_cause(), "printer unreachable");
                        }
                        next = Instant::now() + RETRY;
                    }
                }
            }
        }
    }
}

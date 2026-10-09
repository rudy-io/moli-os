//! Demo driver: a house without hardware.
//!
//! It publishes devices read from a file, exactly as a real driver would
//! (same points, same states: the dashboard cannot tell), then makes them
//! live: orders are obeyed at once, sensors drift around their value,
//! meters turn with the power drawn, motion comes and goes, a printer
//! prints, daylight follows the clock. One instance per imitated driver,
//! named like it (`hue`, `tuya`…), so ids look the same as in a real house.
//!
//! ```toml
//! [[driver]]
//! id = "hue"
//! kind = "demo"
//! [driver.options]
//! fixture = "/demo/villa/devices/hue.json"
//! ```
//!
//! The file: `{ "imitates": "hue", "devices": [ <device as GET /api/devices
//! returns it, with "state": { key: value }, optional "online" and
//! "snapshot" (an image next to the file, for a camera)> ] }`.
//!
//! Nothing here reaches the network: a demo can be opened to anyone.

mod sim;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, ensure};
use moli_core::{Device, DeviceId, Value};
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

pub use sim::Clock;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The devices to play (see the module's documentation).
    pub fixture: PathBuf,
    /// Seconds between two steps of the simulation.
    #[serde(default = "default_tick")]
    pub tick_s: u64,
    /// The house's time zone: daylight, the day's consumption curve.
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_tick() -> u64 {
    15
}

fn default_timezone() -> String {
    "Europe/Paris".to_owned()
}

/// Driver kinds a demo may pass for (`imitates`): what the dashboard shows.
const IMITABLE: &[&str] = &[
    "z2m",
    "hue",
    "tuya",
    "reolink",
    "sonos",
    "philips",
    "frigate",
    "bambu",
    "tapo",
    "igd",
    "moonraker",
    "ipp",
    "host",
    "profile",
    "presence",
    "phones",
    "telegram",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    #[serde(default)]
    imitates: Option<String>,
    devices: Vec<FixtureDevice>,
}

#[derive(Debug, Deserialize)]
struct FixtureDevice {
    #[serde(flatten)]
    device: Device,
    #[serde(default)]
    state: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    online: Option<bool>,
    #[serde(default)]
    snapshot: Option<String>,
}

/// A device as played: what to publish, where it starts.
#[derive(Debug, Clone)]
pub(crate) struct Played {
    pub(crate) native: String,
    pub(crate) device: Device,
    pub(crate) state: BTreeMap<String, Value>,
    pub(crate) online: bool,
    pub(crate) snapshot: Option<Arc<Image>>,
}

#[derive(Debug)]
pub struct Demo {
    config: Config,
    kind: &'static str,
    devices: Vec<Played>,
    tz: jiff::tz::TimeZone,
}

/// A state value as the file gives it: plain, or `{ "value": …, "ts": … }`
/// as `GET /api/devices` returns it.
fn value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Object(o) => o.get("value").map_or(Value::Null, Value::from_json),
        other => Value::from_json(other),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}

impl Demo {
    /// Reads and checks the file (and the cameras' images next to it).
    pub fn new(config: Config) -> anyhow::Result<Self> {
        ensure!(config.tick_s >= 1, "tick_s: one second at least");
        let tz = jiff::tz::TimeZone::get(&config.timezone)
            .with_context(|| format!("unknown time zone {:?}", config.timezone))?;
        let raw = std::fs::read(&config.fixture)
            .with_context(|| format!("cannot read {}", config.fixture.display()))?;
        let fixture: Fixture = serde_json::from_slice(&raw)
            .with_context(|| format!("{} is not a demo file", config.fixture.display()))?;
        let kind = match fixture.imitates.as_deref() {
            None => "demo",
            Some(k) => IMITABLE
                .iter()
                .copied()
                .find(|known| *known == k)
                .with_context(|| format!("imitates {k:?}: not a driver kind"))?,
        };
        let dir = config.fixture.parent().unwrap_or(Path::new("."));
        let mut devices = Vec::with_capacity(fixture.devices.len());
        let mut seen = std::collections::HashSet::new();
        for d in fixture.devices {
            let id = d.device.id.as_str();
            let native = id.split_once(':').map_or(id, |(_, n)| n).to_owned();
            ensure!(!native.is_empty(), "device {id:?}: empty id");
            ensure!(seen.insert(native.clone()), "device {id:?} twice");
            let mut state = BTreeMap::new();
            for (key, json) in &d.state {
                let v = value(json);
                if let Some(spec) = d.device.point(key)
                    && v != Value::Null
                {
                    let v = moli_core::validate(&spec.kind, &v)
                        .map_err(|e| anyhow::anyhow!("{id} {key}: {e}"))?;
                    state.insert(key.clone(), v);
                }
            }
            let snapshot = match &d.snapshot {
                None => None,
                Some(name) => {
                    ensure!(
                        !name.contains("..") && !name.starts_with(['/', '\\']),
                        "{id}: snapshot {name:?} must stay next to the file"
                    );
                    let path = dir.join(name);
                    let bytes = std::fs::read(&path)
                        .with_context(|| format!("{id}: cannot read {}", path.display()))?;
                    Some(Arc::new(Image {
                        content_type: content_type(&path).to_owned(),
                        bytes,
                    }))
                }
            };
            devices.push(Played {
                native,
                device: d.device,
                state,
                online: d.online.unwrap_or(true),
                snapshot,
            });
        }
        Ok(Self {
            config,
            kind,
            devices,
            tz,
        })
    }

    /// How many devices it plays.
    #[must_use]
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

#[derive(Debug)]
struct Still(Arc<Image>);

impl SnapshotSource for Still {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        let image = Image {
            content_type: self.0.content_type.clone(),
            bytes: self.0.bytes.clone(),
        };
        Box::pin(async move { Ok(image) })
    }
}

impl Driver for Demo {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut ids: Vec<DeviceId> = Vec::with_capacity(self.devices.len());
            for played in &self.devices {
                let id = ctx.device_id(&played.native);
                let mut device = played.device.clone();
                device.id = id.clone();
                device.instance = ctx.instance().clone();
                device.members = device
                    .members
                    .iter()
                    .map(|m| {
                        let s = m.as_str();
                        ctx.device_id(s.split_once(':').map_or(s, |(_, n)| n))
                    })
                    .collect();
                ctx.upsert_device(device);
                // A demo starts where its file says, every time.
                for (key, v) in &played.state {
                    ctx.set_state(&id, key, v.clone());
                }
                if let Some(image) = &played.snapshot {
                    ctx.provide_snapshots(&id, Arc::new(Still(image.clone())));
                }
                ctx.set_availability(&id, played.online);
                ids.push(id);
            }
            ctx.ready();
            let mut world = sim::World::new(&self.devices, &ids, self.tz.clone());
            world.step(ctx, 0.0);
            let mut tick = tokio::time::interval(Duration::from_secs(self.config.tick_s));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tokio::select! {
                    command = ctx.next_command() => {
                        let Some(command) = command else { break };
                        world.obey(ctx, &command.device.id, &command.key, &command.value);
                        command.reply(Ok(()));
                    }
                    _ = tick.tick() => {
                        #[allow(clippy::cast_precision_loss)]
                        world.step(ctx, self.config.tick_s as f64);
                    }
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;

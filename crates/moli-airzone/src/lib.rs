mod api;
pub mod cloud;
mod diagnostic;
pub use diagnostic::{DiagnosticRequest, inspect};
mod fields;
mod model;
mod onboarding;
pub use onboarding::{GatewayProbe, probe_read_only};
mod resources;
mod synchronization;
mod write_policy;
#[cfg(test)]
mod write_policy_tests;
use anyhow::{Context as _, ensure};
use model::Snapshot;
use moli_core::{DeviceId, InstanceId};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
pub use write_policy::WriteMode;
const COMMAND_LIMIT: Duration = Duration::from_millis(4500);
const POLL_LIMIT: Duration = Duration::from_secs(30);
fn invalid_identity() -> String {
    moli_i18n::tr!("airzone.identity")
}
fn remaining(end: Instant) -> anyhow::Result<Duration> {
    end.checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .context(moli_i18n::tr!("airzone.expired"))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    /// Explicit opt-in for commands beyond the four historical zone controls.
    #[serde(default)]
    pub write: WriteMode,
    /// Explicit DHW temperature unit when the controller omits it (0 Celsius, 1 Fahrenheit).
    #[serde(default)]
    pub dhw_units: Option<u8>,
    /// SHA-256 certificate fingerprint; presence selects pinned HTTPS.
    #[serde(default)]
    pub tls_fingerprint: Option<String>,
}
fn default_port() -> u16 {
    3000
}
fn default_poll() -> u64 {
    30
}
#[derive(Debug)]
pub struct Airzone {
    config: Config,
    identity: Mutex<Option<String>>,
    known_zones: Mutex<model::ZoneIds>,
    synchronization: Mutex<synchronization::Progress>,
    read_plan: Mutex<resources::ReadPlan>,
}
impl Airzone {
    pub fn new(mut config: Config) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !config.host.is_empty()
                && config.host.len() <= 253
                && config.host.split('.').all(|label| !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')),
            moli_i18n::tr!("airzone.bad_config")
        );
        anyhow::ensure!(
            config.port > 0 && (5..=3600).contains(&config.poll_secs),
            moli_i18n::tr!("airzone.bad_config")
        );
        ensure!(
            config.dhw_units.is_none_or(|unit| unit <= 1),
            moli_i18n::tr!("airzone.bad_config")
        );
        if let Some(pin) = &config.tls_fingerprint {
            let normalized: String = pin
                .chars()
                .filter(|c| *c != ':' && !c.is_ascii_whitespace())
                .collect();
            ensure!(
                pin.len() <= 128
                    && normalized.len() == 64
                    && normalized.bytes().all(|b| b.is_ascii_hexdigit()),
                moli_i18n::tr!("airzone.bad_config")
            );
        }
        if let Some(mac) = config.mac {
            config.mac = Some(
                moli_net::lan::mac_hex(&mac)
                    .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("airzone.bad_config")))?,
            );
        }
        Ok(Self {
            identity: Mutex::new(config.mac.clone()),
            known_zones: Mutex::default(),
            read_plan: Mutex::default(),
            synchronization: Mutex::new(synchronization::Progress {
                poll_interval: config.poll_secs,
                ..synchronization::Progress::default()
            }),
            config,
        })
    }
}
impl Driver for Airzone {
    fn kind(&self) -> &'static str {
        "airzone"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.run_inner(ctx))
    }
}
enum Next {
    Command(Option<CommandRequest>),
    Core(anyhow::Result<CoreSnapshot>),
    Resources(Box<resources::ResourceSnapshot>),
    Tick,
}
struct CoreSnapshot {
    mac: String,
    info: serde_json::Value,
    zones: Snapshot,
    end: Instant,
}
#[derive(Debug)]
struct LiveSnapshot {
    zones: Snapshot,
    resources: resources::ResourceSnapshot,
}
impl LiveSnapshot {
    fn entities(
        &self,
        instance: &moli_core::InstanceId,
        mac: &str,
        config: &Config,
    ) -> Vec<model::Entity> {
        let mut entities = self.zones.entities(instance, mac);
        config.restrict_writes(&mut entities, true);
        let mut resources = self
            .resources
            .entities(instance, mac, &self.zones, config.read_only);
        config.restrict_writes(&mut resources, false);
        entities.extend(resources);
        entities
    }
    fn encode(
        &self,
        instance: &moli_core::InstanceId,
        mac: &str,
        device: &DeviceId,
        key: &str,
        value: &moli_core::Value,
    ) -> anyhow::Result<(&'static str, serde_json::Value)> {
        if self
            .zones
            .entities(instance, mac)
            .iter()
            .any(|(d, _)| &d.id == device)
        {
            return self
                .zones
                .encode(instance, mac, device, key, value)
                .map(|body| ("hvac", body));
        }
        self.resources
            .encode(instance, mac, &self.zones, device, key, value)
    }
}
fn publish_resources(
    ctx: &DriverCtx,
    resources: &resources::ResourceSnapshot,
    zones: &Snapshot,
    mac: &str,
    config: &Config,
    progress: &synchronization::Progress,
) {
    let prefix = format!("{}:{mac}_s", ctx.instance());
    let managed = ctx
        .devices()
        .into_iter()
        .filter(|id| {
            !id.as_str()
                .strip_prefix(&prefix)
                .is_some_and(|tail| tail.contains("_z"))
        })
        .filter(|id| resources.attempted_device(id, mac))
        .collect::<Vec<_>>();
    let mut entities = resources.entities(ctx.instance(), mac, zones, config.read_only);
    config.restrict_writes(&mut entities, false);
    publish_entities(ctx, entities, &managed, progress);
}
fn publish_core(
    ctx: &DriverCtx,
    snapshot: &Snapshot,
    mac: &str,
    config: &Config,
    progress: &synchronization::Progress,
) {
    let prefix = format!("{}:{mac}_s", ctx.instance());
    let managed = ctx
        .devices()
        .into_iter()
        .filter(|id| {
            id.as_str()
                .strip_prefix(&prefix)
                .is_some_and(|tail| tail.contains("_z"))
        })
        .collect::<Vec<_>>();
    let mut entities = snapshot.entities(ctx.instance(), mac);
    config.restrict_writes(&mut entities, true);
    publish_entities(ctx, entities, &managed, progress);
}
fn publish_entities(
    ctx: &DriverCtx,
    mut entities: Vec<model::Entity>,
    managed: &[DeviceId],
    progress: &synchronization::Progress,
) {
    for entity in &mut entities {
        let gateway = entity.0.id.as_str().ends_with("_gateway");
        progress.append(entity, ctx, gateway, true);
    }
    let present = entities
        .iter()
        .map(|(d, _)| d.id.clone())
        .collect::<Vec<DeviceId>>();
    for id in managed {
        if !present.contains(id) {
            ctx.set_availability(id, false);
        }
    }
    for (device, states) in entities {
        let id = device.id.clone();
        ctx.upsert_device(device);
        for (key, value) in states {
            ctx.set_state(&id, &key, value);
        }
        ctx.set_availability(&id, true);
    }
}
impl Airzone {
    fn progress(&self) -> synchronization::Progress {
        self.synchronization
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn track(&self, ctx: &DriverCtx, update: impl FnOnce(&mut synchronization::Progress)) {
        update(
            &mut self
                .synchronization
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let mac = self
            .identity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .or_else(|| ctx.known_address().and_then(|address| address.mac));
        let Some(mac) = mac else { return };
        let id = ctx.device_id(&format!("{mac}_gateway"));
        let Some(device) = ctx.device(&id) else {
            return;
        };
        let mut entity = ((*device).clone(), Vec::new());
        self.progress().append(&mut entity, ctx, true, false);
        ctx.upsert_device(entity.0);
        for (key, value) in entity.1 {
            ctx.set_state(&id, &key, value);
        }
    }

    async fn reported_controller(
        &self,
        end: Instant,
    ) -> anyhow::Result<(String, serde_json::Value)> {
        let info = api::exchange(
            &self.config,
            "POST",
            "webserver",
            &json!({}),
            remaining(end)?,
        )
        .await?;
        self.synchronization
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .received();
        let mac = info["mac"]
            .as_str()
            .and_then(moli_net::lan::mac_hex)
            .context(api::Failure::Identity)?;
        Ok((mac, info))
    }
    async fn zones(
        &self,
        instance: &InstanceId,
        known: &[DeviceId],
        mac: &str,
        end: Instant,
    ) -> anyhow::Result<Snapshot> {
        let value = api::exchange(
            &self.config,
            "POST",
            "hvac",
            &json!({"systemID":0,"zoneID":0}),
            remaining(end)?,
        )
        .await?;
        self.synchronization
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .received();
        let mut snapshot = Snapshot::parse(&value)?;
        snapshot.retain_known_scope(
            instance,
            mac,
            known,
            &mut self
                .known_zones
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        Ok(snapshot)
    }
    async fn read_core(
        &self,
        instance: &InstanceId,
        known: &[DeviceId],
        expected: Option<&str>,
        end: Instant,
    ) -> anyhow::Result<CoreSnapshot> {
        let (mac, info) = self.reported_controller(end).await?;
        ensure!(
            expected.is_none_or(|expected| expected == mac),
            api::Failure::Identity
        );
        let zones = self.zones(instance, known, &mac, end).await?;
        Ok(CoreSnapshot {
            mac,
            info,
            zones,
            end,
        })
    }
    async fn execute(
        &self,
        ctx: &DriverCtx,
        mac: &str,
        command: &CommandRequest,
    ) -> anyhow::Result<()> {
        ensure!(!self.config.read_only, moli_i18n::tr!("airzone.read_only"));
        ensure!(
            self.config.write.allows_point(&command.key, true),
            api::Failure::ExtendedWritesDisabled
        );
        ensure!(!command.abandoned(), moli_i18n::tr!("airzone.expired"));
        let end = command.deadline.min(Instant::now() + COMMAND_LIMIT);
        let (reported, info) = self.reported_controller(end).await?;
        ensure!(reported == mac, invalid_identity());
        let zones = self
            .zones(ctx.instance(), &ctx.known_devices(), mac, end)
            .await?;
        self.synchronization
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .core();
        publish_core(ctx, &zones, mac, &self.config, &self.progress());
        self.track(ctx, |_| {});
        let historical_resource = zones
            .entities(ctx.instance(), mac)
            .iter()
            .any(|(device, _)| device.id == command.device.id);
        ensure!(
            self.config
                .write
                .allows_point(&command.key, historical_resource),
            api::Failure::ExtendedWritesDisabled
        );
        let resources = if historical_resource {
            resources::ResourceSnapshot::default()
        } else {
            let prefix = format!("{}:", ctx.instance());
            let native = command
                .device
                .id
                .as_str()
                .strip_prefix(&prefix)
                .context(moli_i18n::tr!("airzone.stale"))?;
            let mut resources = resources::ResourceSnapshot::read_for(
                &self.config,
                &zones,
                &info,
                native,
                mac,
                end,
            )
            .await;
            resources.retain_known_scope(ctx.instance(), mac, &zones, &ctx.known_devices());
            resources
        };
        let snapshot = LiveSnapshot { zones, resources };
        let entities = snapshot.entities(ctx.instance(), mac, &self.config);
        let (current, _) = entities
            .iter()
            .find(|(d, _)| d.id == command.device.id)
            .context(moli_i18n::tr!("airzone.stale"))?;
        ensure!(
            current
                .point(&command.key)
                .is_some_and(|point| point.access.write),
            moli_i18n::tr!("airzone.stale")
        );
        ensure!(
            current.members == command.device.members,
            moli_i18n::tr!("airzone.stale")
        );
        // The raw value retains the unit used at admission, even when the new
        // range overlaps. Never reinterpret it under a refreshed unit.
        ensure!(
            current
                .point(&command.key)
                .zip(command.device.point(&command.key))
                .is_some_and(|(current, admitted)| current.unit == admitted.unit),
            moli_i18n::tr!("airzone.stale")
        );
        let (path, body) = snapshot.encode(
            ctx.instance(),
            mac,
            &command.device.id,
            &command.key,
            &command.value,
        )?;
        // No awaited work occurs between this check and starting the one PUT.
        ensure!(!command.abandoned(), moli_i18n::tr!("airzone.expired"));
        let value = api::exchange(&self.config, "PUT", path, &body, remaining(end)?).await?;
        api::acknowledge(&value, &body)?;
        ctx.set_state(
            &command.device.id,
            "sync_last_command_generation",
            moli_core::Value::Int(i64::try_from(self.progress().generation).unwrap_or(i64::MAX)),
        );
        Ok(())
    }
    async fn run_inner(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        // A driver object may be recreated while cached telemetry survives.
        // Never reuse an earlier generation after a restart or mode change.
        let previous = ctx
            .devices()
            .iter()
            .filter_map(|id| match ctx.current(id, "sync_generation") {
                Some(moli_core::Value::Int(value)) => u64::try_from(value).ok(),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        {
            let mut progress = self
                .synchronization
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            progress.generation = progress.generation.max(previous);
        }
        let period = Duration::from_secs(self.config.poll_secs);
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut core: Option<CoreSnapshot> = None;
        let mut refresh_pending = true;
        let mut end = Instant::now() + POLL_LIMIT;
        self.track(ctx, synchronization::Progress::begin);
        loop {
            let instance = ctx.instance().clone();
            let known = ctx.known_devices();
            let expected = self
                .identity
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .or_else(|| ctx.known_address().and_then(|address| address.mac));
            // Required observations are published before optional metadata.
            // Only one read phase runs; a command cancels it and starts a new
            // post-command generation, so an old phase cannot overwrite it.
            let next = tokio::select! {
                biased;
                command=ctx.next_command()=>Next::Command(command),
                result=self.read_core(&instance,&known,expected.as_deref(),end), if refresh_pending=>Next::Core(result),
                resources=async {
                    let core = core.as_ref().expect("optional phase has a core snapshot");
                    let mut plan = self.read_plan.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    let snapshot = resources::ResourceSnapshot::read_planned(&self.config, &core.zones, &core.info, core.end.min(Instant::now() + period), usize::try_from(self.progress().generation.saturating_sub(1)).unwrap_or(0), &mut plan).await;
                    self.synchronization.lock().unwrap_or_else(PoisonError::into_inner).iaq_systems = plan.iaq_system_count();
                    *self.read_plan.lock().unwrap_or_else(PoisonError::into_inner) = plan;
                    snapshot
                }, if core.is_some()=>Next::Resources(Box::new(resources)),
                _=interval.tick(), if !refresh_pending && core.is_none()=>Next::Tick,
            };
            match next {
                Next::Command(Some(command)) => {
                    core = None;
                    self.track(ctx, |progress| progress.active = false);
                    let result = match expected {
                        Some(mac) => self.execute(ctx, &mac, &command).await,
                        None => Err(api::Failure::Identity.into()),
                    };
                    command.reply(result.as_ref().copied().map_err(ToString::to_string));
                    refresh_pending = true;
                    end = Instant::now() + POLL_LIMIT;
                    self.track(ctx, synchronization::Progress::begin);
                }
                Next::Command(None) => return Ok(()),
                Next::Core(result) => {
                    let value = match result {
                        Ok(value) => value,
                        Err(error) => {
                            self.track(ctx, |progress| progress.failed(&error));
                            return Err(error);
                        }
                    };
                    self.accept_core(ctx, &value).await;
                    core = Some(value);
                    refresh_pending = false;
                }
                Next::Resources(mut resources) => {
                    let core = core.take().expect("optional phase has a core snapshot");
                    resources.retain_known_scope(ctx.instance(), &core.mac, &core.zones, &known);
                    self.synchronization
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .complete();
                    publish_resources(
                        ctx,
                        &resources,
                        &core.zones,
                        &core.mac,
                        &self.config,
                        &self.progress(),
                    );
                }
                Next::Tick => {
                    refresh_pending = true;
                    end = Instant::now() + POLL_LIMIT;
                    self.track(ctx, synchronization::Progress::begin);
                }
            }
        }
    }

    async fn accept_core(&self, ctx: &DriverCtx, core: &CoreSnapshot) {
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = Some(core.mac.clone());
        ctx.remember_address(KnownAddress {
            configured: self.config.host.clone(),
            host: self.config.host.clone(),
            mac: Some(core.mac.clone()),
            identity: Some(core.mac.clone()),
        })
        .await;
        self.synchronization
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .core();
        self.synchronization
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .optional_systems = u64::try_from(core.zones.system_ids().len()).unwrap_or(32);
        let progress = self.progress();
        publish_core(ctx, &core.zones, &core.mac, &self.config, &progress);
        let gateway = ctx.device_id(&format!("{}_gateway", core.mac));
        if ctx.device(&gateway).is_none() {
            let initial = resources::ResourceSnapshot::from_webserver(&self.config, &core.info)
                .gateway(
                    ctx.instance(),
                    &core.mac,
                    &core.zones,
                    self.config.read_only,
                );
            let mut entities = vec![initial];
            self.config.restrict_writes(&mut entities, false);
            publish_entities(ctx, entities, &[], &progress);
        } else {
            self.track(ctx, |_| {});
        }
        ctx.ready();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_read_only_configuration_is_supported() {
        assert!(
            serde_json::from_value::<Config>(json!({
                "host":"controller.example", "read_only":true
            }))
            .is_ok()
        );
    }
    #[test]
    fn omitted_read_only_preserves_legacy_toml_command_mode() {
        let config: Config = serde_json::from_value(json!({"host":"controller.example"})).unwrap();
        assert!(!config.read_only);
        assert_eq!(config.write, WriteMode::Legacy);
        let config: Config =
            serde_json::from_value(json!({"host":"controller.example", "read_only":false}))
                .unwrap();
        assert!(!config.read_only);
        assert_eq!(config.write, WriteMode::Legacy);
    }
    #[test]
    fn extended_writes_require_a_recognized_explicit_option() {
        let config: Config =
            serde_json::from_value(json!({"host":"controller.example", "write":"extended"}))
                .unwrap();
        assert_eq!(config.write, WriteMode::Extended);
        for write in [json!(true), json!("all"), json!(["name"]), json!(null)] {
            assert!(
                serde_json::from_value::<Config>(
                    json!({"host":"controller.example", "write":write})
                )
                .is_err()
            );
        }
    }
    #[test]
    fn constructor_validates_without_network() {
        let good = Config {
            host: "controller.example".into(),
            port: 3000,
            poll_secs: 30,
            mac: Some("aa:bb:cc:dd:ee:ff".into()),
            read_only: true,
            write: WriteMode::Legacy,
            dhw_units: None,
            tls_fingerprint: None,
        };
        assert!(Airzone::new(good.clone()).is_ok());
        for host in [
            "",
            "http://controller.example",
            "host/path",
            "host:3000",
            "host\r\nevil",
            " host",
        ] {
            let mut c = good.clone();
            c.host = host.into();
            assert!(Airzone::new(c).is_err());
        }
        let mut c = good.clone();
        c.port = 0;
        assert!(Airzone::new(c).is_err());
        let mut c = good.clone();
        c.poll_secs = 0;
        assert!(Airzone::new(c).is_err());
        let mut c = good.clone();
        c.poll_secs = 3601;
        assert!(Airzone::new(c).is_err());
        let mut c = good;
        c.mac = Some("invalid".into());
        assert!(Airzone::new(c).is_err());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use axum::{Json as Reply, Router, extract::State, http::Method, routing::any};
    use moli_core::{DeviceId, DriverStatus, Event, InstanceId, Origin, PointId, Value};
    use moli_runtime::{Hub, HubOptions, spawn_driver};
    use serde_json::{Value as Json, json};
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    use tokio_util::sync::CancellationToken;
    mod synchronization_tests;
    #[derive(Clone)]
    struct Sim {
        zones: Arc<Mutex<Json>>,
        mac: Arc<Mutex<String>>,
        webserver_extra: Arc<Mutex<Json>>,
        writes: Arc<Mutex<Vec<Json>>>,
        reject: Arc<Mutex<bool>>,
        delay: Arc<Mutex<u64>>,
        optional_delay: Arc<Mutex<u64>>,
        optional_started: Arc<AtomicUsize>,
        optional_notified: Arc<tokio::sync::Notify>,
        response_status: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
    }
    async fn start() -> (Hub, CancellationToken, Sim, tokio::task::JoinHandle<()>) {
        start_with_zones(
            serde_json::from_str(include_str!(
                "../../../integrations/airzone/fixtures/zones.json"
            ))
            .unwrap(),
        )
        .await
    }
    async fn start_with_zones(
        zones: Json,
    ) -> (Hub, CancellationToken, Sim, tokio::task::JoinHandle<()>) {
        start_with_hub(zones, Hub::new(HubOptions::default()).unwrap()).await
    }
    async fn start_with_hub(
        zones: Json,
        hub: Hub,
    ) -> (Hub, CancellationToken, Sim, tokio::task::JoinHandle<()>) {
        start_with_settings(zones, hub, false, WriteMode::Legacy).await
    }
    async fn start_with_settings(
        zones: Json,
        hub: Hub,
        read_only: bool,
        write: WriteMode,
    ) -> (Hub, CancellationToken, Sim, tokio::task::JoinHandle<()>) {
        let sim = Sim {
            zones: Arc::new(Mutex::new(zones)),
            mac: Arc::new(Mutex::new("aa:bb:cc:dd:ee:ff".into())),
            webserver_extra: Arc::new(Mutex::new(json!({"cloud_connected":true}))),
            writes: Arc::default(),
            reject: Arc::default(),
            delay: Arc::default(),
            optional_delay: Arc::default(),
            optional_started: Arc::default(),
            optional_notified: Arc::default(),
            response_status: Arc::new(AtomicUsize::new(200)),
            reads: Arc::default(),
        };
        let app = Router::new()
            .route(
                "/api/v1/version",
                any(|State(s): State<Sim>| async move {
                    let delay = *s.optional_delay.lock().unwrap();
                    s.optional_started.fetch_add(1, Ordering::SeqCst);
                    s.optional_notified.notify_one();
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    Reply(json!({"version":"1.78"}))
                }),
            )
            .route(
                "/api/v1/webserver",
                any(|State(s): State<Sim>, method: Method| async move {
                    assert_eq!(method, Method::POST);
                    let delay = *s.delay.lock().unwrap();
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    let mut info = s.webserver_extra.lock().unwrap().clone();
                    info["mac"] = json!(*s.mac.lock().unwrap());
                    (
                        axum::http::StatusCode::from_u16(
                            u16::try_from(s.response_status.load(Ordering::SeqCst)).unwrap(),
                        )
                        .unwrap(),
                        Reply(info),
                    )
                }),
            )
            .route(
                "/api/v1/hvac",
                any(
                    |State(s): State<Sim>, method: Method, Reply(body): Reply<Json>| async move {
                        let delay = *s.delay.lock().unwrap();
                        if method == Method::POST {
                            if body != json!({"systemID":0,"zoneID":0}) {
                                return Reply(json!({"errors":[{"error":"method not supported"}]}));
                            }
                            s.reads.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            Reply(s.zones.lock().unwrap().clone())
                        } else {
                            assert_eq!(method, Method::PUT);
                            s.writes.lock().unwrap().push(body.clone());
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            if *s.reject.lock().unwrap() {
                                Reply(json!({"data":body,"errors":{"on":"rejected"}}))
                            } else {
                                Reply(json!({"data":[body]}))
                            }
                        }
                    },
                ),
            )
            .with_state(sim.clone());
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(tcp, app).await.unwrap();
        });
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("az"),
            Arc::new(
                Airzone::new(Config {
                    host: "127.0.0.1".into(),
                    port,
                    poll_secs: 5,
                    mac: None,
                    read_only,
                    write,
                    dhw_units: None,
                    tls_fingerprint: None,
                })
                .unwrap(),
            ),
            cancel.clone(),
        );
        for _ in 0..100 {
            if hub.stats().drivers_running == 1 {
                return (hub, cancel, sim, server);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("not running: {:?}", hub.snapshot().drivers);
    }
    fn point(device: &str, key: &str) -> PointId {
        PointId::new(&DeviceId::from(device), key)
    }
    async fn wait_for_optional_read(sim: &Sim, after: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while sim.optional_started.load(Ordering::SeqCst) <= after {
                sim.optional_notified.notified().await;
            }
        })
        .await
        .expect("optional read started");
    }
    #[tokio::test]
    async fn slow_optional_metadata_does_not_delay_a_zone_command() {
        let (hub, cancel, sim, server) = start().await;
        // Startup publishes the core before the optional phase. Observe that
        // first request explicitly instead of assuming it preceded ready().
        wait_for_optional_read(&sim, 0).await;
        let optional_before = sim.optional_started.load(Ordering::SeqCst);
        *sim.optional_delay.lock().unwrap() = 6000;
        let started = Instant::now();
        hub.command(
            &point("az:aabbccddeeff_s1_z2", "on"),
            Value::Bool(true),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert_eq!(sim.writes.lock().unwrap().len(), 1);
        wait_for_optional_read(&sim, optional_before).await;
        let started = Instant::now();
        hub.command(
            &point("az:aabbccddeeff_s1_z2", "on"),
            Value::Bool(false),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "command interrupts optional poll"
        );
        assert_eq!(sim.writes.lock().unwrap().len(), 2);
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn legacy_default_only_publishes_historical_commands() {
        let (hub, cancel, sim, server) = start().await;
        let snapshot = hub.snapshot();
        let writable = snapshot
            .devices
            .iter()
            .flat_map(|d| &d.device.points)
            .filter(|p| p.access.write)
            .map(|p| p.key.as_ref())
            .collect::<Vec<_>>();
        assert!(writable.contains(&"on"));
        assert!(writable.contains(&"target_temperature"));
        assert!(writable.contains(&"mode"));
        assert!(writable.contains(&"fan_speed"));
        assert!(
            writable
                .iter()
                .all(|key| matches!(*key, "on" | "target_temperature" | "mode" | "fan_speed")),
            "{writable:?}"
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        for (device, key, value) in [
            ("az:aabbccddeeff_s1_z2", "on", Value::Bool(false)),
            (
                "az:aabbccddeeff_s1_z2",
                "target_temperature",
                Value::Float(22.5),
            ),
            ("az:aabbccddeeff_s1_z8_control", "mode", Value::from("heat")),
            (
                "az:aabbccddeeff_s2_z8_control",
                "fan_speed",
                Value::from("2"),
            ),
        ] {
            hub.command(&point(device, key), value, Origin::Ui, None)
                .await
                .unwrap();
        }
        assert_eq!(
            *sim.writes.lock().unwrap(),
            vec![
                json!({"systemID":1,"zoneID":2,"on":0}),
                json!({"systemID":1,"zoneID":2,"setpoint":22.5}),
                json!({"systemID":1,"zoneID":8,"mode":3}),
                json!({"systemID":2,"zoneID":8,"speed":2}),
            ]
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn extended_opt_in_enables_renaming_but_read_only_still_dominates() {
        for read_only in [false, true] {
            let zones = serde_json::from_str(include_str!(
                "../../../integrations/airzone/fixtures/zones.json"
            ))
            .unwrap();
            let (hub, cancel, sim, server) = start_with_settings(
                zones,
                Hub::new(HubOptions::default()).unwrap(),
                read_only,
                WriteMode::Extended,
            )
            .await;
            let device = hub
                .device(&DeviceId::from("az:aabbccddeeff_s1_z2"))
                .unwrap();
            assert_eq!(
                device.device.point("name").unwrap().access.write,
                !read_only
            );
            let result = hub
                .command(
                    &point("az:aabbccddeeff_s1_z2", "name"),
                    Value::from("Zone D"),
                    Origin::Ui,
                    None,
                )
                .await;
            assert_eq!(result.is_ok(), !read_only);
            assert_eq!(sim.writes.lock().unwrap().len(), usize::from(!read_only));
            if !read_only {
                assert_eq!(
                    sim.writes.lock().unwrap()[0],
                    json!({"systemID":1,"zoneID":2,"name":"Zone D"})
                );
            }
            cancel.cancel();
            server.abort();
        }
    }
    #[tokio::test]
    async fn read_only_publishes_no_writable_points_and_rejects_every_origin() {
        let zones = serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap();
        let (hub, cancel, sim, server) = start_with_settings(
            zones,
            Hub::new(HubOptions::default()).unwrap(),
            true,
            WriteMode::Legacy,
        )
        .await;
        assert!(
            hub.snapshot().devices.iter().all(|device| device
                .device
                .points
                .iter()
                .all(|point| !point.access.write))
        );
        for origin in [
            Origin::Ui,
            Origin::Household,
            Origin::Api,
            Origin::Mcp,
            Origin::Cli,
            Origin::System,
            Origin::Assistant,
            Origin::Automation,
        ] {
            assert!(
                hub.command(
                    &point("az:aabbccddeeff_s1_z2", "on"),
                    Value::Bool(true),
                    origin,
                    None
                )
                .await
                .is_err()
            );
        }
        assert!(sim.writes.lock().unwrap().is_empty());
        cancel.cancel();
        server.abort();
    }
    fn assert_no_restart(events: &mut tokio::sync::broadcast::Receiver<Event>) {
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, Event::DeviceRemoved { .. }));
            if let Event::DriverStatus { status, .. } = event {
                assert_eq!(status, DriverStatus::Running);
            }
        }
    }
    async fn wait_for_backoff(events: &mut tokio::sync::broadcast::Receiver<Event>) {
        wait_for_status(events, |status| {
            matches!(status, DriverStatus::Backoff { .. })
        })
        .await;
    }
    async fn wait_for_status(
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        wanted: fn(&DriverStatus) -> bool,
    ) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    events.recv().await.unwrap(),
                    Event::DriverStatus {
                        status,
                        ..
                    } if wanted(&status)
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn refused_command_refreshes_and_keeps_all_zones_available() {
        for refused_by_api in [false, true] {
            let (hub, cancel, sim, server) = start().await;
            let mut events = hub.subscribe();
            *sim.reject.lock().unwrap() = refused_by_api;
            let (key, value) = if refused_by_api {
                ("on", Value::Bool(false))
            } else {
                ("target_temperature", Value::Float(21.3))
            };
            assert!(
                hub.command(
                    &point("az:aabbccddeeff_s1_z2", key),
                    value,
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
            );
            *sim.reject.lock().unwrap() = false;
            hub.command(
                &point("az:aabbccddeeff_s2_z8", "on"),
                Value::Bool(true),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
            // Confirmation may yield to a queued command, but must follow it.
            for _ in 0..100 {
                if sim.reads.load(Ordering::SeqCst) >= 4 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(sim.reads.load(Ordering::SeqCst) >= 4);
            assert_eq!(
                sim.writes.lock().unwrap().len(),
                if refused_by_api { 2 } else { 1 }
            );
            assert_eq!(hub.stats().drivers_running, 1);
            while let Ok(event) = events.try_recv() {
                assert!(!matches!(
                    event,
                    Event::Availability { online: false, .. } | Event::DeviceRemoved { .. }
                ));
                if let Event::DriverStatus { status, .. } = event {
                    assert_eq!(status, DriverStatus::Running);
                }
            }
            cancel.cancel();
            server.abort();
        }
    }
    #[tokio::test]
    async fn malformed_zone_disables_its_shared_control_and_recovers_without_restart() {
        let (hub, cancel, sim, server) = start().await;
        let mut events = hub.subscribe();
        let good = sim.zones.lock().unwrap().clone();
        sim.zones.lock().unwrap()["systems"][0]["data"][0]["roomTemp"] = Json::Null;
        // The admitted shared command must not use stale or reduced membership.
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z8_control", "mode"),
                Value::from("heat"),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        hub.command(
            &point("az:aabbccddeeff_s2_z8", "on"),
            Value::Bool(true),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        for id in ["az:aabbccddeeff_s1_z2", "az:aabbccddeeff_s1_z8_control"] {
            assert_eq!(hub.device(&DeviceId::from(id)).unwrap().online, Some(false));
        }
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z2", "on"),
                Value::Bool(false),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert_eq!(
            sim.writes.lock().unwrap().as_slice(),
            &[json!({"systemID":2,"zoneID":8,"on":1})]
        );
        for id in [
            "az:aabbccddeeff_s1_z8",
            "az:aabbccddeeff_s2_z8",
            "az:aabbccddeeff_s2_z8_control",
        ] {
            assert_eq!(hub.device(&DeviceId::from(id)).unwrap().online, Some(true));
        }
        *sim.zones.lock().unwrap() = good;
        hub.command(
            &point("az:aabbccddeeff_s2_z8", "on"),
            Value::Bool(false),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        for id in ["az:aabbccddeeff_s1_z2", "az:aabbccddeeff_s1_z8_control"] {
            assert_eq!(hub.device(&DeviceId::from(id)).unwrap().online, Some(true));
        }
        assert_no_restart(&mut events);
        assert_eq!(sim.writes.lock().unwrap().len(), 2);
        cancel.cancel();
        server.abort();
    }
    async fn refresh_using_independent_zone(hub: &Hub) {
        hub.command(
            &point("az:aabbccddeeff_s2_z8", "on"),
            Value::Bool(true),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
    }
    fn assert_missing_member_disables_shared_control(hub: &Hub) {
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z8_control"))
                .unwrap()
                .online,
            Some(false)
        );
        assert_eq!(
            hub.rooms_of(&DeviceId::from("az:aabbccddeeff_s1_z8_control")),
            vec!["Protected"]
        );
    }
    #[tokio::test]
    async fn disappeared_known_zones_keep_shared_controls_disabled_across_restarts() {
        for malformed_first in [false, true] {
            let (hub, cancel, sim, server) = start().await;
            let good = sim.zones.lock().unwrap().clone();
            hub.set_label(
                &DeviceId::from("az:aabbccddeeff_s1_z2"),
                moli_runtime::LabelPatch {
                    name: None,
                    room: Some("Protected".into()),
                },
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
            if malformed_first {
                sim.zones.lock().unwrap()["systems"][0]["data"][0]["roomTemp"] = Json::Null;
                refresh_using_independent_zone(&hub).await;
                assert_missing_member_disables_shared_control(&hub);
            }
            sim.zones.lock().unwrap()["systems"][0]["data"]
                .as_array_mut()
                .unwrap()
                .remove(0);
            let missing = sim.zones.lock().unwrap().clone();
            // Direct disappearance must fail in preflight; a previously hidden
            // control must not reappear with reduced members on the next read.
            assert!(
                hub.command(
                    &point("az:aabbccddeeff_s1_z8_control", "mode"),
                    Value::from("heat"),
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
            );
            refresh_using_independent_zone(&hub).await;
            assert_missing_member_disables_shared_control(&hub);
            let mut events = hub.subscribe();
            *sim.zones.lock().unwrap() = json!({});
            assert!(
                hub.command(
                    &point("az:aabbccddeeff_s2_z8", "on"),
                    Value::Bool(true),
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
            );
            wait_for_backoff(&mut events).await;
            *sim.zones.lock().unwrap() = missing;
            wait_for_status(&mut events, |status| *status == DriverStatus::Running).await;
            assert_missing_member_disables_shared_control(&hub);
            assert!(
                hub.command(
                    &point("az:aabbccddeeff_s1_z8_control", "mode"),
                    Value::from("heat"),
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
            );
            assert!(
                sim.writes
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|body| body == &json!({"systemID":2,"zoneID":8,"on":1}))
            );
            *sim.zones.lock().unwrap() = good;
            refresh_using_independent_zone(&hub).await;
            assert_eq!(
                hub.device(&DeviceId::from("az:aabbccddeeff_s1_z8_control"))
                    .unwrap()
                    .online,
                Some(true)
            );
            assert_eq!(
                hub.rooms_of(&DeviceId::from("az:aabbccddeeff_s1_z8_control")),
                vec!["Protected"]
            );
            cancel.cancel();
            server.abort();
        }
    }
    #[tokio::test]
    async fn malformed_zone_identity_is_retained_even_before_first_publication() {
        let mut zones: Json = serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap();
        zones["systems"][0]["data"][0]["roomTemp"] = Json::Null;
        let (hub, cancel, sim, server) = start_with_zones(zones).await;
        sim.zones.lock().unwrap()["systems"][0]["data"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        refresh_using_independent_zone(&hub).await;
        assert!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z8_control"))
                .is_none()
        );
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z8_control", "mode"),
                Value::from("heat"),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert_eq!(sim.writes.lock().unwrap().len(), 1);
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn new_driver_retains_missing_members_from_known_devices() {
        let (hub, cancel, sim, server) = start().await;
        let good = sim.zones.lock().unwrap().clone();
        let mut missing = good.clone();
        missing["systems"][0]["data"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let mut events = hub.subscribe();
        cancel.cancel();
        wait_for_status(&mut events, |status| *status == DriverStatus::Stopped).await;
        server.abort();
        // A new Airzone object has no in-memory inventory; the hub still knows
        // the native zones (and controls) published by the previous object.
        let (hub, cancel, sim, server) = start_with_hub(missing, hub).await;
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z8_control"))
                .unwrap()
                .online,
            Some(false)
        );
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z8_control", "mode"),
                Value::from("heat"),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        *sim.zones.lock().unwrap() = good;
        refresh_using_independent_zone(&hub).await;
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z8_control"))
                .unwrap()
                .online,
            Some(true)
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn writes_use_fresh_snapshot_and_ack_never_fabricates_state() {
        let (hub, cancel, sim, server) = start().await;
        let p = point("az:aabbccddeeff_s1_z2", "on");
        hub.command(&p, Value::Bool(false), Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(
            *sim.writes.lock().unwrap(),
            vec![json!({"systemID":1,"zoneID":2,"on":0})]
        );
        assert_eq!(hub.state(&p).unwrap().value, Value::Bool(true));
        sim.zones.lock().unwrap()["systems"][0]["data"][0]["maxTemp"] = json!(21.);
        // Keep the snapshot valid; the new range still refuses 25 degrees.
        sim.zones.lock().unwrap()["systems"][0]["data"][0]["setpoint"] = json!(20.);
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z2", "target_temperature"),
                Value::Float(25.),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert_eq!(sim.writes.lock().unwrap().len(), 1);
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn changed_temperature_unit_refuses_write_before_put() {
        let mut zones: Json = serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap();
        let zone = &mut zones["systems"][0]["data"][0];
        zone["minTemp"] = json!(5.);
        zone["maxTemp"] = json!(50.);
        zone["temp_step"] = json!(1.);
        let (hub, cancel, sim, server) = start_with_zones(zones).await;
        {
            let mut zones = sim.zones.lock().unwrap();
            let zone = &mut zones["systems"][0]["data"][0];
            zone["units"] = json!(1);
            zone["minTemp"] = json!(41.);
            zone["maxTemp"] = json!(122.);
            zone["temp_step"] = json!(1.);
            zone["roomTemp"] = json!(70.);
            zone["setpoint"] = json!(72.);
        }
        // 45 is valid in both ranges, but the admitted Celsius command cannot
        // be reinterpreted as Fahrenheit by the preflight snapshot.
        let result = hub
            .command(
                &point("az:aabbccddeeff_s1_z2", "target_temperature"),
                Value::Float(45.),
                Origin::Ui,
                None,
            )
            .await;
        assert!(sim.writes.lock().unwrap().is_empty());
        assert!(result.is_err());
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn missing_temperature_unit_refuses_write_before_put() {
        let (hub, cancel, sim, server) = start().await;
        sim.zones.lock().unwrap()["systems"][0]["data"][0]
            .as_object_mut()
            .unwrap()
            .remove("units");
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z2", "target_temperature"),
                Value::Float(25.),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn unchanged_temperature_unit_allows_refreshed_range() {
        let (hub, cancel, sim, server) = start().await;
        {
            let mut zones = sim.zones.lock().unwrap();
            let zone = &mut zones["systems"][0]["data"][0];
            zone["minTemp"] = json!(5.);
            zone["maxTemp"] = json!(50.);
        }
        hub.command(
            &point("az:aabbccddeeff_s1_z2", "target_temperature"),
            Value::Float(25.),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            *sim.writes.lock().unwrap(),
            vec![json!({"systemID":1,"zoneID":2,"setpoint":25.})]
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn shared_control_guard_sees_all_member_rooms() {
        let (hub, cancel, _sim, server) = start().await;
        hub.set_label(
            &DeviceId::from("az:aabbccddeeff_s1_z2"),
            moli_runtime::LabelPatch {
                name: None,
                room: Some("Protected".into()),
            },
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            hub.rooms_of(&DeviceId::from("az:aabbccddeeff_s1_z8_control")),
            vec!["Protected"]
        );
        assert!(
            hub.rooms_of(&DeviceId::from("az:aabbccddeeff_s1_z8"))
                .is_empty()
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn changed_controller_identity_refuses_writes() {
        let (hub, cancel, sim, server) = start().await;
        let mut events = hub.subscribe();
        let p = point("az:aabbccddeeff_s1_z2", "on");
        *sim.mac.lock().unwrap() = "aa:bb:cc:dd:ee:01".into();
        assert!(
            hub.command(&p, Value::Bool(false), Origin::Ui, None)
                .await
                .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        wait_for_backoff(&mut events).await;
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s2_z8"))
                .unwrap()
                .online,
            Some(false)
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn api_errors_are_not_retried() {
        let (hub, cancel, sim, server) = start().await;
        let p = point("az:aabbccddeeff_s1_z2", "on");
        *sim.reject.lock().unwrap() = true;
        assert!(
            hub.command(&p, Value::Bool(false), Origin::Ui, None)
                .await
                .is_err()
        );
        assert_eq!(sim.writes.lock().unwrap().len(), 1);
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn reordered_same_members_accepts_shared_command() {
        let (hub, cancel, sim, server) = start().await;
        sim.zones.lock().unwrap()["systems"][0]["data"]
            .as_array_mut()
            .unwrap()
            .reverse();
        let control = DeviceId::from("az:aabbccddeeff_s1_z8_control");
        hub.command(
            &PointId::new(&control, "mode"),
            Value::from("off"),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            *sim.writes.lock().unwrap(),
            vec![json!({"systemID":1,"zoneID":8,"mode":1})]
        );
        assert_eq!(
            hub.describe(&control).unwrap().0.members,
            vec![
                DeviceId::from("az:aabbccddeeff_s1_z2"),
                DeviceId::from("az:aabbccddeeff_s1_z8")
            ]
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn shared_scope_changes_are_refused_before_put() {
        let (hub, cancel, sim, server) = start().await;
        // A newly discovered affected zone was not included in the hub's guard decision.
        let mut zone = sim.zones.lock().unwrap()["systems"][0]["data"][0].clone();
        zone["zoneID"] = json!(3);
        sim.zones.lock().unwrap()["systems"][0]["data"]
            .as_array_mut()
            .unwrap()
            .push(zone);
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z8_control", "mode"),
                Value::from("off"),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn recovery_reloads_snapshot_without_replaying_failed_command() {
        let (hub, cancel, sim, server) = start().await;
        let mut events = hub.subscribe();
        let good = sim.zones.lock().unwrap().clone();
        *sim.zones.lock().unwrap() = json!({});
        let p = point("az:aabbccddeeff_s1_z2", "on");
        assert!(
            hub.command(&p, Value::Bool(false), Origin::Ui, None)
                .await
                .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        wait_for_backoff(&mut events).await;
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z2"))
                .unwrap()
                .online,
            Some(false)
        );
        *sim.zones.lock().unwrap() = good;
        for _ in 0..300 {
            if hub.stats().drivers_running == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(hub.stats().drivers_running, 1);
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z2"))
                .unwrap()
                .online,
            Some(true)
        );
        assert!(sim.writes.lock().unwrap().is_empty());
        hub.command(&p, Value::Bool(false), Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(sim.writes.lock().unwrap().len(), 1);
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn preflight_and_put_share_one_command_budget() {
        let (hub, cancel, sim, server) = start().await;
        *sim.delay.lock().unwrap() = 1600;
        let began = Instant::now();
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z2", "on"),
                Value::Bool(false),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert!(began.elapsed() < Duration::from_millis(4900));
        assert_eq!(sim.writes.lock().unwrap().len(), 1);
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn abandoned_during_preflight_never_sends_put() {
        let (hub, cancel, sim, server) = start().await;
        *sim.delay.lock().unwrap() = 300;
        let call = tokio::spawn(async move {
            hub.command(
                &point("az:aabbccddeeff_s1_z2", "on"),
                Value::Bool(false),
                Origin::Ui,
                None,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        call.abort();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(sim.writes.lock().unwrap().is_empty());
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn disappeared_zones_go_offline() {
        let (hub, cancel, sim, server) = start().await;
        let p = point("az:aabbccddeeff_s1_z2", "on");
        sim.zones.lock().unwrap()["systems"][0]["data"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        assert!(
            hub.command(&p, Value::Bool(false), Origin::Ui, None)
                .await
                .is_err()
        );
        assert_eq!(
            hub.device(&DeviceId::from("az:aabbccddeeff_s1_z2"))
                .unwrap()
                .online,
            Some(false)
        );
        cancel.cancel();
        server.abort();
    }
    #[tokio::test]
    async fn controller_transport_failure_goes_offline() {
        let (hub, cancel, _sim, server) = start().await;
        server.abort();
        let began = Instant::now();
        assert!(
            hub.command(
                &point("az:aabbccddeeff_s1_z8", "on"),
                Value::Bool(false),
                Origin::Ui,
                None
            )
            .await
            .is_err()
        );
        assert!(began.elapsed() < Duration::from_secs(5));
        cancel.cancel();
    }
}

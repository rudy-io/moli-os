mod api;
mod model;
use anyhow::{Context as _, ensure};
use model::Snapshot;
use moli_core::DeviceId;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
const COMMAND_LIMIT: Duration = Duration::from_millis(4500);
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
        if let Some(mac) = config.mac {
            config.mac = Some(
                moli_net::lan::mac_hex(&mac)
                    .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("airzone.bad_config")))?,
            );
        }
        Ok(Self {
            identity: Mutex::new(config.mac.clone()),
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
fn publish(ctx: &DriverCtx, snapshot: &Snapshot, mac: &str) {
    let entities = snapshot.entities(ctx.instance(), mac);
    let present = entities
        .iter()
        .map(|(d, _)| d.id.clone())
        .collect::<Vec<DeviceId>>();
    for id in ctx.devices() {
        if !present.contains(&id) {
            ctx.set_availability(&id, false);
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
    async fn reported_identity(&self, end: Instant) -> anyhow::Result<String> {
        let info = api::exchange(
            &self.config,
            "POST",
            "webserver",
            &json!({}),
            remaining(end)?,
        )
        .await?;
        info["mac"]
            .as_str()
            .and_then(moli_net::lan::mac_hex)
            .context(invalid_identity())
    }
    async fn zones(&self, end: Instant) -> anyhow::Result<Snapshot> {
        let value = api::exchange(
            &self.config,
            "POST",
            "hvac",
            &json!({"systemID":0,"zoneID":0}),
            remaining(end)?,
        )
        .await?;
        Snapshot::parse(&value)
    }
    async fn refresh(&self, ctx: &DriverCtx, mac: &str, end: Instant) -> anyhow::Result<Snapshot> {
        ensure!(
            self.reported_identity(end).await? == mac,
            invalid_identity()
        );
        let snapshot = self.zones(end).await?;
        publish(ctx, &snapshot, mac);
        Ok(snapshot)
    }
    async fn execute(
        &self,
        ctx: &DriverCtx,
        mac: &str,
        command: &CommandRequest,
    ) -> anyhow::Result<()> {
        ensure!(!command.abandoned(), moli_i18n::tr!("airzone.expired"));
        let end = command.deadline.min(Instant::now() + COMMAND_LIMIT);
        let snapshot = self.refresh(ctx, mac, end).await?;
        let entities = snapshot.entities(ctx.instance(), mac);
        let (current, _) = entities
            .iter()
            .find(|(d, _)| d.id == command.device.id)
            .context(moli_i18n::tr!("airzone.stale"))?;
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
        let body = snapshot.encode(
            ctx.instance(),
            mac,
            &command.device.id,
            &command.key,
            &command.value,
        )?;
        // No awaited work occurs between this check and starting the one PUT.
        ensure!(!command.abandoned(), moli_i18n::tr!("airzone.expired"));
        let value = api::exchange(&self.config, "PUT", "hvac", &body, remaining(end)?).await?;
        api::acknowledge(&value, &body)
    }
    async fn run_inner(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let end = Instant::now() + COMMAND_LIMIT;
        let mac = self.reported_identity(end).await?;
        let expected = self
            .identity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .or_else(|| ctx.known_address().and_then(|a| a.mac));
        ensure!(
            expected.as_ref().is_none_or(|expected| expected == &mac),
            invalid_identity()
        );
        let snapshot = self.zones(end).await?;
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = Some(mac.clone());
        ctx.remember_address(KnownAddress {
            configured: self.config.host.clone(),
            host: self.config.host.clone(),
            mac: Some(mac.clone()),
            identity: Some(mac.clone()),
        })
        .await;
        publish(ctx, &snapshot, &mac);
        ctx.ready();
        let period = Duration::from_secs(self.config.poll_secs);
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
             cmd=ctx.next_command()=>{
              let Some(command)=cmd else{return Ok(());};
              let result=self.execute(ctx,&mac,&command).await;
              command.reply(result.as_ref().copied().map_err(ToString::to_string));
              result?;
              // Acceptance is separate from confirmation, including for shared mode.
              self.refresh(ctx,&mac,Instant::now()+COMMAND_LIMIT).await?;
             }
             _=interval.tick()=>{self.refresh(ctx,&mac,Instant::now()+COMMAND_LIMIT).await?;}
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_validates_without_network() {
        let good = Config {
            host: "controller.example".into(),
            port: 3000,
            poll_secs: 30,
            mac: Some("aa:bb:cc:dd:ee:ff".into()),
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
    use moli_core::{DeviceId, InstanceId, Origin, PointId, Value};
    use moli_runtime::{Hub, HubOptions, spawn_driver};
    use serde_json::{Value as Json, json};
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    use tokio_util::sync::CancellationToken;
    #[derive(Clone)]
    struct Sim {
        zones: Arc<Mutex<Json>>,
        mac: Arc<Mutex<String>>,
        writes: Arc<Mutex<Vec<Json>>>,
        reject: Arc<Mutex<bool>>,
        delay: Arc<Mutex<u64>>,
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
        let sim = Sim {
            zones: Arc::new(Mutex::new(zones)),
            mac: Arc::new(Mutex::new("aa:bb:cc:dd:ee:ff".into())),
            writes: Arc::default(),
            reject: Arc::default(),
            delay: Arc::default(),
        };
        let app = Router::new()
            .route(
                "/api/v1/webserver",
                any(|State(s): State<Sim>, method: Method| async move {
                    assert_eq!(method, Method::POST);
                    let delay = *s.delay.lock().unwrap();
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    Reply(json!({"mac":*s.mac.lock().unwrap()}))
                }),
            )
            .route(
                "/api/v1/hvac",
                any(
                    |State(s): State<Sim>, method: Method, Reply(body): Reply<Json>| async move {
                        let delay = *s.delay.lock().unwrap();
                        if method == Method::POST {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            assert_eq!(body, json!({"systemID":0,"zoneID":0}));
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
        let hub = Hub::new(HubOptions::default()).unwrap();
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
        let p = point("az:aabbccddeeff_s1_z2", "on");
        *sim.mac.lock().unwrap() = "aa:bb:cc:dd:ee:01".into();
        assert!(
            hub.command(&p, Value::Bool(false), Origin::Ui, None)
                .await
                .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
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
        let good = sim.zones.lock().unwrap().clone();
        *sim.zones.lock().unwrap() = json!({});
        let p = point("az:aabbccddeeff_s1_z2", "on");
        assert!(
            hub.command(&p, Value::Bool(false), Origin::Ui, None)
                .await
                .is_err()
        );
        assert!(sim.writes.lock().unwrap().is_empty());
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

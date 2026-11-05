//! The recorder in a real hub, fed by a fake meter whose readings are set
//! by commands.

use std::sync::Arc;
use std::time::Duration;

use moli_core::{
    Access, Device, InstanceId, Kind, Origin, PointId, PointSpec, Semantic, Unit, Value,
};
use moli_energy::{Config, Energy};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub, HubOptions, spawn_driver};
use tokio_util::sync::CancellationToken;

/// A clamp meter: `e` counts Wh, `ptec` is the tariff period.
struct FakeMeter;

fn spec(key: &str, kind: Kind, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: key.into(),
        kind,
        access: Access {
            read: true,
            write: true,
        },
        unit,
        semantic: Semantic::Other,
    }
}

impl Driver for FakeMeter {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("m");
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "m".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: vec![
                    spec(
                        "e",
                        Kind::Numeric {
                            min: None,
                            max: None,
                            step: None,
                        },
                        Some(Unit::WattHour),
                    ),
                    spec("ptec", Kind::Text, None),
                ],
            });
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

const CONFIG: &str = r#"
    # Readings come milliseconds apart here: no power is « impossible ».
    max_power_kw = 1e12
    [tariff]
    period = "fake:m/ptec"
    prices = { "HP" = 0.2, "HC" = 0.1 }
    [[meter]]
    id = "general"
    name = "Général"
    point = "fake:m/e"
    role = "total"
"#;

async fn set(hub: &Hub, key: &str, value: Value) {
    hub.command(
        &PointId::from(format!("fake:m/{key}")),
        value,
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    // Distinct timestamps, and time for the recorder to file it.
    tokio::time::sleep(Duration::from_millis(60)).await;
}

async fn today_kwh(energy: &Energy) -> (f64, Option<f64>) {
    let s = energy.summary().await.unwrap();
    (s.today.total.kwh, s.today.total.cost)
}

#[tokio::test]
async fn readings_become_priced_hours_that_survive_restarts() {
    let dir = std::env::temp_dir().join(format!("moli-energy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = dir.join("energy.db");
    let hub = Hub::new(HubOptions::default()).unwrap();
    let drivers = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(FakeMeter),
        drivers.clone(),
    );
    while hub.stats().drivers_running == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let config: Config = toml::from_str(CONFIG).unwrap();

    let cancel = CancellationToken::new();
    let (energy, task) = Energy::start(&hub, &db, config.clone(), cancel.clone()).unwrap();
    set(&hub, "ptec", Value::from("HP..")).await;
    set(&hub, "e", Value::Float(1000.0)).await; // reference
    set(&hub, "e", Value::Float(3500.0)).await; // +2.5 kWh at 0.2
    set(&hub, "e", Value::Float(3400.0)).await; // bad reading: held…
    set(&hub, "e", Value::Float(3600.0)).await; // …and dismissed: +0.1 at 0.2
    set(&hub, "ptec", Value::from("HC..")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    set(&hub, "e", Value::Float(100.0)).await; // a fall: held…
    set(&hub, "e", Value::Float(200.0)).await; // …confirmed restart: +0.2 at 0.1
    let (kwh, cost) = today_kwh(&energy).await;
    assert!((kwh - 2.8).abs() < 1e-9, "{kwh}");
    assert!((cost.unwrap() - 0.54).abs() < 1e-9, "{cost:?}");

    // Restart: the last reading comes back from the database.
    cancel.cancel();
    task.await.unwrap();
    let cancel = CancellationToken::new();
    let (energy, _task) = Energy::start(&hub, &db, config, cancel.clone()).unwrap();
    set(&hub, "e", Value::Float(600.0)).await; // +0.4 kWh
    let (kwh, _) = today_kwh(&energy).await;
    assert!((kwh - 3.2).abs() < 1e-9, "{kwh}");

    // The same energy, hour by hour.
    let today = energy.summary().await.unwrap().today;
    let report = energy
        .report(today.from, today.to, moli_energy::Step::Hour)
        .await
        .unwrap();
    assert!(report.buckets.len() >= 23);
    let hourly: f64 = report
        .buckets
        .iter()
        .filter_map(|b| b.meters.get("general"))
        .map(|a| a.kwh)
        .sum();
    assert!((hourly - 3.2).abs() < 1e-6, "{hourly}");
    cancel.cancel();
    drivers.cancel();
    let _ = std::fs::remove_dir_all(dir);
}

// A real persisted hub snapshot with a silent driver after restart. Every update
// is synthetic and acknowledged; no command reaches a network or an appliance.
enum CachedUpdate {
    Publish,
    Sample(&'static str, Value),
}

type CachedMessage = (CachedUpdate, tokio::sync::oneshot::Sender<()>);

struct CachedMeter(tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<CachedMessage>>);

fn cached_device(ctx: &DriverCtx) -> Device {
    let mut on = spec("on", Kind::Binary, None);
    on.semantic = Semantic::OnOff;
    let numeric = Kind::Numeric {
        min: None,
        max: None,
        step: None,
    };
    Device {
        id: ctx.device_id("m"),
        instance: ctx.instance().clone(),
        native_name: "Cached meter".into(),
        manufacturer: None,
        model: Some("LCA006".into()),
        description: None,
        native_room: None,
        members: vec![],
        points: vec![
            spec("e", numeric.clone(), Some(Unit::WattHour)),
            spec("w", numeric, Some(Unit::Watt)),
            spec("ptec", Kind::Text, None),
            on,
        ],
    }
}

impl Driver for CachedMeter {
    fn kind(&self) -> &'static str {
        "cached-meter"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut updates = self.0.lock().await;
            ctx.ready();
            while let Some((update, acknowledged)) = updates.recv().await {
                match update {
                    CachedUpdate::Publish => ctx.upsert_device(cached_device(ctx)),
                    CachedUpdate::Sample(key, value) => {
                        ctx.set_state(&ctx.device_id("m"), key, value);
                    }
                }
                let _ = acknowledged.send(());
            }
            std::future::pending().await
        })
    }
}

struct CachedDriver {
    updates: tokio::sync::mpsc::UnboundedSender<CachedMessage>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl CachedDriver {
    fn start(hub: &Hub) -> Self {
        let (updates, receiver) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let task = spawn_driver(
            hub,
            "cached".into(),
            Arc::new(CachedMeter(tokio::sync::Mutex::new(receiver))),
            cancel.clone(),
        );
        Self {
            updates,
            cancel,
            task,
        }
    }

    async fn update(&self, update: CachedUpdate) {
        let (acknowledged, received) = tokio::sync::oneshot::channel();
        self.updates.send((update, acknowledged)).unwrap();
        received.await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

impl Drop for CachedDriver {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

struct CachedFixture {
    hub: Hub,
    driver: CachedDriver,
    dir: std::path::PathBuf,
}

impl CachedFixture {
    async fn new(tariff: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "moli-energy-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let options = || HubOptions {
            state_path: Some(dir.join("state.json")),
            ..HubOptions::default()
        };
        let before = Hub::new(options()).unwrap();
        let seeding = CachedDriver::start(&before);
        seeding.update(CachedUpdate::Publish).await;
        for (key, value) in [
            ("e", Value::Float(1000.0)),
            ("w", Value::Float(3600.0)),
            ("on", Value::Bool(true)),
        ] {
            seeding.update(CachedUpdate::Sample(key, value)).await;
        }
        if tariff {
            seeding
                .update(CachedUpdate::Sample("ptec", Value::from("HP..")))
                .await;
        }
        assert!(before.persist_state().await.unwrap());
        drop(seeding);
        let hub = Hub::new(options()).unwrap();
        let driver = CachedDriver::start(&hub);
        assert_eq!(hub.device(&"cached:m".into()).unwrap().online, Some(false));
        Self { hub, driver, dir }
    }

    fn db(&self) -> std::path::PathBuf {
        self.dir.join("energy.db")
    }

    fn recorded_kwh(&self) -> f64 {
        rusqlite::Connection::open(self.db())
            .unwrap()
            .query_row("SELECT coalesce(sum(kwh), 0) FROM energy_hour", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn recorded_meters(&self) -> i64 {
        rusqlite::Connection::open(self.db())
            .unwrap()
            .query_row("SELECT count(*) FROM meter_last", [], |row| row.get(0))
            .unwrap()
    }
}

impl Drop for CachedFixture {
    fn drop(&mut self) {
        self.driver.cancel.cancel();
        self.driver.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn offline_cached_power_is_not_integrated_when_recording_starts() {
    let f = CachedFixture::new(false).await;
    let config = toml::from_str(
        r#"
        max_power_kw = 1e12
        [[meter]]
        id = "power"
        name = "Power"
        point = "cached:m/w"
        role = "total"
        integrate = true
        price = 0.2
    "#,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let (energy, task) = Energy::start(&f.hub, &f.db(), config, cancel.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        f.recorded_meters(),
        0,
        "cached power must not seed a held integral"
    );
    f.driver.update(CachedUpdate::Publish).await;
    f.driver
        .update(CachedUpdate::Sample("w", Value::Float(0.0)))
        .await;
    assert!((today_kwh(&energy).await.0 - 0.0).abs() < 1e-9);
    f.driver
        .update(CachedUpdate::Sample("w", Value::Float(3600.0)))
        .await;
    f.driver
        .update(CachedUpdate::Sample("w", Value::Float(0.0)))
        .await;
    assert!(
        f.recorded_kwh() > 0.0,
        "fresh power still integrates below the summary rounding threshold"
    );
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn offline_cached_counter_is_not_replayed_when_the_first_tariff_arrives() {
    let f = CachedFixture::new(false).await;
    let config = toml::from_str(&CONFIG.replace("fake:m", "cached:m")).unwrap();
    let cancel = CancellationToken::new();
    let (energy, task) = Energy::start(&f.hub, &f.db(), config, cancel.clone()).unwrap();
    f.driver.update(CachedUpdate::Publish).await;
    f.driver
        .update(CachedUpdate::Sample("ptec", Value::from("HP..")))
        .await;
    assert_eq!(
        f.recorded_meters(),
        0,
        "a fresh tariff cannot make an old counter current"
    );
    f.driver
        .update(CachedUpdate::Sample("e", Value::Float(1100.0)))
        .await;
    assert!(
        (today_kwh(&energy).await.0 - 0.0).abs() < 1e-9,
        "the first fresh counter is the reference"
    );
    f.driver
        .update(CachedUpdate::Sample("e", Value::Float(1200.0)))
        .await;
    assert!((today_kwh(&energy).await.0 - 0.1).abs() < 1e-9);
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn offline_cached_tariff_and_power_are_not_current_energy_values() {
    let f = CachedFixture::new(true).await;
    let config = toml::from_str(&format!(
        "{}\npower = \"cached:m/w\"\n[live]\npower = \"cached:m/w\"\nmax = 5000\n",
        CONFIG.replace("fake:m", "cached:m")
    ))
    .unwrap();
    let cancel = CancellationToken::new();
    let (energy, task) = Energy::start(&f.hub, &f.db(), config, cancel.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    let summary = energy.summary().await.unwrap();
    assert_eq!(
        summary.period, None,
        "a retained tariff is not the current price"
    );
    assert_eq!(summary.meters[0].power_w, None);
    assert_eq!(summary.live.unwrap().value, None);
    assert_eq!(f.recorded_meters(), 0);
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn offline_cached_light_cannot_restart_an_estimate_without_a_fresh_sample() {
    let f = CachedFixture::new(false).await;
    f.driver.update(CachedUpdate::Publish).await;
    assert!(f.hub.current_state(&"cached:m/on".into()).is_none());
    let config = toml::from_str(
        r#"
        [[meter]]
        id = "light"
        name = "Light"
        point = "cached:m/on"
        role = "appliance"
        estimate = true
        price = 0.2
    "#,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let (energy, task) = Energy::start(&f.hub, &f.db(), config, cancel.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        f.recorded_meters(),
        0,
        "republishing a definition does not refresh its cached on state"
    );
    assert_eq!(energy.summary().await.unwrap().meters[0].power_w, None);
    f.driver
        .update(CachedUpdate::Sample("on", Value::Bool(true)))
        .await;
    assert_eq!(f.recorded_meters(), 1);
    assert!((energy.summary().await.unwrap().meters[0].power_w.unwrap() - 7.3).abs() < 1e-9);
    cancel.cancel();
    task.await.unwrap();
}

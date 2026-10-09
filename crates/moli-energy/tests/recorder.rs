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

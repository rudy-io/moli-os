//! The alarm in a house (a mirror one: it runs in the shadow of the real
//! house), with a helper switch standing for a sensor.

use std::sync::Arc;
use std::time::Duration;

use moli_alarm::{AlarmDriver, Config};
use moli_core::{InstanceId, Origin, PointId, Value};
use moli_helpers::Helpers;
use moli_runtime::{Hub, HubOptions, spawn_driver};
use tokio_util::sync::CancellationToken;

async fn settle(hub: &Hub, running: usize) {
    for _ in 0..300 {
        if hub.stats().drivers_running == running {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("drivers never ran: {:?}", hub.snapshot().drivers);
}

async fn state(hub: &Hub, key: &str) -> Value {
    tokio::time::sleep(Duration::from_millis(50)).await;
    hub.state(&PointId::from(format!("alarme:maison/{key}")))
        .map_or(Value::Null, |s| s.value)
}

#[tokio::test]
async fn armed_it_triggers_on_the_sensor_and_refuses_to_arm_on_it() {
    let hub = Hub::new(HubOptions {
        mirror: true,
        ..HubOptions::default()
    })
    .unwrap();
    let cancel = CancellationToken::new();
    let helpers: moli_helpers::Config = toml::from_str(
        r#"
        [[helper]]
        id = "porte"
        name = "Porte"
        kind = "switch"
        initial = true
        "#,
    )
    .unwrap();
    spawn_driver(
        &hub,
        InstanceId::from("maison"),
        Arc::new(Helpers::new(helpers).unwrap()),
        cancel.clone(),
    );
    settle(&hub, 1).await;
    let alarm: Config = toml::from_str(
        r#"
        [modes.night]
        [[sensor]]
        point = "maison:porte/on"
        modes = ["night"]
        "#,
    )
    .unwrap();
    spawn_driver(
        &hub,
        InstanceId::from("alarme"),
        Arc::new(AlarmDriver::new(alarm).unwrap()),
        cancel.clone(),
    );
    settle(&hub, 2).await;
    let mode = PointId::from("alarme:maison/mode".to_owned());
    let door = PointId::from("maison:porte/on".to_owned());

    // The door is open: refused, and said with its name.
    let refused = hub
        .command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await;
    assert!(format!("{refused:?}").contains("Porte"), "{refused:?}");
    assert_eq!(state(&hub, "state").await, Value::Text("disarmed".into()));
    assert_eq!(state(&hub, "cause").await, Value::Text("Porte".into()));

    // Closed, it arms; opened, it triggers (no delay in this mode).
    hub.command(&door, Value::Bool(false), Origin::Ui, None)
        .await
        .unwrap();
    hub.command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await
        .unwrap();
    assert_eq!(
        state(&hub, "state").await,
        Value::Text("armed_night".into())
    );
    hub.command(&door, Value::Bool(true), Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(state(&hub, "state").await, Value::Text("triggered".into()));
    assert_eq!(state(&hub, "event").await, Value::Text("triggered".into()));

    hub.command(&mode, Value::Text("off".into()), Origin::Automation, None)
        .await
        .unwrap();
    assert_eq!(state(&hub, "state").await, Value::Text("disarmed".into()));
    cancel.cancel();
}

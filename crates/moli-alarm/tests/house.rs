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

/// Waits until the alarm's vault holds this state. The phase is written
/// right after it is shown, in the same turn of the alarm's loop: a power
/// cut in between rightly loses it, so a test that cuts the power waits for
/// the write first (a slow runner showed the state before the write).
async fn kept(hub: &Hub, expected: &str, within: Duration) {
    let alarm = InstanceId::from("alarme");
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let state = hub
            .secret(&alarm, "phase")
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|kept| kept.get("state")?.as_str().map(str::to_owned));
        if state.as_deref() == Some(expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the vault never held {expected:?} (last: {state:?})"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
    // Another mode, or the same one, does not silence it: `off` only.
    let refused = hub
        .command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(state(&hub, "state").await, Value::Text("triggered".into()));
    assert_eq!(
        state(&hub, "silent").await,
        Value::Text(String::new().into())
    );

    hub.command(&mode, Value::Text("off".into()), Origin::Automation, None)
        .await
        .unwrap();
    assert_eq!(state(&hub, "state").await, Value::Text("disarmed".into()));
    cancel.cancel();
}

#[tokio::test]
async fn its_settings_change_from_the_screen_disarmed_only_and_stay() {
    let dir = std::env::temp_dir().join(format!("moli-alarm-settings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = moli_runtime::MasterKey::generate().unwrap();
    let start = |key: moli_runtime::MasterKey| {
        let hub = Hub::new(HubOptions {
            secrets: Some((dir.join("secrets.enc"), key)),
            ..HubOptions::default()
        })
        .unwrap();
        let alarm: Config = toml::from_str("[modes.night]\n").unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("alarme"),
            Arc::new(AlarmDriver::new(alarm).unwrap()),
            cancel.clone(),
        );
        (hub, cancel)
    };
    let (hub, cancel) = start(key.clone());
    settle(&hub, 1).await;
    let mode = PointId::from("alarme:maison/mode".to_owned());
    let setup = PointId::from("alarme:maison/setup".to_owned());
    let new = r#"{"modes":{"night":{"exit":0,"entry":0,"trigger":60},"home":{}},"sensors":[{"point":"maison:porte/on","name":"Porte","modes":["home"]}]}"#;

    // Armed: refused.
    hub.command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await
        .unwrap();
    let refused = hub
        .command(&setup, Value::Text(new.into()), Origin::Ui, None)
        .await;
    assert!(refused.is_err(), "{refused:?}");
    // Disarmed: taken, shown, and the new mode can be armed.
    hub.command(&mode, Value::Text("off".into()), Origin::Automation, None)
        .await
        .unwrap();
    hub.command(&setup, Value::Text(new.into()), Origin::Ui, None)
        .await
        .unwrap();
    let shown = state(&hub, "setup").await;
    assert!(format!("{shown:?}").contains("Porte"), "{shown:?}");
    // The door is not in this house: it does not answer, said on `silent`,
    // and the mode watching it is not armed.
    assert_eq!(state(&hub, "silent").await, Value::Text("Porte".into()));
    let refused = hub
        .command(&mode, Value::Text("home".into()), Origin::Automation, None)
        .await;
    assert!(format!("{refused:?}").contains("Porte"), "{refused:?}");
    assert_eq!(state(&hub, "state").await, Value::Text("disarmed".into()));
    // Night watches nothing: it arms.
    hub.command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await
        .unwrap();
    // Nonsense: refused.
    hub.command(&mode, Value::Text("off".into()), Origin::Automation, None)
        .await
        .unwrap();
    assert!(
        hub.command(&setup, Value::Text("{".into()), Origin::Ui, None)
            .await
            .is_err()
    );
    cancel.cancel();
    drop(hub);

    // After a restart, the screen's settings are still the alarm's.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (hub, cancel) = start(key);
    settle(&hub, 1).await;
    let shown = state(&hub, "setup").await;
    assert!(format!("{shown:?}").contains("Porte"), "{shown:?}");
    cancel.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Armed, then Moli stops at once (a power cut): the state cache, written
/// once a minute, never saw it. The phase kept in the vault brings it back
/// armed; triggered, it comes back triggered.
#[tokio::test]
async fn a_power_cut_right_after_arming_or_triggering_loses_nothing() {
    let dir = std::env::temp_dir().join(format!("moli-alarm-cut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = moli_runtime::MasterKey::generate().unwrap();
    let start = |key: moli_runtime::MasterKey| {
        let hub = Hub::new(HubOptions {
            secrets: Some((dir.join("secrets.enc"), key)),
            ..HubOptions::default()
        })
        .unwrap();
        let alarm: Config = toml::from_str("[modes.night]\ntrigger = 600\n[modes.home]\n").unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("alarme"),
            Arc::new(AlarmDriver::new(alarm).unwrap()),
            cancel.clone(),
        );
        (hub, cancel)
    };
    let mode = PointId::from("alarme:maison/mode".to_owned());

    let (hub, cancel) = start(key.clone());
    settle(&hub, 1).await;
    hub.command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await
        .unwrap();
    assert_eq!(
        state(&hub, "state").await,
        Value::Text("armed_night".into())
    );
    // The cut comes once the phase is written, not merely shown.
    kept(&hub, "armed_night", Duration::from_secs(3)).await;
    cancel.cancel();
    drop(hub);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (hub, cancel) = start(key);
    settle(&hub, 1).await;
    assert_eq!(
        state(&hub, "state").await,
        Value::Text("armed_night".into())
    );
    // And it holds: another mode is refused, off only.
    assert!(
        hub.command(&mode, Value::Text("home".into()), Origin::Automation, None)
            .await
            .is_err()
    );
    cancel.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The vault refuses the write when it is armed (a full or read-only disk):
/// the phase is not counted as kept, and it is tried again a few seconds
/// later on its own (no event of the house needed) once the vault takes it
/// again. A power cut then still finds it armed.
#[tokio::test]
async fn a_phase_the_vault_refused_is_kept_a_few_seconds_later() {
    let dir = std::env::temp_dir().join(format!("moli-alarm-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let vault = dir.join("vault");
    let key = moli_runtime::MasterKey::generate().unwrap();
    let start = |key: moli_runtime::MasterKey| {
        let hub = Hub::new(HubOptions {
            secrets: Some((vault.join("secrets.enc"), key)),
            ..HubOptions::default()
        })
        .unwrap();
        let alarm: Config = toml::from_str("[modes.night]\n[modes.home]\n").unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("alarme"),
            Arc::new(AlarmDriver::new(alarm).unwrap()),
            cancel.clone(),
        );
        (hub, cancel)
    };
    let mode = PointId::from("alarme:maison/mode".to_owned());

    let (hub, cancel) = start(key.clone());
    settle(&hub, 1).await;
    // A file where the vault's folder should be: every write fails.
    let _ = std::fs::remove_dir_all(&vault);
    std::fs::write(&vault, b"in the way").unwrap();
    hub.command(&mode, Value::Text("night".into()), Origin::Automation, None)
        .await
        .unwrap();
    assert_eq!(
        state(&hub, "state").await,
        Value::Text("armed_night".into())
    );
    // The vault works again; nothing happens in the house, the alarm tries
    // again on its own after its wait (5 s): the phase reaches the vault
    // without any event (waited for, a slow runner may need more than 5 s).
    std::fs::remove_file(&vault).unwrap();
    kept(&hub, "armed_night", Duration::from_secs(10)).await;
    cancel.cancel();
    drop(hub);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (hub, cancel) = start(key);
    settle(&hub, 1).await;
    assert_eq!(
        state(&hub, "state").await,
        Value::Text("armed_night".into())
    );
    cancel.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

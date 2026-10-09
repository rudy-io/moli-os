use std::sync::Arc;
use std::time::Duration;

use moli_core::{InstanceId, Origin, PointId, Value};
use moli_runtime::{Hub, HubOptions, spawn_driver};
use tokio_util::sync::CancellationToken;

use super::{Config, Demo};

const FIXTURE: &str = r#"{
  "imitates": "hue",
  "devices": [
    { "id": "hue:lampe", "instance": "hue", "native_name": "Lampe du salon", "native_room": "Salon",
      "points": [
        { "key": "on", "label": "Allumée", "kind": { "type": "binary" }, "access": { "read": true, "write": true }, "semantic": "on_off" },
        { "key": "brightness", "label": "Luminosité", "kind": { "type": "numeric", "min": 0, "max": 100 }, "access": { "read": true, "write": true }, "unit": "%", "semantic": "brightness" },
        { "key": "power", "label": "Puissance", "kind": { "type": "numeric" }, "access": { "read": true, "write": false }, "unit": "W", "semantic": "power" },
        { "key": "energy", "label": "Énergie", "kind": { "type": "numeric" }, "access": { "read": true, "write": false }, "unit": "Wh", "semantic": "energy" }
      ],
      "state": { "on": false, "brightness": { "value": 60, "ts": 1 }, "power": 9.0, "energy": 1000 } },
    { "id": "hue:piece", "instance": "hue", "native_name": "Salon", "members": ["hue:lampe"],
      "points": [
        { "key": "on", "label": "Allumée", "kind": { "type": "binary" }, "access": { "read": true, "write": true }, "semantic": "on_off" }
      ],
      "state": { "on": false } }
  ]
}"#;

fn demo() -> (Demo, tempdir::Dir) {
    let dir = tempdir::Dir::new();
    let path = dir.0.join("hue.json");
    std::fs::write(&path, FIXTURE).unwrap();
    let demo = Demo::new(Config {
        fixture: path,
        tick_s: 1,
        timezone: "Europe/Paris".into(),
    })
    .unwrap();
    (demo, dir)
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Dir {
        pub fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "moli-demo-{}-{}",
                std::process::id(),
                moli_core::now_ms()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

async fn wait(hub: &Hub, point: &PointId) {
    for _ in 0..200 {
        if hub.state(point).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{point} never published");
}

#[test]
fn it_passes_for_the_driver_it_imitates() {
    let (d, _dir) = demo();
    assert_eq!(moli_runtime::Driver::kind(&d), "hue");
    assert_eq!(d.len(), 2);
}

#[test]
fn a_bad_file_is_refused() {
    let dir = tempdir::Dir::new();
    let path = dir.0.join("x.json");
    std::fs::write(
        &path,
        FIXTURE.replace("\"hue\",\n  \"devices\"", "\"zwave\",\n  \"devices\""),
    )
    .unwrap();
    let config = Config {
        fixture: path,
        tick_s: 1,
        timezone: "Europe/Paris".into(),
    };
    assert!(Demo::new(config).is_err(), "unknown driver to imitate");
}

#[tokio::test]
async fn orders_are_obeyed_and_ripple() {
    let (d, _dir) = demo();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(&hub, InstanceId::from("hue"), Arc::new(d), cancel.clone());
    let lamp_on = PointId::from("hue:lampe/on".to_owned());
    let power = PointId::from("hue:lampe/power".to_owned());
    wait(&hub, &lamp_on).await;
    assert_eq!(hub.state(&lamp_on).unwrap().value, Value::Bool(false));
    // The room's order reaches its lamp, which then draws power.
    hub.command(
        &PointId::from("hue:piece/on".to_owned()),
        Value::Bool(true),
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    assert_eq!(hub.state(&lamp_on).unwrap().value, Value::Bool(true));
    let w = hub.state(&power).unwrap().value.as_f64().unwrap();
    assert!(w > 1.0, "a lit lamp draws power: {w}");
    // Brightness to zero then up: the lamp is on.
    hub.command(&lamp_on, Value::Bool(false), Origin::Ui, None)
        .await
        .unwrap();
    hub.command(
        &PointId::from("hue:lampe/brightness".to_owned()),
        Value::Float(80.0),
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    assert_eq!(hub.state(&lamp_on).unwrap().value, Value::Bool(true));
    cancel.cancel();
}

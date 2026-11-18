use std::sync::Arc;
use std::time::Duration;

use moli_core::{InstanceId, Origin, PointId, Value};
use moli_runtime::{Hub, HubOptions, spawn_driver};
use tokio_util::sync::CancellationToken;

use super::*;

/// Stand-ins, as Moli's own helpers: the room's sensor, the valve's
/// thermometer and its setpoint (5 to 30, like a radiator valve).
fn stand_ins() -> moli_helpers::Config {
    toml::from_str(
        r#"
        [[helper]]
        id = "piece"
        name = "Pièce"
        kind = "number"
        min = -20.0
        max = 50.0
        initial = 20.9
        [[helper]]
        id = "vanne_t"
        name = "Thermomètre de la vanne"
        kind = "number"
        min = -20.0
        max = 50.0
        initial = 22.0
        [[helper]]
        id = "vanne_sp"
        name = "Consigne de la vanne"
        kind = "number"
        min = 5.0
        max = 30.0
        step = 0.5
        initial = 16.0
        "#,
    )
    .unwrap()
}

fn rooms() -> Vec<Room> {
    #[derive(Deserialize)]
    struct File {
        heating: Vec<Room>,
    }
    toml::from_str::<File>(
        r#"
        [[heating]]
        id = "parents"
        name = "Chambre"
        sensor = "maison:piece/value"
        valves = ["maison:vanne_sp/value"]
        initial = 20.0
        "#,
    )
    .unwrap()
    .heating
}

fn point(text: &str) -> PointId {
    PointId::from(text.to_owned())
}

async fn until(hub: &Hub, at: &str, value: f64) {
    for _ in 0..300 {
        if hub.state(&point(at)).and_then(|s| s.value.as_f64()) == Some(value) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{at}: {:?}, {value} expected",
        hub.state(&point(at)).map(|s| s.value)
    );
}

async fn house() -> (Hub, CancellationToken) {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("maison"),
        Arc::new(moli_helpers::Helpers::new(stand_ins()).unwrap()),
        cancel.clone(),
    );
    until(&hub, "maison:vanne_sp/value", 16.0).await;
    let mut heating = Heating::new(hub.clone(), rooms(), jiff::tz::TimeZone::UTC);
    heating.settle = Duration::ZERO;
    spawn_driver(
        &hub,
        InstanceId::from(INSTANCE),
        Arc::new(heating),
        cancel.clone(),
    );
    (hub, cancel)
}

#[tokio::test]
async fn the_valve_holds_the_room_by_the_room_sensor() {
    let (hub, cancel) = house().await;
    // The schedule's 20 °C, no shift learned yet: the valve gets 20, whatever
    // its own thermometer says (22, by the radiator).
    until(&hub, "chauffage:parents/occupied_heating_setpoint", 20.0).await;
    until(&hub, "maison:vanne_sp/value", 20.0).await;
    until(&hub, "chauffage:parents/local_temperature", 20.9).await;
    until(&hub, "chauffage:parents/valve_correction", 0.0).await;

    // Held by hand: 21.5, on the half degree.
    let order = |key: &str, value: Value| {
        let hub = hub.clone();
        let at = point(&format!("chauffage:parents/{key}"));
        async move { hub.command(&at, value, Origin::Ui, None).await }
    };
    order("system_mode", Value::from("heat")).await.unwrap();
    order("occupied_heating_setpoint", Value::Float(21.5))
        .await
        .unwrap();
    until(&hub, "maison:vanne_sp/value", 21.5).await;

    // Off: the valve's lowest setpoint.
    order("system_mode", Value::from("off")).await.unwrap();
    until(&hub, "maison:vanne_sp/value", 5.0).await;

    // A schedule the room does not accept is refused, a good one kept as written.
    assert!(
        order("weekly_schedule_monday", Value::from("06:00/20"))
            .await
            .is_err()
    );
    order(
        "weekly_schedule_monday",
        Value::from("00:00/17  06:30/20.5 22:00/17"),
    )
    .await
    .unwrap();
    assert_eq!(
        hub.state(&point("chauffage:parents/weekly_schedule_monday"))
            .unwrap()
            .value,
        Value::from("00:00/17 06:30/20.5 22:00/17")
    );
    assert!(order("system_mode", Value::from("cool")).await.is_err());
    cancel.cancel();
}

#[test]
fn bad_rooms_are_refused() {
    assert!(check(&rooms()).is_ok());
    let cases: [fn(&mut Room); 8] = [
        |r| r.id = "a:b".into(),
        |r| r.name = " ".into(),
        |r| r.sensor = "piece".into(),
        |r| r.valves.clear(),
        |r| r.valves[0] = "maison:vanne_sp".into(),
        |r| r.min = 26.0,
        |r| r.initial = 20.3,
        |r| r.initial = 26.0,
    ];
    for (i, change) in cases.iter().enumerate() {
        let mut list = rooms();
        change(&mut list[0]);
        assert!(check(&list).is_err(), "case {i}");
    }
    let mut twice = rooms();
    twice.push(twice[0].clone());
    assert!(check(&twice).is_err());
}

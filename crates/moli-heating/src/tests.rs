use std::sync::Arc;
use std::time::Duration;

use moli_core::{InstanceId, Origin, PointId, Value};
use moli_runtime::{Hub, HubOptions, spawn_driver};
use tokio_util::sync::CancellationToken;

use super::*;

/// Stand-ins, as Moli's own helpers: the room's sensor, the valve's
/// thermometer, and a number that is not a temperature (no unit).
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
        id = "volume"
        name = "Volume"
        kind = "number"
        min = 5.0
        max = 30.0
        initial = 16.0
        "#,
    )
    .unwrap()
}

/// A radiator valve: its setpoint in °C (5 to 30), as it was written; in
/// the « Bureau ». And a shutter whose position says it is in °C (a driver's
/// mistake): never a valve.
#[derive(Debug)]
struct Valves;

fn setpoint_point(key: &str, unit: Unit, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: key.into(),
        kind: Kind::Numeric {
            min: Some(5.0),
            max: Some(30.0),
            step: Some(0.5),
        },
        access: Access {
            read: true,
            write: true,
        },
        unit: Some(unit),
        semantic,
    }
}

impl Driver for Valves {
    fn kind(&self) -> &'static str {
        "valves"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            for (native, points) in [
                (
                    "v",
                    vec![setpoint_point("sp", Unit::Celsius, Semantic::Other)],
                ),
                (
                    "entiere",
                    vec![setpoint_point("sp", Unit::Celsius, Semantic::Other)],
                ),
                (
                    "volet",
                    vec![
                        setpoint_point("sp", Unit::Celsius, Semantic::Other),
                        setpoint_point("position", Unit::Percent, Semantic::Position),
                    ],
                ),
            ] {
                let id = ctx.device_id(native);
                ctx.upsert_device(Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: native.into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: Some("Bureau".into()),
                    members: Vec::new(),
                    points,
                });
                ctx.set_state(&id, "sp", Value::Float(16.0));
            }
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                // « entiere » keeps whole degrees, cut down, without saying so.
                let value = match command.value.as_f64() {
                    Some(v) if &*command.device.native_name == "entiere" => Value::Float(v.floor()),
                    _ => command.value.clone(),
                };
                ctx.set_state(&command.device.id, &command.key, value);
                command.reply(Ok(()));
            }
            Ok(())
        })
    }
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
        valves = ["vannes:v/sp", "maison:volume/value", "vannes:volet/sp"]
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

async fn until_bool(hub: &Hub, at: &str, value: bool) {
    for _ in 0..300 {
        if hub.state(&point(at)).map(|s| s.value) == Some(Value::Bool(value)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{at}: {:?}, {value} expected",
        hub.state(&point(at)).map(|s| s.value)
    );
}

async fn until_text(hub: &Hub, at: &str, value: &str) {
    for _ in 0..300 {
        if hub.state(&point(at)).map(|s| s.value) == Some(Value::from(value)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{at}: {:?}, {value:?} expected",
        hub.state(&point(at)).map(|s| s.value)
    );
}

async fn house() -> (Hub, CancellationToken) {
    house_with(|_| {}).await
}

async fn house_with(tune: impl FnOnce(&mut Heating)) -> (Hub, CancellationToken) {
    house_of(rooms(), tune).await
}

async fn house_of(rooms: Vec<Room>, tune: impl FnOnce(&mut Heating)) -> (Hub, CancellationToken) {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("maison"),
        Arc::new(moli_helpers::Helpers::new(stand_ins()).unwrap()),
        cancel.clone(),
    );
    spawn_driver(
        &hub,
        InstanceId::from("vannes"),
        Arc::new(Valves),
        cancel.clone(),
    );
    until(&hub, "maison:volume/value", 16.0).await;
    until(&hub, "vannes:v/sp", 16.0).await;
    let mut heating = Heating::new(hub.clone(), rooms, jiff::tz::TimeZone::UTC);
    heating.settle = Duration::ZERO;
    heating.floor = Duration::ZERO;
    tune(&mut heating);
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
    until(&hub, "vannes:v/sp", 20.0).await;
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
    until(&hub, "vannes:v/sp", 21.5).await;

    // Off: the valve's lowest setpoint.
    order("system_mode", Value::from("off")).await.unwrap();
    until(&hub, "vannes:v/sp", 5.0).await;

    // Neither a number without a unit nor a shutter's point was ever written.
    until(&hub, "maison:volume/value", 16.0).await;
    until(&hub, "vannes:volet/sp", 16.0).await;

    // An order to the room is judged on its valves' rooms too.
    let room = DeviceId::from("chauffage:parents");
    assert_eq!(
        hub.device(&room).unwrap().device.members,
        [
            DeviceId::from("vannes:v"),
            DeviceId::from("maison:volume"),
            DeviceId::from("vannes:volet"),
        ]
    );
    assert!(hub.rooms_of(&room).contains(&"Bureau".to_owned()));

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

#[tokio::test]
async fn a_valve_turned_by_hand_is_left_as_it_is_for_a_while() {
    // Looked at often, and written again as soon as it differs: without a
    // hold, Moli would put its own setpoint back at once.
    let (hub, cancel) = house_with(|h| {
        h.tick = Duration::from_millis(20);
        h.interval = Duration::ZERO;
        h.airing_hold = Duration::from_millis(400);
    })
    .await;
    let room = "chauffage:parents";
    let state = |key: &str| format!("{room}/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;
    until_text(&hub, &state("valve_override"), "none").await;
    let turn = |value: f64| {
        let hub = hub.clone();
        async move {
            hub.command(&point("vannes:v/sp"), Value::Float(value), Origin::Ui, None)
                .await
                .unwrap();
        }
    };
    let order = |key: &str, value: Value| {
        let hub = hub.clone();
        let at = point(&format!("{room}/{key}"));
        async move { hub.command(&at, value, Origin::Ui, None).await.unwrap() }
    };

    // Turned to 22 by hand: kept, the room says so and until when (2 h).
    turn(22.0).await;
    until_text(&hub, &state("valve_override"), "manual").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    let Some(Value::Text(end)) = hub
        .state(&point(&state("valve_override_until")))
        .map(|s| s.value)
    else {
        panic!("an end expected");
    };
    let end: jiff::Timestamp = end.parse().unwrap();
    let left = end.duration_since(jiff::Timestamp::now()).as_secs();
    assert!((7100..=7200).contains(&left), "{left} s left");

    // « Resume the schedule »: the room's setpoint again.
    order("valve_override", Value::from("none")).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    until_text(&hub, &state("valve_override"), "none").await;
    assert!(
        hub.command(
            &point(&state("valve_override")),
            Value::from("manual"),
            Origin::Ui,
            None
        )
        .await
        .is_err()
    );

    // Turned down to its lowest: airing. Turned up again: the room's setpoint.
    turn(5.0).await;
    until_text(&hub, &state("valve_override"), "airing").await;
    turn(21.0).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    until_text(&hub, &state("valve_override"), "none").await;

    // Airing over on its own.
    turn(5.0).await;
    until_text(&hub, &state("valve_override"), "airing").await;
    until(&hub, "vannes:v/sp", 20.0).await;
    until_text(&hub, &state("valve_override"), "none").await;

    // A change of the room's own setpoint ends a hold.
    turn(23.0).await;
    until_text(&hub, &state("valve_override"), "manual").await;
    order("system_mode", Value::from("heat")).await;
    order("occupied_heating_setpoint", Value::Float(21.0)).await;
    until(&hub, "vannes:v/sp", 21.0).await;
    until_text(&hub, &state("valve_override"), "none").await;
    cancel.cancel();
}

#[tokio::test]
async fn a_valve_that_rounds_what_it_is_given_is_not_turned_by_hand() {
    #[derive(Deserialize)]
    struct File {
        heating: Vec<Room>,
    }
    let rooms = toml::from_str::<File>(
        r#"
        [[heating]]
        id = "bureau"
        name = "Bureau"
        sensor = "maison:piece/value"
        valves = ["vannes:entiere/sp"]
        initial = 21.5
        "#,
    )
    .unwrap()
    .heating;
    let (hub, cancel) = house_of(rooms, |h| {
        h.tick = Duration::from_millis(20);
        h.interval = Duration::ZERO;
    })
    .await;
    let room = "chauffage:bureau/valve_override";
    // 21.5 written, 21 read back, look after look: still the room's.
    until(&hub, "vannes:entiere/sp", 21.0).await;
    for _ in 0..15 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        until_text(&hub, room, "none").await;
    }
    // Turned on its dial: a hand.
    hub.command(
        &point("vannes:entiere/sp"),
        Value::Float(18.0),
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    until_text(&hub, room, "manual").await;
    until(&hub, "vannes:entiere/sp", 18.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_forced_valve_opens_wide_or_shuts_on_the_room_sensor() {
    #[derive(Deserialize)]
    struct File {
        heating: Vec<Room>,
    }
    let rooms = toml::from_str::<File>(
        r#"
        [[heating]]
        id = "bureau"
        name = "Bureau"
        sensor = "maison:piece/value"
        valves = ["vannes:v/sp"]
        initial = 20.0
        regulation = "force"
        "#,
    )
    .unwrap()
    .heating;
    let (hub, cancel) = house_of(rooms, |h| {
        h.tick = Duration::from_millis(20);
    })
    .await;
    let room = |key: &str| format!("chauffage:bureau/{key}");
    let sensor = |value: f64| {
        let hub = hub.clone();
        async move {
            hub.command(
                &point("maison:piece/value"),
                Value::Float(value),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
        }
    };
    // 20.9 for 20: over it, the valve is shut (20 − 4), no heat asked for.
    until_bool(&hub, &room("heat_demand"), false).await;
    until(&hub, "vannes:v/sp", 16.0).await;
    until(&hub, &room("valve_correction"), 0.0).await;

    // 19.5: under it by more than 0.3, the valve opens wide (20 + 4).
    sensor(19.5).await;
    until(&hub, "vannes:v/sp", 24.0).await;
    until_bool(&hub, &room("heat_demand"), true).await;

    // 20.2: inside the hysteresis, nothing changes.
    sensor(20.2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    until(&hub, "vannes:v/sp", 24.0).await;
    until_bool(&hub, &room("heat_demand"), true).await;

    // 20.4: over it by more than 0.3, shut again.
    sensor(20.4).await;
    until_bool(&hub, &room("heat_demand"), false).await;
    until(&hub, "vannes:v/sp", 16.0).await;

    // A new setpoint moves the forced one with it: 22 for a room at 20.4.
    hub.command(
        &point(&room("system_mode")),
        Value::from("heat"),
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    hub.command(
        &point(&room("occupied_heating_setpoint")),
        Value::Float(22.0),
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    until(&hub, "vannes:v/sp", 26.0).await;

    // Off: the valve's lowest, no heat asked for.
    hub.command(
        &point(&room("system_mode")),
        Value::from("off"),
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    until(&hub, "vannes:v/sp", 5.0).await;
    until_bool(&hub, &room("heat_demand"), false).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_boost_moves_a_forced_room_s_demand_and_valves() {
    #[derive(Deserialize)]
    struct File {
        heating: Vec<Room>,
    }
    let rooms = toml::from_str::<File>(
        r#"
        [[heating]]
        id = "bureau"
        name = "Bureau"
        sensor = "maison:piece/value"
        valves = ["vannes:v/sp"]
        initial = 20.0
        regulation = "force"
        "#,
    )
    .unwrap()
    .heating;
    let (hub, cancel) = house_of(rooms, |h| {
        h.tick = Duration::from_millis(20);
    })
    .await;
    let room = |key: &str| format!("chauffage:bureau/{key}");
    // 20.9 for 20: shut, no heat asked for.
    until_bool(&hub, &room("heat_demand"), false).await;
    until(&hub, "vannes:v/sp", 16.0).await;

    // +2: the room aims at 22, 20.9 is under it, the valve opens wide (22 + 4).
    hub.command(&point(&room("boost")), Value::Float(2.0), Origin::Ui, None)
        .await
        .unwrap();
    until_bool(&hub, &room("heat_demand"), true).await;
    until(&hub, "vannes:v/sp", 26.0).await;

    // The boost over: back to 20, shut again.
    hub.command(&point(&room("boost")), Value::Float(0.0), Origin::Ui, None)
        .await
        .unwrap();
    until_bool(&hub, &room("heat_demand"), false).await;
    until(&hub, "vannes:v/sp", 16.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_forced_room_whose_sensor_went_quiet_gives_its_setpoint_as_it_is() {
    #[derive(Deserialize)]
    struct File {
        heating: Vec<Room>,
    }
    let rooms = toml::from_str::<File>(
        r#"
        [[heating]]
        id = "bureau"
        name = "Bureau"
        sensor = "maison:piece/value"
        valves = ["vannes:v/sp"]
        initial = 20.0
        regulation = "force"
        force_offset = 3.0
        "#,
    )
    .unwrap()
    .heating;
    let (hub, cancel) = house_of(rooms, |h| {
        h.tick = Duration::from_millis(20);
        h.fresh = Duration::from_millis(500);
    })
    .await;
    // 20.9 for 20: shut. Then nothing more from the sensor: the room's 20.
    until(&hub, "vannes:v/sp", 17.0).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    until_bool(&hub, "chauffage:bureau/heat_demand", false).await;
    cancel.cancel();
}

fn order_to(
    hub: &Hub,
    key: &str,
    value: Value,
) -> impl Future<Output = Result<(), String>> + use<> {
    let hub = hub.clone();
    let at = point(&format!("chauffage:parents/{key}"));
    async move {
        hub.command(&at, value, Origin::Ui, None)
            .await
            .map_err(|e| e.to_string())
    }
}

/// How long until the room's `key` (RFC 3339), empty: `None`.
fn left(hub: &Hub, key: &str) -> Option<Duration> {
    let value = hub
        .state(&point(&format!("chauffage:parents/{key}")))
        .map(|s| s.value)?;
    let end = temporary::read(Some(&value))?;
    Some(Duration::from_millis(end.saturating_sub(now_ms())))
}

/// What the room journaled on its own, by point: (key, from, to).
fn journaled(hub: &Hub) -> Vec<(String, Value, Value)> {
    hub.journal(100)
        .iter()
        .filter(|e| e.actor.as_deref() == Some("Chauffage · Chambre"))
        .filter_map(|e| match &e.action {
            moli_core::Action::Setting { name, from, to } => Some((
                name.strip_prefix("chauffage:parents/")?.to_owned(),
                from.clone()?,
                to.clone(),
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_boost_adds_to_the_setpoint_for_a_while() {
    let (hub, cancel) = house_with(|h| {
        h.tick = Duration::from_millis(20);
        h.interval = Duration::ZERO;
        h.minute = Duration::from_millis(20);
    })
    .await;
    let order = |key: &str, value: Value| order_to(&hub, key, value);
    until(&hub, "vannes:v/sp", 20.0).await;
    until(&hub, "chauffage:parents/boost", 0.0).await;
    assert_eq!(left(&hub, "boost_until"), None);
    assert!(
        order("boost_minutes", Value::Int(30)).await.is_err(),
        "no boost yet"
    );

    // +2 °C on the schedule's 20, for its 60 minutes by default.
    order("boost", Value::Float(2.0)).await.unwrap();
    until(&hub, "vannes:v/sp", 22.0).await;
    until(&hub, "chauffage:parents/boost_minutes", 60.0).await;
    let first = left(&hub, "boost_until").unwrap();
    assert!(first <= Duration::from_millis(1200), "{first:?}");
    assert!(journaled(&hub).contains(&("boost_minutes".into(), Value::Int(0), Value::Int(60))));

    // Longer, then +1 only: the end stays where it was.
    order("boost_minutes", Value::Int(600)).await.unwrap();
    until(&hub, "chauffage:parents/boost_minutes", 600.0).await;
    order("boost", Value::Float(1.0)).await.unwrap();
    until(&hub, "vannes:v/sp", 21.0).await;
    let end = left(&hub, "boost_until").unwrap();
    assert!(end > Duration::from_secs(11), "{end:?}");

    // Never above the room's max (25).
    order("system_mode", Value::from("heat")).await.unwrap();
    until(&hub, "chauffage:parents/boost", 0.0).await;
    order("occupied_heating_setpoint", Value::Float(24.0))
        .await
        .unwrap();
    order("boost", Value::Float(3.0)).await.unwrap();
    until(&hub, "vannes:v/sp", 25.0).await;

    // Over on its own: the setpoint again, and journaled.
    order("boost_minutes", Value::Int(5)).await.unwrap();
    until(&hub, "vannes:v/sp", 24.0).await;
    until(&hub, "chauffage:parents/boost", 0.0).await;
    until(&hub, "chauffage:parents/boost_minutes", 0.0).await;
    assert_eq!(left(&hub, "boost_until"), None);
    assert!(journaled(&hub).contains(&("boost".into(), Value::Float(3.0), Value::Float(0.0))));

    // Off is frost guard, boost or not.
    for bad in [3.5, 1.2, -1.0] {
        assert!(order("boost", Value::Float(bad)).await.is_err(), "{bad}");
    }
    order("system_mode", Value::from("off")).await.unwrap();
    assert!(order("boost", Value::Float(1.0)).await.is_err());
    order("boost", Value::Float(0.0)).await.unwrap();
    cancel.cancel();
}

#[tokio::test]
async fn a_setpoint_held_gives_way_to_the_schedule() {
    let mut rooms = rooms();
    rooms[0].hold_default_minutes = 15;
    let (hub, cancel) = house_of(rooms, |h| {
        h.tick = Duration::from_millis(20);
        h.interval = Duration::ZERO;
        h.minute = Duration::from_millis(20);
    })
    .await;
    let order = |key: &str, value: Value| order_to(&hub, key, value);
    until(&hub, "vannes:v/sp", 20.0).await;
    assert_eq!(left(&hub, "hold_until"), None);

    // Changed by hand: held for its 15 minutes, then the schedule's 20.
    order("occupied_heating_setpoint", Value::Float(22.0))
        .await
        .unwrap();
    until(&hub, "chauffage:parents/hold_minutes", 15.0).await;
    assert!(left(&hub, "hold_until").unwrap() <= Duration::from_millis(300));
    until(&hub, "chauffage:parents/occupied_heating_setpoint", 20.0).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    until(&hub, "chauffage:parents/hold_minutes", 0.0).await;
    assert_eq!(left(&hub, "hold_until"), None);
    let log = journaled(&hub);
    assert!(log.contains(&("hold_minutes".into(), Value::Int(0), Value::Int(15))));
    assert!(log.contains(&("hold_minutes".into(), Value::Int(15), Value::Int(0))));

    // A scene's order either way: the hold asked for is kept.
    order("hold_minutes", Value::Int(600)).await.unwrap();
    order("occupied_heating_setpoint", Value::Float(18.0))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 18.0).await;
    until(&hub, "chauffage:parents/hold_minutes", 600.0).await;

    // A new step of the schedule waits for the end of the hold.
    for day in DAYS {
        order(&format!("weekly_schedule_{day}"), Value::from("00:00/19"))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    until(&hub, "chauffage:parents/occupied_heating_setpoint", 18.0).await;
    order("hold_minutes", Value::Int(2)).await.unwrap();
    until(&hub, "chauffage:parents/occupied_heating_setpoint", 19.0).await;
    until(&hub, "vannes:v/sp", 19.0).await;

    // 0: until the next step, as without a hold.
    order("hold_minutes", Value::Int(600)).await.unwrap();
    order("hold_minutes", Value::Int(0)).await.unwrap();
    until(&hub, "chauffage:parents/hold_minutes", 0.0).await;
    assert!(order("hold_minutes", Value::Int(721)).await.is_err());
    assert!(order("hold_minutes", Value::Float(1.5)).await.is_err());

    // A change of mode ends it; out of auto, nothing to hold.
    order("hold_minutes", Value::Int(600)).await.unwrap();
    order("system_mode", Value::from("heat")).await.unwrap();
    until(&hub, "chauffage:parents/hold_minutes", 0.0).await;
    assert!(order("hold_minutes", Value::Int(60)).await.is_err());
    order("occupied_heating_setpoint", Value::Float(21.0))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    until(&hub, "chauffage:parents/hold_minutes", 0.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_boost_and_a_hold_outlive_a_restart() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("maison"),
        Arc::new(moli_helpers::Helpers::new(stand_ins()).unwrap()),
        cancel.clone(),
    );
    spawn_driver(
        &hub,
        InstanceId::from("vannes"),
        Arc::new(Valves),
        cancel.clone(),
    );
    until(&hub, "vannes:v/sp", 16.0).await;
    let start = || {
        let mut heating = Heating::new(hub.clone(), rooms(), jiff::tz::TimeZone::UTC);
        heating.settle = Duration::ZERO;
        heating.floor = Duration::ZERO;
        heating.tick = Duration::from_millis(20);
        let stop = cancel.child_token();
        let task = spawn_driver(
            &hub,
            InstanceId::from(INSTANCE),
            Arc::new(heating),
            stop.clone(),
        );
        (task, stop)
    };
    let order = |key: &str, value: Value| order_to(&hub, key, value);

    let (task, stop) = start();
    until(&hub, "vannes:v/sp", 20.0).await;
    order("occupied_heating_setpoint", Value::Float(18.0))
        .await
        .unwrap();
    order("hold_minutes", Value::Int(180)).await.unwrap();
    order("boost", Value::Float(1.5)).await.unwrap();
    until(&hub, "vannes:v/sp", 19.5).await;
    until(&hub, "chauffage:parents/hold_minutes", 180.0).await;
    let state = |key: &str| {
        hub.state(&point(&format!("chauffage:parents/{key}")))
            .map(|s| s.value)
    };
    let ends = (state("boost_until"), state("hold_until"));
    assert!(ends.0.as_ref().is_some_and(|v| *v != Value::Null));
    stop.cancel();
    task.await.unwrap();

    let (_task, _stop) = start();
    tokio::time::sleep(Duration::from_millis(100)).await;
    until(&hub, "chauffage:parents/occupied_heating_setpoint", 18.0).await;
    until(&hub, "chauffage:parents/boost", 1.5).await;
    until(&hub, "chauffage:parents/boost_minutes", 60.0).await;
    until(&hub, "chauffage:parents/hold_minutes", 180.0).await;
    assert_eq!((state("boost_until"), state("hold_until")), ends);
    until(&hub, "vannes:v/sp", 19.5).await;
    cancel.cancel();
}

#[test]
fn bad_rooms_are_refused() {
    assert!(check(&rooms()).is_ok());
    let cases: [fn(&mut Room); 18] = [
        |r| r.id = "a:b".into(),
        |r| r.name = " ".into(),
        |r| r.sensor = "piece".into(),
        |r| r.valves.clear(),
        |r| r.valves[0] = "vannes:v".into(),
        |r| r.valves[0] = "chauffage:autre/occupied_heating_setpoint".into(),
        |r| r.min = 26.0,
        |r| r.initial = 20.3,
        |r| r.initial = 26.0,
        |r| {
            r.correction = false;
            r.regulation = Some(Regulation::Force);
        },
        |r| r.force_offset = 0.0,
        |r| r.force_offset = 12.0,
        |r| r.hysteresis = -0.1,
        |r| r.max_boost = 0.0,
        |r| r.max_boost = 1.2,
        |r| r.max_boost = 10.5,
        |r| r.boost_default_minutes = 0,
        |r| r.hold_default_minutes = 721,
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

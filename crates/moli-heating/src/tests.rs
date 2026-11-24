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
        [[helper]]
        id = "absent"
        name = "Personne à la maison"
        kind = "switch"
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
            // Presence, as `personnes` tells it: read-only, moved by `set`.
            let presence = ctx.device_id("presence");
            let flag = |key: &str, write: bool| PointSpec {
                key: key.into(),
                label: key.into(),
                kind: Kind::Binary,
                access: Access { read: true, write },
                unit: None,
                semantic: Semantic::Other,
            };
            ctx.upsert_device(Device {
                id: presence.clone(),
                instance: ctx.instance().clone(),
                native_name: "presence".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: vec![flag("away", false), flag("set", true)],
            });
            ctx.set_state(&presence, "away", Value::Bool(false));
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                if command.device.id == presence {
                    ctx.set_state(&presence, "away", command.value.clone());
                }
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
    let (hub, cancel) = stand_ins_running().await;
    start(&hub, rooms, None, tune, &cancel);
    (hub, cancel)
}

async fn stand_ins_running() -> (Hub, CancellationToken) {
    stand_ins_with(HubOptions::default()).await
}

async fn stand_ins_with(options: HubOptions) -> (Hub, CancellationToken) {
    let hub = Hub::new(options).unwrap();
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
    (hub, cancel)
}

/// The heating, stopped by `cancel`.
fn start(
    hub: &Hub,
    rooms: Vec<Room>,
    house: Option<House>,
    tune: impl FnOnce(&mut Heating),
    cancel: &CancellationToken,
) {
    let mut heating = Heating::new(hub.clone(), rooms, house, jiff::tz::TimeZone::UTC);
    heating.settle = Duration::ZERO;
    heating.floor = Duration::ZERO;
    tune(&mut heating);
    spawn_driver(
        hub,
        InstanceId::from(INSTANCE),
        Arc::new(heating),
        cancel.clone(),
    );
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
    // Turned on its dial: a hand. Moli writes this valve at each look here
    // (no interval): a turn that lands just before one of its writes is
    // overwritten unseen, as on a real dial; turned again then.
    for _ in 0..5 {
        hub.command(
            &point("vannes:entiere/sp"),
            Value::Float(18.0),
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        if hub.state(&point(room)).map(|s| s.value) == Some(Value::from("manual")) {
            break;
        }
    }
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
        let mut heating = Heating::new(hub.clone(), rooms(), None, jiff::tz::TimeZone::UTC);
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

const TWO: &str = r#"
    [heating_house]
    programmes = ["Normal", "Télétravail"]
"#;

fn table(text: &str) -> House {
    #[derive(Deserialize)]
    struct File {
        heating_house: House,
    }
    toml::from_str::<File>(text).unwrap().heating_house
}

/// Today in the tests' zone (UTC), and another day.
fn days() -> (&'static str, &'static str) {
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .weekday()
        .to_monday_zero_offset();
    let today = usize::try_from(today).unwrap();
    (DAYS[today], DAYS[(today + 3) % 7])
}

async fn order(hub: &Hub, at: &str, value: Value) -> Result<(), String> {
    hub.command(&point(at), value, Origin::Ui, None)
        .await
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn each_room_keeps_a_week_per_programme_and_plays_the_house_s() {
    let (hub, cancel) = stand_ins_running().await;
    start(&hub, rooms(), Some(table(TWO)), |_| {}, &cancel);
    let (today, other) = days();
    let room = |key: &str| format!("chauffage:parents/{key}");
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    until_text(&hub, "chauffage:maison/today_as", "none").await;
    until_text(&hub, "chauffage:maison/system_mode", "auto").await;
    until(&hub, "vannes:v/sp", 20.0).await;
    until_text(&hub, &room("weekly_schedule_normal_monday"), "00:00/20").await;
    assert_eq!(
        hub.state(&point(&room("weekly_schedule_teletravail_monday")))
            .map(|s| s.value),
        Some(Value::Null)
    );
    // An order to the house is judged on its rooms' and valves' rooms.
    assert!(
        hub.rooms_of(&DeviceId::from("chauffage:maison"))
            .contains(&"Bureau".to_owned())
    );

    // Another programme's day: kept, not played.
    let teletravail = room(&format!("weekly_schedule_teletravail_{today}"));
    order(&hub, &teletravail, Value::from("00:00/22"))
        .await
        .unwrap();
    until_text(&hub, &room(&format!("weekly_schedule_{today}")), "00:00/20").await;
    until(&hub, &room("occupied_heating_setpoint"), 20.0).await;

    // The house plays it: every room's step in force, and its days.
    order(
        &hub,
        "chauffage:maison/programme",
        Value::from("Télétravail"),
    )
    .await
    .unwrap();
    until(&hub, &room("occupied_heating_setpoint"), 22.0).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    until_text(&hub, &room(&format!("weekly_schedule_{today}")), "00:00/22").await;
    // A day never set is the first programme's.
    until_text(&hub, &room(&format!("weekly_schedule_{other}")), "00:00/20").await;

    // `weekly_schedule_<day>` writes the playing programme's.
    order(
        &hub,
        &room(&format!("weekly_schedule_{today}")),
        Value::from("00:00/21"),
    )
    .await
    .unwrap();
    until_text(&hub, &teletravail, "00:00/21").await;
    until(&hub, "vannes:v/sp", 21.0).await;
    until_text(
        &hub,
        &room(&format!("weekly_schedule_normal_{today}")),
        "00:00/20",
    )
    .await;

    // Emptied: the first programme's again. The first one's cannot be.
    order(&hub, &teletravail, Value::from("")).await.unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;
    let normal = room(&format!("weekly_schedule_normal_{today}"));
    assert!(order(&hub, &normal, Value::from("")).await.is_err());

    cancel.cancel();
}

#[tokio::test]
async fn today_as_another_day_the_whole_house_and_a_restart() {
    let (hub, cancel) = stand_ins_running().await;
    let heating = CancellationToken::new();
    start(&hub, rooms(), Some(table(TWO)), |_| {}, &heating);
    let (today, other) = days();
    let room = |key: &str| format!("chauffage:parents/{key}");
    let teletravail = room(&format!("weekly_schedule_teletravail_{today}"));
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    order(
        &hub,
        "chauffage:maison/programme",
        Value::from("Télétravail"),
    )
    .await
    .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;

    // Today played as another day, then as itself again.
    order(
        &hub,
        &room(&format!("weekly_schedule_teletravail_{other}")),
        Value::from("00:00/18"),
    )
    .await
    .unwrap();
    order(&hub, "chauffage:maison/today_as", Value::from(other))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 18.0).await;
    order(&hub, "chauffage:maison/today_as", Value::from("none"))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;

    // The whole house to frost guard, and back.
    order(&hub, "chauffage:maison/system_mode", Value::from("off"))
        .await
        .unwrap();
    until_text(&hub, &room("system_mode"), "off").await;
    until(&hub, "vannes:v/sp", 5.0).await;
    order(&hub, "chauffage:maison/system_mode", Value::from("auto"))
        .await
        .unwrap();
    until_text(&hub, &room("system_mode"), "auto").await;
    until(&hub, "vannes:v/sp", 20.0).await;

    // Restarted: the programme, `today_as` and every week are kept.
    order(&hub, &teletravail, Value::from("00:00/19.5"))
        .await
        .unwrap();
    order(&hub, "chauffage:maison/today_as", Value::from(today))
        .await
        .unwrap();
    heating.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let heating = CancellationToken::new();
    start(&hub, rooms(), Some(table(TWO)), |_| {}, &heating);
    tokio::time::sleep(Duration::from_millis(50)).await;
    until_text(&hub, "chauffage:maison/programme", "Télétravail").await;
    until_text(&hub, "chauffage:maison/today_as", today).await;
    until_text(&hub, &teletravail, "00:00/19.5").await;
    until_text(
        &hub,
        &room(&format!("weekly_schedule_{today}")),
        "00:00/19.5",
    )
    .await;
    until(&hub, &room("occupied_heating_setpoint"), 19.5).await;
    heating.cancel();
    cancel.cancel();
}

/// The heating with an absence point, looked at often: a minute is 50 ms.
fn start_away(hub: &Hub, rooms: Vec<Room>, away_mode: &str, cancel: &CancellationToken) {
    start_away_on(hub, rooms, "vannes:presence/away", away_mode, cancel);
}

fn start_away_on(
    hub: &Hub,
    rooms: Vec<Room>,
    point: &str,
    away_mode: &str,
    cancel: &CancellationToken,
) {
    let table = table(&format!(
        r#"
        [heating_house]
        programmes = ["Normal", "Vacances"]
        away = "{point}"
        away_delay = 2
        away_mode = "{away_mode}"
        "#
    ));
    start(
        hub,
        rooms,
        Some(table),
        |h| {
            h.tick = Duration::from_millis(20);
            h.minute = Duration::from_millis(50);
        },
        cancel,
    );
}

async fn away(hub: &Hub, on: bool) {
    order(hub, "vannes:presence/set", Value::Bool(on))
        .await
        .unwrap();
}

async fn order_as(hub: &Hub, at: &str, value: Value, origin: Origin) {
    hub.command(&point(at), value, origin, None).await.unwrap();
}

/// The heating stopped, then started again on the same hub (its points as
/// the state cache gives them back).
async fn restart(
    heating: CancellationToken,
    again: impl FnOnce(&CancellationToken),
) -> CancellationToken {
    heating.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let heating = CancellationToken::new();
    again(&heating);
    heating
}

#[tokio::test]
async fn nobody_home_the_rooms_play_frost_guard_over_what_was_chosen() {
    let (hub, cancel) = stand_ins_running().await;
    let heating = CancellationToken::new();
    start_away(&hub, rooms(), "off", &heating);
    let room = |key: &str| format!("chauffage:parents/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;
    until_bool(&hub, "chauffage:maison/away", false).await;

    // Gone: the valves at their lowest after the delay; nothing the house or
    // the room chose is written over.
    away(&hub, true).await;
    until_bool(&hub, "chauffage:maison/away", true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    until_text(&hub, &room("system_mode"), "auto").await;
    until(&hub, &room("occupied_heating_setpoint"), 20.0).await;
    until_text(&hub, "chauffage:maison/system_mode", "auto").await;
    until_text(&hub, "chauffage:maison/programme", "Normal").await;

    // Restarted while nobody is home: still away, at once.
    let heating = restart(heating, |c| start_away(&hub, rooms(), "off", c)).await;
    until_bool(&hub, "chauffage:maison/away", true).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // Back: the room's own again.
    away(&hub, false).await;
    until_bool(&hub, "chauffage:maison/away", false).await;
    until(&hub, "vannes:v/sp", 20.0).await;

    // A room held in `heat` is frost-guarded too, and holds again on return.
    order(&hub, &room("system_mode"), Value::from("heat"))
        .await
        .unwrap();
    order(&hub, &room("occupied_heating_setpoint"), Value::Float(22.0))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 22.0).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    until_text(&hub, &room("system_mode"), "heat").await;
    away(&hub, false).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    order(&hub, &room("system_mode"), Value::from("auto"))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;
    heating.cancel();
    cancel.cancel();
}

#[tokio::test]
async fn only_a_person_setting_the_house_lifts_the_absence_kept_across_a_restart() {
    let (hub, cancel) = stand_ins_running().await;
    let heating = CancellationToken::new();
    start_away(&hub, rooms(), "off", &heating);
    until(&hub, "vannes:v/sp", 20.0).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // An automation changes the choice played on return, nothing more.
    let programme = "chauffage:maison/programme";
    order_as(&hub, programme, Value::from("Vacances"), Origin::Automation).await;
    until_text(&hub, programme, "Vacances").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    until_bool(&hub, "chauffage:maison/away_lifted", false).await;

    // A person: lifted until the return, and after a restart too.
    order_as(&hub, programme, Value::from("Normal"), Origin::Ui).await;
    until_bool(&hub, "chauffage:maison/away_lifted", true).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    let heating = restart(heating, |c| start_away(&hub, rooms(), "off", c)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    until_bool(&hub, "chauffage:maison/away_lifted", true).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    away(&hub, false).await;
    until_bool(&hub, "chauffage:maison/away", false).await;
    until_bool(&hub, "chauffage:maison/away_lifted", false).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    heating.cancel();
    cancel.cancel();
}

#[tokio::test]
async fn a_person_ordering_a_room_takes_it_out_of_the_absence_until_it_ends() {
    let (hub, cancel) = stand_ins_running().await;
    let heating = CancellationToken::new();
    start_away(&hub, rooms(), "off", &heating);
    let room = |key: &str| format!("chauffage:parents/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // An automation's setpoint is the room's on return only.
    let setpoint = room("occupied_heating_setpoint");
    order_as(&hub, &setpoint, Value::Float(21.0), Origin::Automation).await;
    until(&hub, &setpoint, 21.0).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    until_bool(&hub, &room("away_lifted"), false).await;

    // A person's: the room out of the absence, after a restart too.
    order_as(&hub, &setpoint, Value::Float(22.0), Origin::Ui).await;
    until_bool(&hub, &room("away_lifted"), true).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    until_bool(&hub, "chauffage:maison/away", true).await;
    let heating = restart(heating, |c| start_away(&hub, rooms(), "off", c)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    until_bool(&hub, &room("away_lifted"), true).await;
    until(&hub, "vannes:v/sp", 20.0).await;

    // The absence over: the room is like the others again.
    away(&hub, false).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    heating.cancel();
    cancel.cancel();
}

#[tokio::test]
async fn nobody_home_the_rooms_play_the_away_programme() {
    let (hub, cancel) = stand_ins_running().await;
    start_away(&hub, rooms(), "Vacances", &cancel);
    let (today, _) = days();
    let room = |key: &str| format!("chauffage:parents/{key}");
    let day = room(&format!("weekly_schedule_{today}"));
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    order(
        &hub,
        &room(&format!("weekly_schedule_vacances_{today}")),
        Value::from("00:00/16"),
    )
    .await
    .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;

    // The away programme played; the house's choice and its days stay.
    away(&hub, true).await;
    until(&hub, &room("occupied_heating_setpoint"), 16.0).await;
    until(&hub, "vannes:v/sp", 16.0).await;
    until_text(&hub, &day, "00:00/20").await;
    until_text(&hub, "chauffage:maison/programme", "Normal").await;

    // `weekly_schedule_<day>` writes the chosen programme, played on return.
    order(&hub, &day, Value::from("00:00/18")).await.unwrap();
    until_text(
        &hub,
        &room(&format!("weekly_schedule_normal_{today}")),
        "00:00/18",
    )
    .await;
    until_text(
        &hub,
        &room(&format!("weekly_schedule_vacances_{today}")),
        "00:00/16",
    )
    .await;
    until(&hub, "vannes:v/sp", 16.0).await;
    away(&hub, false).await;
    until(&hub, &room("occupied_heating_setpoint"), 18.0).await;
    until(&hub, "vannes:v/sp", 18.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_forced_room_is_frost_guarded_while_nobody_is_home() {
    #[derive(Deserialize)]
    struct File {
        heating: Vec<Room>,
    }
    let forced = || {
        toml::from_str::<File>(
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
        .heating
    };
    let (hub, cancel) = stand_ins_running().await;
    start_away(&hub, forced(), "off", &cancel);
    // Under its setpoint: wide open (20 + 4), heat asked for.
    order(&hub, "maison:piece/value", Value::Float(19.5))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 24.0).await;
    until_bool(&hub, "chauffage:bureau/heat_demand", true).await;
    // Nobody home: the valve's lowest, not the forced setpoint.
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    until_bool(&hub, "chauffage:bureau/heat_demand", false).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    away(&hub, false).await;
    until(&hub, "vannes:v/sp", 24.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_writable_absence_point_is_never_followed() {
    let (hub, cancel) = stand_ins_running().await;
    start_away_on(&hub, rooms(), "maison:absent/on", "off", &cancel);
    until(&hub, "vannes:v/sp", 20.0).await;
    order(&hub, "maison:absent/on", Value::Bool(true))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    until_bool(&hub, "chauffage:maison/away", false).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn an_agent_s_order_to_the_house_waits_for_a_person_when_a_room_is_protected() {
    let options = HubOptions {
        guard: moli_runtime::guard::GuardPolicy {
            protected_rooms: vec!["Chambre".into()],
            ..Default::default()
        },
        ..HubOptions::default()
    };
    let (hub, cancel) = stand_ins_with(options).await;
    start(&hub, rooms(), Some(table(TWO)), |_| {}, &cancel);
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    let at = point("chauffage:maison/programme");
    let value = Value::from("Télétravail");
    assert!(hub.would_hold(&at, Some(&value), Origin::Mcp).is_some());
    assert!(hub.would_hold(&at, Some(&value), Origin::Ui).is_none());
    let mode = point("chauffage:maison/system_mode");
    assert!(
        hub.would_hold(&mode, Some(&Value::from("off")), Origin::Mcp)
            .is_some()
    );
    cancel.cancel();
}

#[tokio::test]
async fn without_the_programmes_each_room_keeps_the_first_one_s_week() {
    let (hub, cancel) = stand_ins_running().await;
    let heating = CancellationToken::new();
    start(&hub, rooms(), Some(table(TWO)), |_| {}, &heating);
    let (today, _) = days();
    let room = |key: &str| format!("chauffage:parents/{key}");
    let day = room(&format!("weekly_schedule_{today}"));
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    order(
        &hub,
        "chauffage:maison/programme",
        Value::from("Télétravail"),
    )
    .await
    .unwrap();
    order(&hub, &day, Value::from("00:00/22")).await.unwrap();
    until(&hub, "vannes:v/sp", 22.0).await;

    // `[heating_house]` removed: Normal's week, not the one played last.
    let heating = restart(heating, |c| start(&hub, rooms(), None, |_| {}, c)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    until_text(&hub, &day, "00:00/20").await;
    until(&hub, "vannes:v/sp", 20.0).await;
    assert!(hub.device(&DeviceId::from("chauffage:maison")).is_none());
    heating.cancel();
    cancel.cancel();
}

async fn try_as(hub: &Hub, at: &str, value: Value, origin: Origin) -> bool {
    hub.command(&point(at), value, origin, None).await.is_ok()
}

#[tokio::test]
async fn the_absence_ends_boosts_and_holds_and_a_programme_change_applies_over_a_hold() {
    let (hub, cancel) = stand_ins_running().await;
    start_away(&hub, rooms(), "off", &cancel);
    let room = |key: &str| format!("chauffage:parents/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;

    // Held at 21: a change of the house's programme applies all the same.
    order(&hub, &room("occupied_heating_setpoint"), Value::Float(21.0))
        .await
        .unwrap();
    order(&hub, &room("hold_minutes"), Value::Int(600))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 21.0).await;
    let (today, _) = days();
    order(
        &hub,
        &room(&format!("weekly_schedule_vacances_{today}")),
        Value::from("00:00/19"),
    )
    .await
    .unwrap();
    order(&hub, "chauffage:maison/programme", Value::from("Vacances"))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 19.0).await;
    assert!(left(&hub, "hold_until").is_none());
    order(&hub, "chauffage:maison/programme", Value::from("Normal"))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;

    // A boost and a hold, then nobody home: both end, the valves at their
    // lowest; back, the schedule's 20, not 22.
    order(&hub, &room("boost"), Value::Float(2.0))
        .await
        .unwrap();
    order(&hub, &room("boost_minutes"), Value::Int(600))
        .await
        .unwrap();
    order(&hub, &room("hold_minutes"), Value::Int(600))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 22.0).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    until(&hub, &room("boost"), 0.0).await;
    assert!(left(&hub, "hold_until").is_none());
    away(&hub, false).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn the_house_s_mode_ends_boosts_and_holds() {
    let (hub, cancel) = stand_ins_running().await;
    start(
        &hub,
        rooms(),
        Some(table(TWO)),
        |h| h.tick = Duration::from_millis(20),
        &cancel,
    );
    let room = |key: &str| format!("chauffage:parents/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;
    order(&hub, &room("boost"), Value::Float(2.0))
        .await
        .unwrap();
    order(&hub, &room("boost_minutes"), Value::Int(600))
        .await
        .unwrap();
    order(&hub, &room("hold_minutes"), Value::Int(600))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 22.0).await;
    order(&hub, "chauffage:maison/system_mode", Value::from("off"))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 5.0).await;
    until(&hub, &room("boost"), 0.0).await;
    assert!(left(&hub, "hold_until").is_none());
    order(&hub, "chauffage:maison/system_mode", Value::from("auto"))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn a_person_s_boost_or_hold_takes_the_room_out_of_the_absence_once_taken() {
    let (hub, cancel) = stand_ins_running().await;
    let heating = CancellationToken::new();
    start_away(&hub, rooms(), "off", &heating);
    let room = |key: &str| format!("chauffage:parents/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // Refused by the room (no boost to lengthen): nothing lifted.
    assert!(!try_as(&hub, &room("boost_minutes"), Value::Int(30), Origin::Ui).await);
    tokio::time::sleep(Duration::from_millis(150)).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // A person's boost: out of the absence, the schedule's 20 and the boost.
    assert!(try_as(&hub, &room("boost"), Value::Float(2.0), Origin::Ui).await);
    until_bool(&hub, &room("away_lifted"), true).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    away(&hub, false).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    order(&hub, &room("boost"), Value::Float(0.0))
        .await
        .unwrap();
    until(&hub, "vannes:v/sp", 20.0).await;

    // A person's hold: the same.
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    assert!(try_as(&hub, &room("hold_minutes"), Value::Int(120), Origin::Ui).await);
    until_bool(&hub, &room("away_lifted"), true).await;
    until(&hub, "vannes:v/sp", 20.0).await;
    assert!(left(&hub, "hold_until").is_some());

    // A room in `heat` cannot be held: the order refused, still away.
    away(&hub, false).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    order(&hub, &room("system_mode"), Value::from("heat"))
        .await
        .unwrap();
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    assert!(!try_as(&hub, &room("hold_minutes"), Value::Int(120), Origin::Ui).await);
    tokio::time::sleep(Duration::from_millis(150)).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    until(&hub, "vannes:v/sp", 5.0).await;
    heating.cancel();
    cancel.cancel();
}

#[tokio::test]
async fn a_person_s_hold_outlives_the_return_and_ending_one_never_lifts_the_absence() {
    let (hub, cancel) = stand_ins_running().await;
    start_away(&hub, rooms(), "off", &cancel);
    let room = |key: &str| format!("chauffage:parents/{key}");
    until(&hub, "vannes:v/sp", 20.0).await;
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // Ending a boost or a hold, or none: the room stays away.
    for (key, value) in [
        ("boost", Value::Float(0.0)),
        ("boost_minutes", Value::Int(0)),
        ("hold_minutes", Value::Int(0)),
    ] {
        assert!(try_as(&hub, &room(key), value, Origin::Ui).await, "{key}");
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    until(&hub, "vannes:v/sp", 5.0).await;

    // Held at 22 for 2 h by a person: out of the absence, and still held
    // once someone is back.
    order(&hub, &room("occupied_heating_setpoint"), Value::Float(22.0))
        .await
        .unwrap();
    order(&hub, &room("hold_minutes"), Value::Int(120))
        .await
        .unwrap();
    until_bool(&hub, &room("away_lifted"), true).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    away(&hub, false).await;
    until_bool(&hub, "chauffage:maison/away", false).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    until(&hub, &room("occupied_heating_setpoint"), 22.0).await;
    until(&hub, "vannes:v/sp", 22.0).await;
    assert!(left(&hub, "hold_until").is_some());
    cancel.cancel();
}

#[tokio::test]
async fn a_hold_over_during_the_absence_gives_the_played_programme_s_step() {
    let (hub, cancel) = stand_ins_running().await;
    let mut held = rooms();
    held[0].hold_default_minutes = 4;
    start_away(&hub, held, "Vacances", &cancel);
    let (today, _) = days();
    let room = |key: &str| format!("chauffage:parents/{key}");
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    order(
        &hub,
        &room(&format!("weekly_schedule_vacances_{today}")),
        Value::from("00:00/16"),
    )
    .await
    .unwrap();
    away(&hub, true).await;
    until(&hub, "vannes:v/sp", 16.0).await;

    // An automation's setpoint, held for a while (the room stays away):
    // over, the away programme's 16, not the chosen one's 20.
    order_as(
        &hub,
        &room("occupied_heating_setpoint"),
        Value::Float(21.0),
        Origin::Automation,
    )
    .await;
    until(&hub, "vannes:v/sp", 21.0).await;
    until_bool(&hub, &room("away_lifted"), false).await;
    until(&hub, "vannes:v/sp", 16.0).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    until(&hub, "vannes:v/sp", 16.0).await;
    cancel.cancel();
}

#[tokio::test]
async fn without_the_programmes_after_a_real_restart_the_first_one_s_week() {
    let dir = std::env::temp_dir().join(format!(
        "moli-heating-programmes-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let options = || HubOptions {
        state_path: Some(dir.join("state.json")),
        ..HubOptions::default()
    };
    let (today, _) = days();
    let day = format!("chauffage:parents/weekly_schedule_{today}");

    let (hub, cancel) = stand_ins_with(options()).await;
    start(&hub, rooms(), Some(table(TWO)), |_| {}, &cancel);
    until_text(&hub, "chauffage:maison/programme", "Normal").await;
    order(
        &hub,
        "chauffage:maison/programme",
        Value::from("Télétravail"),
    )
    .await
    .unwrap();
    order(&hub, &day, Value::from("00:00/22")).await.unwrap();
    until(&hub, "vannes:v/sp", 22.0).await;
    hub.persist_state().await.unwrap();
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A new hub from the state on disk, the heating without `[heating_house]`.
    let (hub, cancel) = stand_ins_with(options()).await;
    start(&hub, rooms(), None, |_| {}, &cancel);
    until_text(&hub, &day, "00:00/20").await;
    until(&hub, "chauffage:parents/occupied_heating_setpoint", 20.0).await;
    for _ in 0..100 {
        if hub.device(&DeviceId::from("chauffage:maison")).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(hub.device(&DeviceId::from("chauffage:maison")).is_none());
    cancel.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_house_is_refused() {
    let good = || {
        table(
            r#"
            [heating_house]
            programmes = ["Normal", "Télétravail", "Vacances d'été"]
            away = "personnes:maison/everyone_away"
            away_mode = "Vacances d'été"
            "#,
        )
    };
    assert!(house::check(&good(), &rooms()).is_ok());
    assert_eq!(house::key("Vacances d'été"), "vacances_d_ete");
    assert_eq!(house::key(" Télé-travail 2 "), "tele_travail_2");
    let cases: [fn(&mut House); 8] = [
        |h| h.programmes.clear(),
        |h| h.programmes[1] = String::new(),
        |h| h.programmes[1] = " Normal".into(),
        |h| h.programmes[1] = "normal".into(),
        |h| h.programmes[1] = "!!".into(),
        |h| h.away = Some("chauffage:parents/system_mode".into()),
        |h| h.away_mode = "Absent".into(),
        |h| h.away_delay = 2000,
    ];
    for (i, change) in cases.iter().enumerate() {
        let mut table = good();
        change(&mut table);
        assert!(house::check(&table, &rooms()).is_err(), "case {i}");
    }
    let mut taken = rooms();
    taken[0].id = house::DEVICE.into();
    assert!(house::check(&good(), &taken).is_err());
    assert!(house::check(&good(), &[]).is_err());
}

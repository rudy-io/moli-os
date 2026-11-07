use super::*;
use crate::people::{PersonRef, Role};
use moli_core::{Access, Device, InstanceId, Kind, PointSpec, Semantic};
use moli_runtime::{BoxFuture, Driver, DriverCtx, HubOptions, spawn_driver};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Scenario(Arc<AtomicUsize>);
impl Driver for Scenario {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            ctx.upsert_device(Device {
                id: ctx.device_id("cloud:scenario:fixture"),
                instance: ctx.instance().clone(),
                native_name: "Scenario".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: Some("Guest room".into()),
                members: vec![],
                points: vec![PointSpec {
                    key: "run".into(),
                    label: "Run".into(),
                    kind: Kind::Enum {
                        values: vec!["opaque".into()],
                    },
                    access: Access {
                        read: false,
                        write: true,
                    },
                    unit: None,
                    semantic: Semantic::Control,
                }],
            });
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                self.0.fetch_add(1, Ordering::SeqCst);
                command.reply(Ok(()));
            }
            Ok(())
        })
    }
}
#[tokio::test]
async fn a_guest_with_a_pin_and_a_matching_room_never_reaches_a_scenario_driver() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let task = spawn_driver(
        &hub,
        InstanceId::from("fixture"),
        Arc::new(Scenario(calls.clone())),
        cancel.clone(),
    );
    for _ in 0..100 {
        if hub.stats().drivers_running == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let sessions = Arc::new(Sessions::new(
        hub.clone(),
        Some(crate::UiPin::new("482915")),
        &[],
    ));
    let mut caller = Caller {
        via: Via::Person,
        key: "synthetic-guest".into(),
        person: Some(PersonRef {
            id: "guest".into(),
            name: "Guest".into(),
            role: Role::Guest,
            rooms: vec!["Guest room".into()],
        }),
    };
    let token = sessions.login("482915", &caller).await.unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("cookie", format!("moli_session={token}").parse().unwrap());
    headers.insert("x-moli-origin", "ui".parse().unwrap());
    assert!(sessions.is_human(&headers, &caller.key));
    let guest = caller.person.as_ref().unwrap();
    assert!(guest.guest_devices(&hub, &hub.snapshot()).is_empty());
    assert!(guest.guest_view(&hub).0.devices.is_empty());
    assert!(!guest.guest_sees(&hub, &DeviceId::from("fixture:cloud:scenario:fixture")));
    let body = || {
        Json(CommandBody {
            point: PointId::from("fixture:cloud:scenario:fixture/run"),
            value: Value::from("opaque"),
            actor: None,
        })
    };
    let response = command(
        State(hub.clone()),
        Extension(sessions.clone()),
        caller.clone(),
        headers.clone(),
        body(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    caller.person.as_mut().unwrap().role = Role::Owner;
    let response = command(State(hub), Extension(sessions), caller, headers, body()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cancel.cancel();
    task.await.unwrap();
}

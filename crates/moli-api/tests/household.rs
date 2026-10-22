//! The household's dashboard without a code: obeyed in the house, not in its
//! protected rooms. Anyone on the home network can claim to be the
//! dashboard (`X-Moli-Origin: ui` is public); only the code proves a person.

use std::sync::Arc;
use std::time::Duration;

use moli_core::{Access, Device, InstanceId, Kind, Origin, PointSpec, Semantic, Value};
use moli_runtime::guard::{Dashboard, GuardPolicy};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub, HubOptions, LabelPatch, spawn_driver};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

struct Lamp;

impl Driver for Lamp {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            for name in ["lamp", "bulb"] {
                let id = ctx.device_id(name);
                ctx.upsert_device(Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: name.into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    parent: None,
                    members: Vec::new(),
                    points: vec![PointSpec {
                        key: "state".into(),
                        label: "State".into(),
                        kind: Kind::Binary,
                        access: Access {
                            read: true,
                            write: true,
                        },
                        unit: None,
                        semantic: Semantic::OnOff,
                    }],
                });
                ctx.set_state(&id, "state", Value::Bool(false));
            }
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

const PIN: &str = "482915";
const UI: &str = "X-Moli-Origin: ui\r\n";

/// A hub whose lamp sleeps in a protected bedroom (the bulb has no room).
async fn bedroom(dir: &std::path::Path) -> Hub {
    std::fs::create_dir_all(dir).unwrap();
    let hub = Hub::new(HubOptions {
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::generate().unwrap(),
        )),
        guard: GuardPolicy {
            protected_rooms: vec!["Chambre".into()],
            quiet_hours: None,
            protect_unassigned: false,
            dashboard: Dashboard::Trusted,
        },
        ..HubOptions::default()
    })
    .unwrap();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(Lamp),
        CancellationToken::new(),
    );
    for _ in 0..200 {
        if hub.stats().drivers_running == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let patch = LabelPatch {
        name: None,
        room: Some("Chambre".into()),
    };
    hub.set_label(&"fake:lamp".into(), patch, Origin::Ui, None)
        .await
        .unwrap();
    hub
}

async fn serve(hub: &Hub, options: &moli_api::Options) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = moli_api::router(hub.clone(), CancellationToken::new(), options);
    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Status, the `moli_session` cookie it sets (if any), body.
async fn request(
    addr: &str,
    method: &str,
    path: &str,
    body: &Json,
    extra: &str,
) -> (u16, Option<String>, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.to_string();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let status = raw[9..12].parse().unwrap();
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
    let cookie = headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.eq_ignore_ascii_case("set-cookie") && v.trim().starts_with("moli_session="))
            .then(|| v.trim().split(';').next().unwrap_or_default().to_owned())
    });
    (status, cookie, body.to_owned())
}

fn lamp(hub: &Hub) -> Value {
    hub.state(&"fake:lamp/state".into()).unwrap().value
}

#[tokio::test]
async fn the_dashboard_without_a_code_stops_at_protected_rooms() {
    let dir = std::env::temp_dir().join(format!("moli-household-{}", std::process::id()));
    let hub = bedroom(&dir).await;
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new(PIN)),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let on =
        json!({ "point": "fake:lamp/state", "value": true, "actor": "tableau de bord (papa)" });

    // Claiming to be the dashboard: the rest of the house obeys…
    let bulb = json!({ "point": "fake:bulb/state", "value": true });
    let (status, _, body) = request(&addr, "POST", "/api/command", &bulb, UI).await;
    assert_eq!(status, 200, "{body}");
    // …the bedroom waits for the code.
    let (status, _, body) = request(&addr, "POST", "/api/command", &on, UI).await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(lamp(&hub), Value::Bool(false));
    // The journal says who clicked, not who the client says it is.
    let entry = &hub.journal(1)[0];
    assert_eq!(entry.origin, Origin::Household);
    assert_ne!(entry.actor.as_deref(), Some("tableau de bord (papa)"));

    // Nor can it take the lamp out of the bedroom; naming it is fine.
    let out = json!({ "room": "Salon" });
    let (status, _, _) = request(&addr, "PUT", "/api/labels/fake:lamp", &out, UI).await;
    assert_eq!(status, 403);
    assert!(
        hub.rooms_of(&"fake:lamp".into())
            .contains(&"Chambre".to_owned())
    );
    let name = json!({ "name": "Veilleuse" });
    let (status, _, _) = request(&addr, "PUT", "/api/labels/fake:lamp", &name, UI).await;
    assert_eq!(status, 200);

    // The code makes it a person: obeyed in the bedroom too.
    let pin = json!({ "pin": PIN });
    let (status, cookie, _) = request(&addr, "POST", "/api/session", &pin, "").await;
    assert_eq!(status, 200);
    let person = format!("{UI}Cookie: {}\r\n", cookie.unwrap());
    let (status, _, body) = request(&addr, "POST", "/api/command", &on, &person).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(lamp(&hub), Value::Bool(true));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_page_from_another_site_cannot_command_and_nothing_frames_the_house() {
    let dir = std::env::temp_dir().join(format!("moli-household-web-{}", std::process::id()));
    let hub = bedroom(&dir).await;
    let addr = serve(&hub, &moli_api::Options::default()).await;
    let bulb = json!({ "point": "fake:bulb/state", "value": true });
    // A page elsewhere, opened by a browser of the household.
    let elsewhere = format!("{UI}Origin: https://evil.example\r\nSec-Fetch-Site: cross-site\r\n");
    let (status, _, _) = request(&addr, "POST", "/api/command", &bulb, &elsewhere).await;
    assert_eq!(status, 403);
    assert_eq!(
        hub.state(&"fake:bulb/state".into()).unwrap().value,
        Value::Bool(false)
    );
    // The dashboard itself, from its own page.
    let own = format!("{UI}Origin: http://{addr}\r\nSec-Fetch-Site: same-origin\r\n");
    let (status, _, _) = request(&addr, "POST", "/api/command", &bulb, &own).await;
    assert_eq!(status, 200);

    // Every answer refuses to be framed and keeps its type.
    let mut stream = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let head = format!("GET /api/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    assert!(raw.contains("x-frame-options: deny"), "{raw}");
    assert!(raw.contains("content-security-policy: frame-ancestors 'none'"));
    assert!(raw.contains("x-content-type-options: nosniff"));
    assert!(raw.contains("cache-control: no-store"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_routine_reaching_a_bedroom_wants_the_code_from_the_dashboard() {
    use moli_automation::{Actor, Author, Automation, Automations};
    let dir = std::env::temp_dir().join(format!("moli-household-auto-{}", std::process::id()));
    let hub = bedroom(&dir).await;
    let (autos, _engine) = Automations::start(
        &hub,
        moli_automation::Config::default(),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let person = Actor {
        human: true,
        author: Author::Human,
    };
    let mut fingerprints = Vec::new();
    for (id, point) in [("salon", "fake:bulb/state"), ("nuit", "fake:lamp/state")] {
        let routine: Automation = serde_json::from_value(json!({
            "id": id, "name": id, "enabled": false,
            "graph": {
                "nodes": [
                    { "id": "t", "type": "manual" },
                    { "id": "s", "type": "set", "point": point, "value": true }
                ],
                "edges": [{ "from": "t", "to": "s" }]
            }
        }))
        .unwrap();
        let routine = autos.save(routine, person).await.unwrap();
        autos
            .approve(id, &routine.fingerprint(), person)
            .await
            .unwrap();
        fingerprints.push(routine.fingerprint());
    }
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new(PIN)),
        automations: Some(autos.clone()),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;

    // Approved and harmless: the dashboard runs it without a code (D0).
    let seen = json!({ "fingerprint": fingerprints[0] });
    let (status, _, body) = request(&addr, "POST", "/api/automations/salon/run", &seen, UI).await;
    assert_eq!(status, 200, "{body}");
    // Approved, but it reaches the bedroom: the code first.
    let seen = json!({ "fingerprint": fingerprints[1] });
    let (status, _, body) = request(&addr, "POST", "/api/automations/nuit/run", &seen, UI).await;
    assert_eq!(status, 403, "{body}");
    let pin = json!({ "pin": PIN });
    let (_, cookie, _) = request(&addr, "POST", "/api/session", &pin, "").await;
    let person = format!("{UI}Cookie: {}\r\n", cookie.unwrap());
    let (status, _, body) =
        request(&addr, "POST", "/api/automations/nuit/run", &seen, &person).await;
    assert_eq!(status, 200, "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

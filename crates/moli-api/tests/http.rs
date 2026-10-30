//! End-to-end: a real HTTP server over a hub with a fake driver.

use std::sync::Arc;
use std::time::Duration;

use moli_core::{Access, Device, InstanceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub, HubOptions, spawn_driver};
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
            let id = ctx.device_id("lamp");
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "lamp".into(),
                manufacturer: None,
                model: Some("L1".into()),
                description: None,
                native_room: None,
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
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

async fn server() -> (String, Hub) {
    let hub = lamp_hub().await;
    let addr = serve(&hub, &moli_api::Options::default()).await;
    (addr, hub)
}

/// A hub with the fake lamp running.
async fn lamp_hub() -> Hub {
    let hub = Hub::new(HubOptions::default()).unwrap();
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
    hub
}

/// The API over `hub`, listening; its address.
async fn serve(hub: &Hub, options: &moli_api::Options) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = moli_api::router(hub.clone(), CancellationToken::new(), options);
    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Minimal HTTP/1.1 client: enough for tests, no extra dependency.
async fn request(addr: &str, method: &str, path: &str, body: Option<&Json>) -> (u16, String) {
    request_with(addr, method, path, body, "").await
}

/// `extra`: more header lines, each ending with a CRLF.
async fn request_with(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<&Json>,
    extra: &str,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.map(ToString::to_string).unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         {extra}Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let status = raw[9..12].parse().unwrap();
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
    let body = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_owned()
    };
    (status, body)
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        out.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
    out
}

#[tokio::test]
async fn rest_command_round_trip() {
    let (addr, hub) = server().await;

    let (status, body) = request(&addr, "GET", "/api/devices", None).await;
    assert_eq!(status, 200);
    let snapshot: Json = serde_json::from_str(&body).unwrap();
    assert_eq!(snapshot["devices"][0]["id"], "fake:lamp");
    assert_eq!(snapshot["devices"][0]["state"]["state"]["value"], false);

    let (status, _) = request(
        &addr,
        "POST",
        "/api/command",
        Some(&json!({"point": "fake:lamp/state", "value": true})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        hub.state(&"fake:lamp/state".into()).unwrap().value,
        Value::Bool(true)
    );

    let (status, body) = request(
        &addr,
        "POST",
        "/api/command",
        Some(&json!({"point": "fake:lamp/state", "value": "ON"})),
    )
    .await;
    assert_eq!(status, 422, "{body}");

    let (status, body) = request(&addr, "GET", "/api/health", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Json>(&body).unwrap()["stats"]["devices"],
        1
    );
}

#[tokio::test]
async fn an_approved_routine_runs_from_the_household_dashboard_without_a_code() {
    use moli_automation::{Actor, Author, Automation, Automations};
    let hub = lamp_hub().await;
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
    let routine: Automation = serde_json::from_value(json!({
        "id": "soir", "name": "Soir", "enabled": false,
        "graph": {
            "nodes": [
                { "id": "t", "type": "manual" },
                { "id": "s", "type": "set", "point": "fake:lamp/state", "value": true }
            ],
            "edges": [{ "from": "t", "to": "s" }]
        }
    }))
    .unwrap();
    let routine = autos.save(routine, person).await.unwrap();
    let fingerprint = routine.fingerprint();
    let options = moli_api::Options {
        automations: Some(autos.clone()),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let body = json!({ "fingerprint": fingerprint });

    // Not approved yet: even the dashboard cannot run it.
    let ui = "X-Moli-Origin: ui\r\n";
    let (status, _) =
        request_with(&addr, "POST", "/api/automations/soir/run", Some(&body), ui).await;
    assert_eq!(status, 422);

    autos.approve("soir", &fingerprint, person).await.unwrap();
    // Without the dashboard's mark (an agent): a person is needed.
    let (status, _) = request(&addr, "POST", "/api/automations/soir/run", Some(&body)).await;
    assert_eq!(status, 403);
    assert_eq!(
        hub.state(&"fake:lamp/state".into()).unwrap().value,
        Value::Bool(false)
    );
    // From the household's dashboard: a direct order, no code.
    let (status, body) =
        request_with(&addr, "POST", "/api/automations/soir/run", Some(&body), ui).await;
    assert_eq!(status, 200, "{body}");
    for _ in 0..200 {
        if hub.state(&"fake:lamp/state".into()).unwrap().value == Value::Bool(true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the routine did not switch the lamp on");
}

#[tokio::test]
async fn labels_via_rest() {
    let (addr, _hub) = server().await;
    let (status, body) = request(
        &addr,
        "PUT",
        "/api/labels/fake:lamp",
        Some(&json!({"name": "Lampe", "room": "Salon"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (_, body) = request(&addr, "GET", "/api/devices/fake:lamp", None).await;
    assert_eq!(
        serde_json::from_str::<Json>(&body).unwrap()["label"]["room"],
        "Salon"
    );
}

#[tokio::test]
async fn the_system_map_names_the_house_time_zone() {
    let hub = lamp_hub().await;
    let options = moli_api::Options {
        timezone: Some("Indian/Reunion".into()),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let (status, body) = request(&addr, "GET", "/api/system", None).await;
    assert_eq!(status, 200, "{body}");
    let system: Json = serde_json::from_str(&body).unwrap();
    assert_eq!(system["timezone"], "Indian/Reunion");
}

#[tokio::test]
async fn foreign_host_is_rejected_everywhere() {
    let (addr, _hub) = server().await;
    for path in ["/api/devices", "/mcp", "/"] {
        let mut stream = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let head =
            format!("GET {path} HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        assert!(raw.starts_with("HTTP/1.1 403"), "{path}: {raw}");
    }
}

#[tokio::test]
async fn history_over_rest_and_mcp() {
    let dir = std::env::temp_dir().join(format!("moli-api-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(moli_history::Store::open(&dir.join("h.db")).unwrap());
    let now = moli_core::now_ms();
    store
        .insert(
            &[
                ("fake:lamp/state".into(), Value::Bool(false), now - 60_000),
                ("fake:lamp/state".into(), Value::Bool(true), now - 30_000),
            ],
            false,
        )
        .unwrap();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let options = moli_api::Options {
        history: Some(moli_history::History::from_store(store)),
        ..moli_api::Options::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = moli_api::router(hub, CancellationToken::new(), &options)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (status, body) = request(
        &addr,
        "GET",
        "/api/history?point=fake:lamp/state&hours=1",
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let series: Json = serde_json::from_str(&body).unwrap();
    assert_eq!(series["total"], 2);
    assert_eq!(series["raw"][1][1], true);

    let call = rpc(
        1,
        "tools/call",
        &json!({"name": "history_sql", "arguments": {"sql": "SELECT COUNT(*) AS n FROM history WHERE num = 1"}}),
    );
    let (status, body) = request(&addr, "POST", "/mcp", Some(&call)).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#"\"rows\":[[1]]"#), "{body}");

    let write = rpc(
        2,
        "tools/call",
        &json!({"name": "history_sql", "arguments": {"sql": "DELETE FROM samples"}}),
    );
    let (_, body) = request(&addr, "POST", "/mcp", Some(&write)).await;
    assert!(body.contains("read-only"), "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

fn rpc(id: u64, method: &str, params: &Json) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[tokio::test]
async fn mcp_tools_drive_the_home() {
    let (addr, hub) = server().await;

    let init = rpc(
        1,
        "initialize",
        &json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    let (status, body) = request(&addr, "POST", "/mcp", Some(&init)).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("moli-os"), "{body}");

    let (status, body) = request(
        &addr,
        "POST",
        "/mcp",
        Some(&rpc(2, "tools/list", &json!({}))),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    for tool in [
        "system_status",
        "list_devices",
        "get_device",
        "send_command",
        "set_label",
        "get_journal",
    ] {
        assert!(body.contains(tool), "missing tool {tool}: {body}");
    }

    let call = rpc(
        3,
        "tools/call",
        &json!({"name": "send_command", "arguments": {"point": "fake:lamp/state", "value": true, "actor": "test-agent"}}),
    );
    let (status, body) = request(&addr, "POST", "/mcp", Some(&call)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        hub.state(&"fake:lamp/state".into()).unwrap().value,
        Value::Bool(true)
    );
    let journal = hub.journal(1);
    assert_eq!(journal[0].actor.as_deref(), Some("test-agent"));
}

#[tokio::test]
async fn speaking_without_an_assistant_says_so() {
    let (addr, _hub) = server().await;
    let (status, body) = request(
        &addr,
        "POST",
        "/api/assistant/speak",
        Some(&json!({ "text": "Bonsoir" })),
    )
    .await;
    assert_eq!(status, 503, "{body}");
    let (status, body) = request(&addr, "GET", "/api/assistant", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"speak\":false"), "{body}");
}
#[tokio::test]
async fn deciding_a_proposal_without_an_assistant_says_so() {
    let (addr, _hub) = server().await;
    let (status, body) = request(
        &addr,
        "POST",
        "/api/assistant/proposals/1",
        Some(&json!({ "approve": true })),
    )
    .await;
    assert_eq!(status, 503, "{body}");
}

/// « Pas ça »: the corrections are a person's. Without the code, a script, an agent or the bare
/// dashboard is refused; with it, an unknown turn or rule is simply gone.
#[tokio::test]
async fn only_a_person_corrects_the_assistant_and_manages_what_it_learned() {
    let hub = lamp_hub().await;
    let moli = moli_assistant::Assistant::new(
        hub.clone(),
        None,
        None,
        None,
        moli_assistant::Config::default(),
    )
    .unwrap();
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new("482915")),
        assistant: Some(moli),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let ui = "x-moli-origin: ui\r\n";
    let correct = json!({ "choice": { "tool": "now", "arguments": {} }, "remember": true });
    let approve = json!({ "approve": true });
    for (method, path, body) in [
        ("POST", "/api/assistant/proposals/7", Some(&approve)),
        ("POST", "/api/assistant/turns/7/undo", None),
        ("POST", "/api/assistant/turns/7/correct", Some(&correct)),
        (
            "PUT",
            "/api/assistant/rules/3",
            Some(&json!({ "enabled": false })),
        ),
        ("DELETE", "/api/assistant/rules/3", None),
    ] {
        let (status, text) = request_with(&addr, method, path, body, ui).await;
        assert_eq!(status, 403, "{method} {path}: {text}");
    }
    // The dashboard of a person: the code's session, and the header that says it speaks for the screen.
    let pin = format!("{}{ui}", pin_session(&addr, "482915").await);
    // Another site, even with the person's session, is refused before anything runs.
    let foreign = format!("{pin}sec-fetch-site: cross-site\r\n");
    for (method, path, body) in [
        ("POST", "/api/assistant/proposals/7", Some(&approve)),
        ("POST", "/api/assistant/turns/7/correct", Some(&correct)),
    ] {
        let (status, text) = request_with(&addr, method, path, body, &foreign).await;
        assert_eq!(status, 403, "{method} {path}: {text}");
        assert!(text.contains("cross-site"), "{text}");
    }
    for (method, path, body) in [
        ("POST", "/api/assistant/proposals/7", Some(&approve)),
        ("POST", "/api/assistant/turns/7/undo", None),
        ("POST", "/api/assistant/turns/7/correct", Some(&correct)),
        (
            "PUT",
            "/api/assistant/rules/3",
            Some(&json!({ "enabled": false })),
        ),
        ("DELETE", "/api/assistant/rules/3", None),
    ] {
        let (status, text) = request_with(&addr, method, path, body, &pin).await;
        assert_eq!(status, 410, "{method} {path}: {text}");
    }
    let (status, text) =
        request_with(&addr, "GET", "/api/assistant/turns/7/choices", None, &pin).await;
    assert_eq!(status, 410, "{text}");
    let (status, text) = request_with(&addr, "GET", "/api/assistant/rules", None, &pin).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(text.trim(), "[]");
}

#[tokio::test]
async fn correcting_without_an_assistant_says_so() {
    let (addr, _hub) = server().await;
    for (method, path) in [
        ("POST", "/api/assistant/turns/1/undo"),
        ("GET", "/api/assistant/rules"),
    ] {
        let (status, body) = request(&addr, method, path, None).await;
        assert_eq!(status, 503, "{method} {path}: {body}");
    }
}

#[tokio::test]
async fn only_a_person_gives_the_assistant_a_key() {
    let (addr, _hub) = server().await;
    let (status, body) = request(
        &addr,
        "PUT",
        "/api/assistant/key",
        Some(&json!({ "api_key": "sk-proj-abcdefghijklmnopqrstuvwxyz" })),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, _) = request(&addr, "GET", "/api/assistant/settings", None).await;
    assert_eq!(status, 503);
}

/// The response's `Set-Cookie` value for `name`, and the body.
async fn request_cookie(
    addr: &str,
    method: &str,
    path: &str,
    body: &Json,
    extra: &str,
    name: &str,
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
        (k.eq_ignore_ascii_case("set-cookie") && v.trim().starts_with(&format!("{name}=")))
            .then(|| v.trim().split(';').next().unwrap_or_default().to_owned())
    });
    (status, cookie, body.to_owned())
}

/// A human session opened with the house's code from this machine: the
/// header line that carries it.
async fn pin_session(addr: &str, pin: &str) -> String {
    let (status, cookie, body) = request_cookie(
        addr,
        "POST",
        "/api/session",
        &json!({ "pin": pin }),
        "",
        "moli_session",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    format!("Cookie: {}\r\n", cookie.unwrap())
}

/// What the satellites heard: switched on and listened to by a person only,
/// never by any device of the house.
#[tokio::test]
async fn only_a_person_records_and_hears_the_satellites() {
    let dir = std::env::temp_dir().join(format!("moli-http-recordings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let hub = lamp_hub().await;
    let moli = moli_assistant::Assistant::new(
        hub.clone(),
        None,
        None,
        None,
        moli_assistant::Config::default(),
    )
    .unwrap();
    moli.open_recordings(dir.join("recordings"));
    let name = format!("{}-7.wav", moli_core::now_ms());
    std::fs::write(dir.join("recordings").join(&name), b"RIFF....WAVE").unwrap();
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new("482915")),
        assistant: Some(moli.clone()),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let heard = format!("/api/assistant/recordings/{name}");
    let on = json!({ "days": 7 });
    let ui = "x-moli-origin: ui\r\n";

    // The household's dashboard without the code: nothing heard, nothing on.
    let (status, body) = request_with(&addr, "GET", &heard, None, ui).await;
    assert_eq!(status, 403, "{body}");
    let (status, body) =
        request_with(&addr, "PUT", "/api/assistant/recording", Some(&on), ui).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(moli.recording_until(), None);

    // A person (the code): both.
    let pin = pin_session(&addr, "482915").await;
    let (status, body) = request_with(&addr, "GET", &heard, None, &pin).await;
    assert_eq!(status, 200);
    assert!(body.starts_with("RIFF"));
    let (status, body) =
        request_with(&addr, "PUT", "/api/assistant/recording", Some(&on), &pin).await;
    assert_eq!(status, 200, "{body}");
    assert!(moli.recording_until().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A phone, as the phones driver describes one: where it is, and a point
/// to notify it.
struct Phone;

impl Driver for Phone {
    fn kind(&self) -> &'static str {
        "phones"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("p1");
            let spec = |key: &str, kind: Kind, write: bool| PointSpec {
                key: key.into(),
                label: key.into(),
                kind,
                access: Access {
                    read: !write,
                    write,
                },
                unit: None,
                semantic: Semantic::Other,
            };
            let number = Kind::Numeric {
                min: None,
                max: None,
                step: None,
            };
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "phone".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: vec![
                    spec("latitude", number, false),
                    spec("push", Kind::Text, true),
                ],
            });
            ctx.set_state(&id, "latitude", Value::Float(48.85));
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

/// A house with a code and a store: a lamp in the guest room
/// (`fake:lamp`), one in the living room (`salon:lamp`), a phone labelled in
/// the guest room too (`tel:p1`, whose whereabouts are nobody else's), and a
/// guest of the guest room signed in from the Internet.
struct GuestHouse {
    addr: String,
    hub: Hub,
    dir: std::path::PathBuf,
    /// The owner's session (the house's code, from this machine).
    pin: String,
    /// The guest's headers: from the Internet, its session.
    guest: String,
}

const GUEST_ROOM: &str = "Chambre d'amis";

async fn guest_house(name: &str, more: impl FnOnce(&mut moli_api::Options)) -> GuestHouse {
    use moli_runtime::LabelPatch;
    let dir = std::env::temp_dir().join(format!("moli-http-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hub = Hub::new(HubOptions {
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::generate().unwrap(),
        )),
        ..HubOptions::default()
    })
    .unwrap();
    let drivers: [(&str, Arc<dyn Driver>); 3] = [
        ("fake", Arc::new(Lamp)),
        ("salon", Arc::new(Lamp)),
        ("tel", Arc::new(Phone)),
    ];
    for (instance, driver) in drivers {
        spawn_driver(
            &hub,
            InstanceId::from(instance),
            driver,
            CancellationToken::new(),
        );
    }
    for _ in 0..200 {
        if hub.stats().drivers_running == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    for (device, room) in [
        ("fake:lamp", GUEST_ROOM),
        ("salon:lamp", "Salon"),
        ("tel:p1", GUEST_ROOM),
    ] {
        let room = LabelPatch {
            name: None,
            room: Some(room.into()),
        };
        hub.set_label(&device.into(), room, moli_core::Origin::Ui, None)
            .await
            .unwrap();
    }
    let mut options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new("482915")),
        home_path: Some(dir.join("home.json")),
        ..moli_api::Options::default()
    };
    more(&mut options);
    let addr = serve(&hub, &options).await;
    // The house's code proves the owner while nobody is one.
    let pin = pin_session(&addr, "482915").await;
    let invited = json!({ "name": "Invité", "role": "guest", "rooms": [GUEST_ROOM] });
    let (status, body) = request_with(&addr, "POST", "/api/people", Some(&invited), &pin).await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = request_with(&addr, "POST", "/api/people/invite/invite", None, &pin).await;
    assert_eq!(status, 200, "{body}");
    let code = serde_json::from_str::<Json>(&body).unwrap()["code"].clone();
    let outside = "X-Forwarded-For: 203.0.113.9\r\n";
    let (status, cookie, body) = request_cookie(
        &addr,
        "POST",
        "/api/invitation",
        &json!({ "code": code, "password": "une valise bleue" }),
        outside,
        "moli_person",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let guest = format!(
        "{outside}Cookie: {}\r\nx-moli-origin: ui\r\n",
        cookie.unwrap()
    );
    GuestHouse {
        addr,
        hub,
        dir,
        pin,
        guest,
    }
}

/// The ids of a snapshot's devices, sorted.
fn device_ids(snapshot: &Json) -> Vec<String> {
    let mut ids: Vec<String> = snapshot["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

/// A guest reads what is in its rooms, commands it, and nothing else of the
/// house: not the other rooms, not anyone's whereabouts, not the house's
/// settings, its energy, its journal or its assistant.
#[tokio::test]
async fn a_guest_reads_and_commands_its_rooms_only() {
    let store_dir = std::env::temp_dir().join(format!("moli-http-guest-h-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store_dir);
    let store = Arc::new(moli_history::Store::open(&store_dir.join("h.db")).unwrap());
    let now = moli_core::now_ms();
    store
        .insert(
            &[
                ("fake:lamp/state".into(), Value::Bool(true), now - 60_000),
                ("salon:lamp/state".into(), Value::Bool(true), now - 60_000),
                ("tel:p1/latitude".into(), Value::Float(48.85), now - 60_000),
            ],
            false,
        )
        .unwrap();
    let house = guest_house("guest-reads", |options| {
        options.history = Some(moli_history::History::from_store(store));
    })
    .await;
    let (addr, guest) = (&house.addr, &house.guest);
    let zone = json!([{ "id": "travail", "name": "Travail", "latitude": 48.0, "longitude": 2.0, "radius": 200.0 }]);
    let (status, body) = request_with(addr, "PUT", "/api/zones", Some(&zone), &house.pin).await;
    assert_eq!(status, 200, "{body}");
    let member = json!({ "name": "Sam", "role": "member" });
    let (status, body) = request_with(addr, "POST", "/api/people", Some(&member), &house.pin).await;
    assert_eq!(status, 201, "{body}");

    // The household sees the house; the guest, its room's lamp only.
    let (_, body) = request(addr, "GET", "/api/devices", None).await;
    assert_eq!(
        device_ids(&serde_json::from_str(&body).unwrap()),
        ["fake:lamp", "salon:lamp", "tel:p1"]
    );
    let (status, body) = request_with(addr, "GET", "/api/devices", None, guest).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        device_ids(&serde_json::from_str(&body).unwrap()),
        ["fake:lamp"]
    );
    for (path, expected) in [
        ("/api/devices/fake:lamp", 200),
        ("/api/devices/salon:lamp", 403),
        ("/api/devices/tel:p1", 403),
        ("/api/history?point=fake:lamp/state", 200),
        ("/api/history?point=salon:lamp/state", 403),
        ("/api/history?point=tel:p1/latitude", 403),
        ("/api/history?point=tel:p1", 403),
    ] {
        let (status, body) = request_with(addr, "GET", path, None, guest).await;
        assert_eq!(status, expected, "{path}: {body}");
    }

    // Itself among the people, and no place.
    let (status, body) = request_with(addr, "GET", "/api/people", None, guest).await;
    assert_eq!(status, 200, "{body}");
    let people: Json = serde_json::from_str(&body).unwrap();
    assert_eq!(people["people"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(people["people"][0]["id"], "invite");
    assert_eq!(people["zones"], json!([]));

    // Nothing of the house's settings, energy, journal or assistant.
    for path in [
        "/api/journal",
        "/api/assistant",
        "/api/assistant/exchanges",
        "/api/machines/machines:pc",
        "/api/energy",
        "/api/energy/series?step=day",
        "/api/bench",
        "/api/system",
        "/api/automations",
        "/api/mobile/phones",
        "/api/devices/fake:lamp/snapshot",
    ] {
        let (status, body) = request_with(addr, "GET", path, None, guest).await;
        assert_eq!(status, 403, "{path}: {body}");
    }
    let (status, _) = request_with(addr, "PUT", "/api/zones", Some(&zone), guest).await;
    assert_eq!(status, 403);
    // Its dashboard still opens: the session, the layout, the plan, the sky.
    for path in ["/api/session", "/api/health", "/api/home", "/api/plan"] {
        let (status, body) = request_with(addr, "GET", path, None, guest).await;
        assert_eq!(status, 200, "{path}: {body}");
    }
    // (Not built here, or no position: not refused all the same.)
    for path in ["/", "/api/weather/forecast"] {
        let (status, body) = request_with(addr, "GET", path, None, guest).await;
        assert_ne!(status, 403, "{path}: {body}");
    }

    // It commands its room, nothing else.
    for (point, value, expected) in [
        ("fake:lamp/state", json!(true), 200),
        ("salon:lamp/state", json!(true), 403),
        ("tel:p1/push", json!("Bonjour"), 403),
    ] {
        let order = json!({ "point": point, "value": value });
        let (status, body) = request_with(addr, "POST", "/api/command", Some(&order), guest).await;
        assert_eq!(status, expected, "{point}: {body}");
    }
    assert_eq!(
        house.hub.state(&"salon:lamp/state".into()).unwrap().value,
        Value::Bool(false)
    );
    let _ = std::fs::remove_dir_all(&house.dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}

/// A live stream kept open: the connection, and what was read of it.
struct Live {
    stream: tokio::net::TcpStream,
    read: Vec<u8>,
}

/// Opens a live stream; its status and the stream.
async fn live(addr: &str, path: &str, extra: &str) -> (u16, Live) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n{extra}\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut live = Live {
        stream,
        read: Vec::new(),
    };
    let end = loop {
        if let Some(end) = live.read.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        live.more().await;
    };
    let status = String::from_utf8_lossy(&live.read[9..12]).parse().unwrap();
    live.read.drain(..end + 4);
    (status, live)
}

impl Live {
    async fn more(&mut self) {
        let mut chunk = vec![0u8; 16 * 1024];
        let n = tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut chunk))
            .await
            .expect("nothing came within 5 s")
            .unwrap();
        assert!(n > 0, "the stream ended");
        self.read.extend_from_slice(&chunk[..n]);
    }

    /// Reads on until `text` came.
    async fn until(&mut self, text: &str) {
        while !String::from_utf8_lossy(&self.read).contains(text) {
            self.more().await;
        }
    }

    /// The next event: its name and its data.
    async fn next(&mut self) -> (String, Json) {
        loop {
            let text = match std::str::from_utf8(&self.read) {
                Ok(text) => text,
                Err(e) => std::str::from_utf8(&self.read[..e.valid_up_to()]).unwrap(),
            };
            if let Some(start) = text.find("event: ") {
                let rest = &text[start + 7..];
                if let Some((name, rest)) = rest.split_once('\n')
                    && let Some(data) = rest.strip_prefix("data: ")
                    && let Some(end) = data.find('\n')
                {
                    let event = (name.to_owned(), serde_json::from_str(&data[..end]).unwrap());
                    let used = text.len() - data.len() + end + 1;
                    self.read.drain(..used);
                    return event;
                }
            }
            self.more().await;
        }
    }
}

/// The live stream of a guest: its rooms' devices and changes, never the
/// house's journal; a device moved into its room comes with a new snapshot.
#[tokio::test]
async fn a_guest_follows_its_rooms_only_live() {
    use moli_core::Origin;
    let house = guest_house("guest-live", |_| {}).await;
    let (status, mut guest) = live(&house.addr, "/api/events", &house.guest).await;
    assert_eq!(status, 200);
    let (name, snapshot) = guest.next().await;
    assert_eq!(name, "snapshot");
    assert_eq!(device_ids(&snapshot), ["fake:lamp"]);
    let (_, mut household) = live(&house.addr, "/api/events", "").await;
    assert_eq!(household.next().await.0, "snapshot");

    // The living room, then the guest room, twice: each order journaled.
    for (point, on) in [
        ("salon:lamp/state", true),
        ("fake:lamp/state", true),
        ("fake:lamp/state", false),
    ] {
        house
            .hub
            .command(&point.into(), Value::Bool(on), Origin::Ui, None)
            .await
            .unwrap();
    }
    let (_, first) = household.next().await;
    assert_eq!(first["point"], "salon:lamp/state", "{first}");
    for on in [true, false] {
        let (name, event) = guest.next().await;
        assert_eq!(name, "event");
        assert_eq!(
            (&event["type"], &event["point"], &event["value"]),
            (&json!("state"), &json!("fake:lamp/state"), &json!(on)),
            "{event}"
        );
    }

    // The living room's lamp moved into the guest room: now the guest's.
    let room = moli_runtime::LabelPatch {
        name: None,
        room: Some(GUEST_ROOM.into()),
    };
    house
        .hub
        .set_label(&"salon:lamp".into(), room, Origin::Ui, None)
        .await
        .unwrap();
    let (name, snapshot) = guest.next().await;
    assert_eq!(name, "snapshot");
    assert_eq!(device_ids(&snapshot), ["fake:lamp", "salon:lamp"]);
    let _ = std::fs::remove_dir_all(&house.dir);
}

/// A phone is paired without the code (a device of the home network is the
/// household's): the house hears of each one, and from where.
#[tokio::test]
async fn pairing_a_phone_tells_the_house() {
    let phones_dir = std::env::temp_dir().join(format!("moli-http-pairing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&phones_dir);
    std::fs::create_dir_all(&phones_dir).unwrap();
    let config = moli_phones::Config {
        home_wifi: Vec::new(),
        home: None,
        radius: 150.0,
    };
    let (gateway, _driver) = moli_phones::open(config, &phones_dir).unwrap();
    let hub = lamp_hub().await;
    let options = moli_api::Options {
        phones: Some(gateway),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let mut events = hub.subscribe();

    // The name as the pairer typed it; the house reads it as it is kept.
    let pairing = json!({ "name": " Téléphone\n  du   salon ", "platform": "android" });
    let (status, body) = request(&addr, "POST", "/api/mobile/register", Some(&pairing)).await;
    assert_eq!(status, 200, "{body}");
    let paired: Json = serde_json::from_str(&body).unwrap();
    let id = paired["id"].as_str().unwrap();
    let heard = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match events.recv().await {
                Ok(moli_core::Event::Notice { notice })
                    if notice.message.contains("Téléphone du salon") =>
                {
                    return Some(notice.message);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                _ => {}
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("the house hears of the phone");
    assert!(heard.contains(id), "{heard}");
    assert!(heard.contains("127.0.0.1"), "who paired it: {heard}");
    let _ = std::fs::remove_dir_all(&phones_dir);
}

/// A phone hears every notice of the house: a guest pairs none, removing
/// someone forgets theirs, and the phone of someone no longer of the house
/// (a guest whose stay is over) reports nothing and is told nothing.
#[tokio::test]
async fn a_phone_follows_its_person() {
    use moli_runtime::CommandError;
    let phones_dir = std::env::temp_dir().join(format!("moli-http-phones-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&phones_dir);
    std::fs::create_dir_all(&phones_dir).unwrap();
    let config = moli_phones::Config {
        home_wifi: Vec::new(),
        home: None,
        radius: 150.0,
    };
    let (gateway, driver) = moli_phones::open(config, &phones_dir).unwrap();
    let gate = gateway.clone();
    let house = guest_house("phones", move |options| options.phones = Some(gate)).await;
    spawn_driver(
        &house.hub,
        InstanceId::from("telephones"),
        Arc::new(driver),
        CancellationToken::new(),
    );
    let addr = &house.addr;

    // A guest pairs no phone.
    let pairing = json!({ "name": "Mon téléphone", "platform": "ios" });
    let (status, body) = request_with(
        addr,
        "POST",
        "/api/mobile/register",
        Some(&pairing),
        &house.guest,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(gateway.list().is_empty());

    // Sam's phone, and the phone of a guest whose stay is over.
    for person in [
        json!({ "name": "Sam", "role": "member" }),
        json!({ "name": "Passage", "role": "guest", "expires": 1 }),
    ] {
        let (status, body) =
            request_with(addr, "POST", "/api/people", Some(&person), &house.pin).await;
        assert_eq!(status, 201, "{body}");
    }
    let sam = gateway
        .register("Téléphone de Sam", "ios", "", None, Some("sam"))
        .unwrap();
    let past = gateway
        .register("Téléphone de passage", "ios", "", None, Some("passage"))
        .unwrap();
    let report = |phone: &moli_phones::Paired| {
        (
            format!("/api/phones/{}/report", phone.id),
            format!("Authorization: Bearer {}\r\n", phone.token),
        )
    };
    let (path, bearer) = report(&sam);
    let (status, body) = request_with(addr, "POST", &path, Some(&json!({})), &bearer).await;
    assert_eq!(status, 200, "{body}");

    // The stay is over: its phone reports nothing, and is told nothing.
    let (path, bearer) = report(&past);
    let battery = json!({ "battery": 50 });
    let (status, body) = request_with(addr, "POST", &path, Some(&battery), &bearer).await;
    assert_eq!(status, 403, "{body}");
    let device = moli_core::DeviceId::from(format!("telephones:{}", past.id));
    for _ in 0..200 {
        if house.hub.device(&device).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let push = moli_core::PointId::from(format!("{device}/push"));
    let told = house
        .hub
        .command(
            &push,
            Value::Text("Bonsoir".into()),
            moli_core::Origin::Automation,
            None,
        )
        .await;
    assert_eq!(
        told,
        Err(CommandError::Driver(moli_i18n::tr(
            "pilotes.phones.hors_maison"
        )))
    );

    // Sam leaves the house: Sam's phone is forgotten, its token dead.
    let (status, body) = request_with(addr, "DELETE", "/api/people/sam", None, &house.pin).await;
    assert_eq!(status, 200, "{body}");
    assert!(
        gateway
            .list()
            .iter()
            .all(|p| p.person.as_deref() != Some("sam"))
    );
    let (path, bearer) = report(&sam);
    let (status, body) = request_with(addr, "POST", &path, Some(&json!({})), &bearer).await;
    assert_eq!(status, 401, "{body}");
    let _ = std::fs::remove_dir_all(&house.dir);
    let _ = std::fs::remove_dir_all(&phones_dir);
}

/// Live streams are counted: a caller holds a few at most, and a stream
/// that ends gives its seat back.
#[tokio::test]
async fn live_streams_are_counted() {
    let (addr, hub) = server().await;
    let mut open = Vec::new();
    for _ in 0..8 {
        let (status, live) = live(&addr, "/api/events", "").await;
        assert_eq!(status, 200);
        open.push(live);
    }
    let (status, mut refused) = live(&addr, "/api/events", "").await;
    assert_eq!(status, 503);
    refused
        .until(&moli_i18n::tr("serveur.limite.trop_de_flux"))
        .await;
    // One closed: noticed at the house's next word, its seat is back.
    drop(open.pop());
    for on in (0..100).map(|i| i % 2 == 0) {
        hub.command(
            &"fake:lamp/state".into(),
            Value::Bool(on),
            moli_core::Origin::Ui,
            None,
        )
        .await
        .unwrap();
        if live(&addr, "/api/events", "").await.0 == 200 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the seat of a closed stream never came back");
}

/// Without Cloudflare Access, an owner who has only an e-mail cannot sign in:
/// the house's code keeps managing the people (and can invite that owner).
#[tokio::test]
async fn an_owner_e_mail_without_access_leaves_the_people_to_the_code() {
    let dir = std::env::temp_dir().join(format!("moli-http-owner-mail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hub = Hub::new(HubOptions {
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::generate().unwrap(),
        )),
        ..HubOptions::default()
    })
    .unwrap();
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new("482915")),
        home_path: Some(dir.join("home.json")),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let pin = pin_session(&addr, "482915").await;
    let (status, body) = request_with(
        &addr,
        "POST",
        "/api/people",
        Some(&json!({ "name": "Alex", "role": "owner", "emails": ["alex@example.org"] })),
        &pin,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    // Alex cannot sign in yet: the code still manages the people.
    let (status, body) = request_with(
        &addr,
        "POST",
        "/api/people",
        Some(&json!({ "name": "Sam", "role": "member" })),
        &pin,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = request_with(&addr, "POST", "/api/people/alex/invite", None, &pin).await;
    assert_eq!(status, 200, "the code invites the owner: {body}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Élodie, from the Internet: nothing until she is invited, then the house.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn an_invited_person_reaches_the_house_from_the_internet() {
    let dir = std::env::temp_dir().join(format!("moli-http-people-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hub = Hub::new(HubOptions {
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::generate().unwrap(),
        )),
        ..HubOptions::default()
    })
    .unwrap();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(Lamp),
        CancellationToken::new(),
    );
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new("482915")),
        home_path: Some(dir.join("home.json")),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    // From the Internet (a relayed request is never the home network).
    let outside = "X-Forwarded-For: 203.0.113.9\r\n";

    let (status, _) = request_with(&addr, "GET", "/api/devices", None, outside).await;
    assert_eq!(status, 403, "a stranger reads nothing");
    let (status, page) = request_with(&addr, "GET", "/api/session", None, outside).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Json>(&page).unwrap()["login_required"],
        true
    );
    let (status, _) = request_with(
        &addr,
        "POST",
        "/api/session",
        Some(&json!({ "pin": "482915" })),
        outside,
    )
    .await;
    assert_eq!(status, 403, "the house's code only from the house");

    // At home, the owner (proved by the code: nobody yet) invites Élodie.
    let (status, pin, _) = request_cookie(
        &addr,
        "POST",
        "/api/session",
        &json!({ "pin": "482915" }),
        "",
        "moli_session",
    )
    .await;
    assert_eq!(status, 200);
    let pin = format!("Cookie: {}\r\n", pin.unwrap());
    let (status, body) = request_with(
        &addr,
        "POST",
        "/api/people",
        Some(&json!({ "name": "Élodie", "role": "member" })),
        &pin,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = request_with(&addr, "POST", "/api/people/elodie/invite", None, &pin).await;
    assert_eq!(status, 200, "{body}");
    let code = serde_json::from_str::<Json>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(dir.join("people.json").exists());
    assert!(
        !std::fs::read_to_string(dir.join("people.json"))
            .unwrap()
            .contains(&code)
    );

    // Élodie, from the Internet, with the code.
    let (status, cookie, body) = request_cookie(
        &addr,
        "POST",
        "/api/invitation",
        &json!({ "code": code, "password": "le jardin fleuri" }),
        outside,
        "moli_person",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let me = format!("{outside}Cookie: {}\r\n", cookie.unwrap());
    let (status, _) = request_with(&addr, "GET", "/api/devices", None, &me).await;
    assert_eq!(status, 200, "signed in: the house");
    let (_, session) = request_with(&addr, "GET", "/api/session", None, &me).await;
    let session: Json = serde_json::from_str(&session).unwrap();
    assert_eq!(session["person"]["id"], "elodie");
    assert_eq!(session["owner"], false);
    let (status, _) = request_with(
        &addr,
        "POST",
        "/api/people",
        Some(&json!({ "name": "X", "role": "owner" })),
        &me,
    )
    .await;
    assert_eq!(status, 403, "a member manages nobody");
    let (status, _) = request_with(
        &addr,
        "POST",
        "/api/command",
        Some(&json!({ "point": "fake:lamp/state", "value": true })),
        &format!("{me}x-moli-origin: ui\r\n"),
    )
    .await;
    assert_eq!(status, 200, "and commands it");
    let journal = hub.journal(10);
    assert!(
        journal
            .iter()
            .any(|e| e.actor.as_deref() == Some("tableau de bord (Élodie)")),
        "{journal:?}"
    );
    // Her password works the next time; a wrong one does not.
    let (status, _, _) = request_cookie(
        &addr,
        "POST",
        "/api/session",
        &json!({ "login": "elodie", "password": "le jardin fleuri" }),
        outside,
        "moli_person",
    )
    .await;
    assert_eq!(status, 200);
    let (status, _, _) = request_cookie(
        &addr,
        "POST",
        "/api/session",
        &json!({ "login": "elodie", "password": "le jardin fané" }),
        outside,
        "moli_person",
    )
    .await;
    assert_eq!(status, 401);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A lamp that dims: something for an ambiance to set.
struct Dimmer;

impl Driver for Dimmer {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("dimmer");
            let point = |key: &str, kind: Kind, semantic: Semantic| PointSpec {
                key: key.into(),
                label: key.into(),
                kind,
                access: Access {
                    read: true,
                    write: true,
                },
                unit: None,
                semantic,
            };
            let level = Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None,
            };
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "dimmer".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: vec![
                    point("on", Kind::Binary, Semantic::OnOff),
                    point("brightness", level, Semantic::Brightness),
                ],
            });
            ctx.set_state(&id, "on", Value::Bool(false));
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

/// Under the code everywhere (`dashboard = "pin"`), a lamp and a dimmer,
/// and Sam, a member who signed in from the Internet: the address, the hub
/// and her cookie.
async fn pin_house_with_sam(dir: &std::path::Path) -> (String, Hub, String) {
    let hub = Hub::new(HubOptions {
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::generate().unwrap(),
        )),
        guard: moli_runtime::guard::GuardPolicy {
            dashboard: moli_runtime::guard::Dashboard::Pin,
            ..moli_runtime::guard::GuardPolicy::default()
        },
        ..HubOptions::default()
    })
    .unwrap();
    for (instance, driver) in [
        ("fake", Arc::new(Lamp) as Arc<dyn Driver>),
        ("dim", Arc::new(Dimmer)),
    ] {
        spawn_driver(
            &hub,
            InstanceId::from(instance),
            driver,
            CancellationToken::new(),
        );
    }
    for _ in 0..200 {
        if hub.stats().drivers_running == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new("482915")),
        home_path: Some(dir.join("home.json")),
        ..moli_api::Options::default()
    };
    let addr = serve(&hub, &options).await;
    let pin = pin_session(&addr, "482915").await;
    let sam = json!({ "name": "Sam", "role": "member" });
    let (status, body) = request_with(&addr, "POST", "/api/people", Some(&sam), &pin).await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = request_with(&addr, "POST", "/api/people/sam/invite", None, &pin).await;
    assert_eq!(status, 200, "{body}");
    let code = serde_json::from_str::<Json>(&body).unwrap()["code"].clone();
    let (status, cookie, body) = request_cookie(
        &addr,
        "POST",
        "/api/invitation",
        &json!({ "code": code, "password": "une valise bleue" }),
        "X-Forwarded-For: 203.0.113.9\r\n",
        "moli_person",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    (addr, hub, cookie.unwrap())
}

/// This machine and Sam ask for the same thing: two requests, each replaced
/// by its own caller's newer one only. Sam is Sam wherever she asks from
/// (this machine, then the Internet), and this machine is not Sam when she
/// asks from it. Over REST, ambiances and MCP: the caller is who the house
/// recognized, never the `actor` declared.
#[tokio::test]
async fn a_held_order_is_replaced_by_its_own_callers_only() {
    let dir = std::env::temp_dir().join(format!("moli-http-held-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (addr, hub, cookie) = pin_house_with_sam(&dir).await;
    // This machine; Sam on this machine (its address, but she is Sam); Sam away.
    let here = format!("Cookie: {cookie}\r\n");
    let away = format!("X-Forwarded-For: 203.0.113.9\r\n{here}");
    let callers = [("", true), (here.as_str(), true), (away.as_str(), false)];
    let pending = |origin: moli_core::Origin, point: &str| -> Vec<u64> {
        hub.approvals()
            .iter()
            .filter(|r| r.origin == origin && r.point.as_str() == point)
            .map(|r| r.id)
            .collect()
    };
    let newest =
        |origin: moli_core::Origin, point: &str| -> u64 { *pending(origin, point).last().unwrap() };
    let (api, mcp) = (moli_core::Origin::Api, moli_core::Origin::Mcp);

    let order =
        |value: bool| json!({ "point": "fake:lamp/state", "value": value, "actor": "same-agent" });
    let mut ids = Vec::new();
    for (who, value) in callers {
        let (status, body) =
            request_with(&addr, "POST", "/api/command", Some(&order(value)), who).await;
        assert_eq!(status, 202, "{body}");
        ids.push(newest(api, "fake:lamp/state"));
    }
    assert_eq!(
        pending(api, "fake:lamp/state"),
        [ids[0], ids[2]],
        "this machine's request stays; Sam's newer one replaced hers"
    );

    let look = json!({ "lights": ["dim:dimmer"], "ambiance": "lecture", "actor": "same-agent" });
    let mut ids = Vec::new();
    for (who, _) in callers {
        let (status, body) = request_with(&addr, "POST", "/api/ambiance", Some(&look), who).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"held\":[{"), "{body}");
        ids.push(newest(api, "dim:dimmer/on"));
    }
    assert_eq!(pending(api, "dim:dimmer/on"), [ids[0], ids[2]]);

    let call = |value: bool| {
        rpc(
            1,
            "tools/call",
            &json!({"name": "send_command", "arguments": {"point": "fake:lamp/state", "value": value, "actor": "same-agent"}}),
        )
    };
    let mut ids = Vec::new();
    for (who, value) in callers {
        let (status, body) = request_with(&addr, "POST", "/mcp", Some(&call(value)), who).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("awaiting_human_approval"), "{body}");
        ids.push(newest(mcp, "fake:lamp/state"));
    }
    assert_eq!(pending(mcp, "fake:lamp/state"), [ids[0], ids[2]]);
    let _ = std::fs::remove_dir_all(&dir);
}

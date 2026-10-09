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

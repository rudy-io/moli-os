//! A demo house, reached from the Internet through the tunnel: visitors play
//! with it, nothing they do stays for the next ones.

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
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

/// As the tunnel relays a visitor (no Access assertion).
async fn visitor(addr: &str, method: &str, path: &str, body: &Json) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.to_string();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         cf-ray: 8c00000000000000-CDG\r\ncf-connecting-ip: 203.0.113.7\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let status = raw[9..12].parse().unwrap();
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn a_visitor_plays_with_the_demo_and_leaves_it_as_it_was() {
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
    let options = moli_api::Options {
        demo: true,
        ..moli_api::Options::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = moli_api::router(hub.clone(), CancellationToken::new(), &options)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Playing: the lamp obeys, from the Internet.
    let on = json!({ "point": "fake:lamp/state", "value": true });
    let (status, body) = visitor(&addr, "POST", "/api/command", &on).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        hub.state(&"fake:lamp/state".into()).unwrap().value,
        Value::Bool(true)
    );

    // Names stay, through REST and through MCP alike.
    let rename = json!({ "name": "Un autre nom" });
    let (status, _) = visitor(&addr, "PUT", "/api/labels/fake:lamp", &rename).await;
    assert_eq!(status, 403);
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "set_label",
        "arguments": { "device": "fake:lamp", "name": "Un autre nom" }
    }});
    let (status, body) = visitor(&addr, "POST", "/mcp", &call).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"isError\":true"), "{body}");
    assert_eq!(
        hub.device(&"fake:lamp".into())
            .and_then(|v| v.label.name.clone()),
        None
    );
    // Reading through MCP still works: agents can try the demo.
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "list_devices", "arguments": {}
    }});
    let (status, body) = visitor(&addr, "POST", "/mcp", &list).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("fake:lamp"), "{body}");

    // No routine written for the next visitors.
    let draft = json!({ "name": "x", "graph": { "nodes": [], "edges": [] } });
    let (status, _) = visitor(&addr, "POST", "/api/automations", &draft).await;
    assert_eq!(status, 403);
}

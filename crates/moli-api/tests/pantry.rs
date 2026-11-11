//! `/api/cellier`: the pantry's REST routes, and who may use them.

use moli_pantry::Pantry;
use moli_runtime::{Hub, HubOptions};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const PIN: &str = "482915";

async fn serve(hub: &Hub, options: &moli_api::Options) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = moli_api::router(hub.clone(), CancellationToken::new(), options);
    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Status, the cookie `name` it sets (if any), body.
async fn request(
    addr: &str,
    method: &str,
    path: &str,
    body: &Json,
    extra: &str,
    name: &str,
) -> (u16, Option<String>, Json) {
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
    let body = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_owned()
    };
    let body = serde_json::from_str(&body).unwrap_or(Json::Null);
    (status, cookie, body)
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        out.push_str(&rest[..size]);
        body = rest[size..].trim_start_matches("\r\n");
    }
    out
}

async fn call(addr: &str, method: &str, path: &str, body: &Json, extra: &str) -> (u16, Json) {
    let (status, _, body) = request(addr, method, path, body, extra, "").await;
    (status, body)
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("moli-api-pantry-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A house with a pantry (or without), served on a free port, and a handle
/// on that pantry.
async fn open_house(
    name: &str,
    pantry: bool,
    demo: bool,
) -> (String, std::path::PathBuf, Option<Pantry>) {
    open_house_with(name, pantry, demo, None).await
}

/// The same, with an assistant whose only provider is at `provider` (an
/// OpenAI-compatible API on the local network).
async fn open_house_with(
    name: &str,
    pantry: bool,
    demo: bool,
    provider: Option<String>,
) -> (String, std::path::PathBuf, Option<Pantry>) {
    let dir = temp(name);
    // The people's passwords are kept in the secrets file.
    let hub = Hub::new(HubOptions {
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::generate().unwrap(),
        )),
        ..HubOptions::default()
    })
    .unwrap();
    let pantry = pantry.then(|| Pantry::open(&hub, &dir).unwrap());
    let options = moli_api::Options {
        ui_pin: Some(moli_api::UiPin::new(PIN)),
        home_path: Some(dir.join("home.json")),
        pantry: pantry.clone(),
        assistant: provider.map(|base_url| {
            let config = moli_assistant::Config {
                base_url,
                ..moli_assistant::Config::default()
            };
            moli_assistant::Assistant::new(hub.clone(), None, None, None, config).unwrap()
        }),
        demo,
        ..moli_api::Options::default()
    };
    (serve(&hub, &options).await, dir, pantry)
}

/// A house with a pantry (or without), served on a free port.
async fn house(name: &str, pantry: bool, demo: bool) -> (String, std::path::PathBuf) {
    let (addr, dir, _) = open_house(name, pantry, demo).await;
    (addr, dir)
}

#[tokio::test]
async fn add_then_list() {
    let (addr, dir) = house("add", true, false).await;
    let (status, item) = call(
        &addr,
        "POST",
        "/api/cellier/items",
        &json!({ "text": "lait" }),
        "",
    )
    .await;
    assert_eq!(status, 201, "{item}");
    assert_eq!(item["qty"], 1.0);
    assert_eq!(item["channel"], "ui");

    let (status, view) = call(&addr, "GET", "/api/cellier", &Json::Null, "").await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["items"][0]["text"], "lait");
    assert!(!view["items"][0]["added_by"].as_str().unwrap().is_empty());
    assert_eq!(view["threshold"], 0.3);

    // Change, then drop it.
    let path = format!("/api/cellier/items/{}", item["id"]);
    let (status, item) = call(&addr, "PUT", &path, &json!({ "qty": 3 }), "").await;
    assert_eq!(status, 200, "{item}");
    assert_eq!(item["qty"], 3.0);
    let (status, _) = call(&addr, "DELETE", &path, &Json::Null, "").await;
    assert_eq!(status, 204);
    let (status, body) = call(&addr, "DELETE", &path, &Json::Null, "").await;
    assert_eq!(status, 404, "{body}");
    assert!(body["error"].is_string());
    let (_, view) = call(&addr, "GET", "/api/cellier", &Json::Null, "").await;
    assert_eq!(view["items"], json!([]));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn absent_brick_is_404() {
    let (addr, dir) = house("absent", false, false).await;
    let (status, body) = call(&addr, "GET", "/api/cellier", &Json::Null, "").await;
    assert_eq!(status, 404, "{body}");
    assert!(body["error"].is_string());
    let (status, _) = call(
        &addr,
        "POST",
        "/api/cellier/items",
        &json!({ "text": "lait" }),
        "",
    )
    .await;
    assert_eq!(status, 404);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn guest_is_refused() {
    let (addr, dir) = house("guest", true, false).await;
    // The house's code proves the owner, who invites a guest.
    let (status, cookie, body) = request(
        &addr,
        "POST",
        "/api/session",
        &json!({ "pin": PIN }),
        "",
        "moli_session",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let owner = format!("Cookie: {}\r\n", cookie.unwrap());
    let invited = json!({ "name": "Sam", "role": "guest", "rooms": ["Salon"] });
    let (status, body) = call(&addr, "POST", "/api/people", &invited, &owner).await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = call(&addr, "POST", "/api/people/sam/invite", &Json::Null, &owner).await;
    assert_eq!(status, 200, "{body}");
    let outside = "X-Forwarded-For: 203.0.113.9\r\n";
    let (status, cookie, body) = request(
        &addr,
        "POST",
        "/api/invitation",
        &json!({ "code": body["code"], "password": "une valise bleue" }),
        outside,
        "moli_person",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let guest = format!(
        "{outside}Cookie: {}\r\nx-moli-origin: ui\r\n",
        cookie.unwrap()
    );

    let (status, body) = call(&addr, "GET", "/api/cellier", &Json::Null, &guest).await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = call(
        &addr,
        "POST",
        "/api/cellier/items",
        &json!({ "text": "lait" }),
        &guest,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    // The household still reads it.
    let (status, _) = call(&addr, "GET", "/api/cellier", &Json::Null, &owner).await;
    assert_eq!(status, 200);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn demo_cannot_write() {
    let (addr, dir) = house("demo", true, true).await;
    let (status, body) = call(
        &addr,
        "POST",
        "/api/cellier/items",
        &json!({ "text": "lait" }),
        "",
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bad_threshold_is_400() {
    let (addr, dir) = house("threshold", true, false).await;
    let (status, body) = call(
        &addr,
        "PUT",
        "/api/cellier/settings",
        &json!({ "threshold": 2 }),
        "",
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].is_string());
    let (status, _) = call(
        &addr,
        "PUT",
        "/api/cellier/settings",
        &json!({ "threshold": 0.4 }),
        "",
    )
    .await;
    assert_eq!(status, 204);
    let (_, view) = call(&addr, "GET", "/api/cellier", &Json::Null, "").await;
    assert_eq!(view["threshold"], 0.4);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn products_baskets_and_errors() {
    let (addr, dir) = house("misc", true, false).await;
    let product = json!({ "id": "couches", "name": "Couches", "flexibility": "format" });
    let (status, body) = call(&addr, "PUT", "/api/cellier/products/couches", &product, "").await;
    assert_eq!(status, 204, "{body}");
    let (status, _) = call(&addr, "PUT", "/api/cellier/products/autre", &product, "").await;
    assert_eq!(status, 400, "the id must match the address");
    let (_, view) = call(&addr, "GET", "/api/cellier", &Json::Null, "").await;
    assert_eq!(view["products"][0]["flexibility"], "format");

    let (status, _) = call(
        &addr,
        "POST",
        "/api/cellier/offers/99/confirm",
        &Json::Null,
        "",
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = call(
        &addr,
        "POST",
        "/api/cellier/baskets/99/status",
        &json!({ "status": "dropped" }),
        "",
    )
    .await;
    assert_eq!(status, 404, "no such basket");
    let (status, body) = call(
        &addr,
        "POST",
        "/api/cellier/baskets/99/status",
        &json!({ "status": "bogus" }),
        "",
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let (status, _) = call(
        &addr,
        "POST",
        "/api/cellier/baskets/99/choose",
        &json!({ "option": "split", "accepted": [] }),
        "",
    )
    .await;
    assert_eq!(status, 404, "no such basket");
    let (status, _) = call(
        &addr,
        "PUT",
        "/api/cellier/settings",
        &json!({ "providers": [{ "id": "nowhere", "enabled": true }] }),
        "",
    )
    .await;
    assert_eq!(status, 400, "unknown chain");
    let _ = std::fs::remove_dir_all(dir);
}

/// One MCP tool call: whether it failed, and what it said (parsed when JSON).
async fn tool(addr: &str, name: &str, arguments: &Json) -> (bool, Json) {
    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": name, "arguments": arguments
    }});
    let accept = "Accept: application/json, text/event-stream\r\n";
    let (status, answer) = call(addr, "POST", "/mcp", &rpc, accept).await;
    assert_eq!(status, 200, "{answer}");
    let result = &answer["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let said = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
    (result["isError"] == true, said)
}

/// The two drives set, then a product with two sheets proposed by an agent:
/// the ids of its sheets (all still pending).
async fn shelf(addr: &str) -> Vec<i64> {
    // The household sets the two drives and their stores.
    let (status, body) = call(
        addr,
        "PUT",
        "/api/cellier/settings",
        &json!({ "providers": [
            { "id": "carrefour", "enabled": true, "store": "Ville-A" },
            { "id": "intermarche", "enabled": true, "store": "Ville-B" },
        ] }),
        "",
    )
    .await;
    assert_eq!(status, 204, "{body}");

    // 1. An agent proposes a product with two sheets.
    let (failed, saved) = tool(
        addr,
        "pantry_save_product",
        &json!({
            "product": { "id": "lait", "name": "Lait demi-écrémé" },
            "offers": [
                { "provider": "carrefour", "reference": "C1", "label": "Lait C" },
                { "provider": "intermarche", "reference": "I1", "label": "Lait I",
                  "status": "confirmed" },
            ],
        }),
    )
    .await;
    assert!(!failed, "{saved}");
    let ids: Vec<i64> = saved["offers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "{saved}");
    let (failed, view) = tool(addr, "pantry_overview", &json!({})).await;
    assert!(!failed, "{view}");
    assert!(
        view["offers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["status"] == "pending"),
        "an agent never confirms a sheet: {view}"
    );
    ids
}

#[tokio::test]
async fn mcp_pantry_round_trip() {
    let (addr, dir) = house("mcp", true, false).await;
    let ids = shelf(&addr).await;

    // 2. Prices, read in the stores set for the chains.
    let (failed, said) = tool(
        &addr,
        "pantry_record_prices",
        &json!({
            "actor": "agent-test",
            "readings": [
                { "offer": ids[0], "store": "Ville-A", "price_cents": 120, "available": true },
                { "offer": ids[1], "store": "Ville-B", "price_cents": 100, "available": true },
            ],
        }),
    )
    .await;
    assert!(!failed, "{said}");

    // 3. The need.
    let (failed, item) = tool(
        &addr,
        "pantry_add_item",
        &json!({
            "actor": "agent-test", "on_behalf_of": "Élodie",
            "text": "lait", "product": "lait", "qty": 2,
        }),
    )
    .await;
    assert!(!failed, "{item}");
    assert_eq!(item["added_by"], "agent-test (de la part de Élodie)");
    assert_eq!(item["channel"], "agent");

    // 4. The basket.
    let (failed, basket) = tool(
        &addr,
        "pantry_propose_basket",
        &json!({ "actor": "agent-test" }),
    )
    .await;
    assert!(!failed, "{basket}");
    assert_eq!(basket["status"], "proposed");
    assert_eq!(basket["created_by"], "agent-test");
    assert_eq!(basket["options"]["recommended"], "intermarche", "{basket}");

    // 5. The choice confirms the sheet it relies on.
    let (failed, chosen) = tool(
        &addr,
        "pantry_choose_option",
        &json!({ "actor": "agent-test", "basket": basket["id"], "option": "intermarche" }),
    )
    .await;
    assert!(!failed, "{chosen}");
    assert_eq!(chosen["status"], "chosen");
    let (_, view) = tool(&addr, "pantry_overview", &json!({})).await;
    let status_of = |id: i64| {
        view["offers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == id)
            .map(|o| o["status"].clone())
    };
    assert_eq!(status_of(ids[1]), Some(json!("confirmed")), "{view}");
    assert_eq!(status_of(ids[0]), Some(json!("pending")), "{view}");

    // The agent moves the basket on, never to « done »: the human pays.
    let (failed, said) = tool(
        &addr,
        "pantry_set_basket_status",
        &json!({ "actor": "agent-test", "basket": basket["id"], "status": "done" }),
    )
    .await;
    assert!(failed, "{said}");
    let (failed, said) = tool(
        &addr,
        "pantry_set_basket_status",
        &json!({ "actor": "agent-test", "basket": basket["id"], "status": "filled" }),
    )
    .await;
    assert!(!failed, "{said}");
    assert_eq!(said["status"], "filled");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_pantry_edge_cases() {
    let (addr, dir) = house("mcp-edges", true, false).await;
    // No reading: nothing to store, and the store is not even asked.
    let (failed, said) = tool(&addr, "pantry_record_prices", &json!({ "readings": [] })).await;
    assert!(!failed, "{said}");
    // Confirming purchases needs the basket they belong to.
    let line = json!({
        "product": "lait", "provider": "carrefour", "store": "Ville-A", "qty": 1,
        "price_cents": 100, "at": 1, "source": "drive", "status": "provisional",
    });
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "lines": [line], "confirm": true }),
    )
    .await;
    assert!(failed);
    assert_eq!(said, json!("basket is required to confirm purchases"));
    // An unknown id is an error the agent can read.
    let (failed, said) = tool(&addr, "pantry_remove_item", &json!({ "id": 99 })).await;
    assert!(failed, "{said}");
    // Without a name the agent is « agent ».
    let (_, item) = tool(&addr, "pantry_add_item", &json!({ "text": "pain" })).await;
    assert_eq!(item["added_by"], "agent");
    let _ = std::fs::remove_dir_all(dir);

    // No [pantry]: every tool says so.
    let (addr, dir) = house("mcp-absent", false, false).await;
    for (name, args) in [
        ("pantry_overview", json!({})),
        ("pantry_add_item", json!({ "text": "pain" })),
    ] {
        let (failed, said) = tool(&addr, name, &args).await;
        assert!(failed, "{name}");
        assert_eq!(
            said,
            json!("Le Cellier n'est pas activé sur cette instance")
        );
    }
    let _ = std::fs::remove_dir_all(dir);

    // The demo house is read-only for agents too.
    let (addr, dir) = house("mcp-demo", true, true).await;
    let (failed, _) = tool(&addr, "pantry_add_item", &json!({ "text": "pain" })).await;
    assert!(failed);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_resaving_a_changed_sheet_needs_a_new_check() {
    let (addr, dir) = house("mcp-resave", true, false).await;
    let ids = shelf(&addr).await;
    // A human confirms the sheet.
    let path = format!("/api/cellier/offers/{}/confirm", ids[0]);
    let (status, body) = call(&addr, "POST", &path, &Json::Null, "").await;
    assert_eq!(status, 204, "{body}");
    let resave = |label: &'static str| {
        json!({
            "product": { "id": "lait", "name": "Lait demi-écrémé" },
            "offers": [{ "provider": "carrefour", "reference": "C1", "label": label }],
        })
    };
    let status_of = |said: &Json| {
        let mut sheet = said["offers"].as_array().unwrap().iter();
        sheet.find(|o| o["id"] == ids[0]).unwrap()["status"].clone()
    };
    // Same data: the human's check stands.
    let (failed, said) = tool(&addr, "pantry_save_product", &resave("Lait C")).await;
    assert!(!failed, "{said}");
    assert_eq!(status_of(&said), "confirmed", "{said}");
    // Another label: it must be checked again.
    let (failed, said) = tool(&addr, "pantry_save_product", &resave("Lait entier C")).await;
    assert!(!failed, "{said}");
    assert_eq!(status_of(&said), "pending", "{said}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_purchases_provisional_then_confirmed_once() {
    let (addr, dir) = house("mcp-purchases", true, false).await;
    let ids = shelf(&addr).await;
    let (failed, said) = tool(
        &addr,
        "pantry_record_prices",
        &json!({ "readings": [
            { "offer": ids[0], "store": "Ville-A", "price_cents": 120, "available": true },
        ] }),
    )
    .await;
    assert!(!failed, "{said}");
    let (failed, item) = tool(
        &addr,
        "pantry_add_item",
        &json!({ "text": "lait", "product": "lait", "qty": 2 }),
    )
    .await;
    assert!(!failed, "{item}");
    let (_, basket) = tool(&addr, "pantry_propose_basket", &json!({})).await;
    let id = basket["id"].clone();
    let (failed, said) = tool(
        &addr,
        "pantry_choose_option",
        &json!({ "basket": id, "option": "carrefour" }),
    )
    .await;
    assert!(!failed, "{said}");
    for status in ["filled", "ordered"] {
        let (failed, said) = tool(
            &addr,
            "pantry_set_basket_status",
            &json!({ "basket": id, "status": status }),
        )
        .await;
        assert!(!failed, "{said}");
    }

    // The order confirmation: provisional lines.
    let line = |price: i64, status: &str| {
        json!({
            "product": "lait", "provider": "carrefour", "store": "Ville-A", "qty": 2,
            "price_cents": price, "at": 1, "source": "drive", "status": status,
        })
    };
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "basket": id, "lines": [line(120, "provisional")] }),
    )
    .await;
    assert!(!failed, "{said}");
    assert_eq!(said["recorded"], 1);
    let (_, view) = tool(&addr, "pantry_overview", &json!({})).await;
    assert_eq!(view["basket"]["status"], "ordered", "{view}");
    assert_eq!(view["items"][0]["basket"], id, "{view}");

    // The pick-up, sent twice (a lost answer): the corrected line, once. A
    // receipt it claims is not kept: only a receipt recorded with its lines
    // holds lines.
    let mut corrected = line(115, "confirmed");
    corrected["receipt"] = json!(7);
    for _ in 0..2 {
        let (failed, said) = tool(
            &addr,
            "pantry_record_purchases",
            &json!({ "basket": id, "lines": [corrected], "confirm": true }),
        )
        .await;
        assert!(!failed, "{said}");
        assert_eq!(said["confirmed"], 1);
    }
    let (_, view) = tool(&addr, "pantry_overview", &json!({})).await;
    assert_eq!(view["basket"], Json::Null, "the basket is done: {view}");
    assert_eq!(view["items"], json!([]), "{view}");
    let store = moli_pantry::Store::open(&dir.join(moli_pantry::FILE)).unwrap();
    let bought: Vec<(String, i64, String, Option<i64>)> = store
        .purchases(0, u64::MAX / 4)
        .unwrap()
        .into_iter()
        .map(|p| (p.source, p.price_cents, p.status, p.receipt))
        .collect();
    assert_eq!(
        bought,
        vec![("drive".into(), 115, "confirmed".into(), None)]
    );
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_reads_purchases_and_attaches_a_receipt() {
    let (addr, dir) = house("mcp-receipt", true, false).await;
    let now = moli_core::now_ms();
    let line = |label: &str, price: i64| {
        json!({
            "product": "lait", "provider": "enseigne-x", "store": "Ville-A", "qty": 2,
            "price_cents": price, "discount_cents": 20, "at": now - HOUR, "source": "ticket",
            "status": "confirmed", "label": label,
        })
    };
    let header = json!({
        "provider": "enseigne-x", "store": "Ville-A", "at": now - HOUR, "total_cents": 220
    });
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({
            "lines": [line("LAIT", 120)], "receipt": header,
            "actor": "agent-x", "on_behalf_of": "Sam",
        }),
    )
    .await;
    assert!(!failed, "{said}");
    assert_eq!(said["recorded"], 1);
    let receipt = said["receipt"].as_i64().unwrap();

    // The period default is the last 30 days; the receipt comes with the lines.
    let (failed, bought) = tool(&addr, "pantry_purchases", &json!({})).await;
    assert!(!failed, "{bought}");
    let lines = bought["purchases"].as_array().unwrap();
    assert_eq!(lines.len(), 1, "{bought}");
    assert_eq!(lines[0]["receipt"], receipt);
    assert_eq!(lines[0]["line_cents"], 220, "2 x 120 - 20");
    let receipts = bought["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1, "{bought}");
    assert_eq!(receipts[0]["id"], receipt);
    assert_eq!(receipts[0]["status"], "validated");
    assert_eq!(receipts[0]["photos"], 0);
    assert_eq!(receipts[0]["reader"], "agent");
    assert_eq!(receipts[0]["sent_by"], "agent-x (de la part de Sam)");
    assert_eq!(receipts[0]["read"], Json::Null);

    // Outside the period: nothing.
    let (failed, none) = tool(&addr, "pantry_purchases", &json!({ "from": 0, "to": 1000 })).await;
    assert!(!failed, "{none}");
    assert_eq!(none["purchases"], json!([]));
    assert_eq!(none["receipts"], json!([]));

    // The same receipt twice, a non-ticket line, confirm with a receipt: refused.
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "lines": [line("LAIT", 120)], "receipt": header }),
    )
    .await;
    assert!(failed, "{said}");
    assert!(
        said.as_str().unwrap().contains("already recorded"),
        "{said}"
    );
    let mut drive = line("LAIT", 120);
    drive["source"] = json!("drive");
    let other = json!({ "provider": "enseigne-x", "store": "Ville-A", "at": now });
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "lines": [drive], "receipt": other }),
    )
    .await;
    assert!(failed, "{said}");
    assert!(said.as_str().unwrap().contains("source ticket"), "{said}");
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "basket": 1, "lines": [line("LAIT", 120)], "receipt": other, "confirm": true }),
    )
    .await;
    assert!(failed, "{said}");
    let (_, bought) = tool(&addr, "pantry_purchases", &json!({})).await;
    assert_eq!(bought["purchases"].as_array().unwrap().len(), 1, "{bought}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_receipts_are_refused_in_the_demo_house() {
    let (addr, dir) = house("mcp-demo-receipt", true, true).await;
    let line = json!({
        "product": "lait", "provider": "enseigne-x", "store": "Ville-A", "qty": 1,
        "price_cents": 100, "at": 1, "source": "ticket", "status": "confirmed",
    });
    let receipt = json!({ "provider": "enseigne-x", "store": "Ville-A", "at": 1 });
    let (failed, _) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "lines": [line], "receipt": receipt }),
    )
    .await;
    assert!(failed);
    // Reading stays open.
    let (failed, bought) = tool(&addr, "pantry_purchases", &json!({})).await;
    assert!(!failed, "{bought}");
    assert_eq!(bought["purchases"], json!([]));
    let _ = std::fs::remove_dir_all(dir);
}

// ---- receipts and purchases ---------------------------------------------------

const HOUR: u64 = 3_600_000;

/// The headers (lower-cased) and the body of a GET, for what is not JSON.
async fn fetch(addr: &str, path: &str) -> (u16, String, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let headers = String::from_utf8_lossy(&raw[..split]).to_lowercase();
    let status = headers[9..12].parse().unwrap();
    (status, headers, raw[split + 4..].to_vec())
}

/// Bytes that start like a JPEG, as the dashboard sends them.
fn jpeg() -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F']
}

fn encoded(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

fn line(label: &str, cents: i64, product: &str) -> moli_pantry::DraftLine {
    moli_pantry::DraftLine {
        label: label.into(),
        name: label.to_lowercase(),
        qty: 1.0,
        price_cents: cents,
        line_cents: cents,
        keep: true,
        product: Some(product.into()),
        ..moli_pantry::DraftLine::default()
    }
}

fn draft(total: Option<i64>, lines: Vec<moli_pantry::DraftLine>) -> moli_pantry::ReceiptDraft {
    moli_pantry::ReceiptDraft {
        provider: "enseigne-x".into(),
        store: "Ville-A".into(),
        at: Some(moli_core::now_ms() - 5 * HOUR),
        total_cents: total,
        lines,
        ..moli_pantry::ReceiptDraft::default()
    }
}

/// A pantry with two products and a draft of one receipt (two photos).
async fn with_draft(name: &str) -> (String, std::path::PathBuf, Pantry, i64) {
    let (addr, dir, pantry) = open_house(name, true, false).await;
    let pantry = pantry.unwrap();
    for (id, label) in [("lait", "Lait"), ("pain", "Pain")] {
        let product = moli_pantry::Product {
            id: id.into(),
            name: label.into(),
            ..moli_pantry::Product::default()
        };
        pantry.save_product(&product).await.unwrap();
    }
    let receipt = pantry
        .add_receipt(
            vec![jpeg(), jpeg()],
            draft(
                Some(320),
                vec![line("LAIT", 120, "lait"), line("PAIN", 200, "pain")],
            ),
            json!({ "lines": [] }),
            "model-x".into(),
            "Sam".into(),
        )
        .await
        .unwrap();
    (addr, dir, pantry, receipt.id)
}

/// The cookie header of the household's session (the PIN proves the owner).
async fn owner(addr: &str) -> String {
    let (status, cookie, body) = request(
        addr,
        "POST",
        "/api/session",
        &json!({ "pin": PIN }),
        "",
        "moli_session",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    format!("Cookie: {}\r\n", cookie.unwrap())
}

#[tokio::test]
async fn guest_cannot_send_or_read_receipts() {
    let (addr, dir, _) = open_house("receipt-guest", true, false).await;
    let (status, cookie, body) = request(
        &addr,
        "POST",
        "/api/session",
        &json!({ "pin": PIN }),
        "",
        "moli_session",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let owner = format!("Cookie: {}\r\n", cookie.unwrap());
    let invited = json!({ "name": "Sam", "role": "guest", "rooms": ["Salon"] });
    call(&addr, "POST", "/api/people", &invited, &owner).await;
    let (_, body) = call(&addr, "POST", "/api/people/sam/invite", &Json::Null, &owner).await;
    let outside = "X-Forwarded-For: 203.0.113.9\r\n";
    let (status, cookie, body) = request(
        &addr,
        "POST",
        "/api/invitation",
        &json!({ "code": body["code"], "password": "une valise bleue" }),
        outside,
        "moli_person",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let guest = format!(
        "{outside}Cookie: {}\r\nx-moli-origin: ui\r\n",
        cookie.unwrap()
    );
    let photos = json!({ "photos": [encoded(&jpeg())] });
    let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &photos, &guest).await;
    assert_eq!(status, 403, "{body}");
    for path in ["/api/cellier/receipts", "/api/cellier/purchases"] {
        let (status, body) = call(&addr, "GET", path, &Json::Null, &guest).await;
        assert_eq!(status, 403, "{path}: {body}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn demo_cannot_send_receipts() {
    let (addr, dir) = house("receipt-demo", true, true).await;
    let cookie = owner(&addr).await;
    let photos = json!({ "photos": [encoded(&jpeg())] });
    let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 403, "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sending_photos_without_an_assistant_creates_nothing() {
    let (addr, dir, pantry) = open_house("receipt-503", true, false).await;
    let cookie = owner(&addr).await;
    // A script without a person's session is refused before the body is read.
    let photos = json!({ "photos": [encoded(&jpeg())] });
    let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &photos, "").await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 503, "{body}");
    assert!(body["error"].is_string());
    assert!(pantry.unwrap().receipts(None).await.unwrap().is_empty());
    let kept = dir.join("receipts");
    assert!(!kept.exists() || kept.read_dir().unwrap().next().is_none());

    // Bad photos are refused before anything else.
    let five = json!({ "photos": vec![encoded(&jpeg()); 5] });
    for bad in [
        json!({ "photos": [] }),
        five,
        json!({ "photos": ["not base64 !"] }),
        json!({ "photos": [encoded(b"\x89PNG\r\n\x1a\n")] }),
    ] {
        let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &bad, &cookie).await;
        assert_eq!(status, 400, "{bad}: {body}");
    }
    let (status, body) = call(&addr, "GET", "/api/cellier/reader", &Json::Null, "").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({ "allowed": false, "reason": "no assistant" }));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_draft_is_corrected_validated_and_shown_with_the_purchases() {
    let (addr, dir, _pantry, id) = with_draft("receipt-flow").await;
    let path = format!("/api/cellier/receipts/{id}");

    let (status, body) = call(
        &addr,
        "GET",
        "/api/cellier/receipts?status=draft",
        &Json::Null,
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body[0]["id"], id);
    let (status, body) = call(
        &addr,
        "GET",
        "/api/cellier/receipts?status=validated",
        &Json::Null,
        "",
    )
    .await;
    assert_eq!((status, &body), (200, &json!([])));
    let (status, _) = call(
        &addr,
        "GET",
        "/api/cellier/receipts?status=bogus",
        &Json::Null,
        "",
    )
    .await;
    assert_eq!(status, 400);

    // A person corrects the reading.
    let (status, shown) = call(&addr, "GET", &path, &Json::Null, "").await;
    assert_eq!(status, 200, "{shown}");
    let mut corrected = shown["draft"].clone();
    corrected["lines"][1]["price_cents"] = json!(210);
    corrected["lines"][1]["line_cents"] = json!(210);
    corrected["store"] = json!("Ville-B");
    let (status, saved) = call(&addr, "PUT", &path, &corrected, "").await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["draft"]["store"], "Ville-B");
    assert_eq!(saved["gap_cents"], -10, "the total printed is 320: {saved}");

    // Drafts show next to the purchases before they are validated.
    let (_, before) = call(&addr, "GET", "/api/cellier/purchases", &Json::Null, "").await;
    assert_eq!(before["purchases"], json!([]), "{before}");
    assert_eq!(before["receipts"][0]["id"], id);

    let validate = format!("{path}/validate");
    let cookie = owner(&addr).await;
    let (status, done) = call(&addr, "POST", &validate, &json!({}), &cookie).await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["status"], "validated");
    assert!(done["validated_by"].is_string());

    let (status, shown) = call(&addr, "GET", "/api/cellier/purchases", &Json::Null, "").await;
    assert_eq!(status, 200, "{shown}");
    let bought = shown["purchases"].as_array().unwrap();
    assert_eq!(bought.len(), 2, "{shown}");
    assert!(
        bought
            .iter()
            .all(|p| p["receipt"] == id && p["source"] == "ticket")
    );
    assert!(bought.iter().any(|p| p["price_cents"] == 210));
    let receipts = shown["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1, "{shown}");
    assert_eq!(receipts[0]["id"], id);
    assert_eq!(receipts[0]["status"], "validated");
    assert_eq!(
        receipts[0]["read"],
        Json::Null,
        "no raw reading in the list"
    );

    // The period is honoured: nothing before the receipt.
    let (_, empty) = call(
        &addr,
        "GET",
        "/api/cellier/purchases?from=0&to=1000",
        &Json::Null,
        "",
    )
    .await;
    assert_eq!(empty["purchases"], json!([]), "{empty}");
    assert_eq!(empty["receipts"], json!([]), "{empty}");

    // A validated receipt is frozen.
    let (status, body) = call(&addr, "PUT", &path, &corrected, "").await;
    assert_eq!(status, 400, "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_twin_receipt_needs_force() {
    let (addr, dir, pantry, first) = with_draft("receipt-twin").await;
    let cookie = owner(&addr).await;
    let (status, _) = call(
        &addr,
        "POST",
        &format!("/api/cellier/receipts/{first}/validate"),
        &json!({}),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
    let twin = pantry
        .add_receipt(
            vec![jpeg()],
            draft(
                Some(320),
                vec![line("LAIT", 120, "lait"), line("PAIN", 200, "pain")],
            ),
            json!({}),
            "model-x".into(),
            "Sam".into(),
        )
        .await
        .unwrap();
    assert_eq!(twin.duplicate_of, Some(first));
    let path = format!("/api/cellier/receipts/{}/validate", twin.id);
    let (status, body) = call(&addr, "POST", &path, &json!({}), &cookie).await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = call(&addr, "POST", &path, &json!({ "force": false }), &cookie).await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = call(&addr, "POST", &path, &json!({ "force": true }), &cookie).await;
    assert_eq!(status, 200, "{body}");
    let (_, shown) = call(&addr, "GET", "/api/cellier/purchases", &Json::Null, "").await;
    assert_eq!(shown["purchases"].as_array().unwrap().len(), 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn dropping_a_draft_erases_its_photos() {
    let (addr, dir, _pantry, id) = with_draft("receipt-drop").await;
    let photo = |n: usize| format!("/api/cellier/receipts/{id}/photos/{n}");
    let (status, headers, bytes) = fetch(&addr, &photo(1)).await;
    assert_eq!(status, 200, "{headers}");
    assert!(headers.contains("content-type: image/jpeg"), "{headers}");
    assert!(
        headers.contains("cache-control: private, max-age=86400"),
        "{headers}"
    );
    assert_eq!(bytes, jpeg());
    assert_eq!(fetch(&addr, &photo(2)).await.0, 200);
    for n in [0, 3, 9] {
        assert_eq!(fetch(&addr, &photo(n)).await.0, 404, "photo {n}");
    }

    let path = format!("/api/cellier/receipts/{id}");
    let (status, body) = call(&addr, "DELETE", &path, &Json::Null, "").await;
    assert_eq!(status, 204, "{body}");
    for n in [1, 2] {
        assert_eq!(fetch(&addr, &photo(n)).await.0, 404, "photo {n} is gone");
    }
    let (status, _) = call(&addr, "GET", &path, &Json::Null, "").await;
    assert_eq!(status, 404);
    let (status, _) = call(&addr, "DELETE", &path, &Json::Null, "").await;
    assert_eq!(status, 404);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_purchase_is_corrected_then_dropped() {
    let (addr, dir, _pantry, id) = with_draft("purchase-edit").await;
    let validate = format!("/api/cellier/receipts/{id}/validate");
    let cookie = owner(&addr).await;
    let (status, body) = call(&addr, "POST", &validate, &json!({}), &cookie).await;
    assert_eq!(status, 200, "{body}");
    let (_, shown) = call(&addr, "GET", "/api/cellier/purchases", &Json::Null, "").await;
    let first = shown["purchases"][0]["id"].as_i64().unwrap();
    let path = format!("/api/cellier/purchases/{first}");

    let fix = json!({ "qty": 2, "price_cents": 110, "product": "pain" });
    let (status, fixed) = call(&addr, "PUT", &path, &fix, "").await;
    assert_eq!(status, 200, "{fixed}");
    assert_eq!(fixed["qty"], 2.0);
    assert_eq!(fixed["line_cents"], 220, "recomputed: {fixed}");
    let (_, shown) = call(&addr, "GET", "/api/cellier/purchases", &Json::Null, "").await;
    let again = shown["purchases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == first)
        .unwrap();
    assert_eq!(again["product"], "pain");
    assert_eq!(again["price_cents"], 110);

    let unknown = json!({ "qty": 1, "price_cents": 100, "product": "nope" });
    let (status, _) = call(&addr, "PUT", &path, &unknown, "").await;
    assert_eq!(status, 404, "unknown product");
    let negative = json!({ "qty": -1, "price_cents": 100, "product": "pain" });
    let (status, _) = call(&addr, "PUT", &path, &negative, "").await;
    assert_eq!(status, 400);
    let (status, _) = call(&addr, "PUT", "/api/cellier/purchases/999", &fix, "").await;
    assert_eq!(status, 404);

    let (status, _) = call(&addr, "DELETE", &path, &Json::Null, "").await;
    assert_eq!(status, 204);
    let (status, _) = call(&addr, "DELETE", &path, &Json::Null, "").await;
    assert_eq!(status, 404);
    let (_, shown) = call(&addr, "GET", "/api/cellier/purchases", &Json::Null, "").await;
    assert_eq!(shown["purchases"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// The contract with the assistant: what `Assistant::read_receipt` gives
/// (every field of a draft, `at` in ms, no `items_done`) is a draft.
#[test]
fn a_reading_from_the_assistant_is_a_draft() {
    let reading = json!({
        "provider": "enseigne-x",
        "store": "Ville-A",
        "at": 1_790_000_000_000_u64,
        "total_cents": 1234,
        "unreadable": false,
        "lines": [
            {
                "label": "LAIT 1L", "name": "lait", "qty": 2, "unit": null,
                "price_cents": 99, "discount_cents": 10, "line_cents": 188,
                "vat": 5.5, "aisle": "Cremerie", "keep": true, "reason": "",
                "product": "lait", "new_product": null
            },
            {
                "label": "SAC", "name": "sac", "qty": 1.5, "unit": "kg",
                "price_cents": 5, "discount_cents": 0, "line_cents": 8,
                "vat": null, "aisle": null, "keep": false, "reason": "a bag",
                "product": null, "new_product": "Sac"
            }
        ]
    });
    let draft = serde_json::from_value::<moli_pantry::ReceiptDraft>(reading).unwrap();
    assert_eq!(draft.at, Some(1_790_000_000_000));
    assert_eq!(draft.lines.len(), 2);
    assert_eq!(draft.lines[0].product.as_deref(), Some("lait"));
    assert!(draft.items_done.is_empty());
    assert!(!draft.lines[1].keep);
}

// ---- reading a receipt end to end, with a provider on the local network -------

/// The requests a mock provider received.
type Asked = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A chat provider on 127.0.0.1 that answers every request with `content`
/// (the words of the model); gives its `base_url` and the requests it got.
async fn mock_provider(content: &str) -> (String, Asked) {
    let body = json!({
        "choices": [{ "message": { "role": "assistant", "content": content } }],
        "usage": { "prompt_tokens": 3_000, "completion_tokens": 400 },
    })
    .to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let asked = Asked::default();
    let kept = asked.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (body, kept) = (body.clone(), kept.clone());
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if rest.len() >= len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                kept.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).to_string());
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (url, asked)
}

/// A pantry with a product, an assistant whose model says `content`, and the
/// cookie of a person.
async fn reading_house(
    name: &str,
    content: &str,
) -> (String, std::path::PathBuf, Pantry, Asked, String) {
    let (url, asked) = mock_provider(content).await;
    let (addr, dir, pantry) = open_house_with(name, true, false, Some(url)).await;
    let pantry = pantry.unwrap();
    let product = moli_pantry::Product {
        id: "lait".into(),
        name: "Lait".into(),
        ..moli_pantry::Product::default()
    };
    pantry.save_product(&product).await.unwrap();
    let cookie = owner(&addr).await;
    (addr, dir, pantry, asked, cookie)
}

#[tokio::test]
async fn a_receipt_is_read_into_a_draft() {
    // Floats for the amounts and no label on the second line: still a draft.
    let reading = json!({
        "provider": "Enseigne-X", "store": "Ville-A", "at_local": "2026-10-28 12:37",
        "total_cents": 1047.0, "unreadable": false,
        "lines": [
            { "label": "LAIT 1L", "name": "Lait", "qty": 2.0, "unit": null,
              "price_cents": 99.0, "discount_cents": 0.0, "line_cents": 198.0,
              "vat": 5.5, "aisle": null, "keep": true, "reason": "", "product": "lait",
              "new_product": null },
            { "qty": 1, "price_cents": 849, "line_cents": 849, "keep": true },
        ],
    });
    let (addr, dir, pantry, asked, cookie) =
        reading_house("receipt-read", &reading.to_string()).await;

    let (status, body) = call(&addr, "GET", "/api/cellier/reader", &Json::Null, "").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["allowed"], true, "{body}");

    let photos = json!({ "photos": [encoded(&jpeg()), encoded(&jpeg())] });
    let (status, receipt) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["status"], "draft");
    assert_eq!(receipt["photos"], 2);
    assert_eq!(receipt["draft"]["provider"], "Enseigne-X", "{receipt}");
    assert_eq!(receipt["draft"]["total_cents"], 1047);
    assert_eq!(receipt["draft"]["lines"][0]["price_cents"], 99);
    assert_eq!(receipt["draft"]["lines"][1]["label"], "");
    assert_eq!(receipt["draft"]["lines"][0]["product"], "lait");
    assert!(
        receipt["reader"].as_str().unwrap().contains('/'),
        "{receipt}"
    );
    assert!(!receipt["sent_by"].as_str().unwrap().is_empty());
    assert_eq!(receipt["read"]["lines"].as_array().unwrap().len(), 2);

    let id = receipt["id"].as_i64().unwrap();
    assert_eq!(pantry.receipts(None).await.unwrap().len(), 1);
    for n in [1, 2] {
        assert!(pantry.photo_path(id, n).exists(), "photo {n} is kept");
    }
    // The list leaves the raw reading out; the receipt itself keeps it.
    let (_, listed) = call(&addr, "GET", "/api/cellier/receipts", &Json::Null, "").await;
    assert_eq!(listed[0]["read"], Json::Null, "{listed}");
    let path = format!("/api/cellier/receipts/{id}");
    let (_, shown) = call(&addr, "GET", &path, &Json::Null, "").await;
    assert!(shown["read"]["lines"].is_array(), "{shown}");

    // The model got both photos, as images.
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].matches("data:image/jpeg;base64,").count(), 2);
    drop(asked);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_unreadable_receipt_creates_nothing() {
    let reading = json!({ "unreadable": true, "lines": [] });
    let (addr, dir, pantry, _, cookie) = reading_house("receipt-blur", &reading.to_string()).await;
    let photos = json!({ "photos": [encoded(&jpeg())] });
    let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body, json!({ "error": "unreadable" }));
    assert!(pantry.receipts(None).await.unwrap().is_empty());
    let kept = dir.join("receipts");
    assert!(!kept.exists() || kept.read_dir().unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_garbled_reading_creates_nothing() {
    let (addr, dir, pantry, asked, cookie) =
        reading_house("receipt-garbled", "Je ne peux pas lire ce ticket.").await;
    let photos = json!({ "photos": [encoded(&jpeg())] });
    let (status, body) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 502, "{body}");
    assert!(body["error"].is_string());
    assert_eq!(asked.lock().unwrap().len(), 1, "the model was asked once");
    assert!(pantry.receipts(None).await.unwrap().is_empty());
    let kept = dir.join("receipts");
    assert!(!kept.exists() || kept.read_dir().unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn only_a_person_validates_a_receipt() {
    let (addr, dir, pantry, id) = with_draft("receipt-human").await;
    let path = format!("/api/cellier/receipts/{id}/validate");
    // No session: a script, an agent, the bare dashboard.
    let (status, body) = call(&addr, "POST", &path, &json!({}), "").await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = call(&addr, "POST", &path, &json!({}), "x-moli-origin: ui\r\n").await;
    assert_eq!(status, 403, "{body}");
    assert!(pantry.purchases(0, u64::MAX / 4).await.unwrap().is_empty());
    assert_eq!(
        pantry.receipt(id).await.unwrap().status,
        moli_pantry::ReceiptStatus::Draft
    );
    let cookie = owner(&addr).await;
    let (status, body) = call(&addr, "POST", &path, &json!({}), &cookie).await;
    assert_eq!(status, 200, "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_draft_changed_elsewhere_is_a_conflict() {
    let (addr, dir, _pantry, id) = with_draft("receipt-version").await;
    let path = format!("/api/cellier/receipts/{id}");
    let cookie = owner(&addr).await;
    // Two pages load the draft.
    let (_, loaded) = call(&addr, "GET", &path, &Json::Null, "").await;
    let version = loaded["version"].clone();
    assert!(version.is_u64(), "{loaded}");
    let mut first = loaded["draft"].clone();
    first["store"] = json!("Ville-B");
    first["version"] = version.clone();
    let (status, saved) = call(&addr, "PUT", &path, &first, "").await;
    assert_eq!(status, 200, "{saved}");
    assert_ne!(saved["version"], version);
    // The other page saves, then validates, what it loaded: 409.
    let mut second = loaded["draft"].clone();
    second["version"] = version.clone();
    let (status, body) = call(&addr, "PUT", &path, &second, "").await;
    assert_eq!(status, 409, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("changed elsewhere"),
        "{body}"
    );
    let validate = format!("{path}/validate");
    let (status, body) = call(
        &addr,
        "POST",
        &validate,
        &json!({ "version": version }),
        &cookie,
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let (_, shown) = call(&addr, "GET", &path, &Json::Null, "").await;
    assert_eq!(shown["draft"]["store"], "Ville-B", "the first save is kept");
    // Keeping its version: sent again with the new one.
    second["version"] = saved["version"].clone();
    let (status, kept) = call(&addr, "PUT", &path, &second, "").await;
    assert_eq!(status, 200, "{kept}");
    let (status, done) = call(
        &addr,
        "POST",
        &validate,
        &json!({ "version": kept["version"] }),
        &cookie,
    )
    .await;
    assert_eq!(status, 200, "{done}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_label_matched_by_a_person_fills_the_next_reading() {
    // What the reader gives for a product the pantry does not know: a
    // product to create, every time.
    let reading = json!({
        "provider": "enseigne-x", "store": "Ville-A", "at_local": "2026-10-28 12:37",
        "total_cents": 300, "unreadable": false,
        "lines": [{
            "label": "CHOCO CEREAL 430G", "name": "Céréales choco", "qty": 1, "unit": null,
            "price_cents": 300, "discount_cents": 0, "line_cents": 300, "vat": 5.5,
            "aisle": null, "keep": true, "reason": "", "product": null,
            "new_product": "Céréales choco"
        }],
    });
    let (addr, dir, pantry, _, cookie) = reading_house("receipt-learn", &reading.to_string()).await;
    let cereal = moli_pantry::Product {
        id: "cereales".into(),
        name: "Céréales".into(),
        ..moli_pantry::Product::default()
    };
    pantry.save_product(&cereal).await.unwrap();
    let photos = json!({ "photos": [encoded(&jpeg())] });

    // Ticket 1: the person points the line to the existing product.
    let (status, first) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 201, "{first}");
    assert_eq!(first["draft"]["lines"][0]["product"], Json::Null);
    let path = format!("/api/cellier/receipts/{}", first["id"]);
    let mut corrected = first["draft"].clone();
    corrected["lines"][0]["product"] = json!("cereales");
    corrected["lines"][0]["new_product"] = Json::Null;
    let (status, saved) = call(&addr, "PUT", &path, &corrected, "").await;
    assert_eq!(status, 200, "{saved}");
    let (status, done) = call(
        &addr,
        "POST",
        &format!("{path}/validate"),
        &json!({}),
        &cookie,
    )
    .await;
    assert_eq!(status, 200, "{done}");

    // Ticket 2, read the same way: the line comes pre-filled.
    let (status, second) = call(&addr, "POST", "/api/cellier/receipts", &photos, &cookie).await;
    assert_eq!(status, 201, "{second}");
    let line = &second["draft"]["lines"][0];
    assert_eq!(line["product"], "cereales", "{second}");
    assert_eq!(line["new_product"], Json::Null, "{second}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_settings_say_where_receipt_photos_go() {
    let (addr, dir, _, _, _) = reading_house("receipt-where", "{}").await;
    let (status, body) = call(&addr, "GET", "/api/cellier/reader", &Json::Null, "").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["host"], "127.0.0.1", "{body}");
    assert_eq!(body["at_home"], true, "{body}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_lines_cannot_attach_themselves_to_a_receipt() {
    let (addr, dir) = house("mcp-attach", true, false).await;
    let now = moli_core::now_ms();
    let line = json!({
        "product": "lait", "provider": "enseigne-x", "store": "Ville-A", "qty": 1,
        "price_cents": 100, "at": now - HOUR, "source": "ticket", "status": "confirmed",
        "label": "LAIT",
    });
    let header = json!({ "provider": "enseigne-x", "store": "Ville-A", "at": now - HOUR });
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "lines": [line], "receipt": header }),
    )
    .await;
    assert!(!failed, "{said}");
    let receipt = said["receipt"].as_i64().unwrap();

    // A line sent later, claiming that receipt (and a row id): recorded alone.
    let mut stray = line.clone();
    stray["receipt"] = json!(receipt);
    stray["id"] = json!(1);
    stray["price_cents"] = json!(150);
    let (failed, said) = tool(
        &addr,
        "pantry_record_purchases",
        &json!({ "lines": [stray] }),
    )
    .await;
    assert!(!failed, "{said}");
    let (_, bought) = tool(&addr, "pantry_purchases", &json!({})).await;
    let lines = bought["purchases"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{bought}");
    let attached: Vec<&Json> = lines.iter().filter(|p| p["receipt"] == receipt).collect();
    assert_eq!(attached.len(), 1, "{bought}");
    assert_eq!(attached[0]["price_cents"], 100);
    let _ = std::fs::remove_dir_all(dir);
}

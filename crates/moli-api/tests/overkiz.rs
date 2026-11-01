//! Synthetic HTTP/vault boundary tests; no provider transport or real credentials.
use moli_runtime::{Hub, HubOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const ROOT: &str = "/api/integrations/overkiz-cloud";

struct Server {
    addr: String,
    hub: Hub,
    dir: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl Server {
    async fn new(name: &str) -> Self {
        Self::configured(name, false).await
    }
    async fn configured(name: &str, cloud: bool) -> Self {
        let dir =
            std::env::temp_dir().join(format!("moli-cloud-api-{name}-{}", std::process::id()));
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
        let mut options = moli_api::Options {
            ui_pin: Some(moli_api::UiPin::new("482915")),
            home_path: Some(dir.join("home.json")),
            ..moli_api::Options::default()
        };
        if cloud {
            options
                .overkiz_cloud
                .insert("fixture".into(), moli_overkiz::cloud::Config::default());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let app = moli_api::router(hub.clone(), CancellationToken::new(), &options);
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            addr,
            hub,
            dir,
            task,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: &str,
        cookie: &str,
    ) -> (u16, String, String) {
        let mut stream = tokio::net::TcpStream::connect(&self.addr).await.unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nCookie: {cookie}\r\nContent-Length: {}\r\n\r\n",
            self.addr,
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let raw = String::from_utf8(raw).unwrap();
        let status = raw[9..12].parse().unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        (status, headers.into(), body.into())
    }
    async fn json(
        &self,
        method: &str,
        path: &str,
        body: Value,
        cookie: &str,
    ) -> (u16, String, Value) {
        let (status, headers, body) = self.request(method, path, &body.to_string(), cookie).await;
        (status, headers, serde_json::from_str(&body).unwrap())
    }
    async fn pin(&self) -> String {
        let (status, headers, _) = self
            .json("POST", "/api/session", json!({"pin":"482915"}), "")
            .await;
        assert_eq!(status, 200);
        cookie(&headers)
    }
}
fn cookie(headers: &str) -> String {
    headers
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}

#[tokio::test]
async fn private_cloud_routes_require_owner_before_body_or_vault() {
    let server = Server::new("auth").await;
    server
        .hub
        .store_secret(&"fixture".into(), "cloud_state_v1", "PRIVATE_BROKEN_RECORD")
        .await
        .unwrap();
    for (method, path) in [
        ("GET", ROOT.to_owned()),
        ("GET", format!("{ROOT}/fixture/catalog")),
        (
            "GET",
            format!("{ROOT}/fixture/definitions/scenarios/0?revision=x"),
        ),
        ("POST", format!("{ROOT}/fixture/requests")),
    ] {
        let (status, headers, body) = server
            .request(method, &path, "{SENTINEL_PASSWORD", "")
            .await;
        assert_eq!(status, 403, "{path}: {body}");
        assert!(!body.contains("SENTINEL") && !body.contains("PRIVATE"));
        assert!(headers.contains("no-store"));
    }
    let pin = server.pin().await;
    let (status, _, body) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({"instances":[]}));
}

async fn store(server: &Server, key: &str, value: &Value) {
    server
        .hub
        .store_secret(&"fixture".into(), key, &value.to_string())
        .await
        .unwrap();
}
fn candidate() -> Value {
    json!({"site_oid":"site-fixture","home_id":"home-fixture","label":"<img src=x>","country":"FR","gateway_ids":["gateway-a","gateway-b"],"region":"emea"})
}
fn scope() -> Value {
    json!({"site_oid":"site-fixture","gateway_id":"gateway-a","region":"emea"})
}
async fn seed(server: &Server) -> Vec<u8> {
    use moli_overkiz::inventory::{Catalog, CatalogIdentity, CollectionKind, encode_catalog};
    let now = i64::try_from(moli_core::now_ms()).unwrap();
    let session = json!({"version":1,"instance":"fixture","generation":"generation-a","phase":"ready","scope":scope(),"token_site_oid":"site-fixture","access":"ACCESS_SENTINEL","refresh":"REFRESH_SENTINEL","issued_at_ms":now,"expires_at_ms":now+300_000,"candidates":[candidate()]});
    store(server, "cloud_session_v1", &session).await;
    store(server, "cloud_state_v1", &json!({"version":1,"phase":"ready","generation":"generation-a","request_id":"finished-a","scope":scope(),"candidates":[candidate()],"failure":null,"last_attempt_ms":now,"last_observation_ms":now,"retry_after_secs":null})).await;
    let id: CatalogIdentity = serde_json::from_value(
        json!({"instance":"fixture","connection_generation":"generation-a","scope":scope()}),
    )
    .unwrap();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    catalog.apply_collection(&id, CollectionKind::Scenarios, now, Ok(moli_overkiz::inventory::parse_collection(CollectionKind::Scenarios, br#"[{"oid":"same","label":"<img src=x>","private_email":"fixture@example.com","n":18446744073709551615}]"#).unwrap())).unwrap();
    let bytes = encode_catalog(&catalog).unwrap();
    server
        .hub
        .store_secret(
            &"fixture".into(),
            "cloud_inventory_v1",
            std::str::from_utf8(&bytes).unwrap(),
        )
        .await
        .unwrap();
    bytes
}

#[tokio::test]
async fn configured_instances_queue_atomic_fixed_key_and_redact_invalid_input() {
    let server = Server::configured("queue", true).await;
    let pin = server.pin().await;
    let (status, _, body) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(status, 200);
    assert_eq!(body["instances"][0]["instance"], "fixture");
    for name in ["core", "other", "local-only"] {
        assert_eq!(
            server
                .request(
                    "POST",
                    &format!("{ROOT}/{name}/requests"),
                    r#"{"kind":"login","username":"a","password":"p"}"#,
                    &pin
                )
                .await
                .0,
            404
        );
    }
    for input in [
        r#"{"kind":"login","username":"a","password":"SENTINEL","extra":5}"#.into(),
        "SENTINEL".repeat(4000),
        r#"{"kind":"SENTINEL"}"#.into(),
    ] {
        let (status, _, body) = server
            .request("POST", &format!("{ROOT}/fixture/requests"), &input, &pin)
            .await;
        assert!(status == 400 || status == 413);
        assert!(!body.contains("SENTINEL"));
    }
    let login =
        json!({"kind":"login","username":"fixture@example.com","password":"FIRST_PASSWORD"});
    let (status, _, body) = server
        .json("POST", &format!("{ROOT}/fixture/requests"), login, &pin)
        .await;
    assert_eq!(status, 202);
    assert!(body["request_id"].as_str().is_some_and(|v| !v.is_empty()));
    let saved = server
        .hub
        .secret(&"fixture".into(), "cloud_pending_v1")
        .unwrap();
    let request = moli_overkiz::cloud::decode_request(saved.as_bytes()).unwrap();
    assert_eq!(request.request_id, body["request_id"]);
    assert_eq!(
        server
            .json(
                "POST",
                &format!("{ROOT}/fixture/requests"),
                json!({"kind":"login","username":"other","password":"SECOND_PASSWORD"}),
                &pin
            )
            .await
            .0,
        409
    );
    assert_eq!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_pending_v1")
            .unwrap(),
        saved
    );
    assert!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_session_v1")
            .is_none()
    );
    assert!(!body.to_string().contains("PASSWORD"));
}

#[tokio::test]
async fn cache_binding_staleness_and_raw_revision_preserve_private_definitions_without_tokens() {
    let server = Server::configured("cache", true).await;
    let pin = server.pin().await;
    let bytes = seed(&server).await;
    let (status, _, body) = server
        .request("GET", &format!("{ROOT}/fixture/catalog"), "", &pin)
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("18446744073709551615") && body.contains("private_email"));
    assert!(!body.contains("ACCESS_SENTINEL") && !body.contains("REFRESH_SENTINEL"));
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        response["freshness"]["current"], false,
        "saved Ready alone is never live"
    );
    let revision = response["revision"].as_str().unwrap();
    assert_eq!(revision.len(), 64);
    let path = format!("{ROOT}/fixture/definitions/scenarios/0?revision={revision}");
    let (status, _, body) = server.request("GET", &path, "", &pin).await;
    assert_eq!(status, 200);
    assert!(body.contains("18446744073709551615"));
    for phase in ["refreshing", "login_required"] {
        let mut session: Value = serde_json::from_str(
            &server
                .hub
                .secret(&"fixture".into(), "cloud_session_v1")
                .unwrap(),
        )
        .unwrap();
        session["phase"] = json!(phase);
        store(&server, "cloud_session_v1", &session).await;
        assert_eq!(server.request("GET", &path, "", &pin).await.0, 200);
    }
    // Same JSON value, different persisted bytes: revisions bind exact storage bytes.
    let spaced = format!("\n{} ", std::str::from_utf8(&bytes).unwrap());
    server
        .hub
        .store_secret(&"fixture".into(), "cloud_inventory_v1", &spaced)
        .await
        .unwrap();
    assert_eq!(server.request("GET", &path, "", &pin).await.0, 409);
    let mut session: Value = serde_json::from_str(
        &server
            .hub
            .secret(&"fixture".into(), "cloud_session_v1")
            .unwrap(),
    )
    .unwrap();
    session["generation"] = json!("generation-new");
    store(&server, "cloud_session_v1", &session).await;
    assert_eq!(
        server
            .request("GET", &format!("{ROOT}/fixture/catalog"), "", &pin)
            .await
            .0,
        409
    );
    let (_, _, state) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert!(!state.to_string().contains("generation-a"));
}

#[tokio::test]
async fn malformed_and_oversized_cache_are_unavailable_and_never_echoed() {
    let server = Server::configured("bounds", true).await;
    let pin = server.pin().await;
    seed(&server).await;
    for bytes in ["PRIVATE_MALFORMED".into(), " ".repeat(1_048_577)] {
        server
            .hub
            .store_secret(&"fixture".into(), "cloud_inventory_v1", &bytes)
            .await
            .unwrap();
        let (status, _, body) = server
            .request("GET", &format!("{ROOT}/fixture/catalog"), "", &pin)
            .await;
        assert_eq!(status, 503);
        assert!(!body.contains("PRIVATE_MALFORMED"));
    }
}

async fn invite(server: &Server, authority: &str, name: &str, role: &str) -> String {
    let (status, _, _) = server
        .json(
            "POST",
            "/api/people",
            json!({"name":name,"role":role,"rooms":[]}),
            authority,
        )
        .await;
    assert_eq!(status, 201);
    let (status, _, invitation) = server
        .json(
            "POST",
            &format!("/api/people/{name}/invite"),
            Value::Null,
            authority,
        )
        .await;
    assert_eq!(status, 200);
    let (status, headers, _) = server
        .json(
            "POST",
            "/api/invitation",
            json!({"code":invitation["code"],"password":"synthetic password only"}),
            "",
        )
        .await;
    assert_eq!(status, 200);
    cookie(&headers)
}
#[tokio::test]
async fn recognized_owner_is_allowed_but_guest_member_and_pin_are_not_owners() {
    let server = Server::configured("roles", true).await;
    let pin = server.pin().await;
    let member = invite(&server, &pin, "member", "member").await;
    let guest = invite(&server, &pin, "guest", "guest").await;
    let owner = invite(&server, &pin, "owner", "owner").await;
    seed(&server).await;
    for identity in [String::new(), pin.clone(), member, guest] {
        for suffix in [
            "",
            "/fixture/catalog",
            "/fixture/definitions/scenarios/0?revision=x",
        ] {
            let (status, _, body) = server
                .request("GET", &format!("{ROOT}{suffix}"), "", &identity)
                .await;
            assert_eq!(status, 403);
            assert!(!body.contains("SENTINEL") && !body.contains("site-fixture"));
        }
    }
    let (status, _, _) = server.json("GET", ROOT, Value::Null, &owner).await;
    assert_eq!(status, 200);
    let (_, _, denied) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(denied["auth_policy"], "owner_account");
}

struct Summary;
impl moli_runtime::Driver for Summary {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(
        &'a self,
        ctx: &'a mut moli_runtime::DriverCtx,
    ) -> moli_runtime::BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = moli_overkiz::cloud::summary_device_id(ctx.instance());
            ctx.upsert_device(moli_core::Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "Cloud summary".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: vec![],
                points: vec![],
            });
            ctx.set_availability(&id, true);
            ctx.ready();
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn fresh_requires_live_cloud_source_valid_age_and_qualified_collection() {
    let server = Server::configured("fresh", true).await;
    let pin = server.pin().await;
    seed(&server).await;
    let task = moli_runtime::spawn_driver(
        &server.hub,
        "fixture".into(),
        std::sync::Arc::new(Summary),
        CancellationToken::new(),
    );
    for _ in 0..100 {
        if server.hub.device(&"fixture:cloud:summary".into()).is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let path = format!("{ROOT}/fixture/catalog");
    let (_, _, data) = server.json("GET", &path, Value::Null, &pin).await;
    assert_eq!(data["freshness"]["current"], true);
    assert_eq!(data["freshness"]["collections"]["scenarios"], true);
    assert_eq!(data["freshness"]["collections"]["triggers"], false);
    let mut state: Value = serde_json::from_str(
        &server
            .hub
            .secret(&"fixture".into(), "cloud_state_v1")
            .unwrap(),
    )
    .unwrap();
    for timestamp in [1_u64, u64::try_from(i64::MAX).unwrap()] {
        state["last_observation_ms"] = json!(timestamp);
        store(&server, "cloud_state_v1", &state).await;
        let (_, _, data) = server.json("GET", &path, Value::Null, &pin).await;
        assert_eq!(data["freshness"]["current"], false);
    }
    assert!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_pending_v1")
            .is_none()
    );
    task.abort();
}

#[tokio::test]
async fn session_and_state_validation_do_not_release_a_previous_identity() {
    let server = Server::configured("binding-bounds", true).await;
    let pin = server.pin().await;
    seed(&server).await;
    let original = server
        .hub
        .secret(&"fixture".into(), "cloud_session_v1")
        .unwrap();
    let mut mismatch: Value = serde_json::from_str(&original).unwrap();
    mismatch["instance"] = json!("other-instance");
    for raw in [
        "PRIVATE_INVALID_SESSION".into(),
        " ".repeat(196_609),
        mismatch.to_string(),
    ] {
        server
            .hub
            .store_secret(&"fixture".into(), "cloud_session_v1", &raw)
            .await
            .unwrap();
        let (status, _, body) = server
            .request("GET", &format!("{ROOT}/fixture/catalog"), "", &pin)
            .await;
        assert_eq!(status, 409);
        assert!(!body.contains("PRIVATE") && !body.contains("SENTINEL"));
    }
    server
        .hub
        .store_secret(&"fixture".into(), "cloud_session_v1", &original)
        .await
        .unwrap();
    for raw in ["PRIVATE_INVALID_STATE".into(), " ".repeat(131_073)] {
        server
            .hub
            .store_secret(&"fixture".into(), "cloud_state_v1", &raw)
            .await
            .unwrap();
        let (_, _, body) = server.json("GET", ROOT, Value::Null, &pin).await;
        assert_eq!(
            body["instances"][0]["state"]["failure"]["kind"],
            "invalid_response"
        );
        assert!(!body.to_string().contains("PRIVATE"));
        let (_, _, body) = server
            .json("GET", &format!("{ROOT}/fixture/catalog"), Value::Null, &pin)
            .await;
        assert_eq!(
            body["freshness"]["current"], false,
            "valid same-binding history may survive a malformed status"
        );
    }
}

#[tokio::test]
async fn status_qualifies_historical_ready_with_authoritative_session_and_current_source() {
    let server = Server::configured("status-binding", true).await;
    let pin = server.pin().await;
    seed(&server).await;
    let original = server
        .hub
        .secret(&"fixture".into(), "cloud_state_v1")
        .unwrap();
    for phase in ["ready", "refreshing", "login_required"] {
        let mut session: Value = serde_json::from_str(
            &server
                .hub
                .secret(&"fixture".into(), "cloud_session_v1")
                .unwrap(),
        )
        .unwrap();
        session["phase"] = json!(phase);
        store(&server, "cloud_session_v1", &session).await;
        let (_, _, body) = server.json("GET", ROOT, Value::Null, &pin).await;
        let row = &body["instances"][0];
        assert_eq!(
            row["state"]["phase"], "ready",
            "retain the historical state independently"
        );
        assert_eq!(row["session_phase"], phase);
        assert_eq!(row["online"], false);
        let (status, _, body) = server
            .json("GET", &format!("{ROOT}/fixture/catalog"), Value::Null, &pin)
            .await;
        assert_eq!(status, 200, "same-binding history remains readable");
        assert_eq!(body["freshness"]["current"], false);
        assert_eq!(
            server
                .hub
                .secret(&"fixture".into(), "cloud_state_v1")
                .unwrap(),
            original
        );
    }
    let task = moli_runtime::spawn_driver(
        &server.hub,
        "fixture".into(),
        std::sync::Arc::new(Summary),
        CancellationToken::new(),
    );
    for _ in 0..100 {
        if server.hub.device(&"fixture:cloud:summary".into()).is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    // A live summary cannot override authoritative LoginRequired.
    let (_, _, body) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(body["instances"][0]["online"], false);
    let mut session: Value = serde_json::from_str(
        &server
            .hub
            .secret(&"fixture".into(), "cloud_session_v1")
            .unwrap(),
    )
    .unwrap();
    session["phase"] = json!("ready");
    store(&server, "cloud_session_v1", &session).await;
    let (_, _, body) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(body["instances"][0]["online"], true);
    assert_eq!(body["instances"][0]["session_phase"], "ready");
    assert_eq!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_state_v1")
            .unwrap(),
        original
    );
    task.abort();
}

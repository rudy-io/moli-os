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
        Self::mode(name, cloud, true).await
    }
    async fn mode(name: &str, cloud: bool, read_only: bool) -> Self {
        Self::options(name, cloud, read_only, None).await
    }
    async fn with_private_file(name: &str, cloud: bool, private: Option<&[u8]>) -> Self {
        Self::options(name, cloud, true, private).await
    }
    async fn options(name: &str, cloud: bool, read_only: bool, private: Option<&[u8]>) -> Self {
        let dir =
            std::env::temp_dir().join(format!("moli-cloud-api-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(bytes) = private {
            std::fs::write(dir.join("private-state.enc"), bytes).unwrap();
        }
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
            options.overkiz_cloud.insert(
                "fixture".into(),
                moli_overkiz::cloud::Config {
                    read_only,
                    ..Default::default()
                },
            );
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
        ("DELETE", format!("{ROOT}/fixture")),
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
    assert_eq!(body["instances"][0]["instance"], "fixture");
    assert_eq!(body["instances"][0]["configured"], false);
    assert!(!body.to_string().contains("PRIVATE_BROKEN_RECORD"));
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
    let session = json!({"version":1,"instance":"fixture","generation":"generation-a","phase":"ready","scope":scope(),"token_site_oid":"home-fixture","access":"ACCESS_SENTINEL","refresh":"REFRESH_SENTINEL","issued_at_ms":now,"expires_at_ms":now+300_000,"candidates":[candidate()]});
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
    let private = server.hub.private_state(&"fixture".into()).unwrap();
    let saved = private.get("cloud_pending_v1").unwrap();
    assert!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_pending_v1")
            .is_none()
    );
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
    assert_eq!(private.get("cloud_pending_v1").unwrap(), saved);
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
        let (status, _, body) = server
            .request("DELETE", &format!("{ROOT}/fixture"), "", &identity)
            .await;
        assert_eq!(status, 403);
        assert!(!body.contains("SENTINEL") && !body.contains("site-fixture"));
    }
    let (status, _, _) = server.json("GET", ROOT, Value::Null, &owner).await;
    assert_eq!(status, 200);
    let (_, _, denied) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(denied["auth_policy"], "owner_account");
}

const CLOUD_KEYS: [&str; 5] = [
    "cloud_session_v1",
    "cloud_pending_v1",
    "cloud_state_v1",
    "cloud_inventory_v1",
    "cloud_scenario_permissions_v1",
];

#[tokio::test]
async fn owner_disconnect_purges_legacy_cloud_after_configuration_removal_only_for_its_instance() {
    for configured in [false, true] {
        let server =
            Server::configured(&format!("disconnect-legacy-{configured}"), configured).await;
        let pin = server.pin().await;
        seed(&server).await;
        let instance = "fixture".into();
        server
            .hub
            .store_secret(&instance, "cloud_scenario_permissions_v1", "PRIVATE_POLICY")
            .await
            .unwrap();
        server
            .hub
            .store_secret(&instance, "cloud_pending_v1", "PRIVATE_PENDING")
            .await
            .unwrap();
        for name in ["token", "tls_fingerprint", "cloud_session_v1_extra"] {
            server
                .hub
                .store_secret(&instance, name, "NATIVE_SENTINEL")
                .await
                .unwrap();
        }
        server
            .hub
            .store_secret(
                &"other".into(),
                "cloud_session_v1",
                "OTHER_PRIVATE_SENTINEL",
            )
            .await
            .unwrap();
        let (status, _, list) = server.json("GET", ROOT, Value::Null, &pin).await;
        assert_eq!(status, 200);
        let row = list["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["instance"] == "fixture")
            .unwrap();
        assert_eq!(row["configured"], configured);
        assert_eq!(row["online"], false);
        assert!(!list.to_string().contains("SENTINEL"));
        let (status, headers, body) = server
            .json("DELETE", &format!("{ROOT}/fixture"), Value::Null, &pin)
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body, json!({"disconnected":true}));
        assert!(headers.contains("no-store"));
        for name in CLOUD_KEYS {
            assert!(
                server.hub.secret(&instance, name).is_none(),
                "retained {name}"
            );
        }
        for name in ["token", "tls_fingerprint", "cloud_session_v1_extra"] {
            assert_eq!(
                server.hub.secret(&instance, name).as_deref(),
                Some("NATIVE_SENTINEL")
            );
        }
        assert_eq!(
            server
                .hub
                .secret(&"other".into(), "cloud_session_v1")
                .as_deref(),
            Some("OTHER_PRIVATE_SENTINEL")
        );
        let (_, _, list) = server.json("GET", ROOT, Value::Null, &pin).await;
        let rows = list["instances"].as_array().unwrap();
        assert!(rows.iter().any(|row| row["instance"] == "other"));
        let retained = rows.iter().find(|row| row["instance"] == "fixture");
        if configured {
            let retained = retained.unwrap();
            assert_eq!(retained["configured"], true);
            assert_eq!(retained["state"]["phase"], "disconnected");
            assert_eq!(retained["online"], false);
        } else {
            assert!(
                retained.is_none(),
                "an orphan is no longer discoverable after purge"
            );
        }
        assert_eq!(
            server
                .request("DELETE", &format!("{ROOT}/unknown"), "", &pin)
                .await
                .0,
            404
        );
    }
}

#[tokio::test]
async fn failed_disconnect_never_claims_success_when_the_vault_cannot_be_locked() {
    let server = Server::new("disconnect-locked").await;
    let pin = server.pin().await;
    seed(&server).await;
    let path = server.dir.join("secrets.enc");
    let original = std::fs::read(&path).unwrap();
    let lock = server.dir.join("secrets.enc.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let (status, _, raw) = server
        .request("DELETE", &format!("{ROOT}/fixture"), "", &pin)
        .await;
    assert_eq!(status, 503);
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_ne!(body["disconnected"], true);
    assert!(!body.to_string().contains("SENTINEL"));
    assert_eq!(std::fs::read(path).unwrap(), original);
    assert!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_session_v1")
            .is_some()
    );
}

#[tokio::test]
async fn owner_can_disconnect_an_orphan_present_only_in_the_encrypted_private_cache() {
    let server = Server::new("disconnect-private").await;
    let pin = server.pin().await;
    let instance = "fixture".into();
    let state = server.hub.private_state(&instance).unwrap();
    state
        .set_if_changed(
            CLOUD_KEYS[1..]
                .iter()
                .map(|name| {
                    (
                        name.to_string(),
                        "PRIVATE_RECORD".into(),
                        "PRIVATE_RECORD".into(),
                    )
                })
                .collect(),
        )
        .await
        .unwrap();
    let other = server.hub.private_state(&"other".into()).unwrap();
    other
        .set_if_changed(vec![(
            "cloud_inventory_v1".into(),
            "OTHER_PRIVATE_RECORD".into(),
            "OTHER_PRIVATE_RECORD".into(),
        )])
        .await
        .unwrap();
    let (_, _, list) = server.json("GET", ROOT, Value::Null, &pin).await;
    let rows = list["instances"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["instance"] == "fixture")
        .unwrap();
    assert_eq!(row["configured"], false);
    assert_eq!(row["online"], false);
    assert!(!list.to_string().contains("PRIVATE_RECORD"));
    let (status, _, body) = server
        .json("DELETE", &format!("{ROOT}/fixture"), Value::Null, &pin)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"disconnected":true}));
    let fresh = server.hub.private_state(&instance).unwrap();
    for name in &CLOUD_KEYS[1..] {
        assert!(
            state.get(name).is_none(),
            "old handles cannot expose the purged account"
        );
        assert!(fresh.get(name).is_none());
    }
    assert!(
        state
            .store_secret("cloud_session_v1", "LATE_SENTINEL")
            .await
            .is_err()
    );
    assert_eq!(
        other.get("cloud_inventory_v1").as_deref(),
        Some("OTHER_PRIVATE_RECORD")
    );
    let (_, _, list) = server.json("GET", ROOT, Value::Null, &pin).await;
    let rows = list["instances"].as_array().unwrap();
    assert!(!rows.iter().any(|row| row["instance"] == "fixture"));
    assert!(rows.iter().any(|row| row["instance"] == "other"));
}

#[tokio::test]
async fn removed_configuration_exposes_only_stale_cached_reads_and_never_queues_requests() {
    let server = Server::new("orphan-read-only").await;
    let pin = server.pin().await;
    seed(&server).await;
    let task = moli_runtime::spawn_driver(
        &server.hub,
        "fixture".into(),
        std::sync::Arc::new(Summary),
        CancellationToken::new(),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while server
            .hub
            .device(&"fixture:cloud:summary".into())
            .is_none_or(|d| d.online != Some(true))
        {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let (status, _, body) = server
        .json("GET", &format!("{ROOT}/fixture/catalog"), Value::Null, &pin)
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["freshness"]["current"], false);
    assert_eq!(body["freshness"]["collections"]["scenarios"], false);
    let (_, _, list) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(list["instances"][0]["configured"], false);
    assert_eq!(list["instances"][0]["online"], false);
    for input in [
        json!({"kind":"login","username":"fixture@example.com","password":"ORPHAN_PASSWORD_SENTINEL"}),
        json!({"kind":"refresh","generation":"generation-a"}),
    ] {
        let (status, _, body) = server
            .json("POST", &format!("{ROOT}/fixture/requests"), input, &pin)
            .await;
        assert_eq!(status, 404);
        assert!(!body.to_string().contains("SENTINEL"));
    }
    assert!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_pending_v1")
            .is_none()
    );
    assert!(
        server
            .hub
            .private_state(&"fixture".into())
            .unwrap()
            .get("cloud_pending_v1")
            .is_none()
    );
    task.abort();
}

#[tokio::test]
async fn private_cache_takes_precedence_over_legacy_values_even_when_the_new_value_is_invalid() {
    let server = Server::configured("private-precedence", true).await;
    let pin = server.pin().await;
    seed(&server).await;
    let private = server.hub.private_state(&"fixture".into()).unwrap();
    private
        .set_if_changed(vec![(
            "cloud_inventory_v1".into(),
            "PRIVATE_INVALID".into(),
            "PRIVATE_INVALID".into(),
        )])
        .await
        .unwrap();
    let (status, _, body) = server
        .request("GET", &format!("{ROOT}/fixture/catalog"), "", &pin)
        .await;
    assert_eq!(
        status, 503,
        "a malformed current record must not revive an older catalogue"
    );
    assert!(!body.contains("PRIVATE_INVALID") && !body.contains("SENTINEL"));
}

#[tokio::test]
async fn unreadable_private_cache_does_not_trap_credentials_or_hide_an_unfinished_disconnect() {
    for configured in [false, true] {
        let server = Server::with_private_file(
            if configured {
                "disconnect-corrupt-configured"
            } else {
                "disconnect-corrupt-orphan"
            },
            configured,
            Some(b"PRIVATE_CORRUPT_CACHE"),
        )
        .await;
        let pin = server.pin().await;
        seed(&server).await;
        let instance = "fixture".into();
        server
            .hub
            .store_secret(&instance, "token", "NATIVE_SENTINEL")
            .await
            .unwrap();
        server
            .hub
            .store_secret(&"other".into(), "cloud_session_v1", "OTHER_SENTINEL")
            .await
            .unwrap();
        assert!(server.hub.private_state(&instance).is_none());
        let before = std::fs::read(server.dir.join("private-state.enc")).unwrap();
        let (status, _, body) = server
            .json("DELETE", &format!("{ROOT}/fixture"), Value::Null, &pin)
            .await;
        assert_eq!(status, 503);
        assert_eq!(body["error"], "cache_unavailable");
        assert_eq!(body["credentials_removed"], true);
        assert_ne!(body["disconnected"], true);
        for name in CLOUD_KEYS {
            assert!(server.hub.secret(&instance, name).is_none());
        }
        assert_eq!(
            server.hub.secret(&instance, "token").as_deref(),
            Some("NATIVE_SENTINEL")
        );
        assert_eq!(
            server
                .hub
                .secret(&"other".into(), "cloud_session_v1")
                .as_deref(),
            Some("OTHER_SENTINEL")
        );
        assert_eq!(
            std::fs::read(server.dir.join("private-state.enc")).unwrap(),
            before,
            "an unreadable shared file must not be destroyed"
        );
        let (_, _, list) = server.json("GET", ROOT, Value::Null, &pin).await;
        let row = list["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["instance"] == "fixture")
            .expect("partial cleanup remains discoverable");
        assert_eq!(row["configured"], configured);
        assert_eq!(row["cleanup_pending"], true);
        assert_eq!(row["online"], false);
        assert!(
            !list.to_string().contains("SENTINEL") && !list.to_string().contains("PRIVATE_CORRUPT")
        );
        let (status, _, body) = server
            .json("DELETE", &format!("{ROOT}/fixture"), Value::Null, &pin)
            .await;
        assert_eq!(
            status, 503,
            "the owner can retry the same orphan rather than receive404"
        );
        assert_eq!(body["credentials_removed"], true);
        if configured {
            let (status, _, body) = server
                .request(
                    "POST",
                    &format!("{ROOT}/fixture/requests"),
                    "invalid body",
                    &pin,
                )
                .await;
            assert_eq!(
                status, 503,
                "cleanup blocks reconnection before parsing input"
            );
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["error"], "cache_unavailable");
            assert_eq!(body["credentials_removed"], true);
            assert!(server.hub.secret(&instance, "cloud_pending_v1").is_none());
        }
    }
}

#[tokio::test]
async fn disconnect_rejects_a_request_whose_body_was_still_arriving() {
    let server = Server::configured("disconnect-slow-input", true).await;
    let pin = server.pin().await;
    disconnect_while_body_waits(
        &server,
        &pin,
        "requests",
        r#"{"kind":"refresh","generation":"generation-a"}"#,
        false,
    )
    .await;
}

#[tokio::test]
async fn scenario_permission_cannot_cross_disconnect_and_reconnection_while_body_waits() {
    let server = Server::configured("disconnect-slow-permission", true).await;
    let pin = server.pin().await;
    seed(&server).await;
    let body = json!({"generation":"generation-a","revision":"a".repeat(64),"scenario_id":"same","enabled":false,"pin":"482915"}).to_string();
    disconnect_while_body_waits(&server, &pin, "scenario-permission", &body, true).await;
}

#[tokio::test]
async fn unfinished_disconnect_blocks_permission_before_reading_its_body() {
    let server = Server::with_private_file(
        "partial-scenario-permission",
        true,
        Some(b"PRIVATE_CORRUPT_CACHE"),
    )
    .await;
    let pin = server.pin().await;
    seed(&server).await;
    assert_eq!(
        server
            .json("DELETE", &format!("{ROOT}/fixture"), Value::Null, &pin)
            .await
            .0,
        503
    );
    let (status, _, body) = server
        .request(
            "POST",
            &format!("{ROOT}/fixture/scenario-permission"),
            "invalid body",
            &pin,
        )
        .await;
    assert_eq!(status, 503);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["credentials_removed"], true);
    assert!(
        server
            .hub
            .secret(&"fixture".into(), "cloud_pending_v1")
            .is_none()
    );
}

async fn disconnect_while_body_waits(
    server: &Server,
    pin: &str,
    route: &str,
    body: &str,
    reconnect: bool,
) {
    let mut stream = tokio::net::TcpStream::connect(&server.addr).await.unwrap();
    let head = format!(
        "POST {ROOT}/fixture/{route} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nCookie: {pin}\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
        server.addr,
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    // Hyper emits Continue when the authenticated handler polls the body.
    // This barrier avoids using sleeps to order the request and the purge.
    let mut continued = vec![0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_exact(&mut continued),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(continued, b"HTTP/1.1 100 Continue\r\n\r\n");
    let (status, _, result) = server
        .json("DELETE", &format!("{ROOT}/fixture"), Value::Null, pin)
        .await;
    assert_eq!(status, 200);
    assert_eq!(result["disconnected"], true);
    if reconnect {
        // Even the same visible catalogue must not transfer a waiting request
        // into the new private lifetime created by DELETE.
        seed(server).await;
    }
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 503"),
        "retired request: {response}"
    );
    assert!(
        server
            .hub
            .private_state(&"fixture".into())
            .unwrap()
            .get("cloud_pending_v1")
            .is_none()
    );
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
    // A pre-fix setup-scoped bearer must not make an online cached source current.
    let original_session = server
        .hub
        .secret(&"fixture".into(), "cloud_session_v1")
        .unwrap();
    let mut legacy: Value = serde_json::from_str(&original_session).unwrap();
    legacy["token_site_oid"] = json!("site-fixture");
    store(&server, "cloud_session_v1", &legacy).await;
    let (_, _, legacy_state) = server.json("GET", ROOT, Value::Null, &pin).await;
    assert_eq!(legacy_state["instances"][0]["session_phase"], "incomplete");
    assert!(!legacy_state.to_string().contains("SENTINEL"));
    let (_, _, legacy_catalog) = server.json("GET", &path, Value::Null, &pin).await;
    assert_eq!(legacy_catalog["freshness"]["current"], false);
    assert_eq!(
        legacy_catalog["freshness"]["collections"]["scenarios"],
        false
    );
    store(
        &server,
        "cloud_session_v1",
        &serde_json::from_str(&original_session).unwrap(),
    )
    .await;
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

#[tokio::test]
async fn scenario_permission_requires_fresh_pin_and_respects_global_read_only() {
    let server = Server::configured("scenario-policy", true).await;
    let cookie = server.pin().await;
    seed(&server).await;
    let path = format!("{ROOT}/fixture/scenario-permission");
    let (_, _, catalog) = server
        .json(
            "GET",
            &format!("{ROOT}/fixture/catalog"),
            Value::Null,
            &cookie,
        )
        .await;
    let body = json!({"generation":"generation-a","revision":catalog["revision"],"scenario_id":"same","enabled":true,"pin":"wrong-PIN-sentinel"});
    assert_eq!(
        server.request("POST", &path, &body.to_string(), "").await.0,
        403
    );
    let (status, _, error) = server.json("POST", &path, body.clone(), &cookie).await;
    assert_eq!(status, 401);
    assert_eq!(error["code"], "invalid_pin");
    assert!(!error.to_string().contains("sentinel"));
    let mut correct = body;
    correct["pin"] = json!("482915");
    let (status, _, error) = server.json("POST", &path, correct.clone(), &cookie).await;
    assert_eq!(status, 403);
    assert_eq!(error["code"], "global_read_only");
    assert!(
        server
            .hub
            .private_state(&"fixture".into())
            .unwrap()
            .get("cloud_pending_v1")
            .is_none()
    );
    correct["enabled"] = json!(false);
    let (status, _, result) = server.json("POST", &path, correct, &cookie).await;
    assert_eq!(status, 202);
    assert!(result["request_id"].is_string());
    let saved = server
        .hub
        .private_state(&"fixture".into())
        .unwrap()
        .get("cloud_pending_v1")
        .unwrap();
    assert!(!saved.contains("482915") && !saved.contains("pin"));
    let envelope = moli_overkiz::cloud::decode_request(saved.as_bytes()).unwrap();
    assert_eq!(envelope.request_id, result["request_id"]);
}

#[tokio::test]
async fn generic_observation_requests_cannot_bypass_permission_pin() {
    let server = Server::configured("scenario-bypass", true).await;
    let cookie = server.pin().await;
    let payload = json!({"kind":"scenario_permission","generation":"generation-a","revision":"a".repeat(64),"scenario_id":"same","enabled":true});
    let (status, _, _) = server
        .json(
            "POST",
            &format!("{ROOT}/fixture/requests"),
            payload,
            &cookie,
        )
        .await;
    assert_eq!(status, 400);
    assert!(
        server
            .hub
            .private_state(&"fixture".into())
            .unwrap()
            .get("cloud_pending_v1")
            .is_none()
    );
}

#[tokio::test]
async fn consent_rejects_stale_catalogues_and_pin_brute_force_without_queueing() {
    let server = Server::mode("scenario-stale-limit", true, false).await;
    let cookie = server.pin().await;
    seed(&server).await;
    let path = format!("{ROOT}/fixture/scenario-permission");
    let mut request = json!({"generation":"generation-stale","revision":"a".repeat(64),"scenario_id":"same","enabled":false,"pin":"482915"});
    let (status, _, error) = server.json("POST", &path, request.clone(), &cookie).await;
    assert_eq!(status, 409);
    assert_eq!(error["code"], "catalog_changed");
    request["pin"] = json!("WRONG_SENTINEL");
    for _ in 0..5 {
        let (status, _, error) = server.json("POST", &path, request.clone(), &cookie).await;
        assert_eq!(status, 401);
        assert_eq!(error["code"], "invalid_pin");
        assert!(!error.to_string().contains("SENTINEL"));
    }
    request["pin"] = json!("482915");
    let (status, _, error) = server.json("POST", &path, request, &cookie).await;
    assert_eq!(status, 429);
    assert_eq!(error["code"], "pin_rate_limited");
    assert!(
        server
            .hub
            .private_state(&"fixture".into())
            .unwrap()
            .get("cloud_pending_v1")
            .is_none()
    );
    assert_eq!(
        server
            .json(
                "GET",
                &format!("{ROOT}/fixture/catalog"),
                Value::Null,
                &cookie
            )
            .await
            .0,
        200,
        "a wrong PIN never revokes the existing owner session"
    );
}

#[tokio::test]
async fn enabling_requires_the_exact_current_revision_and_known_scenario() {
    let server = Server::mode("scenario-exact", true, false).await;
    let cookie = server.pin().await;
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
    let (_, _, catalog) = server
        .json(
            "GET",
            &format!("{ROOT}/fixture/catalog"),
            Value::Null,
            &cookie,
        )
        .await;
    assert_eq!(catalog["execution"]["scenarios"][0]["executable"], false);
    assert_eq!(
        catalog["execution"]["scenarios"][0]["reason"],
        "permission_required"
    );
    let path = format!("{ROOT}/fixture/scenario-permission");
    let mut request = json!({"generation":"generation-a","revision":"a".repeat(64),"scenario_id":"same","enabled":true,"pin":"482915"});
    let (status, _, error) = server.json("POST", &path, request.clone(), &cookie).await;
    assert_eq!(status, 409);
    assert_eq!(error["code"], "catalog_changed");
    request["revision"] = catalog["revision"].clone();
    request["scenario_id"] = json!("unknown");
    assert_eq!(
        server.json("POST", &path, request.clone(), &cookie).await.0,
        400
    );
    request["scenario_id"] = json!("same");
    assert_eq!(server.json("POST", &path, request, &cookie).await.0, 202);
    let queued = server
        .hub
        .private_state(&"fixture".into())
        .unwrap()
        .get("cloud_pending_v1")
        .unwrap();
    assert!(!queued.contains("482915"));
    let request = moli_overkiz::cloud::decode_request(queued.as_bytes()).unwrap();
    assert!(matches!(
        request.request,
        moli_overkiz::cloud::PendingRequest::ScenarioPermission { enabled: true, .. }
    ));
    task.abort();
}

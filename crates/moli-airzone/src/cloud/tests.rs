use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct Fake {
    replies: StdMutex<VecDeque<Result<Reply, Error>>>,
    paths: StdMutex<Vec<String>>,
    tokens: StdMutex<Vec<Option<String>>>,
}
impl Fake {
    fn reply(&self, status: u16, body: Value) {
        self.replies
            .lock()
            .unwrap()
            .push_back(Ok(Reply { status, body }));
    }
    fn fail(&self, error: Error) {
        self.replies.lock().unwrap().push_back(Err(error));
    }
    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}
impl Transport for Fake {
    fn exchange(&self, endpoint: Endpoint, token: Option<Secret>) -> Exchange<'_> {
        self.paths.lock().unwrap().push(endpoint.path());
        self.tokens.lock().unwrap().push(token.map(|t| t.0));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected network call");
        Box::pin(async move { reply })
    }
}

#[derive(Default)]
struct FakeVault {
    stored: StdMutex<Option<String>>,
    unavailable: AtomicBool,
    fail_save: AtomicBool,
    fail_clear: AtomicBool,
}
impl vault::Vault for FakeVault {
    fn available(&self) -> bool {
        !self.unavailable.load(Ordering::SeqCst)
    }
    fn load(&self) -> Option<String> {
        self.stored.lock().unwrap().clone()
    }
    fn write(
        &self,
        value: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        let fail = if value.is_some() {
            &self.fail_save
        } else {
            &self.fail_clear
        };
        let result = if fail.load(Ordering::SeqCst) {
            Err(Error::Storage)
        } else {
            *self.stored.lock().unwrap() = value;
            Ok(())
        };
        Box::pin(async move { result })
    }
}

// Deliberately fictional JWT: the payload says exp=4102444800 (2100-01-01),
// with a dummy signature. It never leaves the fake transport.
fn token(suffix: &str) -> String {
    format!("e30.eyJleHAiOjQxMDI0NDQ4MDB9.{suffix}")
}
fn auth(suffix: &str) -> Value {
    json!({"token":token(suffix),"refreshToken":format!("refresh-{suffix}")})
}
async fn reader() -> (CloudClient, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    fake.reply(200, auth("one"));
    let client = CloudClient::with_transport(fake.clone());
    client
        .login("user@example.com", "synthetic-password")
        .await
        .unwrap();
    (client, fake)
}
fn installation(id: usize) -> Value {
    json!({"installation_id":format!("installation-{id}"),"location_id":"location-example",
           "name":"Example installation","access_type":"basic","ws_ids":[]})
}

#[tokio::test]
async fn exact_paths_include_installation_and_configuration_section() {
    let (client, fake) = reader().await;
    for _ in 0..6 {
        fake.reply(200, json!({"isConnected":true}));
    }
    client
        .device_status("installation-example", "device-example")
        .await
        .unwrap();
    for section in [
        ConfigSection::User,
        ConfigSection::Advanced,
        ConfigSection::All,
    ] {
        client
            .device_config("installation-example", "device-example", section)
            .await
            .unwrap();
    }
    client
        .gateway_status("installation-example", "aa:bb:cc:dd:ee:ff", false)
        .await
        .unwrap();
    client
        .gateway_status("installation-example", "gateway-example", true)
        .await
        .unwrap();
    assert_eq!(
        &fake.paths()[1..],
        &[
            "/api/v1/devices/device-example/status?installation_id=installation-example",
            "/api/v1/devices/device-example/config?installation_id=installation-example&type=user",
            "/api/v1/devices/device-example/config?installation_id=installation-example&type=advanced",
            "/api/v1/devices/device-example/config?installation_id=installation-example&type=all",
            "/api/v1/devices/ws/aa%3Abb%3Acc%3Add%3Aee%3Aff/status?installation_id=installation-example",
            "/api/v1/devices/ws/gateway-example/status?installation_id=installation-example&devices=1",
        ]
    );
    for id in [
        "",
        "../user/logout",
        "https://example.com",
        "a?type=all",
        "a\r\n",
    ] {
        assert_eq!(
            client
                .device_status("installation-example", id)
                .await
                .unwrap_err(),
            Error::InvalidInput
        );
    }
    assert_eq!(fake.paths().len(), 7);
}

#[tokio::test]
async fn installation_pagination_returns_every_page_or_no_result() {
    let (client, fake) = reader().await;
    fake.reply(
        200,
        json!({"total":11,"installations":(0..10).map(installation).collect::<Vec<_>>()}),
    );
    fake.reply(200, json!({"total":11,"installations":[installation(10)]}));
    let result = client.installations().await.unwrap();
    assert_eq!(result.len(), 11);
    assert_eq!(result.last().unwrap().installation_id, "installation-10");
    assert_eq!(
        &fake.paths()[1..],
        &[
            "/api/v1/installations?items=10&page=0",
            "/api/v1/installations?items=10&page=1"
        ]
    );
}

#[tokio::test]
async fn pagination_rejects_duplicate_changing_total_empty_and_over_limit_pages() {
    for bad in [
        json!({"total":2,"installations":[installation(0)]}),
        json!({"total":3,"installations":[installation(1)]}),
        json!({"total":2,"installations":[]}),
    ] {
        let (client, fake) = reader().await;
        fake.reply(200, json!({"total":2,"installations":[installation(0)]}));
        fake.reply(200, bad);
        assert_eq!(
            client.installations().await.unwrap_err(),
            Error::IncompleteInventory
        );
    }
    let (client, fake) = reader().await;
    fake.reply(200, json!({"total":1001,"installations":[]}));
    assert_eq!(
        client.installations().await.unwrap_err(),
        Error::IncompleteInventory
    );
    fake.reply(200, json!({"total":0,"installations":[]}));
    assert!(client.installations().await.unwrap().is_empty());
}

#[tokio::test]
async fn one_refresh_serves_concurrent_expired_reads_and_preserves_rotated_pair() {
    let (client, fake) = reader().await;
    client
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    fake.reply(200, auth("two"));
    fake.reply(200, json!({"power":true}));
    fake.reply(200, json!({"power":false}));
    let (a, b) = tokio::join!(
        client.device_status("installation-example", "device-a"),
        client.device_status("installation-example", "device-b")
    );
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(
        fake.paths()
            .iter()
            .filter(|p| p.contains("refreshToken"))
            .count(),
        1
    );
    assert!(
        fake.tokens.lock().unwrap()[2..]
            .iter()
            .all(|t| t.as_ref() == Some(&token("two")))
    );
}

#[tokio::test]
async fn a_proactive_rotation_is_not_repeated_after_an_unauthorized_read() {
    let (client, fake) = reader().await;
    client
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    fake.reply(200, auth("two"));
    fake.reply(401, Value::Null);
    // A second rotation must not consume this pair or make another read.
    fake.reply(200, auth("three"));
    fake.reply(200, json!({"power":true}));
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(
        fake.paths()
            .iter()
            .filter(|p| p.contains("refreshToken"))
            .count(),
        1
    );
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
}

#[tokio::test]
async fn provider_authentication_rejections_require_login_without_exposing_error_bodies() {
    let fake = Arc::new(Fake::default());
    fake.reply(
        400,
        json!({"error":"userNotConfirmed","email":"user@example.com"}),
    );
    let client = CloudClient::with_transport(fake.clone());
    assert_eq!(
        client
            .login("user@example.com", "synthetic-password")
            .await
            .unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(fake.paths().len(), 1);
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
    for status in [400, 403] {
        let (client, fake) = reader().await;
        client
            .state
            .lock()
            .await
            .session
            .as_mut()
            .unwrap()
            .expires_at = unix_now();
        fake.reply(status, Value::Null);
        assert_eq!(
            client
                .device_status("installation-example", "device-example")
                .await
                .unwrap_err(),
            Error::AuthenticationRequired
        );
        assert_eq!(fake.paths().len(), 2);
        assert_eq!(
            client.auth_status().await,
            AuthStatus::AuthenticationRequired
        );
    }
}

#[tokio::test]
async fn unauthorized_read_retries_once_after_refresh_and_second_denial_expires_session() {
    let (client, fake) = reader().await;
    fake.reply(401, Value::Null);
    fake.reply(200, auth("two"));
    fake.reply(401, Value::Null);
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(fake.paths().len(), 4);
}

#[tokio::test]
async fn failed_ambiguous_refresh_is_never_replayed() {
    let (client, fake) = reader().await;
    client
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    fake.fail(Error::Transport);
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::Transport
    );
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(fake.paths().len(), 2);
}

#[tokio::test]
async fn cancelled_refresh_forgets_consumed_credentials_before_network() {
    struct PendingRefresh {
        fake: Arc<Fake>,
        entered: tokio::sync::Notify,
    }
    impl Transport for PendingRefresh {
        fn exchange(&self, endpoint: Endpoint, token: Option<Secret>) -> Exchange<'_> {
            if matches!(endpoint, Endpoint::Refresh(_)) {
                self.entered.notify_one();
                Box::pin(std::future::pending())
            } else {
                self.fake.exchange(endpoint, token)
            }
        }
    }
    let fake = Arc::new(Fake::default());
    fake.reply(200, auth("one"));
    let transport = Arc::new(PendingRefresh {
        fake: fake.clone(),
        entered: tokio::sync::Notify::new(),
    });
    let vault = Arc::new(FakeVault::default());
    let client = Arc::new(CloudClient::build(transport.clone(), Some(vault.clone())));
    client
        .login("user@example.com", "synthetic-password")
        .await
        .unwrap();
    client
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    let task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .device_status("installation-example", "device-example")
                .await
        }
    });
    transport.entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
    assert!(vault.stored.lock().unwrap().is_none());
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn forbidden_and_rate_limits_are_visible_without_token_refresh_or_retry() {
    let (client, fake) = reader().await;
    for (status, error) in [
        (403, Error::Forbidden),
        (429, Error::RateLimited),
        (500, Error::Unavailable),
    ] {
        fake.reply(status, Value::Null);
        assert_eq!(
            client
                .device_status("installation-example", "device-example")
                .await
                .unwrap_err(),
            error
        );
    }
    assert_eq!(fake.paths().len(), 4);
}

#[tokio::test]
async fn migration_pending_is_not_authenticated_inventory_access() {
    let fake = Arc::new(Fake::default());
    let mut value = auth("one");
    value["pendingMigration"] = json!(true);
    fake.reply(200, value);
    let client = CloudClient::with_transport(fake.clone());
    assert_eq!(
        client
            .login("user@example.com", "synthetic-password")
            .await
            .unwrap_err(),
        Error::MigrationPending
    );
    assert_eq!(client.auth_status().await, AuthStatus::MigrationPending);
    assert_eq!(
        client.installations().await.unwrap_err(),
        Error::AuthenticationRequired
    );
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn production_without_vault_refuses_login_before_any_network_call() {
    let fake = Arc::new(Fake::default());
    let vault = Arc::new(FakeVault::default());
    vault.unavailable.store(true, Ordering::SeqCst);
    let client = CloudClient::build(fake.clone(), Some(vault));
    assert_eq!(
        client
            .login("user@example.com", "synthetic-password")
            .await
            .unwrap_err(),
        Error::Storage
    );
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn session_restores_without_network_and_rotation_is_saved_before_use() {
    let fake = Arc::new(Fake::default());
    let vault = Arc::new(FakeVault::default());
    fake.reply(200, auth("one"));
    let client = CloudClient::build(fake.clone(), Some(vault.clone()));
    client
        .login("user@example.com", "synthetic-password")
        .await
        .unwrap();
    let stored = vault.stored.lock().unwrap().clone().unwrap();
    assert!(!stored.contains("synthetic-password") && !stored.contains("user@example.com"));
    let restored = CloudClient::build(fake.clone(), Some(vault.clone()));
    assert!(matches!(
        restored.auth_status().await,
        AuthStatus::Connected { .. }
    ));
    assert_eq!(fake.paths().len(), 1);
    restored
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    fake.reply(200, auth("two"));
    fake.reply(200, json!({"power":true}));
    restored
        .device_status("installation-example", "device-example")
        .await
        .unwrap();
    let stored = vault.stored.lock().unwrap().clone().unwrap();
    assert!(stored.contains("refresh-two") && !stored.contains("refresh-one"));
    restored.disconnect().await.unwrap();
    assert!(vault.stored.lock().unwrap().is_none());
    assert_eq!(restored.auth_status().await, AuthStatus::Disconnected);
}

#[tokio::test]
async fn persistence_failure_never_releases_a_new_access_token() {
    let fake = Arc::new(Fake::default());
    let vault = Arc::new(FakeVault::default());
    fake.reply(200, auth("one"));
    let client = CloudClient::build(fake.clone(), Some(vault.clone()));
    client
        .login("user@example.com", "synthetic-password")
        .await
        .unwrap();
    client
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    vault.fail_save.store(true, Ordering::SeqCst);
    fake.reply(200, auth("two"));
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::Storage
    );
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
    assert!(vault.stored.lock().unwrap().is_none());
    assert_eq!(fake.paths().len(), 2);
}

#[tokio::test]
async fn failed_vault_clear_prevents_refresh_request() {
    let fake = Arc::new(Fake::default());
    let vault = Arc::new(FakeVault::default());
    fake.reply(200, auth("one"));
    let client = CloudClient::build(fake.clone(), Some(vault.clone()));
    client
        .login("user@example.com", "synthetic-password")
        .await
        .unwrap();
    client
        .state
        .lock()
        .await
        .session
        .as_mut()
        .unwrap()
        .expires_at = unix_now();
    vault.fail_clear.store(true, Ordering::SeqCst);
    assert_eq!(
        client
            .device_status("installation-example", "device-example")
            .await
            .unwrap_err(),
        Error::Storage
    );
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn failed_disconnect_does_not_report_persisted_logout_as_successful() {
    let fake = Arc::new(Fake::default());
    let vault = Arc::new(FakeVault::default());
    fake.reply(200, auth("one"));
    let client = CloudClient::build(fake.clone(), Some(vault.clone()));
    client
        .login("user@example.com", "synthetic-password")
        .await
        .unwrap();
    vault.fail_clear.store(true, Ordering::SeqCst);
    assert_eq!(client.disconnect().await.unwrap_err(), Error::Storage);
    assert_eq!(
        client.auth_status().await,
        AuthStatus::AuthenticationRequired
    );
    assert!(vault.stored.lock().unwrap().is_some());
    assert_eq!(fake.paths().len(), 1);
    vault.fail_clear.store(false, Ordering::SeqCst);
    client.disconnect().await.unwrap();
    assert_eq!(client.auth_status().await, AuthStatus::Disconnected);
    assert!(vault.stored.lock().unwrap().is_none());
}

#[test]
fn secrets_are_redacted_and_null_unknown_and_invalid_readings_stay_distinct() {
    assert_eq!(
        format!("{:?}", Secret("fictional-secret".into())),
        "[redacted]"
    );
    assert_eq!(format!("{:?}", CloudClient::new()), "CloudClient { .. }");
    let value = DeviceDocument::parse(
        json!({"power":null,"local_temp":{"celsius":null,"fah":71},"mode":"bad"}),
    )
    .unwrap();
    assert_eq!(value.power().unwrap(), None);
    assert_eq!(value.connected().unwrap(), None);
    assert_eq!(value.room_temperature().unwrap().unwrap().celsius, None);
    assert_eq!(value.mode().unwrap_err(), Error::InvalidResponse);
    assert!(value.fields()["power"].is_null());
    assert_eq!(
        DeviceDocument::parse(json!({"token":"must-not-be-published"})).unwrap_err(),
        Error::InvalidResponse
    );
    assert_eq!(
        DeviceDocument::parse(json!({"values":vec![0;513]})).unwrap_err(),
        Error::InvalidResponse
    );
    assert_eq!(jwt_expiry(&token("fictional")).unwrap(), 4_102_444_800);
    assert!(jwt_expiry("bad.payload.signature").is_err());
    let mut invalid = auth("fictional");
    invalid["refreshToken"] = json!("..");
    assert!(matches!(
        session(invalid, unix_now()),
        Err(Error::InvalidResponse)
    ));
}

#[tokio::test]
async fn installation_location_and_profile_models_match_official_shapes() {
    let (client, fake) = reader().await;
    fake.reply(200,json!({"installation_id":"installation-example","location_id":"location-example","name":"Example","access_type":"admin","groups":[{"group_id":"group-example","name":"Example","devices":[{"device_id":"device-example","name":"Example","type":"az_zone","ws_id":"gateway-example","meta":{"system_number":1,"zone_number":2}}]}]}));
    let value = client.installation("installation-example").await.unwrap();
    assert_eq!(value.groups[0].devices[0].kind, "az_zone");
    fake.reply(200,json!({"_id":"location-example","timezoneId":"Europe/Paris","coords":{"lat":45.0,"lng":3.0},"text":{}}));
    assert_eq!(
        client
            .location("location-example")
            .await
            .unwrap()
            .timezone
            .as_deref(),
        Some("Europe/Paris")
    );
    fake.reply(
        200,
        json!({"_id":"user-example","email":"user@example.com","data":{},"config":{"units":0}}),
    );
    assert_eq!(client.profile().await.unwrap().config["units"], 0);
}

fn installation_with_optional_device_names() -> Value {
    json!({
        "installation_id": "installation-example",
        "location_id": "location-example",
        "name": "Example installation",
        "access_type": "basic",
        "groups": [{
            "group_id": "group-example",
            "name": "Example group",
            "devices": [
                {"device_id":"system-example","type":"az_system","ws_id":"gateway-example"},
                {"device_id":"zone-example","name":"Example zone","type":"az_zone","ws_id":"gateway-example"},
                {"device_id":"system-null","name":null,"type":"az_system","ws_id":"gateway-example"}
            ]
        }]
    })
}

#[tokio::test]
async fn installation_keeps_unnamed_devices_without_fabricating_names() {
    let (client, fake) = reader().await;
    fake.reply(200, installation_with_optional_device_names());
    let value = client.installation("installation-example").await.unwrap();
    let devices = &value.groups[0].devices;
    assert_eq!(devices.len(), 3);
    assert_eq!(devices[0].device_id, "system-example");
    assert_eq!(devices[1].device_id, "zone-example");
    assert_eq!(devices[2].device_id, "system-null");
    let published = serde_json::to_value(value).unwrap();
    assert!(published["groups"][0]["devices"][0]["name"].is_null());
    assert_eq!(published["groups"][0]["devices"][1]["name"], "Example zone");
    assert!(published["groups"][0]["devices"][2]["name"].is_null());
}

#[tokio::test]
async fn installation_rejects_malformed_names_and_required_identifiers() {
    let (client, fake) = reader().await;
    for bad_name in [json!(7), json!(false), json!([]), json!({})] {
        let mut document = installation_with_optional_device_names();
        document["groups"][0]["devices"][1]["name"] = bad_name;
        fake.reply(200, document);
        assert_eq!(
            client
                .installation("installation-example")
                .await
                .unwrap_err(),
            Error::InvalidResponse
        );
    }
    for key in ["device_id", "ws_id", "type"] {
        for missing in [false, true] {
            let mut document = installation_with_optional_device_names();
            let device = document["groups"][0]["devices"][1].as_object_mut().unwrap();
            if missing {
                device.remove(key);
            } else {
                device.insert(key.into(), Value::Null);
            }
            fake.reply(200, document);
            assert_eq!(
                client
                    .installation("installation-example")
                    .await
                    .unwrap_err(),
                Error::InvalidResponse
            );
        }
    }
}

#[tokio::test]
async fn installation_still_checks_identity_when_device_names_are_absent() {
    let (client, fake) = reader().await;
    let mut document = installation_with_optional_device_names();
    document["installation_id"] = json!("another-installation");
    fake.reply(200, document);
    assert_eq!(
        client
            .installation("installation-example")
            .await
            .unwrap_err(),
        Error::InvalidResponse
    );
    let mut document = installation_with_optional_device_names();
    document["groups"][0]["devices"][0]["device_id"] = json!("../invalid-device");
    fake.reply(200, document);
    assert_eq!(
        client
            .installation("installation-example")
            .await
            .unwrap_err(),
        Error::InvalidInput
    );
}

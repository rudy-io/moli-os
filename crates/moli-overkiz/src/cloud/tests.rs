use super::protocol::{self, Endpoint, form};
use crate::inventory::{CollectionKind, CollectionStatus as S, Region};

#[test]
fn closed_routes_allow_only_oauth_posts_directory_and_exact_reads() {
    for region in [Region::Emea, Region::Apac, Region::Snaba] {
        let expected = match region {
            Region::Emea => "ha101-1.overkiz.com",
            Region::Apac => "ha201-1.overkiz.com",
            Region::Snaba => "ha401-1.overkiz.com",
        };
        for (kind, suffix) in CollectionKind::ALL.into_iter().zip([
            "actionGroups",
            "setup/calendar/days",
            "setup/calendar/rules",
            "triggers",
            "exec/schedule",
            "exec/delayedTriggerSchedule",
        ]) {
            assert_eq!(
                Endpoint::Collection(kind).route(region),
                (
                    "GET",
                    expected,
                    format!("/enduser-mobile-web/enduserAPI/{suffix}").as_str()
                )
            );
        }
        for (endpoint, suffix) in [
            (Endpoint::Location, "setup/location"),
            (Endpoint::Devices, "setup/devices"),
            (Endpoint::Gateways, "setup/gateways"),
        ] {
            assert_eq!(
                endpoint.route(region),
                (
                    "GET",
                    expected,
                    format!("/enduser-mobile-web/enduserAPI/{suffix}").as_str()
                )
            );
        }
        assert_eq!(
            Endpoint::Accounts.route(region),
            ("POST", "accounts.somfy.com", "/oauth/oauth/v2/token/jwt")
        );
        for endpoint in [Endpoint::Exchange, Endpoint::Refresh] {
            assert_eq!(
                endpoint.route(region),
                (
                    "POST",
                    "ginaite-prod.ovkube.net",
                    "/realms/somfy-tahoma/protocol/openid-connect/token"
                )
            );
        }
        assert_eq!(
            Endpoint::Directory.route(region),
            (
                "GET",
                "backoffice-service.ovkube.net",
                "/site-api/public/v1/sites"
            )
        );
    }
}

#[test]
fn forms_encode_utf8_plus_ampersand_percent_and_spaces_without_injection() {
    assert_eq!(
        form(&[("username", "été +&% ="), ("password", "a?b/雪")]),
        "username=%C3%A9t%C3%A9+%2B%26%25+%3D&password=a%3Fb%2F%E9%9B%AA"
    );
}

use super::client::CloudClient;
use super::protocol::{Request, Response, Transport};
use super::session::SessionSink;
use super::{
    Config, ConnectionState, Error, PendingRequest, RequestEnvelope, decode_request, decode_state,
    encode_request, encode_state,
};
use moli_runtime::BoxFuture;
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default, Debug)]
struct Trace(Arc<Mutex<Vec<String>>>);
impl Trace {
    fn push(&self, s: String) {
        self.0.lock().unwrap().push(s);
    }
    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}
struct Script {
    replies: VecDeque<Result<Response, Error>>,
    trace: Trace,
    delay: std::time::Duration,
}
impl Transport for Script {
    fn send(&mut self, request: Request) -> BoxFuture<'_, Result<Response, Error>> {
        self.trace.push(format!(
            "request:{:?}:{}:{}:{}",
            request.endpoint,
            request.bearer.as_deref().unwrap_or("none"),
            request.body,
            request.path().expect("closed request path")
        ));
        let response = self
            .replies
            .pop_front()
            .expect("unexpected provider request");
        let delay = self.delay;
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            response
        })
    }
}

#[path = "scope_tests.rs"]
mod scope_tests;
#[derive(Default)]
struct Sink {
    saved: Vec<Vec<u8>>,
    fail_at: Option<usize>,
    stall_at: Option<usize>,
    delay: std::time::Duration,
    calls: usize,
    trace: Trace,
}
impl SessionSink for Sink {
    fn save<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), Error>> {
        self.calls += 1;
        self.trace
            .push(format!("save:{}", String::from_utf8_lossy(bytes)));
        let fail = self.fail_at == Some(self.calls);
        let stall = self.stall_at == Some(self.calls);
        let delay = self.delay;
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if stall {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            }
            if fail {
                Err(Error::Persistence)
            } else {
                self.saved.push(bytes.to_vec());
                Ok(())
            }
        })
    }
}
#[allow(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "Script entries mix fabricated responses with transport errors."
)]
fn response(status: u16, body: serde_json::Value) -> Result<Response, Error> {
    Ok(Response {
        status,
        retry_after: None,
        body: serde_json::to_vec(&body).unwrap(),
    })
}
fn token(access: &str, refresh: &str) -> Result<Response, Error> {
    response(
        200,
        json!({"access_token":access,"refresh_token":refresh,"expires_in":300}),
    )
}
fn directory(country: &str) -> Result<Response, Error> {
    response(
        200,
        json!({"totalCount":1,"results":[{"siteOID":"parent","name":"Private synthetic label","country":country,"subSites":[{"externalOID":"setup","gateways":[{"gatewayId":"anchor"},{"gatewayId":"second"}]}]}]}),
    )
}
fn client(replies: Vec<Result<Response, Error>>) -> (CloudClient<Script>, Sink, Trace) {
    let trace = Trace::default();
    (
        CloudClient::with_transport(
            "synthetic",
            Script {
                replies: replies.into(),
                trace: trace.clone(),
                delay: std::time::Duration::ZERO,
            },
        )
        .unwrap(),
        Sink {
            trace: trace.clone(),
            ..Sink::default()
        },
        trace,
    )
}
async fn login(c: &mut CloudClient<Script>, s: &mut Sink) {
    c.login("user@example.com".into(), "PASSWORD_SENTINEL".into(), s)
        .await
        .unwrap();
}
async fn select(
    c: &mut CloudClient<Script>,
    s: &mut Sink,
    region: Option<Region>,
) -> Result<(), Error> {
    let generation = c.state().generation.unwrap();
    c.select_site(&generation, "setup", "anchor", region, s)
        .await
}
#[test]
fn request_and_state_are_bounded_and_debug_redacts_private_values() {
    let envelope = RequestEnvelope {
        version: 1,
        request_id: "request".into(),
        submitted_at_ms: 1,
        request: PendingRequest::Login {
            username: "user@example.com".into(),
            password: "PASSWORD_SENTINEL".into(),
        },
    };
    let bytes = encode_request(&envelope).unwrap();
    assert!(decode_request(&bytes).is_ok());
    assert!(!format!("{envelope:?}").contains("SENTINEL"));
    assert!(decode_request(&vec![b' '; super::MAX_PENDING_BYTES + 1]).is_err());
    assert!(
        decode_request(
            br#"{"version":1,"request_id":"r","submitted_at_ms":0,"request":{"kind":"logout"}}"#
        )
        .is_err()
    );
    assert!(Config::default().validate().is_ok());
    for poll in [0, 299, 86401] {
        assert!(Config { poll_secs: poll }.validate().is_err());
    }
    let state = ConnectionState::default();
    assert!(decode_state(&encode_state(&state).unwrap()).is_ok());
    assert!(decode_state(&vec![b' '; super::MAX_STATE_BYTES + 1]).is_err());
}
#[tokio::test]
async fn login_discovers_whole_setup_but_never_selects_first_site_or_persists_password() {
    let (mut c, mut s, trace) = client(vec![
        token("account", "unused"),
        token("unscoped", "r1"),
        directory("FR"),
    ]);
    login(&mut c, &mut s).await;
    let state = c.state();
    assert!(state.scope.is_none());
    assert_eq!(state.candidates.len(), 1);
    assert_eq!(state.candidates[0].site_oid, "setup");
    assert_eq!(state.candidates[0].gateway_ids, vec!["anchor", "second"]);
    assert_eq!(state.candidates[0].region, Some(Region::Emea));
    assert!(
        s.saved
            .iter()
            .all(|v| !String::from_utf8_lossy(v).contains("PASSWORD_SENTINEL"))
    );
    assert!(!format!("{state:?}").contains("Private"));
    let entries = trace.entries();
    assert!(entries[1].contains("username=user%40example.com&password=PASSWORD_SENTINEL"));
    assert!(entries[2].contains("subject_token=account"));
    assert!(entries[0].contains("login_required"));
    assert!(entries[3].starts_with("save:"));
    assert!(entries[4].starts_with("request:Directory:unscoped"));
}
#[tokio::test]
async fn refresh_checkpoints_then_saves_rotation_before_gateway_or_inventory_reads() {
    let (mut c, mut s, trace) = client(vec![
        token("account", "unused"),
        token("unscoped", "r1"),
        directory("FR"),
        token("scoped", "r2"),
        response(200, json!([{"gatewayId":"anchor"},{"gatewayId":"second"}])),
    ]);
    login(&mut c, &mut s).await;
    select(&mut c, &mut s, None).await.unwrap();
    let entries = trace.entries();
    let post = entries
        .iter()
        .position(|v| v.starts_with("request:Refresh"))
        .unwrap();
    assert!(entries[post - 1].contains("\"phase\":\"refreshing\""));
    assert!(entries[post + 1].contains("\"refresh\":\"r2\""));
    assert!(entries[post + 2].starts_with("request:Gateways:scoped"));
    assert_eq!(c.state().scope.unwrap().gateway_id, "anchor");
    assert!(c.identity().is_ok());
}
#[tokio::test]
async fn rejects_known_region_override_unknown_region_guess_and_stale_generation() {
    let (mut c, mut s, _) = client(vec![token("a", "u"), token("u", "r1"), directory("FR")]);
    login(&mut c, &mut s).await;
    assert_eq!(
        select(&mut c, &mut s, Some(Region::Apac)).await,
        Err(Error::ScopeMismatch)
    );
    assert_eq!(
        c.select_site("stale", "setup", "anchor", None, &mut s)
            .await,
        Err(Error::ScopeMismatch)
    );
    let (mut c, mut s, _) = client(vec![token("a", "u"), token("u", "r1"), directory("ZZ")]);
    login(&mut c, &mut s).await;
    assert_eq!(
        select(&mut c, &mut s, None).await,
        Err(Error::ScopeMismatch)
    );
}
#[tokio::test]
async fn lost_refresh_response_and_restarting_checkpoint_never_replay_old_token() {
    let (mut c, mut s, trace) = client(vec![
        token("a", "u"),
        token("u", "r1"),
        directory("FR"),
        Err(Error::Network),
    ]);
    login(&mut c, &mut s).await;
    assert_eq!(
        select(&mut c, &mut s, None).await,
        Err(Error::LoginRequired)
    );
    assert_eq!(
        select(&mut c, &mut s, None).await,
        Err(Error::LoginRequired)
    );
    assert_eq!(
        trace
            .entries()
            .iter()
            .filter(|v| v.starts_with("request:Refresh"))
            .count(),
        1
    );
    let checkpoint = s
        .saved
        .iter()
        .find(|b| String::from_utf8_lossy(b).contains("\"phase\":\"refreshing\""))
        .unwrap();
    let (mut restart, mut sink, t) = client(vec![]);
    restart.restore(checkpoint).unwrap();
    assert_eq!(restart.refresh(&mut sink).await, Err(Error::LoginRequired));
    assert!(t.entries().is_empty());
}

fn scoped_prefix() -> Vec<Result<Response, Error>> {
    vec![
        token("a", "unused"),
        token("u", "r1"),
        directory("FR"),
        token("s", "r2"),
        response(200, json!([{"gatewayId":"anchor"}])),
    ]
}
async fn scoped(replies: Vec<Result<Response, Error>>) -> (CloudClient<Script>, Sink, Trace) {
    let mut all = scoped_prefix();
    all.extend(replies);
    let (mut c, mut s, t) = client(all);
    login(&mut c, &mut s).await;
    select(&mut c, &mut s, None).await.unwrap();
    (c, s, t)
}
fn gate() -> Result<Response, Error> {
    response(200, json!([{"gatewayId":"anchor"}]))
}
fn cycle_empty() -> Vec<Result<Response, Error>> {
    let mut v = vec![gate()];
    for _ in CollectionKind::ALL {
        v.push(response(200, json!([])));
    }
    v.push(response(
        200,
        json!({"timezone":"Europe/Paris","address":"PRIVATE"}),
    ));
    v.push(response(200, json!([])));
    v
}
fn catalog(c: &CloudClient<Script>) -> crate::inventory::Catalog {
    crate::inventory::Catalog::new(c.identity().unwrap()).unwrap()
}

#[tokio::test]
async fn new_login_generation_is_persisted_even_when_bootstrap_is_rejected() {
    let (mut c, mut s, t) = scoped(vec![response(401, json!({"error":"PRIVATE_SENTINEL"}))]).await;
    let old = c.state().generation.unwrap();
    assert_eq!(
        c.login("user@example.com".into(), "new password".into(), &mut s)
            .await,
        Err(Error::LoginRequired)
    );
    assert_ne!(c.state().generation.unwrap(), old);
    assert!(String::from_utf8_lossy(s.saved.last().unwrap()).contains("login_required"));
    assert!(t.entries().last().unwrap().starts_with("request:Accounts"));
}
#[tokio::test]
async fn save_failure_retries_only_replacement_and_never_replays_spent_refresh() {
    let (mut c, mut s, t) = scoped(vec![token("new", "r3")]).await;
    s.fail_at = Some(s.calls + 2);
    assert_eq!(c.refresh(&mut s).await, Err(Error::Persistence));
    let calls = t
        .entries()
        .iter()
        .filter(|v| v.starts_with("request:"))
        .count();
    c.retry_persistence(&mut s).await.unwrap();
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:"))
            .count(),
        calls
    );
    assert!(String::from_utf8_lossy(s.saved.last().unwrap()).contains("\"refresh\":\"r3\""));
    let trace = t.entries();
    let pos = trace
        .iter()
        .rposition(|v| v.starts_with("request:Refresh"))
        .unwrap();
    assert!(trace[pos].contains("refresh_token=r2"));
    assert!(trace[pos + 1].contains("r3"));
    assert!(trace[pos + 2].contains("r3"));
}
#[tokio::test]
async fn failed_checkpoint_ack_sends_no_refresh_and_cannot_be_retried_as_old_session() {
    let (mut c, mut s, t) = scoped(vec![]).await;
    s.fail_at = Some(s.calls + 1);
    assert_eq!(c.refresh(&mut s).await, Err(Error::Persistence));
    c.retry_persistence(&mut s).await.unwrap();
    assert_eq!(c.refresh(&mut s).await, Err(Error::LoginRequired));
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Refresh"))
            .count(),
        1
    );
}
#[tokio::test]
async fn partial_token_response_preserves_replacement_before_returning_error() {
    for body in [
        json!({"refresh_token":"r3"}),
        json!({"refresh_token":"r3","access_token":"new","expires_in":0}),
        json!({"refresh_token":"r3","access_token":"new","expires_in":"invalid"}),
    ] {
        let (mut c, mut s, t) = scoped(vec![response(200, body), token("recovered", "r4")]).await;
        assert_eq!(c.refresh(&mut s).await, Err(Error::InvalidResponse));
        assert!(String::from_utf8_lossy(s.saved.last().unwrap()).contains("\"refresh\":\"r3\""));
        c.refresh(&mut s).await.unwrap();
        assert!(
            t.entries()
                .iter()
                .rfind(|v| v.starts_with("request:Refresh"))
                .unwrap()
                .contains("refresh_token=r3")
        );
    }
}
#[tokio::test]
async fn absent_refresh_is_retained_only_for_otherwise_valid_response() {
    let (mut c, mut s, _) = scoped(vec![response(200, json!({"access_token":"new"}))]).await;
    c.refresh(&mut s).await.unwrap();
    let saved: serde_json::Value = serde_json::from_slice(s.saved.last().unwrap()).unwrap();
    assert_eq!(saved["refresh"], "r2");
    assert_eq!(
        saved["expires_at_ms"].as_i64().unwrap() - saved["issued_at_ms"].as_i64().unwrap(),
        300_000
    );
    let (mut c, mut s, _) = scoped(vec![response(200, json!({"expires_in":300}))]).await;
    assert_eq!(c.refresh(&mut s).await, Err(Error::InvalidResponse));
    let saved: serde_json::Value = serde_json::from_slice(s.saved.last().unwrap()).unwrap();
    assert_eq!(saved["refresh"], serde_json::Value::Null);
    assert_eq!(c.refresh(&mut s).await, Err(Error::LoginRequired));
}
#[tokio::test]
async fn unauthorized_read_retries_once_but_forbidden_and_missing_collections_are_independent() {
    let (mut c, mut s, t) = scoped(vec![
        gate(),
        response(401, json!({})),
        token("new", "r3"),
        response(401, json!({})),
        response(403, json!({})),
        response(404, json!({})),
        response(405, json!({})),
        response(204, json!(null)),
        response(200, json!([])),
        response(200, json!({"timezone":"Europe/Paris"})),
        response(200, json!([])),
    ])
    .await;
    let mut inventory = catalog(&c);
    assert!(c.read_cycle(&mut inventory, &mut s).await.is_err());
    for (kind, status) in CollectionKind::ALL.into_iter().zip([
        S::Unauthorized,
        S::Forbidden,
        S::Unsupported,
        S::Unsupported,
        S::InvalidResponse,
        S::Available,
    ]) {
        assert_eq!(inventory.collections[&kind].status, status);
    }
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Refresh"))
            .count(),
        2
    );
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Collection(Scenarios)"))
            .count(),
        2
    );
}
#[tokio::test]
async fn rate_limit_preserves_previous_success_and_stops_further_network_until_deadline() {
    let mut reads = cycle_empty();
    reads.push(gate());
    reads.push(Ok(Response {
        status: 429,
        retry_after: Some("120".into()),
        body: b"PRIVATE_UPSTREAM_BODY".to_vec(),
    }));
    let (mut c, mut s, t) = scoped(reads).await;
    let mut inventory = catalog(&c);
    c.read_cycle(&mut inventory, &mut s).await.unwrap();
    assert!(c.read_cycle(&mut inventory, &mut s).await.is_err());
    let scenarios = &inventory.collections[&CollectionKind::Scenarios];
    assert!(scenarios.data.is_some());
    assert!(scenarios.last_observation_ms.is_some());
    assert_eq!(
        scenarios.failure,
        Some(crate::inventory::CollectionFailure::RateLimited {
            retry_after_secs: Some(120)
        })
    );
    let before = t.entries().len();
    assert!(matches!(
        c.read_cycle(&mut inventory, &mut s).await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(t.entries().len(), before);
}
#[test]
fn retry_after_validates_http_dates_and_caps_advertised_seconds() {
    use super::client::retry_after;
    let now = 784_111_777_000; // Sun, 06 Nov 1994 08:49:37 GMT
    assert_eq!(retry_after(Some("Sun, 06 Nov 1994 08:51:37 GMT"), now), 120);
    assert_eq!(retry_after(Some("Sun, 06 Nov 1994 08:00:00 GMT"), now), 0);
    assert_eq!(retry_after(Some("999999999"), now), 3600);
    for value in ["Sunday, 06-Nov-94 08:51:37 GMT", "Sun Nov  6 08:51:37 1994"] {
        assert_eq!(retry_after(Some(value), now), 120);
    }
    assert_eq!(
        retry_after(Some("Mon, 07 Nov 1994 08:49:37 GMT"), now),
        3600
    );
    for value in ["-1", "garbage", "Sun, 99 Nov 1994 08:51:37 GMT", "1.5"] {
        assert_eq!(retry_after(Some(value), now), 60);
    }
}
#[tokio::test]
async fn selected_anchor_must_exist_in_scoped_gateways_before_catalog_identity() {
    let (mut c, mut s, _) = client(vec![
        token("a", "unused"),
        token("u", "r1"),
        directory("FR"),
        token("s", "r2"),
        response(200, json!([{"gatewayId":"wrong"}])),
    ]);
    login(&mut c, &mut s).await;
    assert_eq!(
        select(&mut c, &mut s, None).await,
        Err(Error::ScopeMismatch)
    );
    assert!(c.identity().is_err());
}
#[tokio::test]
async fn oversized_token_body_redirect_and_upstream_text_are_never_forwarded_or_retried() {
    for response in [
        Ok(Response {
            status: 200,
            retry_after: None,
            body: vec![b'x'; protocol::TOKEN_LIMIT + 1],
        }),
        response(
            302,
            json!({"location":"https://evil.invalid","error":"PRIVATE_SENTINEL"}),
        ),
        response(401, json!({"error":"PRIVATE_SENTINEL"})),
    ] {
        let (mut c, mut s, t) = client(vec![response]);
        let error = c
            .login("user@example.com".into(), "password".into(), &mut s)
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("PRIVATE_SENTINEL"));
        assert_eq!(
            t.entries()
                .iter()
                .filter(|v| v.starts_with("request:"))
                .count(),
            1
        );
    }
}
#[tokio::test]
async fn pagination_collects_second_page_and_rejects_overflow_or_incomplete_directory() {
    let site = |i: usize| json!({"siteOID":format!("parent{i}"),"country":"FR","subSites":[{"externalOID":format!("setup{i}"),"gateways":[{"gatewayId":format!("gateway{i}")}]}]});
    let mut replies = vec![
        token("a", "unused"),
        token("u", "r1"),
        response(
            200,
            json!({"results":(0..20).map(site).collect::<Vec<_>>(),"totalCount":21}),
        ),
        response(200, json!({"results":[site(20)],"totalCount":21})),
    ];
    let (mut c, mut s, t) = client(std::mem::take(&mut replies));
    login(&mut c, &mut s).await;
    assert_eq!(c.state().candidates.len(), 21);
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Directory"))
            .count(),
        2
    );
    for page in [
        json!({"results":[site(0)],"totalCount":201}),
        json!({"results":[site(0)],"totalCount":21}),
        json!({"results":(0..21).map(site).collect::<Vec<_>>(),"totalCount":21}),
    ] {
        let (mut c, mut s, _) = client(vec![
            token("a", "unused"),
            token("u", "r1"),
            response(200, page),
        ]);
        assert!(
            c.login("user@example.com".into(), "p".into(), &mut s)
                .await
                .is_err()
        );
        assert!(c.state().candidates.is_empty());
    }
}
#[tokio::test(start_paused = true)]
async fn every_transport_future_has_ten_second_deadline_and_is_send() {
    fn assert_send<T: Send>(_: T) {}
    let trace = Trace::default();
    let mut c = CloudClient::with_transport(
        "synthetic",
        Script {
            replies: vec![token("a", "r")].into(),
            trace,
            delay: std::time::Duration::from_secs(20),
        },
    )
    .unwrap();
    let mut s = Sink::default();
    let before = tokio::time::Instant::now();
    assert_eq!(
        c.login("user@example.com".into(), "p".into(), &mut s).await,
        Err(Error::Deadline)
    );
    assert_eq!(before.elapsed(), std::time::Duration::from_secs(10));
    assert_send(c.login("user@example.com".into(), "p".into(), &mut s));
}

#[tokio::test(start_paused = true)]
async fn cycle_has_ninety_second_total_budget_and_records_interrupted_collection() {
    let (c, s, _) = scoped(vec![]).await;
    let identity = c.identity().unwrap();
    let saved = s.saved.last().unwrap();
    let trace = Trace::default();
    let mut replies = vec![gate()];
    for i in 0..6 {
        replies.push(response(401, json!({})));
        replies.push(token("new", &format!("r{i}")));
        replies.push(response(200, json!([])));
    }
    let mut delayed = CloudClient::with_transport(
        "synthetic",
        Script {
            replies: replies.into(),
            trace: trace.clone(),
            delay: std::time::Duration::from_secs(9),
        },
    )
    .unwrap();
    delayed.restore(saved).unwrap();
    let mut sink = Sink::default();
    let mut inventory = crate::inventory::Catalog::new(identity).unwrap();
    let before = tokio::time::Instant::now();
    assert_eq!(
        delayed.read_cycle(&mut inventory, &mut sink).await,
        Err(Error::Deadline)
    );
    assert_eq!(before.elapsed(), std::time::Duration::from_secs(90));
    assert_eq!(
        inventory.collections[&CollectionKind::Scenarios].status,
        S::Available
    );
    assert_eq!(
        inventory.collections[&CollectionKind::Triggers].status,
        S::Unavailable
    );
}
#[tokio::test(start_paused = true)]
async fn cancellation_during_rotation_persistence_keeps_new_token_only_in_memory() {
    let (mut c, mut s, t) = scoped(vec![token("new", "r3")]).await;
    s.stall_at = Some(s.calls + 2);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), c.refresh(&mut s))
            .await
            .is_err()
    );
    let binding = super::read_session_binding(s.saved.last().unwrap(), "synthetic").unwrap();
    assert_eq!(binding.phase, super::SessionPhase::Refreshing);
    let calls = t
        .entries()
        .iter()
        .filter(|v| v.starts_with("request:"))
        .count();
    c.retry_persistence(&mut s).await.unwrap();
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:"))
            .count(),
        calls
    );
    assert!(String::from_utf8_lossy(s.saved.last().unwrap()).contains("\"refresh\":\"r3\""));
}
#[tokio::test(start_paused = true)]
async fn monotonic_expiry_refreshes_with_five_second_margin() {
    let mut replies = cycle_empty();
    replies.push(token("new", "r3"));
    replies.extend(cycle_empty());
    let (mut c, mut s, t) = scoped(replies).await;
    let mut inventory = catalog(&c);
    tokio::time::advance(std::time::Duration::from_secs(294)).await;
    c.read_cycle(&mut inventory, &mut s).await.unwrap();
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Refresh"))
            .count(),
        1
    );
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    c.read_cycle(&mut inventory, &mut s).await.unwrap();
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Refresh"))
            .count(),
        2
    );
}
#[tokio::test]
async fn public_session_binding_invalidates_old_generation_but_exposes_no_credentials() {
    let (mut c, mut s, _) = scoped(vec![response(401, json!({}))]).await;
    let old = c.identity().unwrap();
    assert!(
        c.login("user@example.com".into(), "p".into(), &mut s)
            .await
            .is_err()
    );
    let marker = s.saved.last().unwrap();
    let binding = super::read_session_binding(marker, "synthetic").unwrap();
    assert_ne!(binding.generation, old.connection_generation);
    assert!(binding.scope.is_none());
    assert_eq!(binding.phase, super::SessionPhase::LoginRequired);
    let encoded = serde_json::to_string(&binding).unwrap();
    for secret in ["refresh", "access", "password", "Private synthetic label"] {
        assert!(!encoded.contains(secret));
    }
    assert!(super::read_session_binding(marker, "different").is_err());
    assert!(
        super::read_session_binding(&vec![b' '; super::MAX_SESSION_BYTES + 1], "synthetic")
            .is_err()
    );
    let (mut restart, _, _) = client(vec![]);
    restart.restore(marker).unwrap();
    assert_eq!(restart.state().generation, Some(binding.generation));
    assert!(restart.identity().is_err());
}
#[tokio::test]
async fn failed_new_login_checkpoint_sends_zero_credentials() {
    let (mut c, mut s, t) = client(vec![]);
    s.fail_at = Some(1);
    assert_eq!(
        c.login("user@example.com".into(), "p".into(), &mut s).await,
        Err(Error::Persistence)
    );
    assert!(!t.entries().iter().any(|v| v.starts_with("request:")));
}
#[test]
fn protocol_query_is_closed_encoded_and_response_debug_is_redacted() {
    use super::protocol::Query;
    let request = |endpoint, query| Request {
        endpoint,
        query,
        region: Region::Emea,
        bearer: None,
        body: String::new(),
    };
    assert_eq!(
        request(Endpoint::Directory, Query::Offset(20))
            .path()
            .unwrap(),
        "/site-api/public/v1/sites?withGateways=true&limit=20&offset=20"
    );
    for query in [
        Query::Offset(1),
        Query::Offset(200),
        Query::Site("setup".into()),
        Query::None,
    ] {
        assert!(request(Endpoint::Directory, query).path().is_err());
    }
    assert_eq!(
        request(Endpoint::Refresh, Query::Site("été &+/?".into()))
            .path()
            .unwrap(),
        "/realms/somfy-tahoma/protocol/openid-connect/token?siteOID=%C3%A9t%C3%A9+%26%2B%2F%3F"
    );
    assert!(
        request(
            Endpoint::Collection(CollectionKind::Schedule),
            Query::Site("/logout".into())
        )
        .path()
        .is_err()
    );
    let response = Response {
        status: 400,
        retry_after: Some("PRIVATE_SENTINEL".into()),
        body: b"PRIVATE_SENTINEL".to_vec(),
    };
    assert!(!format!("{response:?}").contains("PRIVATE"));
    let request = Request {
        endpoint: Endpoint::Accounts,
        query: Query::None,
        region: Region::Emea,
        bearer: Some("PRIVATE_SENTINEL".into()),
        body: "PRIVATE_SENTINEL".into(),
    };
    assert!(!format!("{request:?}").contains("PRIVATE"));
}
#[tokio::test]
async fn metadata_replacement_is_atomic_and_failed_collection_retains_last_good() {
    let mut replies = cycle_empty();
    replies.push(gate());
    replies.push(response(200, json!({"error":"PRIVATE"})));
    for _ in 0..5 {
        replies.push(response(200, json!([])));
    }
    replies.push(response(200, json!({"timezone":"Asia/Tokyo"})));
    replies.push(response(200, json!({"error":"invalid devices"})));
    let (mut c, mut s, _) = scoped(replies).await;
    let mut inventory = catalog(&c);
    c.read_cycle(&mut inventory, &mut s).await.unwrap();
    assert!(c.read_cycle(&mut inventory, &mut s).await.is_err());
    assert_eq!(inventory.provider_timezone.as_deref(), Some("Europe/Paris"));
    let scenarios = &inventory.collections[&CollectionKind::Scenarios];
    assert_eq!(scenarios.status, S::InvalidResponse);
    assert!(scenarios.data.is_some());
    assert_eq!(scenarios.data.as_ref().unwrap().definitions.len(), 0);
    assert!(crate::inventory::encode_catalog(&inventory).is_ok());
}

#[tokio::test]
async fn quota_from_metadata_stops_following_reads_and_returns_qualified_failure() {
    let mut reads = vec![gate()];
    for _ in CollectionKind::ALL {
        reads.push(response(200, json!([])));
    }
    reads.push(Ok(Response {
        status: 429,
        retry_after: Some("180".into()),
        body: b"{}".to_vec(),
    }));
    let (mut c, mut s, t) = scoped(reads).await;
    let mut inventory = catalog(&c);
    assert_eq!(
        c.read_cycle(&mut inventory, &mut s).await,
        Err(Error::RateLimited {
            retry_after_secs: 180
        })
    );
    assert!(!t.entries().iter().any(|v| v.starts_with("request:Devices")));
    assert!(
        inventory
            .collections
            .values()
            .all(|v| v.status == S::Available)
    );
}
#[tokio::test(start_paused = true)]
async fn rotated_state_must_ack_before_any_later_collection_uses_new_access() {
    let mut replies = vec![token("new", "r3")];
    replies.extend(cycle_empty());
    let (mut c, mut s, t) = scoped(replies).await;
    let mut inventory = catalog(&c);
    s.fail_at = Some(s.calls + 2);
    assert_eq!(c.refresh(&mut s).await, Err(Error::Persistence));
    let pos = t.entries().len();
    s.fail_at = Some(s.calls + 1);
    assert_eq!(
        c.read_cycle(&mut inventory, &mut s).await,
        Err(Error::Persistence)
    );
    assert!(t.entries()[pos..].iter().all(|v| v.starts_with("save:")));
    c.retry_persistence(&mut s).await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    c.read_cycle(&mut inventory, &mut s).await.unwrap();
    let trace = t.entries();
    let read = trace
        .iter()
        .rposition(|v| v.starts_with("request:Gateways:new"))
        .unwrap();
    assert!(trace[read - 1].contains("\"refresh\":\"r3\""));
}
#[tokio::test]
async fn invalid_grant_refresh_and_invalid_token_strings_never_reuse_old_token() {
    for body in [
        response(
            400,
            json!({"error":"invalid_grant","error_description":"PRIVATE"}),
        ),
        response(
            200,
            json!({"access_token":"new","refresh_token":"bad\nheader"}),
        ),
        response(
            200,
            json!({"access_token":"x".repeat(protocol::TOKEN_STRING_LIMIT+1)}),
        ),
    ] {
        let (mut c, mut s, t) = scoped(vec![body]).await;
        assert!(c.refresh(&mut s).await.is_err());
        assert_eq!(c.refresh(&mut s).await, Err(Error::LoginRequired));
        assert_eq!(
            t.entries()
                .iter()
                .filter(|v| v.starts_with("request:Refresh"))
                .count(),
            2
        );
    }
}
#[tokio::test]
async fn restored_expired_or_rollback_wall_clock_forces_scoped_refresh_before_read() {
    let (c, s, _) = scoped(vec![]).await;
    let identity = c.identity().unwrap();
    let record: serde_json::Value = serde_json::from_slice(s.saved.last().unwrap()).unwrap();
    for (issued, expires) in [(1, 300_001), (i64::MAX - 300_001, i64::MAX - 1)] {
        let mut record = record.clone();
        record["issued_at_ms"] = json!(issued);
        record["expires_at_ms"] = json!(expires);
        let mut reads = vec![token("new", "r3")];
        reads.extend(cycle_empty());
        let (mut restart, mut sink, t) = client(reads);
        restart
            .restore(&serde_json::to_vec(&record).unwrap())
            .unwrap();
        let mut inventory = crate::inventory::Catalog::new(identity.clone()).unwrap();
        restart.read_cycle(&mut inventory, &mut sink).await.unwrap();
        assert!(
            t.entries()
                .iter()
                .find(|v| v.starts_with("request:"))
                .unwrap()
                .starts_with("request:Refresh")
        );
    }
}
#[tokio::test]
async fn gateway_entry_and_flattened_directory_limits_reject_whole_discovery() {
    let entries = |count: usize| {
        (0..count)
            .map(|i| json!({"gatewayId":format!("gateway{i}")}))
            .collect::<Vec<_>>()
    };
    for subs in [
        json!([{"externalOID":"one","gateways":entries(201)}]),
        json!([{"externalOID":"one","gateways":entries(101)},{"externalOID":"two","gateways":entries(100)}]),
    ] {
        let page =
            json!({"totalCount":1,"results":[{"siteOID":"parent","country":"FR","subSites":subs}]});
        let (mut c, mut s, _) = client(vec![
            token("a", "unused"),
            token("u", "r1"),
            response(200, page),
        ]);
        assert!(
            c.login("user@example.com".into(), "p".into(), &mut s)
                .await
                .is_err()
        );
        assert!(c.state().candidates.is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn advertised_retry_after_is_capped_to_one_hour_including_overflow() {
    for seconds in [86_400, u64::MAX] {
        let mut reads = vec![
            gate(),
            Ok(Response {
                status: 429,
                retry_after: Some(seconds.to_string()),
                body: b"{}".to_vec(),
            }),
        ];
        reads.extend(cycle_empty());
        let (mut c, mut sink, trace) = scoped(reads).await;
        let mut inventory = catalog(&c);
        assert_eq!(
            c.read_cycle(&mut inventory, &mut sink).await,
            Err(Error::RateLimited {
                retry_after_secs: 3600
            })
        );
        assert_eq!(c.state().retry_after_secs, Some(3600));
        tokio::time::advance(std::time::Duration::from_secs(3599)).await;
        let before = trace.entries().len();
        assert!(matches!(
            c.read_cycle(&mut inventory, &mut sink).await,
            Err(Error::RateLimited { .. })
        ));
        assert_eq!(trace.entries().len(), before);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert_eq!(c.state().retry_after_secs, None);
    }
}

#[tokio::test]
async fn zero_retry_after_still_stops_current_cycle_without_fanning_out() {
    let (mut c, mut s, t) = scoped(vec![
        gate(),
        Ok(Response {
            status: 429,
            retry_after: Some("0".into()),
            body: b"{}".to_vec(),
        }),
    ])
    .await;
    let mut inventory = catalog(&c);
    assert_eq!(
        c.read_cycle(&mut inventory, &mut s).await,
        Err(Error::RateLimited {
            retry_after_secs: 0
        })
    );
    assert_eq!(
        inventory.collections[&CollectionKind::CalendarDays].last_attempt_ms,
        None
    );
    assert_eq!(
        t.entries()
            .iter()
            .filter(|v| v.starts_with("request:Collection"))
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn fallback_backoff_doubles_to_one_hour_and_expires_without_early_retry() {
    let mut replies = scoped_prefix();
    replies[3] = response(
        200,
        json!({"access_token":"long","refresh_token":"r2","expires_in":86400}),
    );
    for _ in 0..8 {
        replies.push(gate());
        for _ in CollectionKind::ALL {
            replies.push(response(500, json!({})));
        }
        replies.push(response(200, json!({})));
        replies.push(response(200, json!([])));
    }
    let (mut c, mut s, t) = client(replies);
    login(&mut c, &mut s).await;
    select(&mut c, &mut s, None).await.unwrap();
    let mut inventory = catalog(&c);
    for delay in [60, 120, 240, 480, 960, 1920, 3600, 3600] {
        assert_eq!(
            c.read_cycle(&mut inventory, &mut s).await,
            Err(Error::Network)
        );
        assert_eq!(c.state().retry_after_secs, Some(delay));
        let before = t.entries().len();
        assert!(matches!(
            c.read_cycle(&mut inventory, &mut s).await,
            Err(Error::RateLimited { .. })
        ));
        assert_eq!(t.entries().len(), before);
        tokio::time::advance(std::time::Duration::from_secs(delay)).await;
        assert_eq!(c.state().retry_after_secs, None);
    }
}

#[tokio::test]
async fn duplicate_setup_entries_merge_gateways_into_one_explicit_choice() {
    let page = json!({"totalCount":1,"results":[{"siteOID":"parent","name":"Private synthetic label","country":"FR","subSites":[{"externalOID":"setup","gateways":[{"gatewayId":"anchor"}]},{"externalOID":"setup","gateways":[{"gatewayId":"second"},{"gatewayId":"anchor"}]}]}]});
    let (mut c, mut s, _) = client(vec![
        token("a", "unused"),
        token("u", "r1"),
        response(200, page),
    ]);
    login(&mut c, &mut s).await;
    let state = c.state();
    assert_eq!(state.candidates.len(), 1);
    assert_eq!(state.candidates[0].gateway_ids, vec!["anchor", "second"]);
    assert!(state.scope.is_none());
}

#[tokio::test]
async fn known_swedish_country_uses_pinned_emea_mapping() {
    let (mut c, mut s, _) = client(vec![
        token("a", "unused"),
        token("u", "r1"),
        directory("SE"),
    ]);
    login(&mut c, &mut s).await;
    assert_eq!(c.state().candidates[0].region, Some(Region::Emea));
}

async fn oauth_refresh_quota_scenario(seconds: u64) {
    let mut reads = cycle_empty();
    reads.extend([
        gate(),
        response(401, json!({})),
        Ok(Response {
            status: 429,
            retry_after: Some(seconds.to_string()),
            body: b"{}".to_vec(),
        }),
    ]);
    let (mut c, mut s, t) = scoped(reads).await;
    let mut inventory = catalog(&c);
    c.read_cycle(&mut inventory, &mut s).await.unwrap();
    let prior = inventory.clone();
    let before = t.entries().len();
    assert_eq!(
        c.read_cycle(&mut inventory, &mut s).await,
        Err(Error::RateLimited {
            retry_after_secs: seconds
        })
    );
    let scenario = &inventory.collections[&CollectionKind::Scenarios];
    assert_eq!(scenario.status, S::RateLimited);
    assert_eq!(
        scenario.failure,
        Some(crate::inventory::CollectionFailure::RateLimited {
            retry_after_secs: Some(seconds)
        })
    );
    assert_eq!(
        scenario.data,
        prior.collections[&CollectionKind::Scenarios].data
    );
    assert_eq!(
        scenario.last_observation_ms,
        prior.collections[&CollectionKind::Scenarios].last_observation_ms
    );
    for kind in CollectionKind::ALL.into_iter().skip(1) {
        assert_eq!(inventory.collections[&kind], prior.collections[&kind]);
    }
    let entries = t.entries();
    let reads = entries[before..]
        .iter()
        .filter(|v| v.starts_with("request:"))
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 3);
    assert!(reads[0].starts_with("request:Gateways:"));
    assert!(reads[1].starts_with("request:Collection(Scenarios):"));
    assert!(reads[2].starts_with("request:Refresh:"));
    assert_eq!(
        c.state().failure,
        Some(Error::RateLimited {
            retry_after_secs: seconds
        })
    );
    let saved = s.saved.last().unwrap().clone();
    assert_eq!(
        super::read_session_binding(&saved, "synthetic")
            .unwrap()
            .phase,
        super::SessionPhase::LoginRequired
    );
    tokio::time::advance(std::time::Duration::from_secs(seconds)).await;
    assert_eq!(c.refresh(&mut s).await, Err(Error::LoginRequired));
    assert_eq!(entries, t.entries());
    let (mut restart, mut sink, trace) = client(vec![]);
    restart.restore(&saved).unwrap();
    assert_eq!(restart.refresh(&mut sink).await, Err(Error::LoginRequired));
    assert!(trace.entries().is_empty());
}
#[tokio::test(start_paused = true)]
async fn review_oauth_refresh_zero_quota_stops_cycle_and_preserves_unattempted_history() {
    oauth_refresh_quota_scenario(0).await;
}
#[tokio::test(start_paused = true)]
async fn review_oauth_refresh_positive_quota_preserves_qualification_and_never_replays_token() {
    oauth_refresh_quota_scenario(120).await;
}

#[tokio::test(start_paused = true)]
async fn review_outer_login_timeout_is_recorded_in_connection_failure() {
    let mut replies = vec![token("account", "unused"), token("unscoped", "r1")];
    for page in 0..10 {
        let results=(page*20..page*20+20).map(|i|json!({"siteOID":format!("parent{i}"),"country":"FR","subSites":[{"externalOID":format!("setup{i}"),"gateways":[{"gatewayId":format!("gateway{i}")}]}]})).collect::<Vec<_>>();
        replies.push(response(200, json!({"totalCount":200,"results":results})));
    }
    let mut c = CloudClient::with_transport(
        "synthetic",
        Script {
            replies: replies.into(),
            trace: Trace::default(),
            delay: std::time::Duration::from_secs(9),
        },
    )
    .unwrap();
    let mut sink = Sink {
        delay: std::time::Duration::from_secs(9),
        ..Sink::default()
    };
    let before = tokio::time::Instant::now();
    assert_eq!(
        c.login("user@example.com".into(), "p".into(), &mut sink)
            .await,
        Err(Error::Deadline)
    );
    assert_eq!(before.elapsed(), std::time::Duration::from_secs(90));
    assert_eq!(c.state().failure, Some(Error::Deadline));
}
#[tokio::test(start_paused = true)]
async fn review_outer_selection_timeout_is_recorded_in_connection_failure() {
    let (mut seed, mut saved, _) = client(vec![
        token("account", "unused"),
        token("unscoped", "r1"),
        directory("FR"),
    ]);
    login(&mut seed, &mut saved).await;
    let short = || {
        response(
            200,
            json!({"access_token":"short","refresh_token":"new","expires_in":6}),
        )
    };
    let mut c = CloudClient::with_transport(
        "synthetic",
        Script {
            replies: vec![short(), short(), response(401, json!({})), short(), gate()].into(),
            trace: Trace::default(),
            delay: std::time::Duration::from_secs(9),
        },
    )
    .unwrap();
    c.restore(saved.saved.last().unwrap()).unwrap();
    let mut sink = Sink {
        delay: std::time::Duration::from_secs(9),
        ..Sink::default()
    };
    let before = tokio::time::Instant::now();
    assert_eq!(select(&mut c, &mut sink, None).await, Err(Error::Deadline));
    assert_eq!(before.elapsed(), std::time::Duration::from_secs(90));
    assert_eq!(c.state().failure, Some(Error::Deadline));
}

#[tokio::test]
async fn predispatch_refresh_failure_restores_exact_session_before_retry() {
    let (mut c, mut sink, trace) =
        scoped(vec![Err(Error::NotDispatched), token("next", "r3")]).await;
    let before = sink.saved.last().unwrap().clone();
    assert_eq!(c.refresh(&mut sink).await, Err(Error::NotDispatched));
    assert_eq!(sink.saved.last(), Some(&before));
    assert_eq!(c.state().phase, super::ConnectionPhase::Unavailable);
    c.refresh(&mut sink).await.unwrap();
    assert_eq!(
        trace
            .entries()
            .iter()
            .filter(
                |entry| entry.starts_with("request:Refresh") && entry.contains("refresh_token=r2")
            )
            .count(),
        2
    );
}

#[tokio::test]
async fn predispatch_rollback_must_be_acknowledged_before_any_new_request() {
    let (mut c, mut sink, trace) =
        scoped(vec![Err(Error::NotDispatched), token("next", "r3")]).await;
    sink.fail_at = Some(sink.calls + 2);
    assert_eq!(c.refresh(&mut sink).await, Err(Error::Persistence));
    assert_eq!(c.state().phase, super::ConnectionPhase::PersistencePending);
    let requests = trace
        .entries()
        .iter()
        .filter(|entry| entry.starts_with("request:"))
        .count();
    sink.fail_at = Some(sink.calls + 1);
    assert_eq!(c.refresh(&mut sink).await, Err(Error::Persistence));
    assert_eq!(
        trace
            .entries()
            .iter()
            .filter(|entry| entry.starts_with("request:"))
            .count(),
        requests
    );
    sink.fail_at = None;
    c.retry_persistence(&mut sink).await.unwrap();
    c.refresh(&mut sink).await.unwrap();
}

#[tokio::test]
async fn dispatched_refresh_server_failure_still_retires_old_token() {
    let (mut c, mut sink, trace) = scoped(vec![response(503, json!({}))]).await;
    assert_eq!(c.refresh(&mut sink).await, Err(Error::LoginRequired));
    let count = trace.entries().len();
    assert_eq!(c.refresh(&mut sink).await, Err(Error::LoginRequired));
    assert_eq!(trace.entries().len(), count);
}

#[tokio::test(start_paused = true)]
async fn discovery_failures_back_off_increasingly_and_unchanged_success_does_not_save_session() {
    for status in [403, 503, 200] {
        let mut replies = vec![
            token("account", "unused"),
            response(
                200,
                json!({"access_token":"long", "refresh_token":"r1", "expires_in":86400}),
            ),
            directory("FR"),
        ];
        for _ in 0..8 {
            replies.push(if status == 200 {
                response(200, json!({"totalCount":201,"results":[]}))
            } else {
                response(status, json!({}))
            });
        }
        replies.push(directory("FR"));
        replies.push(directory("FR"));
        let (mut c, mut sink, trace) = client(replies);
        login(&mut c, &mut sink).await;
        let saved = sink.calls;
        for delay in [60, 120, 240, 480, 960, 1920, 3600, 3600] {
            assert!(c.discover(&mut sink).await.is_err());
            assert_eq!(c.state().retry_after_secs, Some(delay));
            let count = trace.entries().len();
            assert!(matches!(
                c.discover(&mut sink).await,
                Err(Error::RateLimited { .. })
            ));
            assert_eq!(trace.entries().len(), count);
            assert_eq!(sink.calls, saved);
            tokio::time::advance(std::time::Duration::from_secs(delay)).await;
        }
        c.discover(&mut sink).await.unwrap();
        c.discover(&mut sink).await.unwrap();
        assert_eq!(
            sink.calls, saved,
            "unchanged directory must not rewrite the vault"
        );
        assert_eq!(c.state().retry_after_secs, None);
    }
}

#[test]
fn persisted_retry_after_and_failure_are_capped_on_decode() {
    let state = ConnectionState {
        retry_after_secs: Some(u64::MAX),
        failure: Some(Error::RateLimited {
            retry_after_secs: u64::MAX,
        }),
        ..Default::default()
    };
    let decoded = decode_state(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(decoded.retry_after_secs, Some(3600));
    assert_eq!(
        decoded.failure,
        Some(Error::RateLimited {
            retry_after_secs: 3600
        })
    );
}

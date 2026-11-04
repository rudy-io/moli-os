use super::*;
use crate::cloud::execution::{self, Policy};
use crate::inventory;
use moli_core::Value;
use std::time::{Duration, Instant};

fn scene() -> serde_json::Value {
    json!([{"oid":"saved-scene","label":"Private scenario","actions":[{"deviceURL":"io://1234-5678-9012/1","commands":[{"name":"close","parameters":[]}]}]}])
}
fn ticket(client: &CloudClient<Script>) -> execution::Ticket {
    let mut catalog = catalog(client);
    catalog
        .apply_collection(
            &catalog.identity(),
            CollectionKind::Scenarios,
            1000,
            Ok(inventory::parse_collection(
                CollectionKind::Scenarios,
                &serde_json::to_vec(&scene()).unwrap(),
            )
            .unwrap()),
        )
        .unwrap();
    let mut policy = Policy::default();
    policy.change(&catalog, "saved-scene", true).unwrap();
    let status = execution::view(&catalog, &policy, false, true, true)
        .scenarios
        .remove(0);
    let (device, _) = status.point.split().unwrap();
    execution::resolve(
        &catalog,
        &policy,
        &execution::catalog_revision(&catalog).unwrap(),
        &device,
        "run",
        &Value::from(status.value.as_str()),
    )
    .unwrap()
}

#[tokio::test]
async fn saved_scenario_uses_one_exact_id_post_after_scope_and_definition_reads() {
    let (mut client, mut sink, trace) = scoped(vec![
        gate(),
        response(200, scene()),
        response(200, json!({"execId":"synthetic-execution"})),
    ])
    .await;
    let ticket = ticket(&client);
    client.execution_read_only(false);
    let start = trace.entries().len();
    assert_eq!(
        client
            .execute_scenario(
                &ticket,
                Instant::now() + Duration::from_secs(5),
                || true,
                &mut sink
            )
            .await,
        Ok(())
    );
    let entries = &trace.entries()[start..];
    assert_eq!(entries.len(), 3);
    assert!(entries[0].starts_with("request:Gateways"));
    assert!(entries[1].starts_with("request:Collection(Scenarios)"));
    assert!(entries[2].starts_with("request:Scenario"));
    assert!(entries[2].ends_with("/enduser-mobile-web/enduserAPI/exec/saved-scene"));
    assert!(!entries[2].contains("actions"));
}

#[tokio::test]
async fn read_only_and_abandoned_requests_never_reach_the_provider() {
    for read_only in [true, false] {
        let (mut client, mut sink, trace) = scoped(vec![]).await;
        let ticket = ticket(&client);
        client.execution_read_only(read_only);
        let before = trace.entries().len();
        let result = client
            .execute_scenario(
                &ticket,
                Instant::now() + Duration::from_secs(5),
                || false,
                &mut sink,
            )
            .await;
        assert_eq!(
            result,
            Err(if read_only {
                Error::Forbidden
            } else {
                Error::Deadline
            })
        );
        assert_eq!(trace.entries().len(), before);
    }
}

#[tokio::test]
async fn changed_removed_and_wrong_scope_scenarios_send_no_post() {
    for definitions in [json!([]), json!([{"oid":"saved-scene","label":"changed"}])] {
        let (mut client, mut sink, trace) = scoped(vec![gate(), response(200, definitions)]).await;
        let ticket = ticket(&client);
        client.execution_read_only(false);
        assert_eq!(
            client
                .execute_scenario(
                    &ticket,
                    Instant::now() + Duration::from_secs(5),
                    || true,
                    &mut sink
                )
                .await,
            Err(Error::ScopeMismatch)
        );
        assert!(
            !trace
                .entries()
                .iter()
                .any(|e| e.starts_with("request:Scenario"))
        );
    }
    let (mut client, mut sink, trace) = scoped(vec![]).await;
    let mut ticket = ticket(&client);
    ticket.identity.scope.site_oid = "replacement".into();
    client.execution_read_only(false);
    let before = trace.entries().len();
    assert_eq!(
        client
            .execute_scenario(
                &ticket,
                Instant::now() + Duration::from_secs(5),
                || true,
                &mut sink
            )
            .await,
        Err(Error::ScopeMismatch)
    );
    assert_eq!(trace.entries().len(), before);
}

#[tokio::test]
async fn submission_failures_never_retry_or_refresh_after_post() {
    for reply in [
        response(401, json!({})),
        response(403, json!({})),
        response(200, json!({"success":true})),
        Err(Error::Network),
    ] {
        let (mut client, mut sink, trace) =
            scoped(vec![gate(), response(200, scene()), reply]).await;
        let ticket = ticket(&client);
        client.execution_read_only(false);
        let before = trace.entries().len();
        assert!(
            client
                .execute_scenario(
                    &ticket,
                    Instant::now() + Duration::from_secs(5),
                    || true,
                    &mut sink
                )
                .await
                .is_err()
        );
        let entries = &trace.entries()[before..];
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.starts_with("request:Scenario"))
                .count(),
            1
        );
        assert!(!entries.iter().any(|e| e.starts_with("request:Refresh")));
    }
}

#[test]
fn scenario_route_cannot_escape_its_selected_id_or_accept_an_action_payload() {
    for region in [Region::Emea, Region::Apac, Region::Snaba] {
        for id in [
            "../logout",
            "x/y",
            "x?b=1",
            ".",
            "",
            "apply",
            "schedule",
            "currentExecutions",
            "%2f",
            "#",
        ] {
            let request = Request {
                endpoint: Endpoint::Scenario,
                region,
                query: protocol::Query::Scenario(id.into()),
                bearer: None,
                body: String::new(),
            };
            assert!(request.path().is_err());
        }
        let request = Request {
            endpoint: Endpoint::Scenario,
            region,
            query: protocol::Query::Scenario("saved-scene".into()),
            bearer: None,
            body: "{}".into(),
        };
        assert!(request.path().is_err());
        assert_eq!(Endpoint::Scenario.route(region).0, "POST");
    }
}

#[tokio::test]
async fn abandonment_after_fresh_reads_and_expired_deadlines_send_no_post() {
    let (mut client, mut sink, trace) = scoped(vec![gate(), response(200, scene())]).await;
    let ticket = ticket(&client);
    client.execution_read_only(false);
    let result = client
        .execute_scenario(
            &ticket,
            Instant::now() + Duration::from_secs(5),
            || {
                !trace
                    .entries()
                    .iter()
                    .any(|entry| entry.starts_with("request:Collection(Scenarios)"))
            },
            &mut sink,
        )
        .await;
    assert_eq!(result, Err(Error::Deadline));
    assert!(
        !trace
            .entries()
            .iter()
            .any(|entry| entry.starts_with("request:Scenario"))
    );
    let (mut client, mut sink, trace) = scoped(vec![]).await;
    let ticket = self::ticket(&client);
    client.execution_read_only(false);
    let count = trace.entries().len();
    assert_eq!(
        client
            .execute_scenario(&ticket, Instant::now(), || true, &mut sink)
            .await,
        Err(Error::Deadline)
    );
    assert_eq!(trace.entries().len(), count);
}

struct StalledSubmission(Script);
impl Transport for StalledSubmission {
    fn send(&mut self, request: Request) -> BoxFuture<'_, Result<Response, Error>> {
        let stall = request.endpoint == Endpoint::Scenario;
        let response = self.0.send(request);
        Box::pin(async move {
            let reply = response.await;
            if stall {
                std::future::pending::<()>().await;
            }
            reply
        })
    }
}
#[tokio::test(start_paused = true)]
async fn timeout_during_post_is_uncertain_and_never_replayed() {
    let (scoped, mut sink, _) = scoped(vec![]).await;
    let ticket = ticket(&scoped);
    let mut catalog = super::catalog(&scoped);
    let trace = Trace::default();
    let mut replies = vec![gate()];
    for kind in CollectionKind::ALL {
        replies.push(response(
            200,
            if kind == CollectionKind::Scenarios {
                scene()
            } else {
                json!([])
            },
        ));
    }
    replies.extend([
        response(200, json!({"timeZone":"Europe/Paris"})),
        response(200, json!([])),
        gate(),
        response(200, scene()),
        response(200, json!({"execId":"synthetic"})),
    ]);
    let mut client = CloudClient::with_transport(
        "synthetic",
        StalledSubmission(Script {
            replies: replies.into(),
            trace: trace.clone(),
            delay: Duration::ZERO,
        }),
    )
    .unwrap();
    client.restore(sink.saved.last().unwrap()).unwrap();
    client.read_cycle(&mut catalog, &mut sink).await.unwrap();
    client.execution_read_only(false);
    assert_eq!(
        client
            .execute_scenario(
                &ticket,
                Instant::now() + Duration::from_secs(1),
                || true,
                &mut sink
            )
            .await,
        Err(Error::ExecutionUncertain)
    );
    assert_eq!(
        trace
            .entries()
            .iter()
            .filter(|entry| entry.starts_with("request:Scenario"))
            .count(),
        1
    );
    assert!(
        !trace
            .entries()
            .iter()
            .any(|entry| entry.starts_with("request:Refresh"))
    );
}

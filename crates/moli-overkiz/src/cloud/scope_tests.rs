//! Contract regression: BOB's parent siteOID and setup externalOID are distinct.
use super::*;
use crate::cloud::SessionPhase;

fn legacy_record(phase: &str) -> serde_json::Value {
    let now = jiff::Timestamp::now().as_millisecond();
    json!({
        "version":1, "instance":"synthetic", "generation":"generation",
        "phase":phase,
        "scope":{"site_oid":"setup", "gateway_id":"anchor", "region":"emea"},
        "token_site_oid":"setup", "access":"legacy-bearer", "refresh":"legacy-refresh",
        "issued_at_ms":now, "expires_at_ms":now+300_000,
        "candidates":[{"site_oid":"setup", "home_id":"parent", "label":null,
            "country":"FR", "gateway_ids":["anchor","second"], "region":"emea"}]
    })
}

#[tokio::test]
async fn parent_scope_refresh_wire_uses_home_id_while_catalogue_keeps_setup_identity() {
    let (c, s, trace) = scoped(vec![]).await;
    let wire = trace
        .entries()
        .into_iter()
        .find(|e| e.starts_with("request:Refresh:"))
        .unwrap();
    assert!(wire.ends_with("?siteOID=parent"), "{wire}");
    assert_eq!(c.identity().unwrap().scope.site_oid, "setup");
    let record: serde_json::Value = serde_json::from_slice(s.saved.last().unwrap()).unwrap();
    assert_eq!(record["token_site_oid"], "parent");
    assert_eq!(record["scope"]["site_oid"], "setup");
}

#[tokio::test]
async fn parent_scope_legacy_record_rotates_once_before_reads_without_password_grant() {
    let bytes = serde_json::to_vec(&legacy_record("ready")).unwrap();
    let binding = super::super::read_session_binding(&bytes, "synthetic").unwrap();
    assert_eq!(binding.phase, SessionPhase::Incomplete);
    let mut replies = vec![token("correct-parent-bearer", "replacement-refresh")];
    replies.extend(cycle_empty());
    let (mut c, mut sink, trace) = client(replies);
    c.restore(&bytes).unwrap();
    let mut inventory = crate::inventory::Catalog::new(c.selected_identity().unwrap()).unwrap();
    c.read_cycle(&mut inventory, &mut sink).await.unwrap();
    let entries = trace.entries();
    let requests: Vec<_> = entries
        .iter()
        .filter(|e| e.starts_with("request:"))
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|e| e.starts_with("request:Refresh:"))
            .count(),
        1
    );
    assert!(requests[0].ends_with("?siteOID=parent"));
    assert!(requests[0].contains("refresh_token=legacy-refresh"));
    assert!(requests[1].starts_with("request:Gateways:correct-parent-bearer:"));
    assert!(
        requests
            .iter()
            .all(|e| !e.contains("legacy-bearer") && !e.contains("request:Accounts"))
    );
    let refresh = entries
        .iter()
        .position(|e| e.starts_with("request:Refresh:"))
        .unwrap();
    assert!(entries[refresh - 1].contains("\"phase\":\"refreshing\""));
    assert!(entries[refresh + 1].contains("\"refresh\":\"replacement-refresh\""));
    assert_eq!(inventory.identity().scope.site_oid, "setup");
    assert_eq!(inventory.identity().connection_generation, "generation");
}

#[tokio::test]
async fn parent_scope_legacy_terminal_records_never_replay_refresh() {
    for phase in ["refreshing", "login_required"] {
        let bytes = serde_json::to_vec(&legacy_record(phase)).unwrap();
        let (mut c, mut sink, trace) = client(vec![]);
        c.restore(&bytes).unwrap();
        let mut inventory = crate::inventory::Catalog::new(c.selected_identity().unwrap()).unwrap();
        assert_eq!(
            c.read_cycle(&mut inventory, &mut sink).await,
            Err(Error::LoginRequired)
        );
        assert!(trace.entries().iter().all(|e| !e.starts_with("request:")));
    }
}

#[tokio::test(start_paused = true)]
async fn parent_scope_recovery_persistence_failure_keeps_only_the_replacement_refresh() {
    let bytes = serde_json::to_vec(&legacy_record("ready")).unwrap();
    let mut replies = vec![token("correct-parent-bearer", "replacement-refresh")];
    replies.extend(cycle_empty());
    let (mut c, mut sink, trace) = client(replies);
    c.restore(&bytes).unwrap();
    sink.fail_at = Some(2);
    let mut inventory = crate::inventory::Catalog::new(c.selected_identity().unwrap()).unwrap();
    assert_eq!(
        c.read_cycle(&mut inventory, &mut sink).await,
        Err(Error::Persistence)
    );
    assert_eq!(
        trace
            .entries()
            .iter()
            .filter(|e| e.starts_with("request:"))
            .count(),
        1
    );
    let checkpoint = sink.saved.last().unwrap().clone();
    let (mut restart, mut empty_sink, restarted_trace) = client(vec![]);
    restart.restore(&checkpoint).unwrap();
    assert_eq!(
        restart.read_cycle(&mut inventory, &mut empty_sink).await,
        Err(Error::LoginRequired)
    );
    assert!(
        restarted_trace
            .entries()
            .iter()
            .all(|e| !e.starts_with("request:"))
    );

    c.retry_persistence(&mut sink).await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    c.read_cycle(&mut inventory, &mut sink).await.unwrap();
    let requests: Vec<_> = trace
        .entries()
        .into_iter()
        .filter(|e| e.starts_with("request:"))
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|e| e.starts_with("request:Refresh:"))
            .count(),
        1
    );
    assert!(requests[1].starts_with("request:Gateways:correct-parent-bearer:"));
    let saved: serde_json::Value = serde_json::from_slice(sink.saved.last().unwrap()).unwrap();
    assert_eq!(saved["refresh"], "replacement-refresh");
    assert_eq!(saved["token_site_oid"], "parent");
}

#[tokio::test(start_paused = true)]
async fn parent_scope_recovery_checkpoint_failure_sends_no_provider_request() {
    let bytes = serde_json::to_vec(&legacy_record("ready")).unwrap();
    let (mut c, mut sink, trace) = client(vec![]);
    c.restore(&bytes).unwrap();
    sink.fail_at = Some(1);
    let mut inventory = crate::inventory::Catalog::new(c.selected_identity().unwrap()).unwrap();
    assert_eq!(
        c.read_cycle(&mut inventory, &mut sink).await,
        Err(Error::Persistence)
    );
    assert!(trace.entries().iter().all(|e| !e.starts_with("request:")));
    assert!(sink.saved.is_empty());
    // Retrying local durability cannot itself repeat or start the remote rotation.
    c.retry_persistence(&mut sink).await.unwrap();
    assert_eq!(
        c.refresh(&mut sink).await,
        Err(Error::RateLimited {
            retry_after_secs: 60
        })
    );
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert_eq!(c.refresh(&mut sink).await, Err(Error::LoginRequired));
    assert!(trace.entries().iter().all(|e| !e.starts_with("request:")));
}

#[test]
fn parent_scope_binding_accepts_parent_but_rejects_foreign_or_malformed_mapping() {
    let mut valid = legacy_record("ready");
    valid["token_site_oid"] = json!("parent");
    let binding =
        super::super::read_session_binding(&serde_json::to_vec(&valid).unwrap(), "synthetic")
            .unwrap();
    assert_eq!(binding.phase, SessionPhase::Ready);
    assert_eq!(binding.scope.unwrap().site_oid, "setup");
    let normalized = super::super::session::Session::decode(
        &serde_json::to_vec(&legacy_record("ready")).unwrap(),
        "synthetic",
    )
    .unwrap();
    assert!(normalized.access.is_none());
    assert!(normalized.token_site_oid.is_none());
    assert!(normalized.encode().is_ok());
    let mut wrong = normalized;
    wrong.token_site_oid = Some("setup".into());
    assert_eq!(wrong.encode(), Err(Error::ScopeMismatch));
    for bad in [
        {
            let mut v = valid.clone();
            v["token_site_oid"] = json!("foreign-parent");
            v
        },
        {
            let mut v = legacy_record("ready");
            v["access"] = json!("bad bearer");
            v
        },
        {
            let mut v = legacy_record("ready");
            v["candidates"][0]["home_id"] = json!("");
            v
        },
        {
            let mut v = legacy_record("ready");
            v["candidates"][0]["gateway_ids"] = json!(["other"]);
            v
        },
        {
            let mut v = legacy_record("ready");
            let duplicate = v["candidates"][0].clone();
            v["candidates"].as_array_mut().unwrap().push(duplicate);
            v
        },
    ] {
        assert!(
            super::super::read_session_binding(&serde_json::to_vec(&bad).unwrap(), "synthetic")
                .is_err()
        );
    }
}

#[tokio::test]
async fn parent_scope_two_setups_in_one_home_still_require_selected_gateway_membership() {
    let directory = response(
        200,
        json!({"totalCount":1,"results":[{
        "siteOID":"parent","country":"FR","subSites":[
            {"externalOID":"setup","gateways":[{"gatewayId":"anchor"}]},
            {"externalOID":"other-setup","gateways":[{"gatewayId":"other-anchor"}]}
        ]}]}),
    );
    let (mut c, mut sink, trace) = client(vec![
        token("a", "r0"),
        token("unscoped", "r1"),
        directory,
        token("scoped", "r2"),
        gate(),
    ]);
    login(&mut c, &mut sink).await;
    let generation = c.state().generation.unwrap();
    assert_eq!(
        c.select_site(&generation, "other-setup", "other-anchor", None, &mut sink)
            .await,
        Err(Error::ScopeMismatch)
    );
    assert!(c.identity().is_err());
    assert_eq!(c.selected_identity().unwrap().scope.site_oid, "other-setup");
    let requests = trace.entries();
    assert!(
        requests
            .iter()
            .any(|e| e.starts_with("request:Refresh:") && e.ends_with("?siteOID=parent"))
    );
    assert!(
        requests
            .iter()
            .all(|e| !e.starts_with("request:Collection"))
    );
}

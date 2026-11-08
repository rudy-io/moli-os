use super::*;
use crate::inventory::{self, CollectionKind, Region, Scope};
use serde_json::json;

fn catalog() -> Catalog {
    let identity = CatalogIdentity {
        instance: "synthetic".into(),
        connection_generation: "session-one".into(),
        scope: Scope {
            site_oid: "setup".into(),
            gateway_id: "anchor".into(),
            region: Region::Emea,
        },
    };
    let mut catalog = Catalog::new(identity.clone()).unwrap();
    let data = inventory::parse_collection(CollectionKind::Scenarios, &serde_json::to_vec(&json!([
        {"oid":"saved-scene","label":"PRIVATE_SCENARIO_NAME","actions":[{"deviceURL":"io://1234-5678-9012/1","commands":[{"name":"close","parameters":[]}]}]},
        {"oid":"other-scene","label":"Other"}
    ])).unwrap()).unwrap();
    catalog
        .apply_collection(&identity, CollectionKind::Scenarios, 1_000, Ok(data))
        .unwrap();
    catalog
}

#[test]
fn consent_is_explicit_per_scenario_and_revocable() {
    let catalog = catalog();
    let mut policy = Policy::default();
    assert!(!policy.permits(&catalog, "saved-scene"));
    policy.change(&catalog, "saved-scene", true).unwrap();
    assert!(policy.permits(&catalog, "saved-scene"));
    assert!(!policy.permits(&catalog, "other-scene"));
    policy.change(&catalog, "saved-scene", false).unwrap();
    assert!(!policy.permits(&catalog, "saved-scene"));
}

#[test]
fn consent_does_not_survive_changed_definition_generation_or_setup() {
    let original = catalog();
    let mut policy = Policy::default();
    policy.change(&original, "saved-scene", true).unwrap();
    let mut replacement = original.clone();
    replacement.connection_generation.push_str("-replacement");
    assert!(!policy.permits(&replacement, "saved-scene"));
    let mut replacement = original.clone();
    replacement.scope.site_oid = "other-setup".into();
    assert!(!policy.permits(&replacement, "saved-scene"));
    let mut replacement = original.clone();
    let data = inventory::parse_collection(
        CollectionKind::Scenarios,
        br#"[{"oid":"saved-scene","label":"Changed","actions":[]}]"#,
    )
    .unwrap();
    replacement
        .apply_collection(
            &replacement.identity(),
            CollectionKind::Scenarios,
            2000,
            Ok(data),
        )
        .unwrap();
    assert!(!policy.permits(&replacement, "saved-scene"));
    let restored = Policy::decode(policy.encode().unwrap().as_bytes()).unwrap();
    assert!(restored.permits(&original, "saved-scene"));
}

#[test]
fn global_read_only_and_stale_status_dominate_saved_consent() {
    let catalog = catalog();
    let mut policy = Policy::default();
    policy.change(&catalog, "saved-scene", true).unwrap();
    for (read_only, online, fresh, reason) in [
        (true, true, true, "global_read_only"),
        (false, false, true, "offline"),
        (false, true, false, "stale"),
    ] {
        let status = view(&catalog, &policy, read_only, online, fresh)
            .scenarios
            .remove(0);
        assert!(status.permitted);
        assert!(!status.executable);
        assert_eq!(status.reason, Some(reason));
    }
    let status = view(&catalog, &Policy::default(), false, true, true)
        .scenarios
        .remove(0);
    assert_eq!(status.reason, Some("permission_required"));
    assert!(!format!("{policy:?} {status:?}").contains("saved-scene"));
}

#[test]
fn command_token_binds_every_catalog_revision_and_exact_scope() {
    let catalog = catalog();
    let mut policy = Policy::default();
    policy.change(&catalog, "saved-scene", true).unwrap();
    let original = view(&catalog, &policy, false, true, true)
        .scenarios
        .remove(0);
    let (device, _) = original.point.split().unwrap();
    let value = Value::from(original.value.as_str());
    assert!(
        resolve(
            &catalog,
            &policy,
            &catalog_revision(&catalog).unwrap(),
            &device,
            "run",
            &value
        )
        .is_ok()
    );
    assert!(
        resolve(
            &catalog,
            &policy,
            &catalog_revision(&catalog).unwrap(),
            &device,
            "run",
            &Value::Bool(true)
        )
        .is_err()
    );
    assert!(
        resolve(
            &catalog,
            &policy,
            &catalog_revision(&catalog).unwrap(),
            &device,
            "other",
            &value
        )
        .is_err()
    );
    let mut updated = catalog.clone();
    updated.provider_timezone = Some("Europe/Paris".into());
    assert!(policy.permits(&updated, "saved-scene"));
    assert!(
        resolve(
            &updated,
            &policy,
            &catalog_revision(&updated).unwrap(),
            &device,
            "run",
            &value
        )
        .is_err()
    );
    assert_ne!(
        view(&updated, &policy, false, true, true).scenarios[0].value,
        original.value
    );
    assert!(!original.point.as_str().contains("saved-scene"));
}

#[test]
fn unknown_and_path_like_identifiers_never_gain_permission() {
    let catalog = catalog();
    let mut policy = Policy::default();
    for id in ["unknown", "../logout", "x/y", ".", "a%2Fb", "x?query=1", ""] {
        assert!(policy.change(&catalog, id, true).is_err());
        assert!(!policy.permits(&catalog, id));
    }
    assert!(
        Policy::decode(br#"{"version":1,"identity":null,"allowed":{"saved-scene":"f"}}"#).is_err()
    );
    assert!(Policy::decode(&vec![b' '; POLICY_LIMIT + 1]).is_err());
}

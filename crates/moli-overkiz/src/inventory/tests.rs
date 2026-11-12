use super::*;
use serde_json::{Value, json};

const FIXTURES: [(CollectionKind, &str); 6] = [
    (
        CollectionKind::Scenarios,
        include_str!("../../../../integrations/overkiz/fixtures/cloud-action-groups.json"),
    ),
    (
        CollectionKind::CalendarDays,
        include_str!("../../../../integrations/overkiz/fixtures/cloud-calendar-days.json"),
    ),
    (
        CollectionKind::CalendarRules,
        include_str!("../../../../integrations/overkiz/fixtures/cloud-calendar-rules.json"),
    ),
    (
        CollectionKind::Triggers,
        include_str!("../../../../integrations/overkiz/fixtures/cloud-triggers.json"),
    ),
    (
        CollectionKind::Schedule,
        include_str!("../../../../integrations/overkiz/fixtures/cloud-scheduled-executions.json"),
    ),
    (
        CollectionKind::DelayedSchedule,
        include_str!("../../../../integrations/overkiz/fixtures/cloud-delayed-executions.json"),
    ),
];
fn identity() -> CatalogIdentity {
    CatalogIdentity {
        instance: "synthetic-cloud".into(),
        connection_generation: "synthetic-generation".into(),
        scope: Scope {
            site_oid: "synthetic-setup".into(),
            gateway_id: "synthetic-anchor".into(),
            region: Region::Emea,
        },
    }
}
fn parse(kind: CollectionKind, value: &Value) -> Result<CollectionData, InventoryError> {
    parse_collection(kind, &serde_json::to_vec(value).unwrap())
}
fn padded_definition(size: usize) -> Value {
    let mut value = json!({"oid":"synthetic","padding": []});
    let chunks = value["padding"].as_array_mut().unwrap();
    // Allocate in independently bounded strings; exact compact definition byte size.
    for _ in 0..((size - 100) / 8_003) {
        chunks.push(Value::String("x".repeat(8_000)));
    }
    let used = serde_json::to_vec(&value).unwrap().len();
    value["padding"]
        .as_array_mut()
        .unwrap()
        .push(Value::String("x".repeat(size - used - 3)));
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), size);
    value
}

#[test]
fn official_synthetic_collections_preserve_definitions_and_links() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    for (kind, body) in FIXTURES {
        let data = parse_collection(kind, body.as_bytes()).unwrap();
        let expected: Vec<Value> = serde_json::from_str(body).unwrap();
        assert_eq!(
            data.definitions
                .iter()
                .map(|d| d.raw.as_value().clone())
                .collect::<Vec<_>>(),
            expected
        );
        catalog
            .apply_collection(&id, kind, 1_000, Ok(data))
            .unwrap();
    }
    let rules = catalog.collections[&CollectionKind::CalendarRules]
        .data
        .as_ref()
        .unwrap();
    assert_eq!(
        rules.definitions[0].references[0].collection,
        CollectionKind::CalendarDays
    );
    assert_eq!(
        rules.definitions[0].references[0].id,
        "00000000-0000-4000-8000-000000000002"
    );
    let day = catalog.collections[&CollectionKind::CalendarDays]
        .data
        .as_ref()
        .unwrap();
    assert_eq!(
        day.definitions[0].references[0].collection,
        CollectionKind::Scenarios
    );
    assert_eq!(
        decode_catalog(&encode_catalog(&catalog).unwrap()).unwrap(),
        catalog
    );
}
#[test]
fn unknown_wrappers_and_unresolved_references_are_retained() {
    let body =
        include_bytes!("../../../../integrations/overkiz/fixtures/cloud-unknown-trigger.json");
    let data = parse_collection(CollectionKind::Triggers, body).unwrap();
    assert_eq!(
        data.definitions[0].raw.as_value()["syntheticFutureTrigger"]["unknownRule"]["arbitrary"],
        "preserve, do not execute"
    );
    assert!(
        data.definitions[0]
            .warnings
            .contains(&InterpretationWarning::UnknownVariant)
    );
    let value = json!([{"specificDayCalendarRule":{"oid":"rule","calendarDayOID":"missing","year":2027,"month":1,"day":1}}]);
    assert_eq!(
        parse(CollectionKind::CalendarRules, &value)
            .unwrap()
            .definitions[0]
            .references[0]
            .id,
        "missing"
    );
}
#[test]
fn empty_success_is_distinct_from_error_object_scalar_and_malformed_entries() {
    for kind in CollectionKind::ALL {
        assert!(
            parse_collection(kind, b"[]")
                .unwrap()
                .definitions
                .is_empty()
        );
        for body in [
            b"{}".as_slice(),
            b"null",
            b"1",
            b"<html>",
            b"[null]",
            b"[1]",
        ] {
            assert!(parse_collection(kind, body).is_err());
        }
    }
    assert_eq!(
        CollectionFailure::from_http_status(204).status(),
        CollectionStatus::InvalidResponse
    );
    assert_eq!(
        CollectionFailure::from_http_status(404).status(),
        CollectionStatus::Unsupported
    );
    assert_eq!(
        CollectionFailure::from_http_status(405).status(),
        CollectionStatus::Unsupported
    );
    assert_eq!(
        CollectionFailure::from_http_status(403).status(),
        CollectionStatus::Forbidden
    );
    assert_eq!(
        CollectionFailure::from_http_status(401).status(),
        CollectionStatus::Unauthorized
    );
    assert_eq!(
        CollectionFailure::from_http_status(429).status(),
        CollectionStatus::RateLimited
    );
}
#[test]
fn duplicate_recognized_ids_and_malformed_known_wrappers_are_rejected() {
    assert_eq!(
        parse(
            CollectionKind::Scenarios,
            &json!([{"oid":"same"},{"oid":"same"}])
        ),
        Err(InventoryError::DuplicateId)
    );
    assert_eq!(
        parse(
            CollectionKind::Triggers,
            &json!([{"ifThenSetupTrigger":{"oid":"same"}},{"discreteTrigger":{"oid":"same"}}])
        ),
        Err(InventoryError::DuplicateId)
    );
    for value in [
        json!([{"weeklyCalendarRule":null}]),
        json!([{"weeklyCalendarRule":{}}]),
        json!([{"weeklyCalendarRule":{"oid":1}}]),
    ] {
        assert!(parse(CollectionKind::CalendarRules, &value).is_err());
    }
    // Occurrences can legitimately repeat the same program reference.
    assert!(
        parse(
            CollectionKind::DelayedSchedule,
            &json!([{"triggerId":"same","utcTime":1},{"triggerId":"same","utcTime":2}])
        )
        .is_ok()
    );
}
#[test]
fn collection_and_definition_byte_limits_have_exact_boundaries() {
    let mut body = b"[]".to_vec();
    body.resize(MAX_COLLECTION_BYTES, b' ');
    assert!(parse_collection(CollectionKind::Scenarios, &body).is_ok());
    body.push(b' ');
    assert_eq!(
        parse_collection(CollectionKind::Scenarios, &body),
        Err(InventoryError::ByteLimit)
    );
    assert!(
        parse(
            CollectionKind::Scenarios,
            &json!([padded_definition(MAX_DEFINITION_BYTES)])
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            CollectionKind::Scenarios,
            &json!([padded_definition(MAX_DEFINITION_BYTES + 1)])
        ),
        Err(InventoryError::DefinitionLimit)
    );
}
#[test]
fn string_label_identifier_limits_are_bytes_and_keep_unicode() {
    for (key, limit, error) in [
        ("arbitrary", MAX_STRING_BYTES, InventoryError::StringLimit),
        ("label", MAX_LABEL_BYTES, InventoryError::LabelLimit),
        ("oid", MAX_ID_BYTES, InventoryError::IdLimit),
    ] {
        let mut v = json!({"oid":"synthetic"});
        v[key] = Value::String("é".repeat(limit / 2));
        let parsed = parse(CollectionKind::Scenarios, &json!([v.clone()])).unwrap();
        assert_eq!(parsed.definitions[0].raw.as_value()[key], v[key]);
        v[key].as_str().unwrap();
        v[key] = Value::String("é".repeat(limit / 2 + 1));
        assert_eq!(parse(CollectionKind::Scenarios, &json!([v])), Err(error));
    }
}
#[test]
fn deeply_nested_conditions_have_a_hard_boundary() {
    let mut child = Value::Null;
    for _ in 2..MAX_DEPTH {
        child = json!([child]);
    }
    let mut object = json!({"oid":"synthetic", "condition":child});
    assert!(parse(CollectionKind::Scenarios, &json!([object.clone()])).is_ok());
    object["condition"] = json!([object["condition"].clone()]);
    assert_eq!(
        parse(CollectionKind::Scenarios, &json!([object])),
        Err(InventoryError::DepthLimit)
    );
}
#[test]
fn node_and_entry_counts_have_independent_boundaries() {
    let mut entries: Vec<Value> = (0..MAX_OBJECTS)
        .map(|i| json!({"oid":format!("synthetic-{i}")}))
        .collect();
    assert!(parse(CollectionKind::Scenarios, &json!(entries)).is_ok());
    entries.push(json!({"oid":"over"}));
    assert_eq!(
        parse(CollectionKind::Scenarios, &json!(entries)),
        Err(InventoryError::ObjectLimit)
    );
    // One array node + 4*(object, two keys, id value, nested array) = 21 overhead.
    let mut nodes: Vec<Value> = (0..4)
        .map(|i| json!({"oid":format!("synthetic-{i}"),"v":[]}))
        .collect();
    let remaining = MAX_NODES - 21;
    for (i, entry) in nodes.iter_mut().enumerate() {
        entry["v"] = json!(vec![
            Value::Null;
            remaining / 4 + usize::from(i < remaining % 4)
        ]);
    }
    assert!(parse(CollectionKind::Scenarios, &json!(nodes)).is_ok());
    nodes[0]["v"].as_array_mut().unwrap().push(Value::Null);
    assert_eq!(
        parse(CollectionKind::Scenarios, &json!(nodes)),
        Err(InventoryError::NodeLimit)
    );
}
#[test]
fn failed_updates_preserve_last_good_definitions_and_observation_time() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    let original = parse_collection(CollectionKind::Scenarios, FIXTURES[0].1.as_bytes()).unwrap();
    catalog
        .apply_collection(&id, CollectionKind::Scenarios, 1_000, Ok(original.clone()))
        .unwrap();
    for failure in [
        CollectionFailure::Forbidden,
        CollectionFailure::Unsupported,
        CollectionFailure::Unauthorized,
        CollectionFailure::RateLimited {
            retry_after_secs: Some(120),
        },
        CollectionFailure::Unavailable,
        InventoryError::Malformed.into(),
    ] {
        catalog
            .apply_collection(&id, CollectionKind::Scenarios, 2_000, Err(failure))
            .unwrap();
        let state = &catalog.collections[&CollectionKind::Scenarios];
        assert_eq!(state.data, Some(original.clone()));
        assert_eq!(state.last_observation_ms, Some(1_000));
        assert_eq!(state.last_attempt_ms, Some(2_000));
        assert_eq!(state.status, failure.status());
    }
    assert_eq!(
        catalog.collections[&CollectionKind::Triggers].status,
        CollectionStatus::NotObserved
    );
}
#[test]
fn generation_scope_and_instance_mismatches_refuse_updates_without_mutation() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    for field in 0..5 {
        let mut other = id.clone();
        match field {
            0 => other.instance.push('x'),
            1 => other.connection_generation.push('x'),
            2 => other.scope.site_oid.push('x'),
            3 => other.scope.gateway_id.push('x'),
            _ => other.scope.region = Region::Apac,
        }
        let before = catalog.clone();
        assert_eq!(
            catalog.apply_collection(
                &other,
                CollectionKind::Scenarios,
                1_000,
                Err(CollectionFailure::Unavailable)
            ),
            Err(InventoryError::IdentityMismatch)
        );
        assert_eq!(catalog, before);
    }
}
#[test]
fn native_wall_times_astronomical_labels_and_utc_milliseconds_stay_distinct() {
    let day = parse_collection(CollectionKind::CalendarDays, FIXTURES[1].1.as_bytes()).unwrap();
    let raw = day.definitions[0].raw.as_value();
    assert_eq!(
        raw["dayTimeTriggers"][0]["startActionGroupTimeTrigger"]["hours"],
        7
    );
    assert_eq!(
        raw["dayTimeTriggers"][1]["startActionGroupTimeTrigger"]["triggerType"],
        "DUSK"
    );
    assert!(
        raw["dayTimeTriggers"][1]["startActionGroupTimeTrigger"]
            .get("utcTime")
            .is_none()
    );
    let dawn = json!([{"oid":"day","dayTimeTriggers":[{"startActionGroupTimeTrigger":{"triggerType":"DAWN","actionGroupOID":"missing"}}]}]);
    assert_eq!(
        parse(CollectionKind::CalendarDays, &dawn)
            .unwrap()
            .definitions[0]
            .raw
            .as_value(),
        &dawn[0]
    );
    let scheduled = parse_collection(CollectionKind::Schedule, FIXTURES[4].1.as_bytes()).unwrap();
    assert_eq!(
        scheduled.definitions[0].raw.as_value()["utcTime"],
        1_793_520_000_000_i64
    );
}
#[test]
fn typed_parameters_and_action_order_are_preserved_without_execution() {
    let value = json!([{"oid":"scene","label":"Été 🌅","actions":[{"deviceURL":"io://example.invalid/second","commands":[{"name":"close","parameters":[1,true,null,"001",{"unknown":2}]}]},{"deviceURL":"io://example.invalid/first","commands":[{"name":"open","parameters":[]}]}]}]);
    let data = parse(CollectionKind::Scenarios, &value).unwrap();
    assert_eq!(data.definitions[0].raw.as_value(), &value[0]);
    assert_eq!(data.definitions[0].label.as_deref(), Some("Été 🌅"));
}
#[test]
fn metadata_keeps_only_target_labels_and_provider_timezone() {
    let timezone = parse_location_timezone(
        br#"{"timezone":"Europe/Paris","city":"PRIVATE_MARKER","latitude":1}"#,
    )
    .unwrap();
    assert_eq!(timezone.as_deref(), Some("Europe/Paris"));
    assert_eq!(parse_location_timezone(b"{}"), Ok(None));
    assert!(parse_location_timezone(br#"{"timezone":"../../bad"}"#).is_err());
    let targets = parse_target_labels(br#"[{"deviceURL":"io://example.invalid/target","label":"Synthetic","states":["PRIVATE_MARKER"],"location":"PRIVATE_MARKER"}]"#).unwrap();
    assert_eq!(
        serde_json::to_value(targets).unwrap(),
        json!([{"device_url":"io://example.invalid/target","label":"Synthetic"}])
    );
}
#[test]
fn encrypted_round_trip_revalidates_content_and_debug_is_redacted() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    let data = parse(CollectionKind::Scenarios, &json!([{"oid":"PRIVATE_MARKER_ID","label":"PRIVATE_MARKER_LABEL","actions":[{"parameters":["PRIVATE_MARKER_RAW"]}]}])).unwrap();
    let debug = format!("{data:?} {:?}", data.definitions[0].raw);
    assert!(!debug.contains("PRIVATE_MARKER"));
    catalog
        .apply_collection(&id, CollectionKind::Scenarios, 1_000, Ok(data))
        .unwrap();
    catalog.target_labels = vec![TargetLabel {
        device_url: "PRIVATE_MARKER_URL".into(),
        label: Some("PRIVATE_MARKER_TARGET".into()),
    }];
    assert!(!format!("{catalog:?} {:?}", catalog.target_labels).contains("PRIVATE_MARKER"));
    let encoded = encode_catalog(&catalog).unwrap();
    assert!(encoded.len() <= MAX_CATALOG_BYTES);
    assert_eq!(decode_catalog(&encoded).unwrap(), catalog);
    let mut hostile: Value = serde_json::from_slice(&encoded).unwrap();
    hostile["collections"]["scenarios"]["data"]["definitions"][0]["raw"]["label"] =
        Value::String("x".repeat(MAX_LABEL_BYTES + 1));
    assert!(decode_catalog(&serde_json::to_vec(&hostile).unwrap()).is_err());
    hostile["version"] = json!(2);
    assert!(decode_catalog(&serde_json::to_vec(&hostile).unwrap()).is_err());
    let mut too_large = encoded;
    too_large.resize(MAX_CATALOG_BYTES + 1, b' ');
    assert_eq!(decode_catalog(&too_large), Err(InventoryError::ByteLimit));
}
#[test]
fn cache_validation_rejects_forged_descriptors_status_and_aggregate_limits() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    catalog
        .apply_collection(
            &id,
            CollectionKind::Scenarios,
            1_000,
            Ok(parse_collection(CollectionKind::Scenarios, FIXTURES[0].1.as_bytes()).unwrap()),
        )
        .unwrap();
    let mut forged = catalog.clone();
    forged
        .collections
        .get_mut(&CollectionKind::Scenarios)
        .unwrap()
        .data
        .as_mut()
        .unwrap()
        .definitions[0]
        .label = Some("Forged".into());
    assert!(encode_catalog(&forged).is_err());
    forged = catalog.clone();
    forged
        .collections
        .get_mut(&CollectionKind::Scenarios)
        .unwrap()
        .last_observation_ms = None;
    assert!(encode_catalog(&forged).is_err());
    let entries: Vec<Value> = (0..MAX_OBJECTS)
        .map(|i| json!({"oid":format!("synthetic-{i}")}))
        .collect();
    catalog
        .apply_collection(
            &id,
            CollectionKind::Scenarios,
            2_000,
            Ok(parse(CollectionKind::Scenarios, &json!(entries)).unwrap()),
        )
        .unwrap();
    let result = catalog.apply_collection(
        &id,
        CollectionKind::CalendarDays,
        3_000,
        Ok(parse(CollectionKind::CalendarDays, &json!([{"oid":"extra"}])).unwrap()),
    );
    assert_eq!(result, Err(InventoryError::ObjectLimit));
    assert!(
        catalog.collections[&CollectionKind::CalendarDays]
            .data
            .is_none()
    );
    assert_eq!(
        catalog.collections[&CollectionKind::CalendarDays].status,
        CollectionStatus::InvalidResponse
    );
}
#[test]
fn arbitrary_typed_parameter_ids_are_not_mistaken_for_program_identifiers() {
    let value = json!([{"oid":"scene","actions":[{"commands":[{"name":"opaque","parameters":[{"id":7,"triggerId":false,"oid":null},{"id":"","oid":""}]}]}]}]);
    assert_eq!(
        parse(CollectionKind::Scenarios, &value)
            .unwrap()
            .definitions[0]
            .raw
            .as_value(),
        &value[0]
    );
    let value = json!([{"syntheticFutureTrigger":{"oid":42,"label":false,"payload":true}}]);
    let data = parse(CollectionKind::Triggers, &value).unwrap();
    assert_eq!(data.definitions[0].raw.as_value(), &value[0]);
    assert_eq!(data.definitions[0].id, None);
    assert!(
        data.definitions[0]
            .warnings
            .contains(&InterpretationWarning::UnknownVariant)
    );
}
#[test]
fn aggregate_nodes_and_bytes_reject_incoming_data_without_erasing_last_good() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    let mut values: Vec<Value> = (0..4)
        .map(|i| json!({"oid":format!("scene-{i}"),"v":[]}))
        .collect();
    let remaining = MAX_NODES - 21;
    for (i, value) in values.iter_mut().enumerate() {
        value["v"] = json!(vec![
            Value::Null;
            remaining / 4 + usize::from(i < remaining % 4)
        ]);
    }
    catalog
        .apply_collection(
            &id,
            CollectionKind::Scenarios,
            1_000,
            Ok(parse(CollectionKind::Scenarios, &json!(values)).unwrap()),
        )
        .unwrap();
    assert_eq!(
        catalog.apply_collection(
            &id,
            CollectionKind::CalendarDays,
            2_000,
            Ok(parse(CollectionKind::CalendarDays, &json!([{"oid":"day"}])).unwrap())
        ),
        Err(InventoryError::NodeLimit)
    );
    assert_eq!(
        catalog.collections[&CollectionKind::Scenarios].last_observation_ms,
        Some(1_000)
    );
    let mut catalog = Catalog::new(id.clone()).unwrap();
    for (index, kind) in [
        CollectionKind::Scenarios,
        CollectionKind::CalendarDays,
        CollectionKind::Schedule,
    ]
    .into_iter()
    .enumerate()
    {
        let values: Vec<Value> = (0..7)
            .map(|i| {
                let mut value = padded_definition(60_000);
                value["oid"] = json!(format!("synthetic-{i}"));
                value
            })
            .collect();
        let result =
            catalog.apply_collection(&id, kind, 2_000, Ok(parse(kind, &json!(values)).unwrap()));
        if index == 2 {
            assert_eq!(result, Err(InventoryError::ByteLimit));
        } else {
            result.unwrap();
        }
    }
    assert_eq!(
        catalog.collections[&CollectionKind::Scenarios]
            .data
            .as_ref()
            .unwrap()
            .definitions
            .len(),
        7
    );
    assert!(
        catalog.collections[&CollectionKind::Schedule]
            .data
            .is_none()
    );
    assert_eq!(
        catalog.collections[&CollectionKind::Schedule].status,
        CollectionStatus::InvalidResponse
    );
    assert!(encode_catalog(&catalog).unwrap().len() < MAX_CATALOG_BYTES);
}
#[test]
fn catalog_byte_limit_accepts_exact_boundary_and_rejects_the_next_byte() {
    let mut catalog = Catalog::new(identity()).unwrap();
    loop {
        let target = TargetLabel {
            device_url: format!(
                "{}{:04}",
                "x".repeat(MAX_STRING_BYTES - 4),
                catalog.target_labels.len()
            ),
            label: None,
        };
        let mut candidate = catalog.clone();
        candidate.target_labels.push(target);
        if serde_json::to_vec(&candidate).unwrap().len() + 128 > MAX_CATALOG_BYTES {
            break;
        }
        catalog = candidate;
    }
    catalog.target_labels.push(TargetLabel {
        device_url: "last".into(),
        label: None,
    });
    let size = serde_json::to_vec(&catalog).unwrap().len();
    let last = catalog.target_labels.last_mut().unwrap();
    last.device_url
        .push_str(&"y".repeat(MAX_CATALOG_BYTES - size));
    assert_eq!(encode_catalog(&catalog).unwrap().len(), MAX_CATALOG_BYTES);
    catalog
        .target_labels
        .last_mut()
        .unwrap()
        .device_url
        .push('z');
    assert_eq!(encode_catalog(&catalog), Err(InventoryError::ByteLimit));
}
fn metadata_near_catalog_limit(remaining: usize) -> Catalog {
    let mut catalog = Catalog::new(identity()).unwrap();
    let desired = MAX_CATALOG_BYTES - remaining;
    loop {
        let target = TargetLabel {
            device_url: format!(
                "{}{:04}",
                "x".repeat(MAX_STRING_BYTES - 4),
                catalog.target_labels.len()
            ),
            label: None,
        };
        let mut candidate = catalog.clone();
        candidate.target_labels.push(target);
        if serde_json::to_vec(&candidate).unwrap().len() + 128 > desired {
            break;
        }
        catalog = candidate;
    }
    catalog.target_labels.push(TargetLabel {
        device_url: "last".into(),
        label: None,
    });
    let size = serde_json::to_vec(&catalog).unwrap().len();
    catalog
        .target_labels
        .last_mut()
        .unwrap()
        .device_url
        .push_str(&"y".repeat(desired - size));
    catalog
}
#[test]
fn bounded_updates_reserve_room_for_future_failure_envelopes() {
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    let too_close = metadata_near_catalog_limit(40);
    let before = catalog.clone();
    assert_eq!(
        catalog.apply_metadata(&id, None, too_close.target_labels),
        Err(InventoryError::ByteLimit)
    );
    assert_eq!(catalog, before);
    let near = metadata_near_catalog_limit(1_500);
    catalog
        .apply_metadata(&id, Some("Europe/Paris".into()), near.target_labels)
        .unwrap();
    catalog
        .apply_collection(
            &id,
            CollectionKind::Scenarios,
            1_000,
            Ok(parse_collection(CollectionKind::Scenarios, b"[]").unwrap()),
        )
        .unwrap();
    for kind in CollectionKind::ALL {
        catalog
            .apply_collection(
                &id,
                kind,
                i64::MAX,
                Err(InventoryError::InconsistentCatalog.into()),
            )
            .unwrap();
    }
    assert!(encode_catalog(&catalog).unwrap().len() <= MAX_CATALOG_BYTES);
    assert_eq!(
        catalog.collections[&CollectionKind::Scenarios].last_observation_ms,
        Some(1_000)
    );
    let mut wrong = id;
    wrong.connection_generation.push('x');
    let before = catalog.clone();
    assert_eq!(
        catalog.apply_metadata(&wrong, None, vec![]),
        Err(InventoryError::IdentityMismatch)
    );
    assert_eq!(catalog, before);
}
#[test]
fn integer_literals_are_exact_or_rejected_without_scanning_quoted_strings() {
    let body = br#"[{"oid":"scene","values":[-9223372036854775808,18446744073709551615,1.25,1e3,1e40,"18446744073709551616","escaped \" 18446744073709551616 \\ end"]}]"#;
    let data = parse_collection(CollectionKind::Scenarios, body).unwrap();
    let numbers = &data.definitions[0].raw.as_value()["values"];
    assert_eq!(numbers[0].as_i64(), Some(i64::MIN));
    assert_eq!(numbers[1].as_u64(), Some(u64::MAX));
    assert_eq!(numbers[2].as_f64(), Some(1.25));
    assert_eq!(numbers[3].as_f64(), Some(1_000.0));
    assert_eq!(numbers[5].as_str(), Some("18446744073709551616"));
    for body in [
        br#"[{"oid":"scene","value":18446744073709551616}]"#.as_slice(),
        br#"[{"oid":"scene","value":-9223372036854775809}]"#,
        br#"[{"oid":"scene","value":1e309}]"#,
    ] {
        assert_eq!(
            parse_collection(CollectionKind::Scenarios, body),
            Err(InventoryError::Malformed)
        );
    }
    let id = identity();
    let mut catalog = Catalog::new(id.clone()).unwrap();
    catalog
        .apply_collection(&id, CollectionKind::Scenarios, 1_000, Ok(data))
        .unwrap();
    let encoded = encode_catalog(&catalog).unwrap();
    assert_eq!(decode_catalog(&encoded).unwrap(), catalog);
    // Cached input follows the same exact-integer policy before serde can round it.
    let valid = String::from_utf8(encoded).unwrap();
    let oversized = valid.replace("18446744073709551615,", "18446744073709551616,");
    assert_ne!(oversized, valid);
    assert_eq!(
        decode_catalog(oversized.as_bytes()),
        Err(InventoryError::Malformed)
    );
}

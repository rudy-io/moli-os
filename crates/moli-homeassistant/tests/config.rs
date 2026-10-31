use moli_homeassistant::{Config, HomeAssistant};
use serde_json::{Value, json};

fn config() -> Value {
    json!({"host":"bridge.example.org","port":8123,"transport":"http",
        "entities":[{"native_id":"lamp-1","entity_id":"light.example","domain":"light",
        "room":"Workshop","safety":"ordinary"}]})
}

fn valid(raw: Value) -> bool {
    serde_json::from_value::<Config>(raw).is_ok_and(|c| HomeAssistant::new(c).is_ok())
}

#[test]
fn read_only_is_default_and_endpoint_choice_is_explicit() {
    let c: Config = serde_json::from_value(config()).unwrap();
    assert!(c.entities[0].writable.is_empty());
    assert!(HomeAssistant::new(c).is_ok());
    let mut raw = config();
    raw.as_object_mut().unwrap().remove("transport");
    assert!(!valid(raw));
}

#[test]
fn rejects_identity_collisions_locators_and_unknown_effects() {
    for native in ["", "a/b", "a:b", "a b", &"x".repeat(65)] {
        let mut raw = config();
        raw["entities"][0]["native_id"] = json!(native);
        assert!(!valid(raw), "accepted native id {native}");
    }
    for entity in [
        "light.x/y",
        "light.all",
        "switch.example",
        "light.x?token=y",
        "light.",
    ] {
        let mut raw = config();
        raw["entities"][0]["entity_id"] = json!(entity);
        assert!(!valid(raw), "accepted locator {entity}");
    }
    let mut raw = config();
    let duplicate = raw["entities"][0].clone();
    raw["entities"].as_array_mut().unwrap().push(duplicate);
    assert!(!valid(raw));
    let mut raw = config();
    raw["entities"][0]["safety"] = json!("unknown");
    raw["entities"][0]["writable"] = json!(["on"]);
    assert!(!valid(raw));
}

#[test]
fn forbids_opaque_domains_credentials_urls_and_unsupported_points() {
    for domain in [
        "scene",
        "script",
        "group",
        "automation",
        "lock",
        "homeassistant",
    ] {
        let mut raw = config();
        raw["entities"][0]["domain"] = json!(domain);
        raw["entities"][0]["entity_id"] = json!(format!("{domain}.example"));
        assert!(!valid(raw));
    }
    for host in [
        "https://bridge.example.org",
        "bridge.example.org/path",
        "user@example.org",
        "x\r\ny",
    ] {
        let mut raw = config();
        raw["host"] = json!(host);
        assert!(!valid(raw));
    }
    let mut raw = config();
    raw["token"] = json!("fixture-secret");
    assert!(!valid(raw));
    for point in ["toggle", "control", "play_media", "on,all"] {
        let mut raw = config();
        raw["entities"][0]["writable"] = json!([point]);
        assert!(!valid(raw));
    }
}

#[test]
fn pin_is_mandatory_for_https_and_forbidden_on_plain_http() {
    let mut raw = config();
    raw["transport"] = json!("pinned_https");
    assert!(!valid(raw.clone()));
    raw["cert_sha256"] = json!("ab".repeat(32));
    assert!(valid(raw.clone()));
    raw["cert_sha256"] = json!("not-a-pin");
    assert!(!valid(raw));
    let mut raw = config();
    raw["cert_sha256"] = json!("ab".repeat(32));
    assert!(!valid(raw));
}

#[test]
fn reviewed_light_may_opt_in_to_only_colour_and_temperature() {
    let mut raw = config();
    raw["entities"][0]["writable"] = json!(["color", "color_temp"]);
    assert!(valid(raw.clone()));
    raw["entities"][0]["safety"] = json!("unknown");
    assert!(!valid(raw));
}

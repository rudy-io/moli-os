//! The room temperature some Echo measure (Echo 4th generation, Dot 5th
//! generation, some Show): Amazon files it as a smart-home sensor tied to the
//! speaker, read through the same unofficial web API as the rest. Read only:
//! nothing here sends an order.

use serde_json::{Value as Json, json};

/// The account's smart-home endpoints, just enough to find the temperature
/// sensors and the speaker each one sits in.
pub(crate) const ENDPOINTS: &str = "query { endpoints(endpointsQueryParams: { paginationParams: { disablePagination: true } }) { items { legacyAppliance { applianceId applianceTypes alexaDeviceIdentifierList } } } }";

/// (speaker serial, sensor id) for each temperature sensor built into a
/// speaker, from the answer to [`ENDPOINTS`].
pub(crate) fn sensors(answer: &Json) -> Vec<(String, String)> {
    answer["data"]["endpoints"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let appliance = &item["legacyAppliance"];
            let thermometer = appliance["applianceTypes"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|t| t.as_str() == Some("TEMPERATURE_SENSOR"));
            if !thermometer {
                return None;
            }
            let id = appliance["applianceId"].as_str()?.to_owned();
            // A raw JSON field: objects, or JSON in strings.
            let serial = appliance["alexaDeviceIdentifierList"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| match d {
                    Json::String(s) => serde_json::from_str::<Json>(s).ok(),
                    other => Some(other.clone()),
                })
                .find_map(|d| d["dmsDeviceSerialNumber"].as_str().map(str::to_owned))?;
            Some((serial, id))
        })
        .collect()
}

/// The GraphQL errors of an answer, in short (to say why nothing was found).
pub(crate) fn errors(answer: &Json) -> Option<String> {
    let all = answer["errors"].as_array()?;
    let said: Vec<&str> = all.iter().filter_map(|e| e["message"].as_str()).collect();
    (!said.is_empty()).then(|| said.join(" ; ").chars().take(300).collect())
}

/// The state request for these sensors.
pub(crate) fn request(ids: &[&str]) -> Json {
    json!({ "stateRequests": ids.iter().map(|id| json!({
        "entityId": id,
        "entityType": "APPLIANCE",
    })).collect::<Vec<_>>() })
}

/// (sensor id, °C) from a state answer. Each capability state comes as a
/// JSON object or as JSON in a string.
pub(crate) fn temperatures(answer: &Json) -> Vec<(String, f64)> {
    answer["deviceStates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|state| {
            let id = state["entity"]["entityId"].as_str()?.to_owned();
            let celsius = state["capabilityStates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| match c {
                    Json::String(s) => serde_json::from_str::<Json>(s).ok(),
                    other => Some(other.clone()),
                })
                .find(|c| {
                    c["namespace"].as_str() == Some("Alexa.TemperatureSensor")
                        && c["name"].as_str() == Some("temperature")
                })
                .and_then(|c| celsius(&c["value"]))?;
            Some((id, celsius))
        })
        .collect()
}

/// `{ "value": 70.7, "scale": "FAHRENHEIT" }` → 21.5, to the tenth.
fn celsius(value: &Json) -> Option<f64> {
    let v = value["value"].as_f64().filter(|v| v.is_finite())?;
    let c = match value["scale"].as_str().unwrap_or("CELSIUS") {
        "FAHRENHEIT" => (v - 32.0) * 5.0 / 9.0,
        "KELVIN" => v - 273.15,
        _ => v,
    };
    // A room, not an oven nor a freezer: anything else is a misreading.
    (-30.0..=60.0)
        .contains(&c)
        .then(|| (c * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_thermometers_inside_a_speaker() {
        let answer = json!({ "data": { "endpoints": { "items": [
            { "legacyAppliance": {
                "applianceId": "AAA_SonarCloudService_00000000-0000-0000-0000-000000000001",
                "applianceTypes": ["TEMPERATURE_SENSOR"],
                "alexaDeviceIdentifierList": [{ "dmsDeviceSerialNumber": "G0911" }] } },
            { "legacyAppliance": {
                "applianceId": "AAA_SonarCloudService_00000000-0000-0000-0000-000000000002",
                "applianceTypes": ["TEMPERATURE_SENSOR"],
                "alexaDeviceIdentifierList": ["{\"dmsDeviceSerialNumber\":\"G0922\"}"] } },
            { "legacyAppliance": {
                "applianceId": "SKILL_lamp", "applianceTypes": ["LIGHT"],
                "alexaDeviceIdentifierList": [] } },
            { "legacyAppliance": {
                "applianceId": "SKILL_probe", "applianceTypes": ["TEMPERATURE_SENSOR"] } },
            { "legacyAppliance": null }
        ] } } });
        let found = sensors(&answer);
        let serials: Vec<&str> = found.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(serials, ["G0911", "G0922"]);
        assert!(found[0].1.ends_with("0001"));
        assert!(sensors(&json!({})).is_empty());
    }

    #[test]
    fn a_refused_query_says_why() {
        let answer = json!({ "errors": [{ "message": "Field 'x' undefined" }], "data": null });
        assert_eq!(errors(&answer).as_deref(), Some("Field 'x' undefined"));
        assert_eq!(errors(&json!({ "data": {} })), None);
    }

    #[test]
    fn the_temperature_in_celsius_whatever_the_scale() {
        let answer = json!({ "deviceStates": [
            { "entity": { "entityId": "A", "entityType": "APPLIANCE" },
              "capabilityStates": [
                "{\"namespace\":\"Alexa.EndpointHealth\",\"name\":\"connectivity\",\"value\":{\"value\":\"OK\"}}",
                "{\"namespace\":\"Alexa.TemperatureSensor\",\"name\":\"temperature\",\"value\":{\"value\":21.46,\"scale\":\"CELSIUS\"}}"
              ] },
            { "entity": { "entityId": "B" },
              "capabilityStates": [{ "namespace": "Alexa.TemperatureSensor", "name": "temperature",
                                     "value": { "value": 68.0, "scale": "FAHRENHEIT" } }] },
            { "entity": { "entityId": "C" },
              "capabilityStates": [{ "namespace": "Alexa.TemperatureSensor", "name": "temperature",
                                     "value": { "value": 999.0, "scale": "CELSIUS" } }] }
        ], "errors": [] });
        assert_eq!(
            temperatures(&answer),
            [("A".to_owned(), 21.5), ("B".to_owned(), 20.0)]
        );
        assert_eq!(
            request(&["A"]),
            json!({ "stateRequests": [{ "entityId": "A", "entityType": "APPLIANCE" }] })
        );
    }
}

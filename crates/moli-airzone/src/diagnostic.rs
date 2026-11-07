//! Explicit read-only diagnostics. Demonstration data never enters the hub.
use crate::{Airzone, Config, api};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::time::Duration;

/// A closed set of read operations on the configured controller.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticRequest {
    Hvac {
        system: u8,
        zone: Option<u8>,
        #[serde(default)]
        get: bool,
    },
    Iaq {
        system: u8,
        sensor: u8,
        #[serde(default)]
        get: bool,
    },
    Version {},
    Webserver {},
    Integration {},
    Demo {},
    Thermostat {},
}

impl DiagnosticRequest {
    /// Validate selectors before even issuing the gateway identity read.
    ///
    /// # Errors
    /// Rejects selectors outside the documented system, zone and sensor ranges.
    pub fn validate(&self) -> anyhow::Result<()> {
        let valid = match self {
            Self::Hvac { system, zone, .. } => match zone {
                Some(zone) => *system <= 32 && *zone <= 32,
                None => *system <= 32 || *system == 127,
            },
            Self::Iaq { system, sensor, .. } => *system <= 32 && *sensor <= 32,
            _ => true,
        };
        anyhow::ensure!(valid, api::Failure::InvalidRequest);
        Ok(())
    }
}

/// Inspect one operation without starting a driver or permitting a write.
///
/// # Errors
/// Returns a bounded error on invalid configuration, identity, addressing,
/// transport, or response. The response removes names and native MAC addresses.
pub async fn inspect(mut config: Config, request: DiagnosticRequest) -> anyhow::Result<Json> {
    request.validate()?;
    config.read_only = true;
    let client = Airzone::new(config)?;
    let config = &client.config;
    let budget = Duration::from_secs(4);
    let identity = api::read(
        config,
        &api::ReadRequest::Webserver,
        api::ReadMethod::Post,
        budget,
    )
    .await?;
    let mac = identity["mac"]
        .as_str()
        .and_then(moli_net::lan::mac_hex)
        .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("airzone.identity")))?;
    anyhow::ensure!(
        config.mac.as_ref().is_none_or(|expected| expected == &mac),
        moli_i18n::tr!("airzone.identity")
    );
    let synthetic = matches!(request, DiagnosticRequest::Demo {});
    let (request, get) = match request {
        DiagnosticRequest::Hvac { system, zone, get } => {
            (api::ReadRequest::Hvac { system, zone }, get)
        }
        DiagnosticRequest::Iaq {
            system,
            sensor,
            get,
        } => (api::ReadRequest::Iaq { system, sensor }, get),
        DiagnosticRequest::Version {} => (api::ReadRequest::Version, false),
        DiagnosticRequest::Webserver {} => {
            return Ok(json!({"read_only":true,"synthetic":false,"response":redact(&identity,0)}));
        }
        DiagnosticRequest::Integration {} => (api::ReadRequest::Integration, false),
        DiagnosticRequest::Demo {} => (api::ReadRequest::Demo, false),
        DiagnosticRequest::Thermostat {} => {
            let html = api::thermostat(config, budget).await?;
            return Ok(
                json!({"read_only":true,"synthetic":false,"interface_available":true,"response_bytes":html.len()}),
            );
        }
    };
    let method = if get {
        api::ReadMethod::Get
    } else {
        api::ReadMethod::Post
    };
    let response = api::read(config, &request, method, budget).await?;
    Ok(json!({"read_only":true,"synthetic":synthetic,"response":redact(&response,0)}))
}

fn redact(value: &Json, depth: usize) -> Json {
    if depth > 8 {
        return Json::Null;
    }
    match value {
        Json::Object(values) => Json::Object(
            values
                .iter()
                .filter_map(|(key, value)| {
                    let known = matches!(
                        key.as_str(),
                        "data" | "systems" | "schema" | "errors" | "error"
                    ) || crate::fields::is_known_field(key);
                    if !known {
                        return None;
                    }
                    let value = if key == "errors" {
                        alarm_codes(value)
                    } else if matches!(key.as_str(), "name" | "mac" | "driver" | "error") {
                        json!("[redacted]")
                    } else {
                        redact(value, depth + 1)
                    };
                    Some((key.clone(), value))
                })
                .take(192)
                .collect(),
        ),
        Json::Array(values) => Json::Array(
            values
                .iter()
                .take(32)
                .map(|v| redact(v, depth + 1))
                .collect(),
        ),
        Json::String(value) => Json::String(
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(240)
                .collect(),
        ),
        other => other.clone(),
    }
}

fn alarm_codes(value: &Json) -> Json {
    let Some(errors) = value.as_array() else {
        return json!("[redacted]");
    };
    Json::Array(
        errors
            .iter()
            .take(32)
            .map(|error| {
                let mut normalized = serde_json::Map::new();
                if let Some(object) = error.as_object() {
                    for (scope, value) in object {
                        let scope = scope.to_ascii_lowercase();
                        if !matches!(scope.as_str(), "zone" | "system") {
                            continue;
                        }
                        let text = value.as_str().unwrap_or("").trim();
                        let code = text
                            .strip_prefix("Error ")
                            .or_else(|| text.strip_prefix("error "))
                            .unwrap_or(text);
                        let known = match code {
                            "3" | "4" | "5" | "6" | "7" | "8" | "9" | "11" | "13" | "14" | "15"
                            | "16" | "C02" | "C09" | "C11" | "IU" | "IAQ1" | "IAQ2" | "IAQ3"
                            | "IAQ4" => code.to_owned(),
                            _ => match code.to_ascii_lowercase().as_str() {
                                "occupancy" => "occupancy",
                                "window" => "window",
                                "anti-freezing" => "anti_freezing",
                                "active dew protection" => "active_dew_protection",
                                "zone without thermostat" => "zone_without_thermostat",
                                "low battery" => "low_battery",
                                "active dew" => "active_dew",
                                _ => "unrecognized",
                            }
                            .to_owned(),
                        };
                        normalized.insert(scope, Json::String(known));
                    }
                }
                Json::Object(normalized)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json as Reply, Router, extract::State, http::Method, routing::any};
    use std::sync::{Arc, Mutex};
    #[test]
    fn diagnostic_keeps_known_alarm_codes_but_redacts_free_text() {
        let value = redact(
            &json!({"errors":[{"Zone":"Error 3"},{"system":"Error C09"},{"zone":"Low battery"},{"zone":"private household text"}]}),
            0,
        );
        assert_eq!(value["errors"][0]["zone"], "3");
        assert_eq!(value["errors"][1]["system"], "C09");
        assert_eq!(value["errors"][2]["zone"], "low_battery");
        assert!(!value.to_string().contains("private household"));
    }
    #[tokio::test]
    async fn diagnostic_reads_are_closed_redacted_and_do_not_publish_demo() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new().fallback(any(|State(calls): State<Arc<Mutex<Vec<String>>>>, method:Method, uri:axum::http::Uri| async move {
            calls.lock().unwrap().push(format!("{method} {}",uri.path()));
            Reply(json!({"mac":"aa:bb:cc:dd:ee:ff","name":"Private room","data":[{"systemID":1,"zoneID":2,"name":"Private room","roomTemp":22.5}]}))
        })).with_state(calls.clone());
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            axum::serve(tcp, app).await.unwrap();
        });
        let config: Config =
            serde_json::from_value(json!({"host":"127.0.0.1","port":port,"read_only":false}))
                .unwrap();
        let report = inspect(config, DiagnosticRequest::Demo {}).await.unwrap();
        assert_eq!(report["synthetic"], true);
        assert_eq!(report["read_only"], true);
        assert_eq!(report["response"]["data"][0]["roomTemp"], 22.5);
        let serialized = report.to_string();
        assert!(!serialized.contains("Private room"));
        assert!(!serialized.contains("aa:bb"));
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| call.starts_with("POST "))
        );
        assert!(
            serde_json::from_value::<DiagnosticRequest>(json!({"operation":"put","path":"hvac"}))
                .is_err()
        );
        task.abort();
    }
}

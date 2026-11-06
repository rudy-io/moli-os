//! Closed Local API operations. PUT is single, addressed and never retried.
use crate::Config;
use anyhow::{Context as _, ensure};
use serde_json::{Value as Json, json};
use std::{
    fmt::{self, Write as _},
    time::Duration,
};

const MAX_HTML: usize = 64 * 1024;
const SELECTORS: [&str; 3] = ["systemID", "zoneID", "iaqsensorID"];

/// Only error categories cross the transport boundary, never a response body,
/// URL or backend error containing a private address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Http(u16),
    Timeout,
    Transport,
    Certificate,
    Api,
    InvalidResponse,
    InvalidRequest,
    ReadOnly,
    ExtendedWritesDisabled,
    Expired,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Http(_) => moli_i18n::tr!("airzone.http_failed"),
            Self::Timeout => moli_i18n::tr!("airzone.timeout"),
            Self::Transport => moli_i18n::tr!("airzone.transport_failed"),
            Self::Certificate => moli_i18n::tr!("airzone.certificate"),
            Self::Api => moli_i18n::tr!("airzone.api_failed"),
            Self::InvalidResponse | Self::InvalidRequest => moli_i18n::tr!("airzone.invalid"),
            Self::ReadOnly => moli_i18n::tr!("airzone.read_only"),
            Self::ExtendedWritesDisabled => moli_i18n::tr!("airzone.extended_writes_disabled"),
            Self::Expired => moli_i18n::tr!("airzone.expired"),
        };
        f.write_str(&text)
    }
}
impl std::error::Error for Failure {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadMethod {
    Post,
    Get,
}

/// The caller selects an operation, never a URL or arbitrary HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadRequest {
    Hvac { system: u8, zone: Option<u8> },
    Iaq { system: u8, sensor: u8 },
    Integration,
    Version,
    Webserver,
    Demo,
}

pub(crate) async fn read(
    config: &Config,
    request: &ReadRequest,
    method: ReadMethod,
    budget: Duration,
) -> anyhow::Result<Json> {
    let (path, body) = match *request {
        ReadRequest::Hvac { system, zone } => {
            let mut body = json!({"systemID":system});
            if let Some(zone) = zone {
                body["zoneID"] = json!(zone);
            }
            ("hvac", body)
        }
        ReadRequest::Iaq { system, sensor } => {
            ("iaq", json!({"systemID":system,"iaqsensorID":sensor}))
        }
        ReadRequest::Integration => ("integration", json!({})),
        ReadRequest::Version => ("version", json!({})),
        ReadRequest::Webserver => ("webserver", json!({})),
        ReadRequest::Demo => ("demo", json!({})),
    };
    let method = match method {
        ReadMethod::Post => "POST",
        ReadMethod::Get => "GET",
    };
    exchange(config, method, path, &body, budget).await
}

fn check_errors(value: &Json) -> anyhow::Result<()> {
    if let Some(errors) = value.get("errors") {
        ensure!(
            errors.is_null()
                || errors.as_array().is_some_and(Vec::is_empty)
                || errors.as_object().is_some_and(serde_json::Map::is_empty),
            Failure::Api
        );
    }
    Ok(())
}

fn id(body: &Json, key: &str, minimum: u64, maximum: u64) -> anyhow::Result<u64> {
    body[key]
        .as_u64()
        .filter(|n| (minimum..=maximum).contains(n))
        .context(Failure::InvalidRequest)
}

fn validate_read(path: &str, body: &Json) -> anyhow::Result<()> {
    let object = body.as_object().context(Failure::InvalidRequest)?;
    match path {
        "hvac" => {
            let system = id(body, "systemID", 0, 127)?;
            if body.get("zoneID").is_some() {
                id(body, "zoneID", 0, 32)?;
                ensure!(system <= 32 && object.len() == 2, Failure::InvalidRequest);
            } else {
                ensure!(
                    (system <= 32 || system == 127) && object.len() == 1,
                    Failure::InvalidRequest
                );
            }
        }
        "iaq" => {
            id(body, "systemID", 0, 32)?;
            id(body, "iaqsensorID", 0, 32)?;
            ensure!(object.len() == 2, Failure::InvalidRequest);
        }
        "integration" | "version" | "webserver" | "demo" => {
            ensure!(object.is_empty(), Failure::InvalidRequest);
        }
        _ => anyhow::bail!(Failure::InvalidRequest),
    }
    Ok(())
}

fn validate_write(path: &str, body: &Json) -> anyhow::Result<()> {
    let object = body.as_object().context(Failure::InvalidRequest)?;
    let parameter = object
        .iter()
        .filter(|(key, _)| !SELECTORS.contains(&key.as_str()))
        .collect::<Vec<_>>();
    ensure!(parameter.len() == 1, Failure::InvalidRequest);
    let (key, value) = parameter[0];
    // Typed parameter validation and capability checks belong to the registry;
    // the transport additionally forbids broadcast, nested and compound writes.
    ensure!(
        value.is_boolean()
            || value.is_number()
            || value
                .as_str()
                .is_some_and(|text| text.chars().count() <= moli_core::MAX_TEXT),
        Failure::InvalidRequest
    );
    match path {
        "hvac" => {
            let system = id(body, "systemID", 0, 32)?;
            ensure!(body.get("iaqsensorID").is_none(), Failure::InvalidRequest);
            if body.get("zoneID").is_some() {
                id(body, "zoneID", 1, 32)?;
                ensure!(system > 0, Failure::InvalidRequest);
            } else if system == 0 {
                ensure!(
                    matches!(key.as_str(), "acs_power" | "acs_powerful" | "acs_setpoint"),
                    Failure::InvalidRequest
                );
            }
        }
        "iaq" => {
            id(body, "systemID", 1, 32)?;
            id(body, "iaqsensorID", 1, 32)?;
            ensure!(body.get("zoneID").is_none(), Failure::InvalidRequest);
            ensure!(
                matches!(key.as_str(), "name" | "iaq_mode_vent" | "IAQ_Mode_Vent"),
                Failure::InvalidRequest
            );
        }
        "integration" => {
            ensure!(
                object.len() == 1 && key == "driver" && value.is_string(),
                Failure::InvalidRequest
            );
        }
        _ => anyhow::bail!(Failure::InvalidRequest),
    }
    Ok(())
}

fn query(path: &str, body: &Json) -> String {
    let mut uri = format!("/api/v1/{path}?systemid={}", body["systemID"]);
    for (key, query_key) in [("zoneID", "zoneid"), ("iaqsensorID", "iaqsensorid")] {
        if let Some(value) = body.get(key) {
            let _ = write!(uri, "&{query_key}={value}");
        }
    }
    uri
}

fn transport(error: &anyhow::Error) -> Failure {
    if error
        .chain()
        .any(|cause| cause.to_string().contains(moli_net::PIN_MISMATCH))
    {
        Failure::Certificate
    } else if error
        .chain()
        .any(<dyn std::error::Error + 'static>::is::<tokio::time::error::Elapsed>)
    {
        Failure::Timeout
    } else {
        Failure::Transport
    }
}

pub(crate) async fn exchange(
    config: &Config,
    method: &str,
    path: &str,
    body: &Json,
    budget: Duration,
) -> anyhow::Result<Json> {
    ensure!(!budget.is_zero(), Failure::Expired);
    match method {
        "PUT" => {
            ensure!(!config.read_only, Failure::ReadOnly);
            validate_write(path, body)?;
            ensure!(
                config.write.allows_wire(path, body),
                Failure::ExtendedWritesDisabled
            );
        }
        "GET" => {
            ensure!(matches!(path, "hvac" | "iaq"), Failure::InvalidRequest);
            validate_read(path, body)?;
        }
        "POST" => validate_read(path, body)?,
        _ => anyhow::bail!(Failure::InvalidRequest),
    }
    let uri = if method == "GET" {
        query(path, body)
    } else {
        format!("/api/v1/{path}")
    };
    let builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{}:{}", config.host, config.port));
    let request = if method == "GET" {
        moli_net::empty(builder)?
    } else {
        moli_net::json_body(builder, body)?
    };
    let operation = async {
        if let Some(pin) = &config.tls_fingerprint {
            moli_net::PinnedHttps::new(&config.host, config.port, Some(pin.clone()))?
                .send_once(request)
                .await
        } else {
            moli_net::plain(&config.host, config.port, request).await
        }
    };
    let (status, bytes) = tokio::time::timeout(budget, operation)
        .await
        .map_err(|_| Failure::Timeout)?
        .map_err(|error| transport(&error))?;
    ensure!(status.is_success(), Failure::Http(status.as_u16()));
    let value: Json = serde_json::from_slice(&bytes).map_err(|_| Failure::InvalidResponse)?;
    ensure!(value.is_object(), Failure::InvalidResponse);
    check_errors(&value)?;
    Ok(value)
}

/// The manufacturer's HTML is diagnostic data only, never a Moli view. A
/// separate transport cap prevents a thermostat page consuming the JSON limit.
pub(crate) async fn thermostat(config: &Config, budget: Duration) -> anyhow::Result<String> {
    ensure!(!budget.is_zero(), Failure::Expired);
    let request = moli_net::empty(
        http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", format!("{}:{}", config.host, config.port)),
    )?;
    let operation = async {
        if let Some(pin) = &config.tls_fingerprint {
            moli_net::PinnedHttps::new(&config.host, config.port, Some(pin.clone()))?
                .send_once(request)
                .await
        } else {
            moli_net::web(&config.host, config.port, false, request, budget, MAX_HTML).await
        }
    };
    let (status, bytes) = tokio::time::timeout(budget, operation)
        .await
        .map_err(|_| Failure::Timeout)?
        .map_err(|error| transport(&error))?;
    ensure!(status.is_success(), Failure::Http(status.as_u16()));
    // PinnedHttps also bounds its read (MAX_BODY); the accepted HTML is capped
    // further here. It never enters the live device model or a browser renderer.
    ensure!(bytes.len() <= MAX_HTML, Failure::InvalidResponse);
    String::from_utf8(bytes.to_vec()).map_err(|_| Failure::InvalidResponse.into())
}

pub(crate) fn acknowledge(value: &Json, command: &Json) -> anyhow::Result<()> {
    check_errors(value)?;
    let integration = command.get("driver").is_some();
    let data = match &value["data"] {
        Json::Object(_) => &value["data"],
        Json::Array(a) if a.len() == 1 => &a[0],
        Json::Null if integration && value.is_object() => value,
        _ => anyhow::bail!(Failure::InvalidResponse),
    };
    ensure!(data.is_object(), Failure::InvalidResponse);
    // An echoed alias is still an echoed value: never treat a differently
    // cased parameter as an absent acknowledgement and accidentally accept it.
    let data = crate::fields::normalize(data).map_err(|_| Failure::InvalidResponse)?;
    let command = crate::fields::normalize(command).map_err(|_| Failure::InvalidRequest)?;
    for key in SELECTORS {
        ensure!(data.get(key) == command.get(key), Failure::InvalidResponse);
    }
    let object = command.as_object().context(Failure::InvalidRequest)?;
    for (key, want) in object {
        if SELECTORS.contains(&key.as_str()) {
            continue;
        }
        if let Some(actual) = data.get(key) {
            let equal = actual == want
                || actual
                    .as_f64()
                    .zip(want.as_f64())
                    .is_some_and(|(a, b)| (a - b).abs() < 1e-6);
            ensure!(equal, Failure::InvalidResponse);
        } else {
            // Older HVAC firmware sometimes acknowledges IDs alone; integration
            // has no address IDs, so its echoed driver string is mandatory.
            ensure!(!integration, Failure::InvalidResponse);
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;

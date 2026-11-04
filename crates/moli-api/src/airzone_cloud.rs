//! Human-only, optional Airzone Web API reads. Credentials never enter Debug.
use crate::{airzone_setup::Setup, caller::Caller, rest::origin, session::Sessions};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use moli_core::Origin;
use moli_runtime::Hub;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}
impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AirzoneCloudLogin([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSection {
    All,
    User,
    Advanced,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadRequest {
    Installations {},
    Installation {
        installation_id: String,
    },
    DeviceStatus {
        installation_id: String,
        device_id: String,
    },
    DeviceConfig {
        installation_id: String,
        device_id: String,
        section: ConfigSection,
    },
    GatewayStatus {
        installation_id: String,
        gateway_id: String,
        #[serde(default)]
        include_devices: bool,
    },
    Location {
        location_id: String,
    },
    Profile {},
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}

#[derive(Clone, Debug, Serialize)]
pub struct CloudError {
    pub error: &'static str,
}
impl CloudError {
    pub fn new(error: &'static str) -> Self {
        Self { error }
    }
}
impl IntoResponse for CloudError {
    fn into_response(self) -> Response {
        let status = match self.error {
            "invalid_input" => StatusCode::UNPROCESSABLE_ENTITY,
            "authentication_required" | "forbidden" => StatusCode::FORBIDDEN,
            "migration_pending" => StatusCode::CONFLICT,
            "rate_limited" | "busy" => StatusCode::TOO_MANY_REQUESTS,
            "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
            "storage" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_GATEWAY,
        };
        (status, Json(self)).into_response()
    }
}

pub(crate) async fn status(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(setup) = setup else {
        return CloudError::new("unavailable").into_response();
    };
    match setup.cloud_status().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn login(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(setup) = setup else {
        return CloudError::new("unavailable").into_response();
    };
    match setup.cloud_login(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn read(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<ReadRequest>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(setup) = setup else {
        return CloudError::new("unavailable").into_response();
    };
    match setup.cloud_read(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn disconnect(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
    Json(_request): Json<Empty>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(setup) = setup else {
        return CloudError::new("unavailable").into_response();
    };
    match setup.cloud_disconnect().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

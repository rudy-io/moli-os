//! Human-only Airzone installation. The executable owns persistence and driver
//! lifetime; this surface accepts no arbitrary configuration or device command.

use std::fmt::Debug;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::Origin;
use moli_runtime::{BoxFuture, Hub};
use serde::{Deserialize, Serialize};

use crate::{caller::Caller, rest::origin, session::Sessions};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayRequest {
    pub instance: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls_fingerprint: Option<String>,
    #[serde(default)]
    pub expected_mac: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GatewayPreview {
    pub instance: String,
    pub mac: String,
    pub gateway_type: Option<String>,
    pub api_version: Option<String>,
    pub firmware: Option<String>,
    pub systems: usize,
    pub zones: usize,
    pub incomplete: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct GatewayInstalled {
    pub instance: String,
    pub stored: bool,
    pub read_only: bool,
    pub already_installed: bool,
}

/// Stable machine codes; the dashboard translates them without rendering
/// upstream responses, addresses, credentials or filesystem errors.
#[derive(Clone, Debug, Serialize)]
pub struct SetupError {
    #[serde(rename = "error")]
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_instance: Option<String>,
}

impl SetupError {
    pub fn new(code: &'static str) -> Self {
        Self {
            code,
            existing_instance: None,
        }
    }

    pub fn conflict(code: &'static str, instance: &str) -> Self {
        Self {
            code,
            existing_instance: Some(instance.to_owned()),
        }
    }
}

impl IntoResponse for SetupError {
    fn into_response(self) -> Response {
        let status = match self.code {
            "duplicate_instance" | "duplicate_gateway" | "identity_changed" => StatusCode::CONFLICT,
            "busy" | "limit_reached" => StatusCode::TOO_MANY_REQUESTS,
            "probe_failed" => StatusCode::BAD_GATEWAY,
            "persistence_failed" => StatusCode::INTERNAL_SERVER_ERROR,
            "unavailable" | "shutting_down" | "identity_unavailable" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        (status, Json(self)).into_response()
    }
}

pub trait AirzoneSetup: Send + Sync + Debug {
    fn probe(&self, request: GatewayRequest) -> BoxFuture<'_, Result<GatewayPreview, SetupError>>;
    fn install(
        &self,
        request: GatewayRequest,
        actor: String,
    ) -> BoxFuture<'_, Result<GatewayInstalled, SetupError>>;
}

#[derive(Clone)]
pub(crate) struct Setup(pub Option<Arc<dyn AirzoneSetup>>);

pub(crate) async fn probe(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<GatewayRequest>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(setup) = setup else {
        return SetupError::new("unavailable").into_response();
    };
    match setup.probe(request).await {
        Ok(preview) => Json(preview).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn install(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<GatewayRequest>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(setup) = setup else {
        return SetupError::new("unavailable").into_response();
    };
    match setup.install(request, caller.describe()).await {
        Ok(installed) => Json(installed).into_response(),
        Err(error) => error.into_response(),
    }
}

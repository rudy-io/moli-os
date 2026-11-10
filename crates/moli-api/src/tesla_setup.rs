//! Owner-only Tesla account setup: status, sign-in, sign-out.
//!
//! All three routes are owner-only: UI origin plus the owner role; the two
//! writes additionally require a fresh PIN in the body. The sign-in code is
//! handed to the driver as a redacted command — it reaches the driver but is
//! journaled as `Value::Null`, so it never appears in the journal,
//! `get_journal`, or the approvals list.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::{DeviceId, InstanceId, Origin, PointId, Value};
use moli_runtime::{CommandError, Hub};
use serde::Serialize;

use crate::{
    caller::Caller,
    people::People,
    people_api::is_owner,
    rest::origin,
    session::{LoginError, Sessions},
};

/// The account device key the Tesla driver publishes (one account per
/// driver instance, as it does for its cars).
const ACCOUNT: &str = "account";

/// Machine codes; the dashboard translates them without rendering the
/// driver's raw error strings.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TeslaSetupError {
    #[serde(rename = "error")]
    code: &'static str,
}

impl TeslaSetupError {
    fn new(code: &'static str) -> Self {
        Self { code }
    }
}

impl IntoResponse for TeslaSetupError {
    fn into_response(self) -> Response {
        let status = match self.code {
            "owner_required" | "pin_required" | "pin_disabled" => StatusCode::FORBIDDEN,
            "invalid_pin" => StatusCode::UNAUTHORIZED,
            "pin_rate_limited" | "driver_busy" => StatusCode::TOO_MANY_REQUESTS,
            "driver_not_running" => StatusCode::SERVICE_UNAVAILABLE,
            "timeout" => StatusCode::REQUEST_TIMEOUT,
            "driver_refused" => StatusCode::BAD_GATEWAY,
            "invalid" => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self)).into_response()
    }
}

/// The gate shared by every route of this surface: the dashboard of a human
/// who is an owner of this house. A PIN is checked separately on writes.
#[allow(clippy::result_large_err)]
fn gate(
    hub: &Hub,
    headers: &HeaderMap,
    humans: &Sessions,
    persons: &People,
    caller: &Caller,
) -> Result<(), Response> {
    if origin(hub, headers, humans, caller) != Origin::Ui {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    if !is_owner(caller, humans, persons, headers) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(TeslaSetupError::new("owner_required")),
        )
            .into_response());
    }
    Ok(())
}

/// A fresh PIN in the body, checked against the caller's own budget.
#[allow(clippy::result_large_err)]
fn check_pin(humans: &Sessions, caller: &Caller, pin: &str) -> Result<(), Response> {
    humans.verify(pin, caller).map_err(|error| {
        let (status, code) = match error {
            LoginError::Wrong => (StatusCode::UNAUTHORIZED, "invalid_pin"),
            LoginError::Locked => (StatusCode::TOO_MANY_REQUESTS, "pin_rate_limited"),
            LoginError::Disabled => (StatusCode::FORBIDDEN, "pin_disabled"),
        };
        (status, Json(TeslaSetupError::new(code))).into_response()
    })
}

fn account_point(instance: &str, key: &str) -> PointId {
    PointId::new(&DeviceId::new(&InstanceId::from(instance), ACCOUNT), key)
}

#[allow(clippy::needless_pass_by_value)]
fn command_error_response(error: CommandError) -> Response {
    let code = match &error {
        CommandError::UnknownPoint | CommandError::DriverUnavailable => "driver_not_running",
        CommandError::Busy => "driver_busy",
        CommandError::Timeout => "timeout",
        CommandError::Driver(_) => "driver_refused",
        CommandError::Invalid(_) | CommandError::ReadOnly | CommandError::NeedsApproval { .. } => {
            "invalid"
        }
    };
    TeslaSetupError::new(code).into_response()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Text(t) => t.as_ref().to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Null => String::new(),
    }
}

/// `GET /api/integrations/tesla/{instance}`: the account's status point and
/// the sign-in page address, when the driver is waiting for a sign-in.
/// Read-only, so it needs the owner session but no fresh PIN.
pub(crate) async fn status(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Extension<Arc<People>>,
    Path(instance): Path<String>,
    caller: Caller,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = gate(&hub, &headers, &humans, &persons, &caller) {
        return response;
    }
    let status_sample = hub.state(&account_point(&instance, "status"));
    let sign_in_url = hub
        .state(&account_point(&instance, "sign_in_url"))
        .map(|s| value_text(&s.value))
        .unwrap_or_default();
    let Some(status_sample) = status_sample else {
        // The account device is published by the driver: without it, the
        // driver is not running (or the instance is unknown).
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(TeslaSetupError::new("driver_not_running")),
        )
            .into_response();
    };
    Json(serde_json::json!({
        "instance": instance,
        "status": value_text(&status_sample.value),
        "sign_in_url": sign_in_url,
    }))
    .into_response()
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignInRequest {
    pub(crate) code: String,
    #[serde(default)]
    pub(crate) pin: Option<String>,
}

/// `POST /api/integrations/tesla/{instance}/sign-in`: hands the pasted
/// sign-in address (or bare code) to the driver as a redacted command.
pub(crate) async fn sign_in(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Extension<Arc<People>>,
    Path(instance): Path<String>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<SignInRequest>,
) -> Response {
    if let Err(response) = gate(&hub, &headers, &humans, &persons, &caller) {
        return response;
    }
    if request.code.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(TeslaSetupError::new("invalid")),
        )
            .into_response();
    }
    let Some(pin) = request.pin.as_deref() else {
        return (
            StatusCode::FORBIDDEN,
            Json(TeslaSetupError::new("pin_required")),
        )
            .into_response();
    };
    if let Err(response) = check_pin(&humans, &caller, pin) {
        return response;
    }
    let point = account_point(&instance, "sign_in");
    match hub
        .command_redacted(
            &point,
            Value::Text(request.code.into()),
            Origin::Ui,
            Some(caller.describe()),
        )
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => command_error_response(error),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignOutRequest {
    #[serde(default)]
    pub(crate) pin: Option<String>,
}

/// `POST /api/integrations/tesla/{instance}/sign-out`: the driver erases the
/// refresh token from the vault and goes back to the waiting state.
pub(crate) async fn sign_out(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Extension<Arc<People>>,
    Path(instance): Path<String>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<SignOutRequest>,
) -> Response {
    if let Err(response) = gate(&hub, &headers, &humans, &persons, &caller) {
        return response;
    }
    let Some(pin) = request.pin.as_deref() else {
        return (
            StatusCode::FORBIDDEN,
            Json(TeslaSetupError::new("pin_required")),
        )
            .into_response();
    };
    if let Err(response) = check_pin(&humans, &caller, pin) {
        return response;
    }
    let point = account_point(&instance, "sign_out");
    // The value travels redacted too: a sign-out is a credential-shaped
    // action, and the driver needs no payload to act on it.
    match hub
        .command_redacted(
            &point,
            Value::Text(Arc::from("")),
            Origin::Ui,
            Some(caller.describe()),
        )
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => command_error_response(error),
    }
}

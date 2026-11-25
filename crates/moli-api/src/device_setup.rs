//! A device added from the dashboard by its address: Moli asks it who it is
//! (one read), a person names it, places it and adds it. Humans only; the
//! executable owns what is stored and the driver's lifetime. A key typed
//! here goes to the vault and never comes back out.

use std::fmt::Debug;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::Origin;
use moli_runtime::{BoxFuture, Hub};
use serde::{Deserialize, Serialize};

pub use crate::airzone_setup::SetupError;
use crate::{caller::Caller, rest::origin, session::Sessions};

/// Longest key a person may type (a Meross key is 32 characters).
pub const MAX_KEY: usize = 128;

/// « Who is at this address? » With the key the person typed, when the
/// device refused the ones the house holds.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProbe {
    pub host: String,
    #[serde(default)]
    pub key: Option<String>,
}

impl Debug for DeviceProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceProbe")
            .field("host", &self.host)
            .field("key", &self.key.as_ref().map(|_| "…"))
            .finish()
    }
}

/// What answered. `status`: `recognized`, `needs_key` (a device Moli knows,
/// bound to an account whose key it does not hold), `unknown` (it answered,
/// nothing Moli recognizes) or `silent` (nobody answered).
#[derive(Clone, Debug, Default, Serialize)]
pub struct DevicePreview {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The kind of device, in words (« Prise connectée Meross avec mesure »).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// As the device names itself (`mss315`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// Which key it took: `none` (needs none), `empty` (none set), `known`
    /// (one the house already holds), `given` (the one just typed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<&'static str>,
    /// Already in the house: the instance that runs it, and its device.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_device: Option<String>,
}

/// « Add it »: the identity the person saw, the name and room they chose.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInstall {
    pub host: String,
    pub identity: String,
    pub name: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

impl Debug for DeviceInstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceInstall")
            .field("host", &self.host)
            .field("identity", &self.identity)
            .field("name", &self.name)
            .field("room", &self.room)
            .field("key", &self.key.as_ref().map(|_| "…"))
            .finish()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceInstalled {
    pub instance: String,
    pub device: String,
    /// The name and room are on the device (it answered in time).
    pub labelled: bool,
}

pub trait DeviceSetup: Send + Sync + Debug {
    fn probe(&self, request: DeviceProbe) -> BoxFuture<'_, Result<DevicePreview, SetupError>>;
    fn install(
        &self,
        request: DeviceInstall,
        actor: String,
    ) -> BoxFuture<'_, Result<DeviceInstalled, SetupError>>;
}

#[derive(Clone)]
pub(crate) struct Setup(pub Option<Arc<dyn DeviceSetup>>);

fn key_is_valid(key: Option<&String>) -> bool {
    key.is_none_or(|key| {
        !key.is_empty() && key.len() <= MAX_KEY && !key.chars().any(char::is_control)
    })
}

pub(crate) async fn probe(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(Setup(setup)): Extension<Setup>,
    caller: Caller,
    headers: HeaderMap,
    Json(request): Json<DeviceProbe>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !key_is_valid(request.key.as_ref()) {
        return SetupError::new("invalid_key").into_response();
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
    Json(request): Json<DeviceInstall>,
) -> Response {
    if origin(&hub, &headers, &humans, &caller) != Origin::Ui {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !key_is_valid(request.key.as_ref()) {
        return SetupError::new("invalid_key").into_response();
    }
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > moli_runtime::MAX_LABEL {
        return SetupError::new("invalid_name").into_response();
    }
    if request
        .room
        .as_deref()
        .is_some_and(|room| room.trim().chars().count() > moli_runtime::MAX_LABEL)
    {
        return SetupError::new("invalid_room").into_response();
    }
    let Some(setup) = setup else {
        return SetupError::new("unavailable").into_response();
    };
    match setup.install(request, caller.describe()).await {
        Ok(installed) => Json(installed).into_response(),
        Err(error) => error.into_response(),
    }
}

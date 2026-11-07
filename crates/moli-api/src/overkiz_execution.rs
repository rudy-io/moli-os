//! Owner consent only; physical launches continue through the existing Hub command API.
use super::{Cached, Owner, changed, enqueue, error};
use axum::{
    extract::{Path, Request},
    http::StatusCode,
    response::Response,
};
use moli_core::InstanceId;
use moli_overkiz::cloud;
use moli_overkiz::cloud::execution::{self, Policy};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionRequest {
    generation: String,
    revision: String,
    scenario_id: String,
    enabled: bool,
    pin: String,
}

pub(super) async fn update(
    owner: Owner,
    Path(instance): Path<String>,
    request: Request,
) -> Response {
    let (id, config) = match owner.instance(&instance) {
        Ok(value) => value,
        Err((status, code)) => return error(status, code),
    };
    // A body can wait across DELETE or reconnection. Keep the original lifetime
    // and never enqueue its consent under a replacement generation.
    let state = owner.hub.private_state(&id);
    if owner.cleanup_pending(&id) {
        return super::cleanup_pending();
    }
    let Some(state) = state else {
        return super::unavailable();
    };
    let Ok(body) = axum::body::to_bytes(request.into_body(), cloud::MAX_PENDING_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "invalid_request");
    };
    let Ok(request) = serde_json::from_slice::<PermissionRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if let Err(pin_error) = owner.sessions.verify(&request.pin, &owner.caller) {
        return match pin_error {
            crate::session::LoginError::Wrong => error(StatusCode::UNAUTHORIZED, "invalid_pin"),
            crate::session::LoginError::Locked => {
                error(StatusCode::TOO_MANY_REQUESTS, "pin_rate_limited")
            }
            crate::session::LoginError::Disabled => error(StatusCode::FORBIDDEN, "pin_disabled"),
        };
    }
    if request.enabled && config.read_only {
        return error(StatusCode::FORBIDDEN, "global_read_only");
    }
    let cached = match owner.cached(&instance) {
        Ok(cached) => cached,
        Err((status, code)) => return error(status, code),
    };
    if request.generation != cached.catalog.connection_generation
        || (request.enabled && request.revision != cached.revision)
    {
        return changed();
    }
    if request.enabled && cached.freshness["collections"]["scenarios"] != true {
        return changed();
    }
    let mut trial = Policy::default();
    if trial
        .change(&cached.catalog, &request.scenario_id, request.enabled)
        .is_err()
    {
        return error(StatusCode::BAD_REQUEST, "not_supported");
    }
    let queued = cloud::PendingRequest::ScenarioPermission {
        generation: request.generation,
        revision: request.revision,
        scenario_id: request.scenario_id,
        enabled: request.enabled,
    };
    enqueue(&owner, &id, &state, queued).await
}

impl Owner {
    pub(super) fn execution(&self, cached: &Cached) -> execution::ExecutionView {
        let instance = InstanceId::from(cached.catalog.instance.clone());
        let policy = self
            .record(&instance, execution::POLICY_KEY)
            .and_then(|value| Policy::decode(value.as_bytes()).ok())
            .unwrap_or_default();
        let mut view = execution::view_at_revision(
            &cached.catalog,
            &policy,
            cached.read_only,
            self.online(&instance),
            cached.freshness["collections"]["scenarios"] == true,
            &cached.revision,
        );
        for status in &mut view.scenarios {
            if !status.executable {
                continue;
            }
            let current=status.point.split().and_then(|(id,key)|self.hub.device(&id).map(|device|(device,key.to_owned())))
                .is_some_and(|(device,key)|device.online == Some(true) && device.device.point(&key).is_some_and(|point|
                    point.access.write && matches!(&point.kind,moli_core::Kind::Enum{values} if values.iter().any(|v|v.as_ref()==status.value))));
            if !current {
                status.executable = false;
                status.reason = Some("stale");
            }
        }
        view
    }
}

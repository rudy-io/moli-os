//! Owner-only cached observations. Only the driver communicates with Somfy.
use crate::{caller::Caller, people::People, people_api, session::Sessions};
use axum::{
    Extension, Json, Router,
    extract::{FromRequestParts, Path, Query, Request},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use moli_core::InstanceId;
use moli_overkiz::{cloud, inventory};
use moli_runtime::Hub;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub(crate) struct Configured(pub Arc<BTreeMap<InstanceId, cloud::Config>>);
struct Owner {
    hub: Hub,
    configured: Configured,
}
impl FromRequestParts<Hub> for Owner {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, hub: &Hub) -> Result<Self, Self::Rejection> {
        let caller = Caller::from_request_parts(parts, hub)
            .await
            .map_err(IntoResponse::into_response)?;
        let Extension(humans) = Extension::<Arc<Sessions>>::from_request_parts(parts, hub)
            .await
            .map_err(IntoResponse::into_response)?;
        let Extension(persons) = Extension::<Arc<People>>::from_request_parts(parts, hub)
            .await
            .map_err(IntoResponse::into_response)?;
        if !people_api::is_owner(&caller, &humans, &persons, &parts.headers) {
            let policy = if !persons.has_owner() && !humans.has_owners() && caller.is_household() {
                "pin_fallback"
            } else {
                "owner_account"
            };
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({"error":"owner_required", "auth_policy":policy})),
            )
                .into_response());
        }
        let Extension(configured) = Extension::<Configured>::from_request_parts(parts, hub)
            .await
            .map_err(IntoResponse::into_response)?;
        Ok(Self {
            hub: hub.clone(),
            configured,
        })
    }
}
pub(crate) fn routes() -> Router<Hub> {
    Router::new()
        .route("/api/integrations/overkiz-cloud", get(list))
        .route(
            "/api/integrations/overkiz-cloud/{instance}/catalog",
            get(catalog),
        )
        .route(
            "/api/integrations/overkiz-cloud/{instance}/definitions/{collection}/{index}",
            get(definition),
        )
        .route(
            "/api/integrations/overkiz-cloud/{instance}/requests",
            post(request),
        )
}
fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}
fn unavailable() -> Response {
    error(StatusCode::SERVICE_UNAVAILABLE, "unavailable")
}
fn changed() -> Response {
    error(StatusCode::CONFLICT, "catalog_changed")
}
impl Owner {
    fn instance(
        &self,
        name: &str,
    ) -> Result<(InstanceId, &cloud::Config), (StatusCode, &'static str)> {
        let instance = InstanceId::from(name);
        let config = self
            .configured
            .0
            .get(&instance)
            .filter(|config| config.validate().is_ok())
            .ok_or((StatusCode::NOT_FOUND, "instance_not_found"))?;
        Ok((instance, config))
    }
    fn binding(&self, instance: &InstanceId) -> Option<cloud::SessionBinding> {
        let body = self.hub.secret(instance, cloud::SESSION_KEY)?;
        cloud::read_session_binding(body.as_bytes(), instance.as_str()).ok()
    }
    fn state(
        &self,
        instance: &InstanceId,
        binding: Option<&cloud::SessionBinding>,
    ) -> cloud::ConnectionState {
        let saved = self.hub.secret(instance, cloud::STATE_KEY);
        let decoded = saved
            .as_ref()
            .and_then(|body| cloud::decode_state(body.as_bytes()).ok());
        let Some(state) = decoded else {
            return cloud::ConnectionState {
                failure: saved.map(|_| cloud::Error::InvalidResponse),
                ..Default::default()
            };
        };
        if state.generation.is_none() && binding.is_none() {
            return state;
        }
        if binding.is_some_and(|b| {
            state.generation.as_ref() == Some(&b.generation) && state.scope == b.scope
        }) {
            return state;
        }
        // A fresh persisted login marker supersedes the old state, even across a crash.
        cloud::ConnectionState {
            phase: cloud::ConnectionPhase::PersistencePending,
            generation: binding.map(|b| b.generation.clone()),
            ..Default::default()
        }
    }
    fn online(&self, instance: &InstanceId) -> bool {
        self.hub
            .device(&cloud::summary_device_id(instance))
            .is_some_and(|d| d.online == Some(true))
    }
    fn cached(&self, name: &str) -> Result<Cached, (StatusCode, &'static str)> {
        let (instance, config) = self.instance(name)?;
        let binding = self
            .binding(&instance)
            .ok_or((StatusCode::CONFLICT, "catalog_changed"))?;
        let bytes = self
            .hub
            .secret(&instance, inventory::INVENTORY_KEY)
            .ok_or((StatusCode::NOT_FOUND, "not_observed"))?;
        let catalog = inventory::decode_catalog(bytes.as_bytes())
            .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "unavailable"))?;
        if catalog.instance != name
            || catalog.connection_generation != binding.generation
            || Some(&catalog.scope) != binding.scope.as_ref()
        {
            return Err((StatusCode::CONFLICT, "catalog_changed"));
        }
        let state = self.state(&instance, Some(&binding));
        let current = self.online(&instance)
            && binding.phase == cloud::SessionPhase::Ready
            && state.phase == cloud::ConnectionPhase::Ready
            && state.failure.is_none()
            && recent(state.last_observation_ms, config.poll_secs);
        let fresh_collections: BTreeMap<_, _> = catalog
            .collections
            .iter()
            .map(|(kind, collection)| {
                (
                    *kind,
                    current
                        && collection.status == inventory::CollectionStatus::Available
                        && recent(collection.last_observation_ms, config.poll_secs),
                )
            })
            .collect();
        Ok(Cached {
            revision: hex(digest::digest(&digest::SHA256, bytes.as_bytes()).as_ref()),
            catalog,
            freshness: json!({"current":current,"observed_at_ms":state.last_observation_ms,"collections":fresh_collections}),
        })
    }
}
struct Cached {
    catalog: inventory::Catalog,
    revision: String,
    freshness: serde_json::Value,
}
fn recent(timestamp: Option<i64>, poll_secs: u64) -> bool {
    timestamp
        .and_then(|t| u64::try_from(t).ok())
        .is_some_and(|t| {
            let now = moli_core::now_ms();
            t <= now && now - t <= poll_secs.saturating_mul(1000).saturating_add(90_000)
        })
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}
async fn list(owner: Owner) -> Response {
    let instances: Vec<_> = owner
        .configured
        .0
        .iter()
        .filter(|(_, c)| c.validate().is_ok())
        .map(|(instance, _)| {
            let binding = owner.binding(instance);
            let state = owner.state(instance, binding.as_ref());
            let online = owner.online(instance)
                && binding.as_ref().is_some_and(|b| {
                    state.generation.as_ref() == Some(&b.generation)
                        && state.scope == b.scope
                        && b.phase == cloud::SessionPhase::Ready
                });
            let session_phase = binding.as_ref().map(|binding| binding.phase);
            json!({"instance":instance,"state":state,"online":online,"session_phase":session_phase})
        })
        .collect();
    Json(json!({"instances":instances})).into_response()
}
async fn catalog(owner: Owner, Path(instance): Path<String>) -> Response {
    match owner.cached(&instance) {
        Ok(cached) => Json(json!({"catalog":cached.catalog,"revision":cached.revision,"freshness":cached.freshness})).into_response(),
        Err((status, code)) => error(status, code),
    }
}
#[derive(Deserialize)]
struct Revision {
    revision: String,
}
async fn definition(
    owner: Owner,
    Path((instance, collection, index)): Path<(String, inventory::CollectionKind, usize)>,
    query: Result<Query<Revision>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let cached = match owner.cached(&instance) {
        Ok(cached) => cached,
        Err((status, code)) => return error(status, code),
    };
    if query.revision != cached.revision {
        return changed();
    }
    let Some(definition) = cached
        .catalog
        .collections
        .get(&collection)
        .and_then(|c| c.data.as_ref())
        .and_then(|d| d.definitions.get(index))
    else {
        return error(StatusCode::NOT_FOUND, "definition_not_found");
    };
    // Rust serializes the original numeric value; the browser must use response.text().
    match serde_json::to_string(&definition.raw) {
        Ok(text) => (
            [(
                http::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )],
            text,
        )
            .into_response(),
        Err(_) => unavailable(),
    }
}
async fn request(owner: Owner, Path(instance): Path<String>, request: Request) -> Response {
    let instance = match owner.instance(&instance) {
        Ok((instance, _)) => instance,
        Err((status, code)) => return error(status, code),
    };
    let Ok(body) = axum::body::to_bytes(request.into_body(), cloud::MAX_PENDING_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "invalid_request");
    };
    let request: cloud::PendingRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    let mut random = [0_u8; 16];
    if SystemRandom::new().fill(&mut random).is_err() {
        return unavailable();
    }
    let envelope = cloud::RequestEnvelope {
        version: 1,
        request_id: hex(&random),
        submitted_at_ms: i64::try_from(moli_core::now_ms()).unwrap_or(i64::MAX),
        request,
    };
    let Some(bytes) = cloud::encode_request(&envelope)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
    else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match owner
        .hub
        .insert_secret_if_absent(&instance, cloud::PENDING_KEY, &bytes)
        .await
    {
        Ok(true) => (
            StatusCode::ACCEPTED,
            Json(json!({"request_id":envelope.request_id})),
        )
            .into_response(),
        Ok(false) => error(StatusCode::CONFLICT, "busy"),
        Err(_) => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller::Via;

    #[tokio::test]
    async fn access_owner_and_lan_denials_use_the_existing_policy_without_a_vault() {
        let hub = Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let owners = ["owner@example.org".to_owned()];
        let humans = Arc::new(Sessions::new(
            hub.clone(),
            Some(crate::UiPin::new("482915")),
            &owners,
        ));
        let persons = Arc::new(People::open(hub.clone(), None, &owners));
        for (via, allowed) in [
            (Via::Lan, false),
            (
                Via::Access {
                    email: "member@example.org".into(),
                },
                false,
            ),
            (
                Via::Access {
                    email: "owner@example.org".into(),
                },
                true,
            ),
        ] {
            let (mut parts, ()) = http::Request::new(()).into_parts();
            parts.extensions.insert(Caller {
                via,
                key: "synthetic".into(),
                person: None,
            });
            parts.extensions.insert(humans.clone());
            parts.extensions.insert(persons.clone());
            parts
                .extensions
                .insert(Configured(Arc::new(BTreeMap::new())));
            let result = Owner::from_request_parts(&mut parts, &hub).await;
            assert_eq!(result.is_ok(), allowed);
            if let Err(response) = result {
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
    }
}

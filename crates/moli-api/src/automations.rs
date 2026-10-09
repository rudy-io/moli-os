//! `/api/automations`: the house's automations. Saving never approves: a
//! person approves the exact version they saw (its fingerprint). Running an
//! approved one for real takes a person, or the household's dashboard (a
//! direct order, like switching a lamp: no code); anyone may try a dry run.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::Stream;
use futures::stream::{self, StreamExt};
use moli_automation::{Actor, Author, Automation, AutomationError, Automations, Graph};
use moli_core::Origin;
use moli_runtime::Hub;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::caller::Caller;
use crate::session::Sessions;

#[derive(Clone)]
pub(crate) struct Autos(pub(crate) Option<Automations>);

type Humans = Extension<Arc<Sessions>>;
fn actor(headers: &HeaderMap, humans: &Sessions, caller: &Caller) -> Actor {
    let human = humans.is_human(headers, &caller.key);
    Actor {
        human,
        author: if human { Author::Human } else { Author::Agent },
    }
}

fn off() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "automations are not running",
    )
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

fn failure(e: &AutomationError) -> Response {
    let status = match e {
        AutomationError::Unknown => StatusCode::NOT_FOUND,
        AutomationError::NeedsHuman => StatusCode::FORBIDDEN,
        AutomationError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        AutomationError::Conflict => StatusCode::CONFLICT,
        AutomationError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, &e.to_string())
}

/// An automation as the dashboard lists it.
fn view(autos: &Automations, a: &Automation) -> Json {
    view_with(autos, a, &a.fingerprint(), autos.abilities())
}

fn view_with(
    autos: &Automations,
    a: &Automation,
    fingerprint: &str,
    can: moli_automation::Abilities,
) -> Json {
    let check = autos.check_with(&a.graph, can);
    let last = autos.runs(Some(&a.id), 1).into_iter().next();
    let approved = a.approved.as_deref() == Some(fingerprint);
    json!({
        "automation": a,
        "summary": check.summary,
        "problems": check.problems,
        "protected": check.protected,
        "approved": approved,
        "live": a.enabled && approved,
        // What a person approves: the version they are looking at.
        "fingerprint": fingerprint,
        "can_restore": a.approved_version.is_some() && !approved,
        "last_run": last.map(|r| json!({ "id": r.id, "status": r.status, "started": r.started, "why": r.why, "dry": r.dry })),
    })
}

pub(crate) async fn list(Extension(Autos(autos)): Extension<Autos>) -> Response {
    let Some(autos) = autos else { return off() };
    let mut all = autos.list_fingerprinted();
    all.sort_by_key(|(a, _)| std::cmp::Reverse(a.updated));
    let can = autos.abilities();
    let items: Vec<Json> = all
        .into_iter()
        .map(|(a, fingerprint)| view_with(&autos, &a, &fingerprint, can))
        .collect();
    axum::Json(json!({ "automations": items, "abilities": {
        "telegram": can.telegram,
        "writer": can.writer,
        "voice": can.voice,
    } }))
    .into_response()
}

pub(crate) async fn get(
    Extension(Autos(autos)): Extension<Autos>,
    Path(id): Path<String>,
) -> Response {
    let Some(autos) = autos else { return off() };
    match autos.get(&id) {
        Some(a) => {
            let mut v = view(&autos, &a);
            v["runs"] = json!(autos.runs(Some(&id), 20));
            axum::Json(v).into_response()
        }
        None => error(StatusCode::NOT_FOUND, "unknown automation"),
    }
}

pub(crate) async fn create(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    axum::Json(mut a): axum::Json<Automation>,
) -> Response {
    let Some(autos) = autos else { return off() };
    a.id = String::new();
    save(&autos, a, actor(&headers, &humans, &caller)).await
}

pub(crate) async fn update(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(mut a): axum::Json<Automation>,
) -> Response {
    let Some(autos) = autos else { return off() };
    if autos.get(&id).is_none() {
        return error(StatusCode::NOT_FOUND, "unknown automation");
    }
    a.id = id;
    save(&autos, a, actor(&headers, &humans, &caller)).await
}

async fn save(autos: &Automations, a: Automation, by: Actor) -> Response {
    match autos.save(a, by).await {
        Ok(a) => axum::Json(view(autos, &a)).into_response(),
        Err(e) => failure(&e),
    }
}

pub(crate) async fn delete(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(autos) = autos else { return off() };
    match autos.delete(&id, actor(&headers, &humans, &caller)).await {
        Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
        Err(e) => failure(&e),
    }
}

/// The version the person is looking at.
#[derive(Deserialize)]
pub(crate) struct Seen {
    fingerprint: String,
}

pub(crate) async fn approve(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(seen): axum::Json<Seen>,
) -> Response {
    let Some(autos) = autos else { return off() };
    match autos
        .approve(&id, &seen.fingerprint, actor(&headers, &humans, &caller))
        .await
    {
        Ok(a) => axum::Json(view(&autos, &a)).into_response(),
        Err(e) => failure(&e),
    }
}

#[derive(Deserialize)]
pub(crate) struct Enabled {
    on: bool,
    #[serde(default)]
    fingerprint: Option<String>,
}

pub(crate) async fn enabled(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Enabled>,
) -> Response {
    let Some(autos) = autos else { return off() };
    match autos
        .set_enabled(
            &id,
            body.on,
            body.fingerprint.as_deref(),
            actor(&headers, &humans, &caller),
        )
        .await
    {
        Ok(a) => axum::Json(view(&autos, &a)).into_response(),
        Err(e) => failure(&e),
    }
}

pub(crate) async fn restore(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(autos) = autos else { return off() };
    match autos.restore(&id, actor(&headers, &humans, &caller)).await {
        Ok(a) => axum::Json(view(&autos, &a)).into_response(),
        Err(e) => failure(&e),
    }
}

pub(crate) async fn run(
    State(hub): State<Hub>,
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(seen): axum::Json<Seen>,
) -> Response {
    let Some(autos) = autos else { return off() };
    // An approved routine launched from the household's dashboard is a
    // direct order (D0): the same rule as a command, no code.
    let mut by = actor(&headers, &humans, &caller);
    if !by.human && crate::rest::origin(&hub, &headers, &humans, &caller) == Origin::Ui {
        by = Actor {
            human: true,
            author: Author::Human,
        };
    }
    match autos.run_now(&id, false, Some(&seen.fingerprint), by).await {
        Ok(run) => axum::Json(run).into_response(),
        Err(e) => failure(&e),
    }
}

pub(crate) async fn test(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(autos) = autos else { return off() };
    match autos
        .run_now(&id, true, None, actor(&headers, &humans, &caller))
        .await
    {
        Ok(run) => axum::Json(run).into_response(),
        Err(e) => failure(&e),
    }
}

#[derive(Deserialize)]
pub(crate) struct CheckBody {
    graph: Graph,
}

/// Validation and the French sentence, for a graph being edited.
pub(crate) async fn check(
    Extension(Autos(autos)): Extension<Autos>,
    axum::Json(body): axum::Json<CheckBody>,
) -> Response {
    let Some(autos) = autos else { return off() };
    axum::Json(autos.check(&body.graph)).into_response()
}

#[derive(Deserialize)]
pub(crate) struct RunsQuery {
    #[serde(default)]
    automation: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(crate) async fn runs(
    Extension(Autos(autos)): Extension<Autos>,
    Query(q): Query<RunsQuery>,
) -> Response {
    let Some(autos) = autos else { return off() };
    axum::Json(autos.runs(q.automation.as_deref(), q.limit.unwrap_or(50).min(500))).into_response()
}

/// Runs as they happen: the editor lights the nodes up.
pub(crate) async fn live(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(shutdown): Extension<CancellationToken>,
) -> Response {
    let Some(autos) = autos else { return off() };
    let rx = autos.subscribe();
    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(live) => {
                    let event = SseEvent::default().event("live").json_data(&live).ok()?;
                    return Some((Ok::<_, Infallible>(event), rx));
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .take_until(shutdown.cancelled_owned());
    sse(stream).into_response()
}

fn sse(
    stream: impl Stream<Item = Result<SseEvent, Infallible>> + Send + 'static,
) -> impl IntoResponse {
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
pub(crate) struct DraftBody {
    request: String,
    /// The automation being edited, if any (Moli changes it).
    #[serde(default)]
    current: Option<Automation>,
}

/// Moli drafts (or reworks) an automation from words. Nothing is saved.
pub(crate) async fn draft(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(crate::assistant::Moli(moli)): Extension<crate::assistant::Moli>,
    axum::Json(body): axum::Json<DraftBody>,
) -> Response {
    let (Some(autos), Some(moli)) = (autos, moli) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Moli is not configured");
    };
    match moli
        .draft_automation(&autos, &body.request, body.current.as_ref())
        .await
    {
        Ok(draft) => {
            let check = autos.check(&draft.graph);
            axum::Json(json!({ "name": draft.name, "mode": draft.mode, "graph": draft.graph, "check": check, "note": draft.note }))
                .into_response()
        }
        Err(e) => crate::assistant::failure(&e),
    }
}

#[derive(Deserialize)]
pub(crate) struct ImportBody {
    automations: Vec<moli_assistant::HaAutomation>,
}

/// Home Assistant's automations, translated by Moli into drafts in the
/// background (`tools/home-assistant/ha-automations-export.py`). Drafts only:
/// nothing runs before a person approves. Starting one is a person's
/// gesture too: it spends Moli's budget on up to 150 translations.
pub(crate) async fn import(
    Extension(Autos(autos)): Extension<Autos>,
    Extension(crate::assistant::Moli(moli)): Extension<crate::assistant::Moli>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    body: Result<axum::Json<ImportBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // Before the body is even looked at: without a session, nothing else
    // is answered.
    if !humans.is_human(&headers, &caller.key) {
        return failure(&AutomationError::NeedsHuman);
    }
    let axum::Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let (Some(autos), Some(moli)) = (autos, moli) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Moli is not configured");
    };
    match moli.import_ha(&autos, body.automations) {
        Ok(n) => (StatusCode::ACCEPTED, axum::Json(json!({ "started": n }))).into_response(),
        Err(e) => crate::assistant::failure(&e),
    }
}

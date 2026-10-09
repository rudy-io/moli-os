//! The house's computers' Moli agents (crate `moli-host`, module `agents`).
//!
//! - `POST /api/machines/<machine>/report` (`Authorization: Bearer <token>`,
//!   from the PC on the home network): what the machine does → `{ orders,
//!   every }`.
//! - `GET /api/machines/<device>` (the household): the agent's last report
//!   (processes, disks, AI agent runs) and the orders waiting.
//! - `POST /api/machines/<device>/remote` (a person, with the PIN): the PC
//!   opens Claude and Codex for the phone; woken first when it sleeps (the
//!   order waits for its agent).
//!
//! `<device>` is the dashboard's device id (`machines:pc-bureau`); the agent
//! says its machine id (`pc-bureau`). Both are taken.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use moli_core::{Origin, PointId, Value};
use moli_host::agents::{AgentError, Agents};
use moli_runtime::Hub;
use serde_json::json;

use crate::caller::Caller;
use crate::session::Sessions;

#[derive(Clone)]
pub(crate) struct MachinesGate(pub(crate) Option<Agents>);

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

/// `machines:pc-bureau` or `pc-bureau` → `pc-bureau`.
fn machine(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, m)| m)
}

fn no_gate() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        &moli_i18n::tr!("serveur.pilote.absent", pilote = "host"),
    )
}

pub(crate) async fn report(
    Extension(MachinesGate(gate)): Extension<MachinesGate>,
    Path(id): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    let Some(gate) = gate else { return no_gate() };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    match gate.report(machine(&id), token, body) {
        Ok(answer) => axum::Json(answer).into_response(),
        // The same answer for both: no machine ids to fish for.
        Err(AgentError::BadToken | AgentError::UnknownMachine) => error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("serveur.machine.jeton_refuse"),
        ),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub(crate) async fn view(
    Extension(MachinesGate(gate)): Extension<MachinesGate>,
    caller: Caller,
    Path(id): Path<String>,
) -> Response {
    let Some(gate) = gate else { return no_gate() };
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.machine.foyer_seulement"),
        );
    }
    match gate.view(machine(&id)) {
        Some(view) => axum::Json(view).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.machine.sans_agent"),
        ),
    }
}

/// Claude and Codex opened on the PC for the phone (woken first when it
/// sleeps). A person asks, with the PIN: the phone then reaches the PC.
pub(crate) async fn remote(
    State(hub): State<Hub>,
    Extension(MachinesGate(gate)): Extension<MachinesGate>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(gate) = gate else { return no_gate() };
    if !humans.is_human(&headers, &caller.key) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.machine.distance_humain"),
        );
    }
    let name = machine(&id);
    let order = match gate.remote(name) {
        Ok(order) => order,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, &format!("{e:#}")),
    };
    tracing::info!(client = %caller.describe(), machine = name, order, "remote mode asked");
    // Asleep: woken, the order waits for its agent.
    let mut waking = false;
    if !gate.connected(name) && id.contains(':') {
        let point = PointId::from(format!("{id}/power"));
        waking = hub
            .command(
                &point,
                Value::Bool(true),
                Origin::Ui,
                Some(caller.describe()),
            )
            .await
            .is_ok();
    }
    axum::Json(json!({ "ok": true, "order": order, "waking": waking })).into_response()
}

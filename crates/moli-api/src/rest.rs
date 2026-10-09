use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::{DeviceId, Origin, PointId, Value};
use moli_runtime::guard::Dashboard;
use moli_runtime::{ApprovalError, CommandError, Hub, LabelError, LabelPatch};
use serde::Deserialize;
use serde_json::json;

use crate::caller::{Caller, Via};
use crate::people::People;
use crate::session::{ClaimError, LoginError, Sessions};

const VERSION: &str = env!("CARGO_PKG_VERSION");

type Humans = Extension<Arc<Sessions>>;

pub(crate) async fn health(State(hub): State<Hub>) -> Response {
    let snapshot = hub.snapshot();
    Json(json!({
        "name": "moli-os",
        "version": VERSION,
        "stats": hub.stats(),
        "guard": hub.guard_status(),
        "drivers": snapshot.drivers,
        // Journal entries lost on disk since the start: never silently.
        "journal_failures": hub.journal_failures(),
    }))
    .into_response()
}

pub(crate) async fn devices(State(hub): State<Hub>) -> Response {
    Json(hub.snapshot()).into_response()
}

pub(crate) async fn device(State(hub): State<Hub>, Path(id): Path<String>) -> Response {
    match hub.device(&DeviceId::from(id)) {
        Some(view) => Json(view).into_response(),
        None => error(StatusCode::NOT_FOUND, "unknown device"),
    }
}

#[derive(Deserialize)]
pub(crate) struct CommandBody {
    point: PointId,
    value: Value,
    #[serde(default)]
    actor: Option<String>,
}

/// Who is acting. Declaring is not proving: `ui` takes the household (home
/// network, or a person Cloudflare Access let in) when the dashboard is
/// trusted, a human session (PIN) otherwise; `cli` takes this very machine,
/// not the tunnel that also ends here.
pub(crate) fn origin(hub: &Hub, headers: &HeaderMap, humans: &Sessions, caller: &Caller) -> Origin {
    match headers.get("x-moli-origin").and_then(|v| v.to_str().ok()) {
        Some("ui")
            if (hub.dashboard() == Dashboard::Trusted && caller.is_household())
                || humans.is_human(headers, &caller.key) =>
        {
            Origin::Ui
        }
        Some("cli") if caller.is_local_machine() => Origin::Cli,
        _ => Origin::Api,
    }
}

/// A guest commands in its rooms only.
#[allow(clippy::result_large_err)]
fn guest_may(hub: &Hub, caller: &Caller, device: &DeviceId) -> Result<(), Response> {
    let Some(person) = caller.person.as_ref().filter(|p| p.is_guest()) else {
        return Ok(());
    };
    let rooms = hub.rooms_of(device);
    if !rooms.is_empty() && rooms.iter().all(|r| person.rooms.contains(r)) {
        return Ok(());
    }
    Err(error(
        StatusCode::FORBIDDEN,
        &moli_i18n::tr!("serveur.personnes.invite_piece"),
    ))
}

pub(crate) async fn command(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Json(body): Json<CommandBody>,
) -> Response {
    if let Some((device, _)) = body.point.split()
        && let Err(no) = guest_may(&hub, &caller, &device)
    {
        return no;
    }
    let origin = origin(&hub, &headers, &humans, &caller);
    let actor = match origin {
        Origin::Ui => body.actor.or_else(|| Some(caller.describe())),
        _ => body.actor,
    };
    match hub.command(&body.point, body.value, origin, actor).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        // Accepted, not executed: a human decides.
        Err(CommandError::NeedsApproval { id, reason }) => (
            StatusCode::ACCEPTED,
            Json(json!({ "ok": false, "pending": id, "reason": reason })),
        )
            .into_response(),
        Err(e) => error(command_status(&e), &e.to_string()),
    }
}

pub(crate) async fn ambiances() -> Response {
    Json(moli_ambiance::AMBIANCES).into_response()
}

/// Most lights one gesture reaches (a whole floor is fine).
const MAX_AMBIANCE_LIGHTS: usize = 32;

#[derive(Deserialize)]
pub(crate) struct AmbianceBody {
    /// The lights (a group stands for its members).
    lights: Vec<DeviceId>,
    /// An ambiance id (`GET /api/ambiances`)…
    #[serde(default)]
    ambiance: Option<String>,
    /// …or one colour, `#rrggbb`, for every colour lamp…
    #[serde(default)]
    color: Option<String>,
    /// …or one white, in kelvins, for every lamp that has whites.
    #[serde(default)]
    white: Option<u32>,
    #[serde(default)]
    actor: Option<String>,
}

/// A look for several lights at once: each order goes through the guard.
pub(crate) async fn ambiance(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    headers: HeaderMap,
    Json(body): Json<AmbianceBody>,
) -> Response {
    if body.lights.is_empty() || body.lights.len() > MAX_AMBIANCE_LIGHTS {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("between 1 and {MAX_AMBIANCE_LIGHTS} lights"),
        );
    }
    let look = match (&body.ambiance, &body.color, body.white) {
        (Some(id), None, None) => match moli_ambiance::find(id) {
            Some(a) => moli_ambiance::Look::Ambiance(a),
            None => return error(StatusCode::NOT_FOUND, "unknown ambiance"),
        },
        (None, Some(hex), None) if moli_core::color::hex_to_xy(hex).is_some() => {
            moli_ambiance::Look::Color(hex)
        }
        (None, Some(_), None) => return error(StatusCode::BAD_REQUEST, "color: #rrggbb"),
        (None, None, Some(k)) if (1500..=10_000).contains(&k) => moli_ambiance::Look::White(k),
        (None, None, Some(_)) => {
            return error(StatusCode::BAD_REQUEST, "white: 1500 to 10000 K");
        }
        _ => {
            return error(StatusCode::BAD_REQUEST, "one of ambiance, color or white");
        }
    };
    for light in &body.lights {
        if let Err(no) = guest_may(&hub, &caller, light) {
            return no;
        }
    }
    let origin = origin(&hub, &headers, &humans, &caller);
    let actor = match origin {
        Origin::Ui => body.actor.or_else(|| Some(caller.describe())),
        _ => body.actor,
    };
    let report = moli_ambiance::apply(&hub, look, &body.lights, origin, actor).await;
    Json(report).into_response()
}

pub(crate) async fn approvals(State(hub): State<Hub>) -> Response {
    Json(hub.approvals()).into_response()
}

#[derive(Deserialize)]
pub(crate) struct DecisionBody {
    approve: bool,
    /// The point the human saw on the card.
    point: PointId,
}

/// Approve or deny a held order: a human session (PIN) is required, even
/// from a trusted dashboard: releasing what an agent asked for is a decision.
pub(crate) async fn decide(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<u64>,
    headers: HeaderMap,
    Json(body): Json<DecisionBody>,
) -> Response {
    let origin = if humans.is_human(&headers, &caller.key) {
        Origin::Ui
    } else {
        Origin::Api
    };
    let actor = Some(caller.describe());
    match hub
        .resolve_approval(id, &body.point, body.approve, origin, actor)
        .await
    {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e @ ApprovalError::HumanOnly) => error(StatusCode::FORBIDDEN, &e.to_string()),
        Err(e @ (ApprovalError::Unknown | ApprovalError::Expired)) => {
            error(StatusCode::GONE, &e.to_string())
        }
        Err(e @ ApprovalError::Mismatch) => error(StatusCode::CONFLICT, &e.to_string()),
        Err(ApprovalError::Command(e)) => error(command_status(&e), &e.to_string()),
    }
}

pub(crate) fn command_status(e: &CommandError) -> StatusCode {
    match e {
        CommandError::NeedsApproval { .. } => StatusCode::ACCEPTED,
        CommandError::UnknownPoint => StatusCode::NOT_FOUND,
        CommandError::ReadOnly => StatusCode::FORBIDDEN,
        CommandError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        CommandError::DriverUnavailable | CommandError::Busy => StatusCode::SERVICE_UNAVAILABLE,
        CommandError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        CommandError::Driver(_) => StatusCode::BAD_GATEWAY,
    }
}

pub(crate) async fn set_label(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<LabelPatch>,
) -> Response {
    let origin = origin(&hub, &headers, &humans, &caller);
    let actor = (origin == Origin::Ui).then(|| caller.describe());
    match hub
        .set_label(&DeviceId::from(id), patch, origin, actor)
        .await
    {
        Ok(label) => Json(label).into_response(),
        Err(LabelError::UnknownDevice) => error(StatusCode::NOT_FOUND, "unknown device"),
        Err(e @ LabelError::Guarded(_)) => error(StatusCode::FORBIDDEN, &e.to_string()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ---- human sessions ---------------------------------------------------------

pub(crate) async fn session(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    Extension(persons): Extension<Arc<People>>,
    caller: Caller,
    headers: HeaderMap,
) -> Response {
    let who = match &caller.via {
        Via::Access { email } => Some(email.as_str()),
        _ => None,
    };
    let owner = crate::people_api::is_owner(&caller, &humans, &persons, &headers);
    Json(json!({
        // The person Moli recognised (Access e-mail, password), if any.
        "person": caller.person,
        // From the Internet without signing in: only the sign-in page.
        "login_required": caller.via == Via::Outside,
        "human": humans.is_human(&headers, &caller.key),
        "pin_configured": humans.pin_configured(),
        "locked": humans.locked(&caller.key),
        // Commands from this dashboard need no code.
        "trusted": hub.dashboard() == Dashboard::Trusted && caller.is_household(),
        "via": caller.via_name(),
        "who": who,
        // May choose the code without the current one (`PUT /api/session/pin`).
        "owner": owner,
        // A new house: the installation code (logs) chooses the first code.
        "setup": humans.setup_pending(),
        // The shortest code this house accepts.
        "min_pin": humans.min_pin_len(),
        // The house's language, and those it could speak.
        "language": moli_i18n::language(),
        "languages": moli_i18n::languages(),
        // May change the code: an owner, or anyone with the current code in
        // a house without owners.
        "can_change": owner || (!humans.has_owners() && humans.pin_configured()),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct PinBody {
    pin: String,
    /// The current code, for whoever is not an owner through Access.
    #[serde(default)]
    current: Option<String>,
}

/// A new dashboard code: kept encrypted by Moli, it replaces the vault's,
/// and every session opened with the old one ends; the house is told. A
/// house with owners (Cloudflare Access proves the e-mail): they alone choose
/// it. A house without: whoever proves the current code (same lockouts as
/// the code itself).
pub(crate) async fn set_pin(
    Extension(humans): Humans,
    Extension(persons): Extension<Arc<People>>,
    caller: Caller,
    headers: HeaderMap,
    Json(body): Json<PinBody>,
) -> Response {
    let owner = crate::people_api::is_owner(&caller, &humans, &persons, &headers);
    let has_owners = humans.has_owners() || persons.has_owner();
    if !owner && has_owners {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.code.proprietaire_seul"),
        );
    }
    if !owner {
        let Some(current) = body.current.as_deref() else {
            return error(
                StatusCode::FORBIDDEN,
                &moli_i18n::tr!("serveur.code.actuel_d_abord"),
            );
        };
        match humans.verify(current, &caller.key) {
            Ok(()) => {}
            Err(LoginError::Wrong) => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    &moli_i18n::tr!("serveur.code.actuel_incorrect"),
                );
            }
            Err(LoginError::Locked) => {
                return error(
                    StatusCode::TOO_MANY_REQUESTS,
                    &moli_i18n::tr!("serveur.code.trop_d_essais"),
                );
            }
            Err(LoginError::Disabled) => {
                return error(
                    StatusCode::CONFLICT,
                    &moli_i18n::tr!("serveur.code.installer_d_abord"),
                );
            }
        }
    }
    match humans.set_pin(&body.pin, &caller.describe()).await {
        Ok(()) => {
            tracing::info!(client = %caller.describe(), owner, "dashboard code changed");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &e),
    }
}

#[derive(Deserialize)]
pub(crate) struct SetupBody {
    /// The installation code, from the logs.
    code: String,
    /// The house's code, chosen now.
    pin: String,
    /// The house's language, chosen now (kept in `settings.json`).
    #[serde(default)]
    language: Option<String>,
}

/// Where the house keeps what it chose from the dashboard.
#[derive(Clone)]
pub(crate) struct SettingsFile(pub(crate) Option<std::path::PathBuf>);

/// The installation of a new house: the code from the machine's logs proves
/// its owner, who chooses the house's code; the session opens at once.
pub(crate) async fn setup(
    Extension(humans): Humans,
    Extension(SettingsFile(settings)): Extension<SettingsFile>,
    caller: Caller,
    Json(body): Json<SetupBody>,
) -> Response {
    // The chosen language first: the house announces its installation in it,
    // and an error answers in it too. Put back if the installation fails.
    let before = moli_i18n::language();
    let chosen = humans.setup_pending()
        && body
            .language
            .as_deref()
            .is_some_and(moli_i18n::set_language);
    let claimed = humans.claim(&body.code, &body.pin, &caller.key).await;
    if chosen && claimed.is_err() {
        moli_i18n::set_language(&before);
    }
    match claimed {
        Ok(token) => {
            tracing::info!(client = %caller.describe(), "installation done");
            if chosen {
                let kept = settings
                    .as_deref()
                    .map(|path| crate::settings::set_language(path, &moli_i18n::language()));
                if let Some(Err(e)) = kept {
                    tracing::warn!(error = %e, "language chosen but not kept (settings.json)");
                }
            }
            (
                [(SET_COOKIE, Sessions::cookie(&token))],
                Json(json!({ "human": true })),
            )
                .into_response()
        }
        Err(ClaimError::NotNeeded) => error(
            StatusCode::CONFLICT,
            &moli_i18n::tr!("serveur.code.deja_installee"),
        ),
        Err(ClaimError::Expired) => error(
            StatusCode::GONE,
            &moli_i18n::tr!("serveur.code.installation_expiree"),
        ),
        Err(ClaimError::Wrong) => error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("serveur.code.installation_incorrect"),
        ),
        Err(ClaimError::Locked) => error(
            StatusCode::TOO_MANY_REQUESTS,
            &moli_i18n::tr!("serveur.code.trop_d_essais"),
        ),
        Err(ClaimError::Rejected(why)) => error(StatusCode::UNPROCESSABLE_ENTITY, &why),
    }
}

/// The house's code (`{pin}`, from the household), or a person's password
/// (`{login, password}`, from anywhere).
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum LoginBody {
    Person { login: String, password: String },
    Pin { pin: String },
}

pub(crate) async fn login(
    Extension(humans): Humans,
    Extension(persons): Extension<Arc<People>>,
    caller: Caller,
    Json(body): Json<LoginBody>,
) -> Response {
    let pin = match body {
        LoginBody::Person { login, password } => {
            return match persons.login(&humans, &login, &password, &caller.key).await {
                Ok((token, person)) => (
                    [(SET_COOKIE, People::cookie(&token))],
                    Json(json!({ "person": person })),
                )
                    .into_response(),
                Err(e) => {
                    crate::people_api::login_error(e, "serveur.personnes.identifiants_incorrects")
                }
            };
        }
        LoginBody::Pin { pin } => pin,
    };
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.acces.refuse"),
        );
    }
    match humans.login(&pin, &caller.key).await {
        Ok(token) => (
            [(SET_COOKIE, Sessions::cookie(&token))],
            Json(json!({ "human": true })),
        )
            .into_response(),
        Err(LoginError::Wrong) => error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("serveur.code.incorrect"),
        ),
        Err(LoginError::Locked) => error(
            StatusCode::TOO_MANY_REQUESTS,
            &moli_i18n::tr!("serveur.code.trop_d_essais"),
        ),
        Err(LoginError::Disabled) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.code.installer"),
        ),
    }
}

pub(crate) async fn logout(
    Extension(humans): Humans,
    Extension(persons): Extension<Arc<People>>,
    headers: HeaderMap,
) -> Response {
    humans.logout(&headers).await;
    persons.logout(&headers).await;
    (
        axum::response::AppendHeaders([
            (SET_COOKIE, Sessions::clear_cookie()),
            (SET_COOKIE, People::clear_cookie()),
        ]),
        Json(json!({ "human": false, "person": null })),
    )
        .into_response()
}

// ---- comparison with Home Assistant -----------------------------------------

#[derive(Clone)]
pub(crate) struct BenchFile(pub(crate) Option<std::path::PathBuf>);

pub(crate) async fn bench(Extension(BenchFile(path)): Extension<BenchFile>) -> Response {
    match read_bench(path.as_deref()).await {
        Some(bench) => Json(bench).into_response(),
        None => error(StatusCode::NOT_FOUND, "no comparison available yet"),
    }
}

/// The house's machines as a host script measures them (`data/infra.json`:
/// containers, disks), next to `home.json`.
#[derive(Clone)]
pub(crate) struct InfraFile(pub(crate) Option<std::path::PathBuf>);

pub(crate) async fn infra(Extension(InfraFile(path)): Extension<InfraFile>) -> Response {
    match read_bench(path.as_deref()).await {
        Some(infra) => Json(infra).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "no host measurement yet (data/infra.json)",
        ),
    }
}

/// The host script's latest measurement, if any.
pub(crate) async fn read_bench(path: Option<&std::path::Path>) -> Option<serde_json::Value> {
    let bytes = tokio::fs::read(path?).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ---- history ----------------------------------------------------------------

/// The history handle, if recording is enabled.
#[derive(Clone)]
pub(crate) struct Recorded(pub(crate) Option<moli_history::History>);

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    point: String,
    #[serde(default = "default_hours")]
    hours: f64,
    #[serde(default = "default_points")]
    points: usize,
}

fn default_hours() -> f64 {
    24.0
}

fn default_points() -> usize {
    300
}

pub(crate) async fn history(
    Extension(Recorded(history)): Extension<Recorded>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    let Some(history) = history else {
        return error(StatusCode::NOT_FOUND, "history is disabled");
    };
    let to = moli_core::now_ms();
    let span = window_ms(q.hours);
    match history
        .series(
            &q.point,
            to.saturating_sub(span),
            to,
            q.points.clamp(10, 2_000),
        )
        .await
    {
        Ok(series) => Json(series).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// Hours → ms, clamped to [1 min, 1 year].
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn window_ms(hours: f64) -> u64 {
    let hours = if hours.is_finite() { hours } else { 24.0 };
    (hours.clamp(1.0 / 60.0, 24.0 * 366.0) * 3_600_000.0) as u64
}

// ---- family dashboard layout --------------------------------------------------

#[derive(Clone)]
pub(crate) struct HomeFile(pub(crate) Option<std::path::PathBuf>);

/// The dashboard layout; `{}` (automatic layout) when there is none.
pub(crate) async fn home(Extension(HomeFile(path)): Extension<HomeFile>) -> Response {
    let Some(path) = path else {
        return Json(json!({})).into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(layout) => Json(layout).into_response(),
            Err(e) => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("home.json: {e}"),
            ),
        },
        Err(_) => Json(json!({})).into_response(),
    }
}

// ---- published media ----------------------------------------------------------

/// A short-lived media a device fetches back (an announcement for a Sonos).
/// Its name is 128 random bits: knowing it is the only way in, and it is
/// gone after a few minutes.
pub(crate) async fn media(
    State(hub): State<Hub>,
    caller: Caller,
    Path(name): Path<String>,
) -> Response {
    let media = hub.published_media(&name);
    // Who fetched what: an announcement nobody fetched is visible here.
    tracing::info!(
        client = caller.key,
        name,
        found = media.is_some(),
        bytes = media.as_ref().map_or(0, |m| m.bytes.len()),
        "media fetched"
    );
    match media {
        Some(media) => (
            [
                (axum::http::header::CONTENT_TYPE, media.content_type),
                (axum::http::header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            media.bytes,
        )
            .into_response(),
        None => error(StatusCode::NOT_FOUND, "no such media (expired?)"),
    }
}

// ---- camera images ----------------------------------------------------------

/// A fresh camera image. A camera in a protected room shows itself only to
/// a human session (PIN): an agent or a script never looks into a bedroom.
pub(crate) async fn snapshot(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    use moli_runtime::media::SnapshotError;
    let id = DeviceId::from(id);
    if caller
        .person
        .as_ref()
        .is_some_and(crate::people::PersonRef::is_guest)
    {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.personnes.invite_limite"),
        );
    }
    if let Some(room) = hub.protected_room(&id)
        && !humans.is_human(&headers, &caller.key)
    {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.camera.piece_protegee", room = room),
        );
    }
    match hub.camera_image(&id).await {
        Ok(image) => (
            [
                (axum::http::header::CONTENT_TYPE, image.content_type),
                (axum::http::header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            image.bytes,
        )
            .into_response(),
        Err(e @ (SnapshotError::UnknownDevice | SnapshotError::NotACamera)) => {
            error(StatusCode::NOT_FOUND, &e.to_string())
        }
        Err(e) => error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

// ---- printing ---------------------------------------------------------------

/// Biggest page taken (the dashboard sends 0.5 to 3 MB; Moli lives in 128 MB).
pub(crate) const MAX_PAGE: usize = 16 * 1024 * 1024;
/// IPP names are 255 bytes at most.
const MAX_NAME_BYTES: usize = 240;

#[derive(Deserialize)]
pub(crate) struct PrintQuery {
    /// What the person printed (a file name).
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "one_copy")]
    copies: u32,
    #[serde(default = "in_colour")]
    color: bool,
    #[serde(default)]
    two_sided: bool,
}

fn one_copy() -> u32 {
    1
}

fn in_colour() -> bool {
    true
}

/// Prints a page or a photo (the body as it is, its type in `content-type`).
/// The household's dashboard only, as an order to a lamp: an agent never
/// spends paper and ink. Checked before the body is read.
pub(crate) async fn print(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<String>,
    Query(query): Query<PrintQuery>,
    request: axum::extract::Request,
) -> Response {
    use moli_runtime::media::{Document, PrintError};
    if origin(&hub, request.headers(), &humans, &caller) != Origin::Ui {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.impression.tableau_seulement"),
        );
    }
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    if content_type.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            &moli_i18n::tr!("serveur.impression.type_manquant"),
        );
    }
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_PAGE).await else {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &moli_i18n::tr!("serveur.impression.trop_lourde"),
        );
    };
    let name: String = query.name.filter(|n| !n.trim().is_empty()).map_or_else(
        || "Document".to_owned(),
        |n| {
            let mut name = String::new();
            for c in n.trim().chars().take(120) {
                if name.len() + c.len_utf8() > MAX_NAME_BYTES {
                    break;
                }
                name.push(c);
            }
            name
        },
    );
    tracing::info!(client = %caller.describe(), device = %id, name = %name, bytes = bytes.len(), "print asked");
    let document = Document {
        name,
        content_type,
        // Not copied: the body's buffer becomes the document's.
        bytes: Vec::from(bytes),
        copies: query.copies,
        color: query.color,
        two_sided: query.two_sided,
    };
    match hub.print(&DeviceId::from(id), document).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e @ (PrintError::UnknownDevice | PrintError::NotAPrinter)) => {
            error(StatusCode::NOT_FOUND, &e.to_string())
        }
        Err(e) => error(StatusCode::CONFLICT, &e.to_string()),
    }
}

// ---- energy -----------------------------------------------------------------

/// The energy handle, if `[energy]` is configured.
#[derive(Clone)]
pub(crate) struct Metered(pub(crate) Option<moli_energy::Energy>);

pub(crate) async fn energy(Extension(Metered(energy)): Extension<Metered>) -> Response {
    let Some(energy) = energy else {
        return error(StatusCode::NOT_FOUND, "no [energy] meters configured");
    };
    match energy.summary().await {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize)]
pub(crate) struct SeriesQuery {
    step: moli_energy::Step,
    /// How many buckets back, the current one included.
    #[serde(default)]
    count: Option<usize>,
}

pub(crate) fn default_count(step: moli_energy::Step) -> usize {
    match step {
        moli_energy::Step::Hour => 24,
        moli_energy::Step::Day => 30,
        moli_energy::Step::Month => 12,
    }
}

pub(crate) async fn energy_series(
    Extension(Metered(energy)): Extension<Metered>,
    Query(q): Query<SeriesQuery>,
) -> Response {
    let Some(energy) = energy else {
        return error(StatusCode::NOT_FOUND, "no [energy] meters configured");
    };
    let count = q.count.unwrap_or_else(|| default_count(q.step));
    match energy.recent(q.step, count).await {
        Ok(report) => Json(report).into_response(),
        Err(moli_energy::EnergyError::Invalid(e)) => error(StatusCode::BAD_REQUEST, &e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize)]
pub(crate) struct JournalQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

pub(crate) async fn journal(State(hub): State<Hub>, Query(q): Query<JournalQuery>) -> Response {
    Json(hub.journal(q.limit)).into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

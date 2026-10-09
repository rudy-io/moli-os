//! Moli OS surfaces, all served by one router:
//!
//! - `/api/*`    REST (snapshot, commands, labels, journal, approvals, health)
//! - `/api/energy` consumption and cost (headline figures, series)
//! - `/api/assistant` Moli, the conversation that acts and picks what to show
//! - `/api/automations` automations (graphs), their runs and Moli's drafts
//! - `/api/events` server-sent events: a snapshot, then every change
//! - `/api/session` human sessions (PIN) — what makes a click a human's
//! - `/mcp`      Model Context Protocol (streamable HTTP) for any agent
//! - `/`         the dashboard, embedded in the binary
//!
//! Every route sits behind the same `Host` allowlist (DNS-rebinding guard),
//! then knows who is calling (`caller`: home network, this machine, or a
//! person Cloudflare Access let in through the tunnel).

mod access;
mod assets;
mod assistant;
mod automations;
mod caller;
mod guard;
mod integrations;
mod limit;
mod machines;
mod mcp;
mod phones;
mod plan;
mod rest;
mod session;
pub mod settings;
mod sse;
mod system;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{MethodRouter, get, post, put};
use axum::{Extension, Router, middleware};
use moli_history::History;
use moli_runtime::Hub;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;

pub use access::AccessConfig;
pub use plan::image_name_ok as plan_image_name_ok;

/// A JSON request is small: orders, labels, a graph, a conversation turn.
const BODY: usize = 256 * 1024;
/// Recorded speech, a Home Assistant export.
const BIG_BODY: usize = 4 * 1024 * 1024;
pub use session::UiPin;

/// Surface settings.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Host names (or IPs) this instance is reached by, besides localhost.
    pub allowed_hosts: Vec<String>,
    /// PIN that opens a human session in the dashboard. `None`: nobody is
    /// recognized as human, so guarded orders can only wait (safe default).
    pub ui_pin: Option<UiPin>,
    /// Recorded history, when enabled.
    pub history: Option<History>,
    /// Comparison with Home Assistant, written by a host script
    /// (`bench.json` in the data directory), served as-is when present.
    pub bench_path: Option<std::path::PathBuf>,
    /// Energy meters, when `[energy]` is configured.
    pub energy: Option<moli_energy::Energy>,
    /// The family dashboard's layout (rooms, favourites…), a JSON file
    /// served as-is.
    pub home_path: Option<std::path::PathBuf>,
    /// The home's own assistant, when `[assistant]` is configured.
    pub assistant: Option<moli_assistant::Assistant>,
    /// The automations engine.
    pub automations: Option<moli_automation::Automations>,
    /// The Cloudflare Access application in front of the tunnel, whose
    /// signed assertions name the person (`[server.access]`).
    pub access: Option<AccessConfig>,
    /// The household's phones (the Moli app), when a `phones` driver runs.
    pub phones: Option<moli_phones::Gateway>,
    /// The house's computers' Moli agents, when a host driver runs.
    pub machines: Option<moli_host::agents::Agents>,
}

/// Builds the complete HTTP application. Live streams end when `shutdown`
/// fires, so a graceful shutdown never waits on an open dashboard.
/// A big upload: its own body limit, and only a person sends one (checked
/// before the body is read).
fn upload(
    route: MethodRouter<Hub>,
    max: usize,
    humans: &Arc<session::Sessions>,
) -> MethodRouter<Hub> {
    route
        .layer(DefaultBodyLimit::max(max))
        .layer(middleware::from_fn_with_state(
            Arc::clone(humans),
            session::human_first,
        ))
}

/// The devices, their images, their printing.
fn device_routes() -> Router<Hub> {
    Router::new()
        .route("/api/devices", get(rest::devices))
        .route("/api/devices/{id}", get(rest::device))
        .route("/api/devices/{id}/snapshot", get(rest::snapshot))
        .route(
            "/api/devices/{id}/print",
            post(rest::print).layer(DefaultBodyLimit::max(rest::MAX_PAGE)),
        )
}

/// The house's plan, its phones, its machines.
fn house_routes(humans: &Arc<session::Sessions>, options: &Options) -> Router<Hub> {
    // The host script writes it next to `home.json`.
    let infra = options
        .home_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|dir| dir.join("infra.json"));
    Router::new()
        .route("/api/infra", get(rest::infra))
        .layer(Extension(rest::InfraFile(infra)))
        .route("/api/plan", get(plan::get).put(plan::put))
        .route(
            "/api/plan/images",
            upload(post(plan::upload), plan::MAX_IMAGE, humans),
        )
        .route("/api/plan/images/{name}", get(plan::image))
        .route("/api/mobile/register", post(phones::register))
        .route("/api/phones/{id}/report", post(phones::report))
        .route("/api/phones/{id}", axum::routing::delete(phones::remove))
        .route(
            "/api/integrations/tuya-cloud",
            get(integrations::tuya_cloud).put(integrations::set_tuya_cloud),
        )
        .route("/api/session/pin", put(rest::set_pin))
        .route("/api/session/setup", post(rest::setup))
        .route("/api/machines/{id}", get(machines::view))
        .route("/api/machines/{id}/report", post(machines::report))
        .route("/api/machines/{id}/remote", post(machines::remote))
        .layer(Extension(machines::MachinesGate(options.machines.clone())))
        .layer(Extension(rest::SettingsFile(
            options
                .home_path
                .as_deref()
                .and_then(std::path::Path::parent)
                .map(|dir| dir.join("settings.json")),
        )))
}

/// Who is who: the human sessions (the code, its owners) and the Cloudflare
/// Access check.
fn people(hub: &Hub, options: &Options) -> (Arc<session::Sessions>, Option<Arc<access::Verifier>>) {
    let owners = options
        .access
        .as_ref()
        .map(|a| a.owners.as_slice())
        .unwrap_or_default();
    let humans = session::Sessions::new(hub.clone(), options.ui_pin.clone(), owners);
    let access = options.access.as_ref().map(access::Verifier::new);
    (Arc::new(humans), access.map(Arc::new))
}

pub fn router(hub: Hub, shutdown: CancellationToken, options: &Options) -> Router {
    let allowed = guard::AllowedHosts::new(&options.allowed_hosts);
    let (humans, access) = people(&hub, options);
    Router::new()
        .route("/api/health", get(rest::health))
        .merge(device_routes())
        .route("/api/media/{name}", get(rest::media))
        .route("/api/command", post(rest::command))
        .route("/api/ambiances", get(rest::ambiances))
        .route("/api/ambiance", post(rest::ambiance))
        .route("/api/labels/{id}", put(rest::set_label))
        .route("/api/journal", get(rest::journal))
        .route("/api/approvals", get(rest::approvals))
        .route("/api/approvals/{id}", post(rest::decide))
        .route(
            "/api/session",
            get(rest::session).post(rest::login).delete(rest::logout),
        )
        .route("/api/history", get(rest::history))
        .route("/api/bench", get(rest::bench))
        .route("/api/home", get(rest::home))
        .merge(house_routes(&humans, options))
        .route("/api/system", get(system::system))
        .route("/api/energy", get(rest::energy))
        .route("/api/energy/series", get(rest::energy_series))
        .route(
            "/api/assistant",
            get(assistant::status).post(assistant::turn),
        )
        // Voice (audio) and Home Assistant imports are the only big bodies.
        .route(
            "/api/assistant/listen",
            post(assistant::listen).layer(DefaultBodyLimit::max(BIG_BODY)),
        )
        .route("/api/assistant/speak", post(assistant::speak))
        .route(
            "/api/assistant/settings",
            get(assistant::settings).put(assistant::set_settings),
        )
        .route("/api/assistant/key", put(assistant::set_key))
        .route(
            "/api/automations",
            get(automations::list).post(automations::create),
        )
        .route("/api/automations/check", post(automations::check))
        .route("/api/automations/draft", post(automations::draft))
        .route(
            "/api/automations/import",
            upload(post(automations::import), BIG_BODY, &humans),
        )
        .route("/api/automations/runs", get(automations::runs))
        .route("/api/automations/live", get(automations::live))
        .route(
            "/api/automations/{id}",
            get(automations::get)
                .put(automations::update)
                .delete(automations::delete),
        )
        .route("/api/automations/{id}/approve", post(automations::approve))
        .route("/api/automations/{id}/enabled", post(automations::enabled))
        .route("/api/automations/{id}/run", post(automations::run))
        .route("/api/automations/{id}/test", post(automations::test))
        .route("/api/automations/{id}/restore", post(automations::restore))
        .route("/api/events", get(sse::events))
        .nest_service(
            "/mcp",
            mcp::service(
                hub.clone(),
                options.history.clone(),
                options.bench_path.clone(),
                options.energy.clone(),
                options.automations.clone(),
            ),
        )
        .fallback(assets::serve)
        .layer(Extension(shutdown))
        .layer(Extension(rest::Recorded(options.history.clone())))
        .layer(Extension(rest::BenchFile(options.bench_path.clone())))
        .layer(Extension(rest::HomeFile(options.home_path.clone())))
        .layer(Extension(plan::PlanDir(
            options
                .home_path
                .as_deref()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf),
        )))
        .layer(Extension(rest::Metered(options.energy.clone())))
        .layer(Extension(assistant::Moli(options.assistant.clone())))
        .layer(Extension(automations::Autos(options.automations.clone())))
        .layer(Extension(phones::PhonesGate(options.phones.clone())))
        .layer(Extension(humans))
        .layer(DefaultBodyLimit::max(BODY))
        .layer(middleware::from_fn_with_state(
            Arc::new(limit::Limits::default()),
            limit::check,
        ))
        .layer(middleware::from_fn_with_state(access, caller::identify))
        .layer(middleware::from_fn_with_state(allowed, guard::check))
        .layer(CompressionLayer::new())
        .with_state(hub)
}

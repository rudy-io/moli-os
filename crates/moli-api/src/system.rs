//! `/api/system`: how Moli OS is put together, as it runs right now. The
//! dashboard draws the map from it (drivers → core → bricks → surfaces) and
//! compares with Home Assistant: nothing in this description is written by
//! hand, so the map follows every new driver or brick by itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use moli_runtime::{Hub, Snapshot};
use serde_json::{Value as Json, json};

use crate::automations::Autos;
use crate::rest::{BenchFile, Metered, Recorded, read_bench};

pub(crate) async fn system(
    State(hub): State<Hub>,
    Extension(BenchFile(bench_path)): Extension<BenchFile>,
    Extension(Recorded(history)): Extension<Recorded>,
    Extension(Metered(energy)): Extension<Metered>,
    Extension(crate::assistant::Moli(moli)): Extension<crate::assistant::Moli>,
    Extension(Autos(autos)): Extension<Autos>,
) -> Response {
    let snapshot = hub.snapshot();
    let data_dir = bench_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let size = |name: &str| {
        data_dir
            .as_ref()
            .and_then(|d| std::fs::metadata(d.join(name)).ok())
            .map(|m| m.len())
    };
    let (drivers, cameras) = drivers(&snapshot);

    let energy_facts = match &energy {
        Some(e) => e.summary().await.map_or_else(
            |_| json!({}),
            |s| json!({ "meters": s.meters.len(), "today_kwh": s.today.total.kwh, "db_bytes": size("energy.db") }),
        ),
        None => json!({}),
    };
    let automation_facts = autos.as_ref().map_or_else(
        || json!({}),
        |a| {
            let list = a.list();
            let day_ago = moli_core::now_ms().saturating_sub(24 * 3_600_000);
            json!({
                "live": list.iter().filter(|x| x.is_live()).count(),
                "drafts": list.iter().filter(|x| !x.is_approved()).count(),
                "total": list.len(),
                "runs_24h": a.runs(None, 500).iter().filter(|r| !r.dry && r.started >= day_ago).count(),
            })
        },
    );
    let bricks = json!([
        {
            "id": "guard", "name": moli_i18n::tr!("serveur.systeme.brique.guard"), "active": true,
            "facts": {
                "protected_rooms": snapshot.guard.protected_rooms,
                "quiet_hours": snapshot.guard.quiet_hours,
                "pending_approvals": snapshot.approvals.len(),
            },
        },
        { "id": "history", "name": moli_i18n::tr!("serveur.systeme.brique.history"), "active": history.is_some(), "facts": { "db_bytes": size("history.db") } },
        { "id": "energy", "name": moli_i18n::tr!("serveur.systeme.brique.energy"), "active": energy.is_some(), "facts": energy_facts },
        { "id": "automations", "name": moli_i18n::tr!("serveur.systeme.brique.automations"), "active": autos.is_some(), "facts": automation_facts },
        {
            "id": "assistant", "name": "Moli",
            "active": moli.as_ref().is_some_and(|m| m.status()["ready"] == json!(true)),
            "facts": moli.as_ref().map(moli_assistant::Assistant::status).unwrap_or_default(),
        },
        { "id": "cameras", "name": moli_i18n::tr!("serveur.systeme.brique.cameras"), "active": cameras > 0, "facts": { "cameras": cameras } },
    ]);

    axum::Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "stats": hub.stats(),
        "drivers": drivers,
        "bricks": bricks,
        "surfaces": [
            { "id": "maison", "name": moli_i18n::tr!("serveur.systeme.surface.maison.nom"), "what": moli_i18n::tr!("serveur.systeme.surface.maison.quoi") },
            { "id": "atelier", "name": moli_i18n::tr!("serveur.systeme.surface.atelier.nom"), "what": moli_i18n::tr!("serveur.systeme.surface.atelier.quoi") },
            { "id": "mcp", "name": "Agents (MCP)", "what": moli_i18n::tr!("serveur.systeme.surface.mcp.quoi") },
            { "id": "rest", "name": "API", "what": moli_i18n::tr!("serveur.systeme.surface.rest.quoi") },
        ],
        "perf": perf_history(data_dir.as_ref()),
        "coverage": read_bench(bench_path.as_deref()).await.map(|b| b["coverage"].clone()),
    }))
    .into_response()
}

/// Each driver with its devices, points, unreachable ones and cameras
/// (device ids are `<instance>:<…>`); and the cameras in all.
fn drivers(snapshot: &Snapshot) -> (Vec<Json>, usize) {
    let mut per: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    for d in &snapshot.devices {
        let e = per.entry(d.device.instance.to_string()).or_default();
        e.0 += 1;
        e.1 += d.device.points.len();
        e.2 += usize::from(d.online == Some(false));
        e.3 += usize::from(d.camera);
    }
    let list = snapshot
        .drivers
        .iter()
        .map(|d| {
            let (devices, points, offline, cameras) =
                per.get(d.instance.as_str()).copied().unwrap_or_default();
            json!({
                "instance": d.instance,
                "kind": d.kind,
                "integration": d.integration,
                "status": d.status,
                "devices": devices,
                "points": points,
                "offline": offline,
                "cameras": cameras,
            })
        })
        .collect();
    (list, per.values().map(|v| v.3).sum())
}

/// The last measurements against Home Assistant (`perf.jsonl`, written by a host script).
fn perf_history(data_dir: Option<&PathBuf>) -> Vec<Json> {
    let Some(text) = data_dir.and_then(|d| std::fs::read_to_string(d.join("perf.jsonl")).ok())
    else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..]
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

//! MCP server: the home, as tools any agent can call.
//!
//! Outputs are compact JSON meant to be read by a model: labels resolved,
//! current values inlined, units attached, writability explicit.

use std::sync::Arc;

use moli_core::{DeviceId, Origin, PointId, Value};
use moli_history::History;
use moli_runtime::{CommandError, DeviceView, Hub, LabelPatch};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value as Json, json};

pub(crate) fn service(
    hub: Hub,
    history: Option<History>,
    bench: Option<std::path::PathBuf>,
    energy: Option<moli_energy::Energy>,
    automations: Option<moli_automation::Automations>,
) -> StreamableHttpService<MoliMcp, LocalSessionManager> {
    // Host validation is done once, for every surface, by `guard`.
    let config = StreamableHttpServerConfig::default()
        .disable_allowed_hosts()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    StreamableHttpService::new(
        move || {
            Ok(MoliMcp {
                hub: hub.clone(),
                history: history.clone(),
                bench: bench.clone(),
                energy: energy.clone(),
                automations: automations.clone(),
            })
        },
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct MoliMcp {
    hub: Hub,
    history: Option<History>,
    bench: Option<std::path::PathBuf>,
    energy: Option<moli_energy::Energy>,
    automations: Option<moli_automation::Automations>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AutomationIdArgs {
    /// Automation id (from list_automations).
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DraftArgs {
    /// The automation, as get_automation returns it: `{ "name", "mode"
    /// (restart|single|queued), "graph": { "nodes": [...], "edges": [...] } }`.
    /// Give an `id` to rework an existing one. Node `type`s: when_state
    /// (point, to, from, for_s), when_threshold (point, above, below, for_s),
    /// at_time (at "HH:MM", days 1=Monday..7), at_sun (event rise|set,
    /// offset_min, days), every (minutes), on_start, manual, if (rules, all →
    /// ports yes/no; rules: {kind: state, point, op eq|ne|gt|ge|lt|le, value}
    /// | {kind: time, after, before, days} | {kind: sun, is day|night}), set
    /// (point, value), toggle (point), wait (seconds), wait_for (rule,
    /// timeout_s → ports ok/timeout), notify (title, message, channels
    /// maison|telegram), write (prompt). Edges: {from, port (out by default),
    /// to}.
    #[schemars(with = "serde_json::Value")]
    automation: Json,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EnergyArgs {
    /// Omit for the headline figures (live power, today, yesterday, this
    /// month and its projection, last month, per meter, unexplained part).
    /// `hour`, `day` or `month` for a series instead.
    #[serde(default)]
    step: Option<String>,
    /// Series length, current bucket included (default 24 hours, 30 days
    /// or 12 months).
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HistoryArgs {
    /// Point id: `<device_id>/<key>`.
    point: String,
    /// How far back, in hours (default 24).
    #[serde(default)]
    hours: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SqlArgs {
    /// One read-only SQL statement (SQLite) over the view
    /// `history(point TEXT, ts INTEGER /* ms since epoch */, kind TEXT /* b i f t z */, num REAL, txt TEXT)`.
    /// Example: `SELECT COUNT(*) FROM history WHERE point LIKE '%/contact' AND num = 0 AND ts > strftime('%s','now','-7 days') * 1000`
    sql: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListDevicesArgs {
    /// Only devices in this room (case-insensitive).
    #[serde(default)]
    room: Option<String>,
    /// Only devices whose name, model or id contains this text (case-insensitive).
    #[serde(default)]
    search: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeviceArgs {
    /// Device id, e.g. `z2m:0x00124b0000000001`.
    device: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CommandArgs {
    /// Point id: `<device_id>/<key>`, e.g. `z2m:0x00124b0000000003/state`.
    point: String,
    /// New value: boolean for on/off points, number for numeric, string for enums.
    value: Json,
    /// Who is acting (your agent name). Recorded in the journal.
    #[serde(default)]
    actor: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LabelArgs {
    /// Device id.
    device: String,
    /// Human name. Empty string clears it.
    #[serde(default)]
    name: Option<String>,
    /// Room. Empty string clears it.
    #[serde(default)]
    room: Option<String>,
    /// Who is acting (your agent name).
    #[serde(default)]
    actor: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JournalArgs {
    /// Maximum number of entries, most recent first (default 20).
    #[serde(default)]
    limit: Option<usize>,
}

#[tool_router]
impl MoliMcp {
    #[tool(
        description = "Health of the system: uptime, memory, devices, points and driver statuses."
    )]
    async fn system_status(&self) -> Result<CallToolResult, McpError> {
        let snapshot = self.hub.snapshot();
        let bench = crate::rest::read_bench(self.bench.as_deref()).await;
        ok(&json!({
            "stats": self.hub.stats(),
            "drivers": snapshot.drivers,
            // So an agent knows beforehand where and when it needs a human.
            "guard": snapshot.guard,
            "pending_approvals": snapshot.approvals,
            // How far Moli OS covers the Home Assistant it runs beside.
            "comparison_with_home_assistant": bench.map(|b| json!({
                "coverage": b["coverage"],
                "ha_memory_bytes": b["ha"]["memory_bytes"],
                "moli_memory_bytes": b["moli"]["memory_bytes"],
                "measured_at": b["generated"],
            })),
        }))
    }

    #[tool(
        description = "List devices with their label, room, availability and every point's current value, unit and writability."
    )]
    async fn list_devices(
        &self,
        Parameters(args): Parameters<ListDevicesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let room = args.room.map(|r| r.to_lowercase());
        let search = args.search.map(|s| s.to_lowercase());
        let devices: Vec<Json> = self
            .hub
            .snapshot()
            .devices
            .iter()
            .filter(|d| {
                room.as_ref()
                    .is_none_or(|r| room_of(d).is_some_and(|dr| dr.to_lowercase() == *r))
            })
            .filter(|d| {
                search.as_ref().is_none_or(|s| {
                    [
                        Some(d.device.id.as_str()),
                        Some(&*d.device.native_name),
                        d.label.name.as_deref(),
                        d.device.model.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|field| field.to_lowercase().contains(s))
                })
            })
            .map(compact)
            .collect();
        ok(&json!({ "count": devices.len(), "devices": devices }))
    }

    #[tool(
        description = "Full detail of one device: its points with kind, allowed values or range, unit, access, and current value."
    )]
    async fn get_device(
        &self,
        Parameters(args): Parameters<DeviceArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.hub.device(&DeviceId::from(args.device)) {
            Some(view) => ok(&serde_json::to_value(view).unwrap_or_default()),
            None => Ok(fail("unknown device")),
        }
    }

    #[tool(
        description = "A fresh image from a camera (devices listed with `camera: true`). Cameras in protected rooms are refused: only a human in the dashboard may look into those rooms."
    )]
    async fn camera_snapshot(
        &self,
        Parameters(args): Parameters<DeviceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let id = DeviceId::from(args.device);
        if let Some(room) = self.hub.protected_room(&id) {
            return Ok(fail(&format!(
                "protected room « {room} »: images are for humans in the dashboard only"
            )));
        }
        match self.hub.camera_image(&id).await {
            Ok(image) => Ok(CallToolResult::success(vec![ContentBlock::image(
                base64(&image.bytes),
                image.content_type,
            )])),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Write a value to a writable point. The order is validated against the point's spec, sent to the device and journaled. The device confirms by reporting its new state."
    )]
    async fn send_command(
        &self,
        Parameters(args): Parameters<CommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        let point = PointId::from(args.point);
        let value = Value::from_json(&args.value);
        match self
            .hub
            .command(&point, value, Origin::Mcp, args.actor)
            .await
        {
            Ok(()) => ok(&json!({ "ok": true, "point": point })),
            // Not an error: the order exists, a human decides. Say it plainly
            // so the agent relays it instead of retrying.
            Err(CommandError::NeedsApproval { id, reason }) => ok(&json!({
                "ok": false,
                "status": "awaiting_human_approval",
                "request": id,
                "reason": reason,
                "message": "Not executed. A human must approve this request from the Moli OS dashboard (it expires in 10 minutes). Do not retry; tell the user.",
            })),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Set the human name and/or room of a device. Labels never change identities."
    )]
    async fn set_label(
        &self,
        Parameters(args): Parameters<LabelArgs>,
    ) -> Result<CallToolResult, McpError> {
        let device = DeviceId::from(args.device);
        let patch = LabelPatch {
            name: args.name,
            room: args.room,
        };
        match self
            .hub
            .set_label(&device, patch, Origin::Mcp, args.actor)
            .await
        {
            Ok(label) => ok(&json!({ "ok": true, "device": device, "label": label })),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Recent writes (commands and label changes): who, through which surface, what, and the outcome."
    )]
    async fn get_journal(
        &self,
        Parameters(args): Parameters<JournalArgs>,
    ) -> Result<CallToolResult, McpError> {
        ok(&serde_json::to_value(self.hub.journal(args.limit.unwrap_or(20))).unwrap_or_default())
    }

    #[tool(
        description = "History of one point: number of changes, min/max/average, first and last values, and a compact series (≤ 60 points) over the last N hours."
    )]
    async fn history(
        &self,
        Parameters(args): Parameters<HistoryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(history) = &self.history else {
            return Ok(fail("history is disabled on this instance"));
        };
        let to = moli_core::now_ms();
        let from = to.saturating_sub(crate::rest::window_ms(args.hours.unwrap_or(24.0)));
        match history.series(&args.point, from, to, 60).await {
            Ok(series) => ok(&summarize(&series)),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Electricity consumption and cost. Without arguments: live power, today, yesterday, this month (with projection) and last month, in kWh and money, per meter (Linky tariffs, circuits) and what no circuit explains. With `step` (hour/day/month): a series of kWh and cost per meter. Days and months are local time."
    )]
    async fn energy(
        &self,
        Parameters(args): Parameters<EnergyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(energy) = &self.energy else {
            return Ok(fail("no energy meters are configured on this instance"));
        };
        let result = match args.step.as_deref() {
            None => energy
                .summary()
                .await
                .map(|s| serde_json::to_value(s).unwrap_or_default()),
            Some(step) => {
                let step = match step {
                    "hour" => moli_energy::Step::Hour,
                    "day" => moli_energy::Step::Day,
                    "month" => moli_energy::Step::Month,
                    other => return Ok(fail(&format!("step {other:?}: use hour, day or month"))),
                };
                let count = args
                    .count
                    .unwrap_or_else(|| crate::rest::default_count(step));
                energy
                    .recent(step, count)
                    .await
                    .map(|r| serde_json::to_value(r).unwrap_or_default())
            }
        };
        match result {
            Ok(value) => ok(&value),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Automations of the house (graphs of triggers, conditions, actions): id, name, what it does in French, whether it is live (switched on AND approved by a person)."
    )]
    async fn list_automations(&self) -> Result<CallToolResult, McpError> {
        let Some(autos) = &self.automations else {
            return Ok(fail("automations are not running"));
        };
        let list: Vec<Json> = autos
            .list()
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "name": a.name,
                    "summary": autos.check(&a.graph).summary,
                    "enabled": a.enabled,
                    "approved": a.is_approved(),
                    "live": a.is_live(),
                    "author": a.author,
                })
            })
            .collect();
        ok(&json!({ "automations": list }))
    }

    #[tool(
        description = "One automation: its graph, the checker's problems, protected rooms it reaches, and its last runs (step by step)."
    )]
    async fn get_automation(
        &self,
        Parameters(args): Parameters<AutomationIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(autos) = &self.automations else {
            return Ok(fail("automations are not running"));
        };
        match autos.get(&args.id) {
            Some(a) => ok(&json!({
                "automation": a,
                "check": autos.check(&a.graph),
                "runs": autos.runs(Some(&a.id), 5),
            })),
            None => Ok(fail("unknown automation")),
        }
    }

    #[tool(
        description = "Save an automation as a DRAFT. Nothing runs until a person approves it in the Moli OS dashboard (any change you make to an approved one needs their approval again). Returns the checker's verdict and the French summary: fix the errors it lists."
    )]
    async fn save_automation_draft(
        &self,
        Parameters(args): Parameters<DraftArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(autos) = &self.automations else {
            return Ok(fail("automations are not running"));
        };
        let mut value = args.automation;
        let Some(fields) = value.as_object_mut() else {
            return Ok(fail("automation must be a JSON object"));
        };
        fields.entry("id").or_insert(json!(""));
        fields.entry("enabled").or_insert(json!(false));
        let a: moli_automation::Automation = match serde_json::from_value(value) {
            Ok(a) => a,
            Err(e) => return Ok(fail(&format!("not an automation: {e}"))),
        };
        let by = moli_automation::Actor {
            human: false,
            author: moli_automation::Author::Agent,
        };
        match autos.save(a, by).await {
            Ok(saved) => ok(&json!({
                "id": saved.id,
                "status": "draft: a person must approve it in the dashboard",
                "check": autos.check(&saved.graph),
            })),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Dry run of an automation: conditions are read for real, nothing acts, nothing waits. Shows the path it would take, step by step."
    )]
    async fn test_automation(
        &self,
        Parameters(args): Parameters<AutomationIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(autos) = &self.automations else {
            return Ok(fail("automations are not running"));
        };
        let by = moli_automation::Actor {
            human: false,
            author: moli_automation::Author::Agent,
        };
        match autos.run_now(&args.id, true, None, by).await {
            Ok(run) => ok(&serde_json::to_value(run).unwrap_or_default()),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }

    #[tool(
        description = "Run ONE read-only SQL query (SQLite) over the recorded history. View: history(point, ts ms, kind b/i/f/t/z, num, txt). Writes, ATTACH and PRAGMA are refused; 2 s and 200 rows max. Use it for questions the other tools cannot answer (counts, durations, comparisons)."
    )]
    async fn history_sql(
        &self,
        Parameters(args): Parameters<SqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(history) = &self.history else {
            return Ok(fail("history is disabled on this instance"));
        };
        match history.sql(&args.sql, 200).await {
            Ok(result) => ok(&serde_json::to_value(result).unwrap_or_default()),
            Err(e) => Ok(fail(&e.to_string())),
        }
    }
}

/// Statistics a model can read at a glance, plus the series itself.
fn summarize(series: &moli_history::Series) -> Json {
    // (value, weight): raw changes weigh 1, buckets weigh their sample count.
    // A value held since before the window counts too: a stable point still
    // has statistics.
    let before = series.before.as_ref().and_then(|(_, v)| v.as_f64());
    #[allow(clippy::cast_precision_loss)]
    let weighted: Vec<(f64, f64, f64, f64)> = if series.buckets.is_empty() {
        series
            .raw
            .iter()
            .filter_map(|(_, v)| v.as_f64())
            .chain(before)
            .map(|n| (n, n, n, 1.0))
            .collect()
    } else {
        series
            .buckets
            .iter()
            .map(|b| (b.avg, b.min, b.max, b.count as f64))
            .chain(before.map(|n| (n, n, n, 1.0)))
            .collect()
    };
    let stats = (!weighted.is_empty()).then(|| {
        let min = weighted.iter().map(|w| w.1).fold(f64::INFINITY, f64::min);
        let max = weighted
            .iter()
            .map(|w| w.2)
            .fold(f64::NEG_INFINITY, f64::max);
        let total: f64 = weighted.iter().map(|w| w.3).sum();
        let avg = weighted.iter().map(|w| w.0 * w.3).sum::<f64>() / total;
        json!({ "min": min, "max": max, "avg": avg })
    });
    json!({
        "point": series.point,
        "from": series.from,
        "to": series.to,
        "changes": series.total,
        "value_before_window": series.before.as_ref().map(|(_, v)| v),
        "stats": stats,
        "first": series.first,
        "last": series.last,
        "series": if series.buckets.is_empty() { json!(series.raw) } else { json!(series.buckets) },
        "truncated": series.truncated,
    })
}

// Macro arguments must be literals; the version comes from this crate.
#[allow(clippy::unused_async_trait_impl)] // generated by the macro
#[tool_handler(
    name = "moli-os",
    instructions = "Moli OS controls a real home. Point ids look like `<device_id>/<key>`. Call list_devices first; only points marked writable accept send_command. Every write is journaled with your actor name. Prefer harmless points when testing and restore the previous value."
)]
impl ServerHandler for MoliMcp {}

/// The user's room, else the one the underlying system knows.
fn room_of(view: &DeviceView) -> Option<&str> {
    view.label
        .room
        .as_deref()
        .or(view.device.native_room.as_deref())
}

/// One device, condensed for a model's context window.
fn compact(view: &DeviceView) -> Json {
    let points: Vec<Json> = view
        .device
        .points
        .iter()
        .map(|p| {
            let mut point = json!({
                "key": p.key,
                "value": view.state.get(&p.key).map(|s| &s.value),
            });
            if let Some(unit) = &p.unit {
                point["unit"] = json!(unit);
            }
            if p.access.write {
                point["writable"] = json!(true);
            }
            point
        })
        .collect();
    json!({
        "id": view.device.id,
        "name": view.label.name.as_deref().unwrap_or(&view.device.native_name),
        "room": room_of(view),
        "model": view.device.model,
        "description": view.device.description,
        "online": view.online,
        "points": points,
    })
}

#[allow(clippy::unnecessary_wraps)]
fn ok(value: &Json) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        value.to_string(),
    )]))
}

fn fail(message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.to_owned())])
}

/// Standard base64 (MCP images travel as text).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::base64(&[0xFF, 0xD8, 0xFF]), "/9j/");
    }
}

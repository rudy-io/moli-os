//! `/api/heating/away`: « Je pars », every heater to eco whatever it was
//! doing, and « Je suis rentré », each one put back as it was.
//!
//! - `GET`: `{ "away": bool, "since": ms | null, "heaters": n }`.
//! - `POST {"away": true}`: a person (PIN session: the bedrooms' heaters are
//!   among them) leaves. Each heater's switch and mode are kept in
//!   `<data>/heating-away.json` first, then it is switched on (a stopped one
//!   too) and set to eco. Leaving again keeps what was kept the first time.
//! - `POST {"away": false}`: each heater gets its mode back, then its switch;
//!   one that did not take it stays in the file, for another try.
//!
//! A heater: a pilot-wire module (`device_mode_type`) whose `mode` takes
//! `eco`. Every order goes through the hub (guard, journal).

use std::path::Path as FsPath;
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::{DeviceId, Kind, Origin, PointId, Value};
use moli_runtime::{DeviceView, Hub};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::caller::Caller;
use crate::plan::PlanDir;
use crate::session::Sessions;

const FILE: &str = "heating-away.json";
const ECO: &str = "eco";
/// One leave or return at a time: two phones never mix their saves.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A heater's orders, in the order they leave.
type Orders = Vec<(&'static str, Value)>;

/// A heater as it was before the house was left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Kept {
    device: DeviceId,
    #[serde(default)]
    on: Option<bool>,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Away {
    /// Milliseconds since 1970.
    since: u64,
    heaters: Vec<Kept>,
}

/// The heaters, as they are now.
fn heaters(devices: &[DeviceView]) -> Vec<Kept> {
    devices
        .iter()
        .filter(|v| {
            let d = &v.device;
            d.points.iter().any(|p| &*p.key == "device_mode_type")
                && d.points.iter().any(|p| {
                    &*p.key == "mode"
                        && p.access.write
                        && matches!(&p.kind, Kind::Enum { values } if values.iter().any(|m| &**m == ECO))
                })
        })
        .map(|v| Kept {
            device: v.device.id.clone(),
            on: match v.state.get("on").map(|s| &s.value) {
                Some(Value::Bool(on)) => Some(*on),
                _ => None,
            },
            mode: match v.state.get("mode").map(|s| &s.value) {
                Some(Value::Text(mode)) => Some(mode.to_string()),
                _ => None,
            },
        })
        .collect()
}

/// Leaving: switched on first (a stopped heater takes no mode), then eco.
fn leave_orders(now: &Kept) -> Orders {
    let mut orders = Vec::new();
    if now.on == Some(false) {
        orders.push(("on", Value::Bool(true)));
    }
    if now.mode.as_deref() != Some(ECO) {
        orders.push(("mode", Value::Text(ECO.into())));
    }
    orders
}

/// Back: the mode it had, and its switch as it was; switched on before
/// the mode, switched off after it.
fn back_orders(was: &Kept, now: &Kept) -> Orders {
    let mode = was
        .mode
        .as_ref()
        .filter(|m| now.mode.as_ref() != Some(*m))
        .map(|m| ("mode", Value::Text(m.as_str().into())));
    let switch = was
        .on
        .filter(|on| now.on != Some(*on))
        .map(|on| ("on", Value::Bool(on)));
    match switch {
        Some(on @ (_, Value::Bool(true))) => [Some(on), mode].into_iter().flatten().collect(),
        _ => [mode, switch].into_iter().flatten().collect(),
    }
}

fn read(dir: &FsPath) -> Option<Away> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write(dir: &FsPath, away: Option<&Away>) -> std::io::Result<()> {
    let path = dir.join(FILE);
    let Some(away) = away else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    };
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(away)? + "\n")?;
    std::fs::rename(tmp, path)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Sends each heater its orders, all at once (each one's in turn); the
/// heaters that did not take them all, with why.
async fn send(hub: &Hub, plans: Vec<(DeviceId, Orders)>, actor: &str) -> Vec<(DeviceId, String)> {
    // A loop, not a closure: the futures stay `Send` for the route.
    let mut each = Vec::with_capacity(plans.len());
    for (device, orders) in plans {
        each.push(one(hub, device, orders, actor));
    }
    futures::future::join_all(each)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// One heater's orders, one after the other; the first refusal stops them.
async fn one(
    hub: &Hub,
    device: DeviceId,
    orders: Orders,
    actor: &str,
) -> Option<(DeviceId, String)> {
    for (key, value) in orders {
        let point = PointId::new(&device, key);
        if let Err(e) = hub
            .command(&point, value, Origin::Ui, Some(actor.to_owned()))
            .await
        {
            return Some((device, e.to_string()));
        }
    }
    None
}

pub(crate) async fn get(
    axum::extract::State(hub): axum::extract::State<Hub>,
    Extension(PlanDir(dir)): Extension<PlanDir>,
) -> Response {
    let away = dir.as_deref().and_then(read);
    Json(json!({
        "away": away.is_some(),
        "since": away.as_ref().map(|a| a.since),
        "heaters": heaters(&hub.snapshot().devices).len(),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct AwayBody {
    away: bool,
}

pub(crate) async fn post(
    axum::extract::State(hub): axum::extract::State<Hub>,
    Extension(PlanDir(dir)): Extension<PlanDir>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    Json(body): Json<AwayBody>,
) -> Response {
    if !humans.is_human(&headers, &caller.key) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.chauffage.humain_part"),
        );
    }
    let Some(dir) = dir else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.plan.pas_de_dossier"),
        );
    };
    let _one = ONE_AT_A_TIME.lock().await;
    let actor = caller.describe();
    let now = heaters(&hub.snapshot().devices);
    let kept = read(&dir);
    let failed = if body.away {
        // Kept before anything moves; leaving twice keeps the first.
        let away = kept.unwrap_or_else(|| Away {
            since: now_ms(),
            heaters: now.clone(),
        });
        if let Err(e) = write(&dir, Some(&away)) {
            return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
        let plans = now
            .iter()
            .map(|h| (h.device.clone(), leave_orders(h)))
            .collect();
        send(&hub, plans, &actor).await
    } else {
        let Some(mut away) = kept else {
            return Json(json!({ "away": false, "failed": [] })).into_response();
        };
        let plans = away
            .heaters
            .iter()
            .filter_map(|was| {
                let current = now.iter().find(|h| h.device == was.device)?;
                Some((was.device.clone(), back_orders(was, current)))
            })
            .collect();
        let failed = send(&hub, plans, &actor).await;
        // What did not come back is tried again at the next « back ».
        away.heaters
            .retain(|was| failed.iter().any(|(device, _)| *device == was.device));
        let left = (!away.heaters.is_empty()).then_some(&away);
        if let Err(e) = write(&dir, left) {
            return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
        failed
    };
    let away = read(&dir);
    Json(json!({
        "away": away.is_some(),
        "since": away.as_ref().map(|a| a.since),
        "failed": failed
            .iter()
            .map(|(device, why)| json!({ "device": device, "error": why }))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(on: Option<bool>, mode: Option<&str>) -> Kept {
        Kept {
            device: DeviceId::from("tuya:radiateur"),
            on,
            mode: mode.map(str::to_owned),
        }
    }

    fn keys(orders: &[(&str, Value)]) -> Vec<String> {
        orders
            .iter()
            .map(|(k, v)| match v {
                Value::Bool(b) => format!("{k}={b}"),
                Value::Text(t) => format!("{k}={t}"),
                other => format!("{k}={other:?}"),
            })
            .collect()
    }

    fn heater(id: &str, wiring: bool, modes: &[&str], on: bool, mode: &str) -> DeviceView {
        let mut points = vec![
            json!({ "key": "on", "label": "on", "kind": { "type": "binary" },
                    "access": { "read": true, "write": true }, "semantic": "other" }),
            json!({ "key": "mode", "label": "mode", "kind": { "type": "enum", "values": modes },
                    "access": { "read": true, "write": true }, "semantic": "other" }),
        ];
        if wiring {
            points.push(json!({ "key": "device_mode_type", "label": "wiring",
                "kind": { "type": "enum", "values": ["four", "six", "switch"] },
                "access": { "read": true, "write": true }, "semantic": "other" }));
        }
        let device: moli_core::Device = serde_json::from_value(json!({
            "id": id, "instance": "tuya", "native_name": id, "points": points,
        }))
        .unwrap();
        let sample = |value| moli_core::Sample { value, ts: 1 };
        DeviceView {
            device: Arc::new(device),
            label: moli_core::Label::default(),
            online: Some(true),
            state: [
                (Arc::from("on"), sample(Value::Bool(on))),
                (Arc::from("mode"), sample(Value::Text(mode.into()))),
            ]
            .into_iter()
            .collect(),
            camera: false,
            printer: false,
        }
    }

    #[test]
    fn heaters_are_pilot_wire_modules_that_take_eco() {
        let found = heaters(&[
            heater(
                "tuya:salon",
                true,
                &["comfort", "eco", "program"],
                false,
                "program",
            ),
            // A pellet stove's mode is its state, not an order: no wiring.
            heater("micronova:poele", false, &["off", "on", "eco"], true, "on"),
            heater("tuya:autre", true, &["comfort"], true, "comfort"),
        ]);
        assert_eq!(
            found,
            [Kept {
                device: DeviceId::from("tuya:salon"),
                on: Some(false),
                mode: Some("program".into()),
            }]
        );
    }

    #[test]
    fn leaving_switches_on_then_eco() {
        assert_eq!(
            keys(&leave_orders(&kept(Some(false), Some("program")))),
            ["on=true", "mode=eco"]
        );
        assert_eq!(
            keys(&leave_orders(&kept(Some(true), Some("comfort")))),
            ["mode=eco"]
        );
        assert!(leave_orders(&kept(Some(true), Some("eco"))).is_empty());
    }

    #[test]
    fn coming_back_puts_mode_and_switch_back() {
        let eco = kept(Some(true), Some("eco"));
        // Stopped before: its mode, then switched off.
        assert_eq!(
            keys(&back_orders(&kept(Some(false), Some("program")), &eco)),
            ["mode=program", "on=false"]
        );
        // Switched off while away, on before: switched on first.
        assert_eq!(
            keys(&back_orders(
                &kept(Some(true), Some("comfort")),
                &kept(Some(false), Some("eco"))
            )),
            ["on=true", "mode=comfort"]
        );
        // Eco before: nothing to do.
        assert!(back_orders(&kept(Some(true), Some("eco")), &eco).is_empty());
        // Unknown before: left as it is.
        assert!(back_orders(&kept(None, None), &eco).is_empty());
    }

    #[test]
    fn the_file_round_trips_and_goes_away() {
        let dir = std::env::temp_dir().join(format!("moli-away-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read(&dir).is_none());
        let away = Away {
            since: 5,
            heaters: vec![kept(Some(false), Some("program"))],
        };
        write(&dir, Some(&away)).unwrap();
        let back = read(&dir).unwrap();
        assert_eq!((back.since, back.heaters), (5, away.heaters));
        write(&dir, None).unwrap();
        assert!(read(&dir).is_none());
        // Gone twice is fine.
        write(&dir, None).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

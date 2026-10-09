//! Rate limits on what writes, per caller (`Caller::key`).
//!
//! A misbehaving agent, script or device on the network must not flood the
//! house with orders, the journal with entries or the dashboard with
//! approval cards. Reads are free; writes get a budget per ten seconds. The
//! dashboard's own budget is larger (a slider sends many values).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::caller::Caller;

const WINDOW: Duration = Duration::from_secs(10);
/// Past this many callers tracked, stale windows are forgotten.
const MAX_TRACKED: usize = 1_024;

/// Per (caller, class): when the current window started, what was spent.
type Windows = HashMap<(String, &'static str), (Instant, u32)>;

#[derive(Debug, Default)]
pub(crate) struct Limits {
    windows: Mutex<Windows>,
}

/// What a request spends, and how much of it a caller may spend per window.
fn budget(method: &Method, path: &str, from_dashboard: bool) -> Option<(&'static str, u32)> {
    let writes = *method == Method::POST || *method == Method::PUT || *method == Method::DELETE;
    if !writes {
        return None;
    }
    if path == "/api/command" || path.starts_with("/api/labels/") {
        return Some(if from_dashboard {
            ("dashboard", 100)
        } else {
            ("command", 30)
        });
    }
    if path.starts_with("/api/approvals/") {
        return Some(("decide", 30));
    }
    if path.starts_with("/mcp") {
        return Some(("mcp", 120));
    }
    if path == "/api/session" || path == "/api/session/pin" || path == "/api/session/setup" {
        return Some(("session", 10));
    }
    if path.starts_with("/api/phones/") || path == "/api/mobile/register" {
        return Some(("phone", 20));
    }
    if path.starts_with("/api/assistant") || path.starts_with("/api/plan/images") {
        return Some(("heavy", 20));
    }
    // A computer's agent reports every 5 s; a launch is a person's.
    if path.starts_with("/api/machines/") {
        return Some(("agent", 30));
    }
    // Pages of a document, sent one after the other.
    if path.starts_with("/api/devices/") && path.ends_with("/print") {
        return Some(("print", 40));
    }
    None
}

impl Limits {
    /// Whether `caller` may spend one more unit of `class` (capacity `max`).
    pub(crate) fn allow(&self, key: &str, class: &'static str, max: u32) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        if windows.len() > MAX_TRACKED {
            windows.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        }
        let slot = windows.entry((key.to_owned(), class)).or_insert((now, 0));
        if now.duration_since(slot.0) >= WINDOW {
            *slot = (now, 0);
        }
        if slot.1 >= max {
            return false;
        }
        slot.1 += 1;
        true
    }
}

/// Middleware, after `caller::identify`.
pub(crate) async fn check(
    State(limits): State<Arc<Limits>>,
    request: Request,
    next: Next,
) -> Response {
    // The dashboard's larger budget is for the household only (a header
    // alone is not enough).
    let from_dashboard = request
        .headers()
        .get("x-moli-origin")
        .is_some_and(|v| v.as_bytes() == b"ui")
        && request
            .extensions()
            .get::<Caller>()
            .is_some_and(Caller::is_household);
    if let (Some((class, max)), Some(caller)) = (
        budget(request.method(), request.uri().path(), from_dashboard),
        request.extensions().get::<Caller>(),
    ) && !limits.allow(&caller.key, class, max)
    {
        tracing::warn!(client = caller.key, class, "rate limit reached");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(json!({ "error": moli_i18n::tr!("serveur.limite.trop_de_demandes") })),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_have_a_budget_per_caller() {
        let limits = Limits::default();
        for _ in 0..30 {
            assert!(limits.allow("192.168.0.30", "command", 30));
        }
        assert!(!limits.allow("192.168.0.30", "command", 30), "31st refused");
        assert!(
            limits.allow("192.168.0.20", "command", 30),
            "another caller is not affected"
        );
        assert!(
            limits.allow("192.168.0.30", "mcp", 120),
            "another class neither"
        );
    }

    #[test]
    fn only_writes_are_counted() {
        assert_eq!(budget(&Method::GET, "/api/devices", false), None);
        assert_eq!(
            budget(&Method::POST, "/api/command", false),
            Some(("command", 30))
        );
        assert_eq!(
            budget(&Method::POST, "/api/command", true),
            Some(("dashboard", 100))
        );
        assert_eq!(
            budget(&Method::PUT, "/api/labels/x", false),
            Some(("command", 30))
        );
        assert_eq!(budget(&Method::POST, "/mcp", false), Some(("mcp", 120)));
        assert_eq!(
            budget(&Method::POST, "/api/assistant", false),
            Some(("heavy", 20))
        );
        assert_eq!(
            budget(&Method::POST, "/api/session", true),
            Some(("session", 10))
        );
        assert_eq!(budget(&Method::POST, "/api/automations/x/run", false), None);
    }
}

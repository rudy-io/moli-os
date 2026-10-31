//! Bounded local requests. PUT is never retried, even on ambiguous failure.
use crate::Config;
use anyhow::{Context as _, ensure};
use serde_json::Value as Json;
use std::time::Duration;
fn failure() -> String {
    moli_i18n::tr!("airzone.request_failed")
}
fn check_errors(value: &Json) -> anyhow::Result<()> {
    if let Some(errors) = value.get("errors") {
        ensure!(
            errors.is_null()
                || errors.as_array().is_some_and(Vec::is_empty)
                || errors.as_object().is_some_and(serde_json::Map::is_empty),
            failure()
        );
    }
    Ok(())
}
pub(crate) async fn exchange(
    config: &Config,
    method: &str,
    path: &str,
    body: &Json,
    budget: Duration,
) -> anyhow::Result<Json> {
    ensure!(!budget.is_zero(), moli_i18n::tr!("airzone.expired"));
    let request = moli_net::json_body(
        http::Request::builder()
            .method(method)
            .uri(format!("/api/v1/{path}"))
            .header("host", format!("{}:{}", config.host, config.port)),
        body,
    )?;
    let (status, bytes) =
        tokio::time::timeout(budget, moli_net::plain(&config.host, config.port, request))
            .await
            .map_err(|_| anyhow::anyhow!(failure()))?
            .map_err(|_| anyhow::anyhow!(failure()))?;
    ensure!(status.is_success(), failure());
    let value: Json = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("airzone.invalid")))?;
    ensure!(value.is_object(), moli_i18n::tr!("airzone.invalid"));
    check_errors(&value)?;
    Ok(value)
}
pub(crate) fn acknowledge(value: &Json, command: &Json) -> anyhow::Result<()> {
    check_errors(value)?;
    let data = match &value["data"] {
        Json::Object(_) => &value["data"],
        Json::Array(a) if a.len() == 1 => &a[0],
        _ => anyhow::bail!(failure()),
    };
    for key in ["systemID", "zoneID"] {
        ensure!(data[key] == command[key], failure());
    }
    let object = command.as_object().context(failure())?;
    for (key, want) in object {
        if key == "systemID" || key == "zoneID" {
            continue;
        }
        if let Some(actual) = data.get(key) {
            let equal = actual == want
                || actual
                    .as_f64()
                    .zip(want.as_f64())
                    .is_some_and(|(a, b)| (a - b).abs() < 1e-6);
            ensure!(equal, failure());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json as Reply, Router,
        body::Bytes,
        extract::State,
        http::{Method, Uri},
        routing::any,
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    async fn server(response: Json) -> (Config, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/api/v1/hvac",
                any(
                    |State((response, calls)): State<(Json, Arc<AtomicUsize>)>,
                     method: Method,
                     uri: Uri,
                     body: Bytes| async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(method, Method::POST);
                        assert_eq!(uri.path(), "/api/v1/hvac");
                        assert_eq!(
                            serde_json::from_slice::<Json>(&body).unwrap(),
                            json!({"systemID":0,"zoneID":0})
                        );
                        Reply(response)
                    },
                ),
            )
            .with_state((response, calls.clone()));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            axum::serve(tcp, app).await.unwrap();
        });
        (
            Config {
                host: "127.0.0.1".into(),
                port,
                poll_secs: 30,
                mac: None,
            },
            task,
            calls,
        )
    }
    #[tokio::test]
    async fn post_exact_broadcast_read_and_reject_api_errors_without_retry() {
        let (c, t, n) = server(json!({"systems":[]})).await;
        assert_eq!(
            exchange(
                &c,
                "POST",
                "hvac",
                &json!({"systemID":0,"zoneID":0}),
                Duration::from_secs(1)
            )
            .await
            .unwrap(),
            json!({"systems":[]})
        );
        assert_eq!(n.load(Ordering::SeqCst), 1);
        t.abort();
        let (c, t, n) = server(json!({"data":{},"errors":{"on":"rejected"}})).await;
        assert!(
            exchange(
                &c,
                "POST",
                "hvac",
                &json!({"systemID":0,"zoneID":0}),
                Duration::from_secs(1)
            )
            .await
            .is_err()
        );
        assert_eq!(n.load(Ordering::SeqCst), 1);
        t.abort();
    }
    #[tokio::test]
    async fn expired_exchange_never_reaches_network() {
        let (c, t, n) = server(json!({"systems":[]})).await;
        assert!(
            exchange(
                &c,
                "POST",
                "hvac",
                &json!({"systemID":0,"zoneID":0}),
                Duration::ZERO
            )
            .await
            .is_err()
        );
        assert_eq!(n.load(Ordering::SeqCst), 0);
        t.abort();
    }
    #[tokio::test]
    async fn http_failures_invalid_json_and_timeout_are_not_success() {
        use axum::http::StatusCode;
        for (status, body, delay) in [
            (StatusCode::INTERNAL_SERVER_ERROR, "{}", 0),
            (StatusCode::OK, "{", 0),
            (StatusCode::OK, "", 0),
            (StatusCode::OK, "{}", 100),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let app = Router::new().route(
                "/api/v1/hvac",
                any(move || {
                    let counted = counted.clone();
                    async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        (status, body)
                    }
                }),
            );
            let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = tcp.local_addr().unwrap().port();
            let task = tokio::spawn(async move {
                axum::serve(tcp, app).await.unwrap();
            });
            let config = Config {
                host: "127.0.0.1".into(),
                port,
                poll_secs: 30,
                mac: None,
            };
            assert!(
                exchange(
                    &config,
                    "PUT",
                    "hvac",
                    &json!({"systemID":1,"zoneID":2,"on":1}),
                    Duration::from_millis(40)
                )
                .await
                .is_err()
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            task.abort();
        }
    }
    #[test]
    fn accepts_both_ack_shapes_but_rejects_wrong_ids_values_and_envelope_errors() {
        let command = json!({"systemID":1,"zoneID":8,"mode":3});
        for response in [
            json!({"data":{"systemID":1,"zoneID":8,"mode":3}}),
            json!({"data":[{"systemID":1,"zoneID":8}]}),
        ] {
            acknowledge(&response, &command).unwrap();
        }
        for response in [
            json!({"data":{"systemID":1,"zoneID":2}}),
            json!({"data":{"systemID":1,"zoneID":8,"mode":2}}),
            json!({"data":{"systemID":1,"zoneID":8},"errors":[{"mode":"rejected"}]}),
            json!({"data":[]}),
            json!({"data":true}),
        ] {
            assert!(acknowledge(&response, &command).is_err());
        }
    }
}

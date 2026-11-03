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
            read_only: false,
            tls_fingerprint: None,
            dhw_units: None,
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
            read_only: false,
            tls_fingerprint: None,
            dhw_units: None,
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

#[test]
fn acknowledgements_address_iaq_system_dhw_and_integration_exactly() {
    for (command, response) in [
        (
            json!({"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1}),
            json!({"data":{"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1}}),
        ),
        (
            json!({"systemID":1,"qAdapt":2}),
            json!({"data":{"systemID":1,"qAdapt":2}}),
        ),
        (
            json!({"systemID":0,"acs_power":1}),
            json!({"data":{"systemID":0,"acs_power":1}}),
        ),
        (json!({"driver":"moli-1"}), json!({"driver":"moli-1"})),
    ] {
        assert!(acknowledge(&response, &command).is_ok(), "{command}");
    }
    for (command, response) in [
        (
            json!({"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1}),
            json!({"data":{"systemID":1,"iaqsensorID":3,"iaq_mode_vent":1}}),
        ),
        (
            json!({"systemID":1,"qAdapt":2}),
            json!({"data":{"systemID":1,"zoneID":2,"qAdapt":2}}),
        ),
        (
            json!({"systemID":0,"acs_power":1}),
            json!({"data":{"systemID":1,"acs_power":1}}),
        ),
        (json!({"driver":"moli-1"}), json!({"driver":"other"})),
    ] {
        assert!(acknowledge(&response, &command).is_err(), "{response}");
    }
}

#[test]
fn iaq_acknowledgement_cannot_confirm_a_different_sensor() {
    assert!(
        acknowledge(
            &json!({"data":{"systemID":1,"iaqsensorID":3}}),
            &json!({"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1})
        )
        .is_err()
    );
}

#[test]
fn acknowledgements_validate_documented_alias_values_and_reject_conflicts() {
    let command = json!({"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1});
    assert!(
        acknowledge(
            &json!({"data":{"systemID":1,"iaqsensorID":2,"IAQ_Mode_Vent":2}}),
            &command
        )
        .is_err()
    );
    assert!(
        acknowledge(
            &json!({"data":{"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1,"IAQ_Mode_Vent":2}}),
            &command
        )
        .is_err()
    );
    acknowledge(
        &json!({"data":{"systemid":1,"iaqsensorid":2,"IAQ_Mode_Vent":1}}),
        &command,
    )
    .unwrap();
}

async fn recording_server() -> (Config, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = Router::new().fallback(any(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        async { Reply(json!({"data":{}})) }
    }));
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
            read_only: false,
            tls_fingerprint: None,
            dhw_units: None,
        },
        task,
        calls,
    )
}

#[tokio::test]
async fn read_only_transport_never_sends_any_put() {
    let (mut config, task, calls) = recording_server().await;
    config.read_only = true;
    for (path, body) in [
        ("hvac", json!({"systemID":1,"zoneID":2,"on":1})),
        ("hvac", json!({"systemID":0,"acs_power":1})),
        (
            "iaq",
            json!({"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1}),
        ),
        ("integration", json!({"driver":"moli"})),
    ] {
        assert!(
            exchange(&config, "PUT", path, &body, Duration::from_secs(1))
                .await
                .is_err()
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn exchange_rejects_unknown_paths_broadcast_writes_and_multiple_parameters() {
    let (config, task, calls) = recording_server().await;
    for (method, path, body) in [
        ("POST", "../outside", json!({})),
        ("DELETE", "hvac", json!({"systemID":1,"zoneID":2})),
        ("PUT", "webserver", json!({"mac":"aa:bb:cc:dd:ee:ff"})),
        ("PUT", "hvac", json!({"systemID":0,"zoneID":2,"on":1})),
        ("PUT", "hvac", json!({"systemID":1,"zoneID":0,"on":1})),
        ("PUT", "hvac", json!({"systemID":127,"mode":1})),
        (
            "PUT",
            "iaq",
            json!({"systemID":1,"iaqsensorID":0,"iaq_mode_vent":1}),
        ),
        (
            "PUT",
            "hvac",
            json!({"systemID":1,"zoneID":2,"on":1,"setpoint":20}),
        ),
    ] {
        assert!(
            exchange(&config, method, path, &body, Duration::from_secs(1))
                .await
                .is_err(),
            "{method} {path} {body}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One matrix keeps exact route, method and body expectations together.
async fn typed_reads_use_exact_documented_paths_queries_and_bodies() {
    use std::sync::Mutex;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let app = Router::new().fallback(any(move |method: Method, uri: Uri, body: Bytes| {
        observed
            .lock()
            .unwrap()
            .push((method.to_string(), uri.to_string(), body.to_vec()));
        async { Reply(json!({"data":{}})) }
    }));
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
        read_only: true,
        tls_fingerprint: None,
        dhw_units: None,
    };
    for (request, method, expected_method, expected_uri, expected_body) in [
        (
            ReadRequest::Hvac {
                system: 0,
                zone: Some(0),
            },
            ReadMethod::Post,
            "POST",
            "/api/v1/hvac",
            Some(json!({"systemID":0,"zoneID":0})),
        ),
        (
            ReadRequest::Hvac {
                system: 1,
                zone: Some(2),
            },
            ReadMethod::Get,
            "GET",
            "/api/v1/hvac?systemid=1&zoneid=2",
            None,
        ),
        (
            ReadRequest::Hvac {
                system: 127,
                zone: None,
            },
            ReadMethod::Post,
            "POST",
            "/api/v1/hvac",
            Some(json!({"systemID":127})),
        ),
        (
            ReadRequest::Hvac {
                system: 0,
                zone: None,
            },
            ReadMethod::Get,
            "GET",
            "/api/v1/hvac?systemid=0",
            None,
        ),
        (
            ReadRequest::Iaq {
                system: 1,
                sensor: 0,
            },
            ReadMethod::Post,
            "POST",
            "/api/v1/iaq",
            Some(json!({"systemID":1,"iaqsensorID":0})),
        ),
        (
            ReadRequest::Iaq {
                system: 1,
                sensor: 2,
            },
            ReadMethod::Get,
            "GET",
            "/api/v1/iaq?systemid=1&iaqsensorid=2",
            None,
        ),
        (
            ReadRequest::Integration,
            ReadMethod::Post,
            "POST",
            "/api/v1/integration",
            Some(json!({})),
        ),
        (
            ReadRequest::Version,
            ReadMethod::Post,
            "POST",
            "/api/v1/version",
            Some(json!({})),
        ),
        (
            ReadRequest::Webserver,
            ReadMethod::Post,
            "POST",
            "/api/v1/webserver",
            Some(json!({})),
        ),
        (
            ReadRequest::Demo,
            ReadMethod::Post,
            "POST",
            "/api/v1/demo",
            Some(json!({})),
        ),
    ] {
        read(&config, &request, method, Duration::from_secs(1))
            .await
            .unwrap();
        let (actual_method, actual_uri, actual_body) = requests.lock().unwrap().pop().unwrap();
        assert_eq!(actual_method, expected_method);
        assert_eq!(actual_uri, expected_uri);
        match expected_body {
            Some(expected) => assert_eq!(
                serde_json::from_slice::<Json>(&actual_body).unwrap(),
                expected
            ),
            None => assert!(actual_body.is_empty()),
        }
    }
    for (request, method) in [
        (
            ReadRequest::Hvac {
                system: 127,
                zone: Some(0),
            },
            ReadMethod::Post,
        ),
        (
            ReadRequest::Hvac {
                system: 33,
                zone: None,
            },
            ReadMethod::Post,
        ),
        (
            ReadRequest::Iaq {
                system: 33,
                sensor: 1,
            },
            ReadMethod::Get,
        ),
        (ReadRequest::Version, ReadMethod::Get),
    ] {
        let error = read(&config, &request, method, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<Failure>(),
            Some(&Failure::InvalidRequest)
        );
    }
    assert!(requests.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn each_documented_put_is_single_and_preserves_its_addressed_body() {
    use std::sync::Mutex;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let app = Router::new().fallback(any(move |method: Method, uri: Uri, body: Bytes| {
        let body: Json = serde_json::from_slice(&body).unwrap();
        observed
            .lock()
            .unwrap()
            .push((method.to_string(), uri.to_string(), body.clone()));
        async {
            if body.get("driver").is_some() {
                Reply(body)
            } else {
                Reply(json!({"data":body}))
            }
        }
    }));
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
        read_only: false,
        tls_fingerprint: None,
        dhw_units: None,
    };
    for (path, body) in [
        ("hvac", json!({"systemID":1,"zoneID":2,"setpoint":20.5})),
        ("hvac", json!({"systemID":1,"qAdapt":2})),
        ("hvac", json!({"systemID":0,"acs_power":1})),
        (
            "iaq",
            json!({"systemID":1,"iaqsensorID":2,"iaq_mode_vent":1}),
        ),
        ("integration", json!({"driver":"moli"})),
    ] {
        let response = exchange(&config, "PUT", path, &body, Duration::from_secs(1))
            .await
            .unwrap();
        acknowledge(&response, &body).unwrap();
        assert_eq!(
            requests.lock().unwrap().pop().unwrap(),
            ("PUT".into(), format!("/api/v1/{path}"), body)
        );
    }
    assert!(requests.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn transport_failures_are_typed_without_disclosing_addresses_or_response_details() {
    use axum::http::StatusCode;
    for (status, body, delay, want) in [
        (StatusCode::FORBIDDEN, "private body", 0, Failure::Http(403)),
        (StatusCode::OK, "{", 0, Failure::InvalidResponse),
        (StatusCode::OK, "[]", 0, Failure::InvalidResponse),
        (
            StatusCode::OK,
            "{\"errors\":[{\"error\":\"private body\"}]}",
            0,
            Failure::Api,
        ),
        (StatusCode::OK, "{}", 100, Failure::Timeout),
    ] {
        let app = Router::new().fallback(any(move || async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            (status, body)
        }));
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
            read_only: true,
            tls_fingerprint: None,
            dhw_units: None,
        };
        let error = read(
            &config,
            &ReadRequest::Version,
            ReadMethod::Post,
            Duration::from_millis(30),
        )
        .await
        .unwrap_err();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&want));
        assert!(!error.to_string().contains("private body"));
        assert!(!error.to_string().contains("127.0.0.1"));
        task.abort();
    }
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = Config {
        host: "127.0.0.1".into(),
        port: tcp.local_addr().unwrap().port(),
        poll_secs: 30,
        mac: None,
        read_only: true,
        tls_fingerprint: None,
        dhw_units: None,
    };
    drop(tcp);
    let error = read(
        &config,
        &ReadRequest::Version,
        ReadMethod::Post,
        Duration::from_secs(1),
    )
    .await
    .unwrap_err();
    assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Transport));
}

#[tokio::test]
async fn thermostat_is_a_bounded_root_get_and_not_a_json_api_request() {
    let requests = Arc::new(AtomicUsize::new(0));
    for (body, success) in [
        ("<html>Thermostat</html>".to_string(), true),
        ("x".repeat(MAX_HTML + 1), false),
    ] {
        let observed = requests.clone();
        let app =
            Router::new().fallback(any(move |method: Method, uri: Uri, request_body: Bytes| {
                observed.fetch_add(1, Ordering::SeqCst);
                assert_eq!(method, Method::GET);
                assert_eq!(uri.path(), "/");
                assert!(request_body.is_empty());
                let body = body.clone();
                async move { ([("content-type", "text/html")], body) }
            }));
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
            read_only: true,
            tls_fingerprint: None,
            dhw_units: None,
        };
        let response = thermostat(&config, Duration::from_secs(1)).await;
        assert_eq!(response.is_ok(), success);
        if success {
            assert_eq!(response.unwrap(), "<html>Thermostat</html>");
        }
        task.abort();
    }
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

async fn tls_server(
    reply: Option<&'static str>,
) -> (Config, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    use std::fmt::Write as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, certificate.cert.der());
    let mut fingerprint = String::new();
    for byte in digest.as_ref() {
        write!(fingerprint, "{byte:02x}").unwrap();
    }
    let private_key =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate.cert.der().clone()], private_key.into())
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let task = tokio::spawn(async move {
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = acceptor.accept(socket).await else {
                continue;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while let Ok(length) = stream.read(&mut buffer).await {
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..length]);
                assert!(request.len() < 16_384);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    if request.len() >= header_end + 4 + content_length {
                        observed.fetch_add(1, Ordering::SeqCst);
                        if let Some(body) = reply {
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        }
                        let _ = stream.shutdown().await;
                        break;
                    }
                }
            }
        }
    });
    (
        Config {
            host: "localhost".into(),
            port,
            poll_secs: 30,
            mac: None,
            read_only: false,
            tls_fingerprint: Some(fingerprint),
            dhw_units: None,
        },
        count,
        task,
    )
}

#[tokio::test]
async fn pinned_https_reads_and_writes_work_but_changed_certificate_sends_no_http() {
    let (mut config, calls, task) =
        tls_server(Some("{\"data\":{\"systemID\":1,\"zoneID\":2,\"on\":1}}")).await;
    read(
        &config,
        &ReadRequest::Version,
        ReadMethod::Post,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    let command = json!({"systemID":1,"zoneID":2,"on":1});
    let response = exchange(&config, "PUT", "hvac", &command, Duration::from_secs(1))
        .await
        .unwrap();
    acknowledge(&response, &command).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    config.tls_fingerprint = Some("00".repeat(32));
    let error = exchange(&config, "PUT", "hvac", &command, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Certificate));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!error.to_string().contains("localhost"));
    task.abort();
}

#[tokio::test]
async fn ambiguous_https_failure_is_never_replayed_and_read_only_never_sends_http() {
    let (mut config, calls, task) = tls_server(None).await;
    let command = json!({"systemID":1,"zoneID":2,"on":1});
    config.read_only = true;
    let error = exchange(&config, "PUT", "hvac", &command, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::ReadOnly));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    config.read_only = false;
    assert!(
        exchange(&config, "PUT", "hvac", &command, Duration::from_secs(1))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn thermostat_html_also_uses_the_explicit_certificate_pin() {
    let (config, calls, task) = tls_server(Some("<html>Thermostat</html>")).await;
    assert_eq!(
        thermostat(&config, Duration::from_secs(1)).await.unwrap(),
        "<html>Thermostat</html>"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

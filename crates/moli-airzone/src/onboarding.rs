//! A bounded read-only identity and inventory check before installation.
use crate::{Airzone, Config, api, model::Snapshot};
use anyhow::{Context as _, ensure};
use std::time::Duration;

const PROBE_LIMIT: Duration = Duration::from_secs(6);

/// Validated controller identity and a small inventory summary. The identity
/// belongs in server-side configuration, not in an untrusted installation request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayProbe {
    pub mac: String,
    pub gateway_type: Option<String>,
    pub api_version: Option<String>,
    pub firmware: Option<String>,
    pub systems: usize,
    pub zones: usize,
    pub incomplete: bool,
}

/// Reads the controller's identity and HVAC inventory without starting a
/// driver or permitting a command. The complete operation takes at most 6 s.
pub async fn probe_read_only(mut config: Config) -> anyhow::Result<GatewayProbe> {
    config.read_only = true;
    let client = Airzone::new(config)?;
    tokio::time::timeout(PROBE_LIMIT, probe(&client.config))
        .await
        .map_err(|_| api::Failure::Timeout)?
}

async fn probe(config: &Config) -> anyhow::Result<GatewayProbe> {
    let identity = api::read(
        config,
        &api::ReadRequest::Webserver,
        api::ReadMethod::Post,
        PROBE_LIMIT,
    )
    .await?;
    let mac = identity["mac"]
        .as_str()
        .and_then(moli_net::lan::mac_hex)
        .context(api::Failure::InvalidResponse)?;
    ensure!(
        config.mac.as_ref().is_none_or(|expected| expected == &mac),
        moli_i18n::tr!("airzone.identity")
    );
    let gateway_type = identity["ws_type"]
        .as_str()
        .filter(|kind| matches!(*kind, "ws_az" | "ws_aidoo"))
        .context(api::Failure::InvalidResponse)?;
    let firmware = identity
        .get("ws_firmware")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .filter(|text| {
                    !text.is_empty() && text.len() <= 64 && !text.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .context(api::Failure::InvalidResponse)
        })
        .transpose()?;
    let hvac = api::read(
        config,
        &api::ReadRequest::Hvac {
            system: 0,
            zone: Some(0),
        },
        api::ReadMethod::Get,
        PROBE_LIMIT,
    )
    .await?;
    let snapshot = Snapshot::parse(&hvac)?;
    let (systems, zones, incomplete) = snapshot.inventory_summary();
    Ok(GatewayProbe {
        mac,
        gateway_type: Some(gateway_type.to_owned()),
        // API version comes from the separate /version operation, not from
        // an arbitrary look-alike field in a webserver response.
        api_version: None,
        firmware,
        systems,
        zones,
        incomplete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{Method, StatusCode, Uri},
        response::IntoResponse as _,
        routing::any,
    };
    use serde_json::{Value as Json, json};
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[derive(Clone)]
    struct Reply {
        status: StatusCode,
        body: String,
        delay: Duration,
    }
    impl From<Json> for Reply {
        fn from(value: Json) -> Self {
            Self {
                status: StatusCode::OK,
                body: value.to_string(),
                delay: Duration::ZERO,
            }
        }
    }
    type Call = (String, String, String);
    struct Server {
        config: Config,
        calls: Arc<Mutex<Vec<Call>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn server(identity: Reply, hvac: Reply) -> Server {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .fallback(any(
                move |State(calls): State<Arc<Mutex<Vec<Call>>>>,
                      method: Method,
                      uri: Uri,
                      body: Bytes| {
                    let reply = if uri.path() == "/api/v1/webserver" {
                        identity.clone()
                    } else {
                        hvac.clone()
                    };
                    async move {
                        calls.lock().unwrap().push((
                            method.to_string(),
                            uri.to_string(),
                            String::from_utf8(body.to_vec()).unwrap(),
                        ));
                        tokio::time::sleep(reply.delay).await;
                        (reply.status, reply.body).into_response()
                    }
                },
            ))
            .with_state(calls.clone());
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            axum::serve(tcp, app).await.unwrap();
        });
        let config =
            serde_json::from_value(json!({"host":"127.0.0.1","port":port,"read_only":false}))
                .unwrap();
        Server {
            config,
            calls,
            task,
        }
    }
    fn identity() -> Json {
        json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"ws_az","ws_firmware":"4.21"})
    }
    fn inventory() -> Json {
        serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn probe_reads_only_identity_and_inventory_and_returns_validated_counts() {
        let server = server(identity().into(), inventory().into()).await;
        let report = probe_read_only(server.config.clone()).await.unwrap();
        assert_eq!(
            report,
            GatewayProbe {
                mac: "aabbccddeeff".into(),
                gateway_type: Some("ws_az".into()),
                api_version: None,
                firmware: Some("4.21".into()),
                systems: 2,
                zones: 3,
                incomplete: false
            }
        );
        assert_eq!(
            *server.calls.lock().unwrap(),
            vec![
                ("POST".into(), "/api/v1/webserver".into(), "{}".into()),
                (
                    "GET".into(),
                    "/api/v1/hvac?systemid=0&zoneid=0".into(),
                    String::new()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn probe_honors_a_canonical_expected_identity_before_reading_zones() {
        let server = server(identity().into(), inventory().into()).await;
        let mut config = server.config.clone();
        config.mac = Some("02:00:00:00:00:01".into());
        let error = probe_read_only(config).await.unwrap_err().to_string();
        assert!(!error.contains("02:00") && !error.contains("aa:bb"));
        assert_eq!(server.calls.lock().unwrap().len(), 1);
        let mut config = server.config.clone();
        config.mac = Some("AA:BB:CC:DD:EE:FF".into());
        assert!(probe_read_only(config).await.is_ok());
    }

    #[tokio::test]
    async fn missing_or_unrecognized_gateway_identity_does_not_pass_as_airzone() {
        for identity in [
            json!({"mac":"aa:bb:cc:dd:ee:ff"}),
            json!({"mac":"invalid","ws_type":"ws_az"}),
            json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"unrelated-device"}),
            json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"ws_az","ws_firmware":"private\ntext"}),
            json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"ws_az","ws_firmware":"x".repeat(65)}),
        ] {
            let server = server(identity.into(), inventory().into()).await;
            assert!(probe_read_only(server.config.clone()).await.is_err());
            assert_eq!(server.calls.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn empty_gateway_and_isolated_invalid_zones_are_distinguished() {
        let empty = server(
            json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"ws_aidoo"}).into(),
            json!({"systems":[]}).into(),
        )
        .await;
        let report = probe_read_only(empty.config.clone()).await.unwrap();
        assert_eq!(
            (report.systems, report.zones, report.incomplete),
            (0, 0, false)
        );
        assert_eq!(report.firmware, None);
        let partial = server(identity().into(),json!({"data":[{"systemID":1,"zoneID":1,"on":1},{"systemID":1,"zoneID":2,"on":"not-a-bit"}]}).into()).await;
        let report = probe_read_only(partial.config.clone()).await.unwrap();
        assert_eq!(
            (report.systems, report.zones, report.incomplete),
            (1, 1, true)
        );
    }

    #[tokio::test]
    async fn malformed_or_ambiguous_inventory_is_not_a_valid_gateway() {
        for hvac in [
            json!({"mac":"aa:bb:cc:dd:ee:ff"}),
            json!({"systems":{}}),
            json!({"data":[{"systemID":1,"zoneID":1},{"systemID":1,"zoneID":1}]}),
            json!({"systems":[{"systemID":2,"data":[{"systemID":1,"zoneID":1}]}]}),
        ] {
            let server = server(identity().into(), hvac.into()).await;
            assert!(probe_read_only(server.config.clone()).await.is_err());
            assert_eq!(server.calls.lock().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn network_errors_do_not_return_private_payloads_and_have_a_body_bound() {
        for reply in [
            Reply {
                status: StatusCode::BAD_GATEWAY,
                body: "private-device-at-address".into(),
                delay: Duration::ZERO,
            },
            Reply {
                status: StatusCode::OK,
                body: "private-invalid-json".into(),
                delay: Duration::ZERO,
            },
            json!({"errors":["private-api-error"]}).into(),
            Reply {
                status: StatusCode::OK,
                body: "x".repeat(moli_net::MAX_BODY + 1),
                delay: Duration::ZERO,
            },
        ] {
            let server = server(identity().into(), reply).await;
            let error = probe_read_only(server.config.clone())
                .await
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private-") && !error.contains("127.0.0.1"));
            assert_eq!(server.calls.lock().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn the_two_reads_share_one_deadline_instead_of_restarting_the_budget() {
        let mut identity: Reply = identity().into();
        identity.delay = Duration::from_millis(3500);
        let mut hvac: Reply = inventory().into();
        hvac.delay = Duration::from_millis(3500);
        let server = server(identity, hvac).await;
        let result = tokio::time::timeout(
            Duration::from_secs(9),
            probe_read_only(server.config.clone()),
        )
        .await
        .expect("probe must finish inside its global deadline");
        assert!(result.is_err());
        assert_eq!(server.calls.lock().unwrap().len(), 2);
    }
}

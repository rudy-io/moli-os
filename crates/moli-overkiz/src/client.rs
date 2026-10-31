//! Local API transport. Credentials and response bodies never appear in errors.
use moli_net::{Method, PinnedHttps};
use serde_json::Value as Json;
use std::time::{Duration, Instant};
const BASE: &str = "/enduser-mobile-web/1/enduserAPI";
#[derive(Debug)]
pub(crate) enum Failure {
    Auth,
    Certificate,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            Self::Auth => moli_i18n::tr!("overkiz.auth"),
            Self::Certificate => moli_i18n::tr!("overkiz.cert"),
        })
    }
}
impl std::error::Error for Failure {}
pub(crate) fn network(error: &anyhow::Error) -> anyhow::Error {
    if format!("{error:#}").contains(moli_net::PIN_MISMATCH) {
        Failure::Certificate.into()
    } else {
        anyhow::anyhow!(moli_i18n::tr!("overkiz.network"))
    }
}
pub(crate) struct Client {
    pub http: PinnedHttps,
    token: String,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}
impl Client {
    pub fn new(http: PinnedHttps, token: String) -> Self {
        Self { http, token }
    }
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
    ) -> anyhow::Result<Json> {
        let read = method == Method::GET;
        let builder = self
            .http
            .request(method, &format!("{BASE}{path}"))
            .header("authorization", format!("Bearer {}", self.token));
        let request = match body {
            Some(body) => moli_net::json_body(builder, body),
            None => moli_net::empty(builder),
        }
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("overkiz.refused")))?;
        let (status, body) = if read {
            self.http.send(request).await
        } else {
            self.http.send_once(request).await
        }
        .map_err(|e| network(&e))?;
        if status == http::StatusCode::UNAUTHORIZED || status == http::StatusCode::FORBIDDEN {
            return Err(Failure::Auth.into());
        }
        if !status.is_success() {
            anyhow::bail!(moli_i18n::tr!("overkiz.response"));
        }
        serde_json::from_slice(&body)
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("overkiz.response")))
    }
    pub async fn apply_before(&self, body: &Json, deadline: Instant) -> anyhow::Result<()> {
        if Instant::now() >= deadline {
            anyhow::bail!(moli_i18n::tr!("overkiz.timeout"));
        }
        let duration = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(4500));
        let result = tokio::time::timeout(
            duration,
            self.request(Method::POST, "/exec/apply", Some(body)),
        )
        .await
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("overkiz.timeout")))??;
        if !result["execId"]
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.len() <= 256)
        {
            anyhow::bail!(moli_i18n::tr!("overkiz.response"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    async fn server(
        response: &'static str,
        delay: Duration,
    ) -> (Client, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert.der().clone()], key.into())
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let task = tokio::spawn(async move {
            let acceptor = TlsAcceptor::from(Arc::new(tls));
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut stream = acceptor.accept(socket).await.unwrap();
                let mut data = vec![0; 8192];
                let n = stream.read(&mut data).await.unwrap();
                if n == 0 {
                    continue;
                }
                seen.fetch_add(1, Ordering::SeqCst);
                let request = String::from_utf8_lossy(&data[..n]);
                assert!(
                    request
                        .starts_with("POST /enduser-mobile-web/1/enduserAPI/exec/apply HTTP/1.1")
                );
                assert!(request.contains("Bearer synthetic-test-token"));
                tokio::time::sleep(delay).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (
            Client::new(
                PinnedHttps::new("localhost", port, None).unwrap(),
                "synthetic-test-token".into(),
            ),
            count,
            task,
        )
    }
    #[tokio::test]
    async fn status_and_response_errors_are_sanitised() {
        for response in [
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 20\r\nConnection: close\r\n\r\nsynthetic-test-token",
            "HTTP/1.1 500 Error\r\nContent-Length: 20\r\nConnection: close\r\n\r\nsynthetic-test-token",
            "HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nsynthetic-test-token",
        ] {
            let (client, count, task) = server(response, Duration::ZERO).await;
            let error = client
                .request(Method::POST, "/exec/apply", Some(&json!({})))
                .await
                .unwrap_err();
            assert!(!format!("{error:#}").contains("synthetic-test-token"));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            task.abort();
        }
    }
    #[tokio::test]
    async fn accepted_exec_is_only_an_ack_and_never_replayed() {
        let (client,count,task)=server("HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"execId\":\"example\"}",Duration::ZERO).await;
        assert_eq!(
            client
                .request(Method::POST, "/exec/apply", Some(&json!({})))
                .await
                .unwrap()["execId"],
            "example"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
        let (client, count, task) = server("", Duration::ZERO).await;
        assert!(
            client
                .request(Method::POST, "/exec/apply", Some(&json!({})))
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn delayed_write_times_out_without_retry() {
        let (client, count, task) = server("", Duration::from_secs(10)).await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(250),
                client.request(Method::POST, "/exec/apply", Some(&json!({})))
            )
            .await
            .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn expired_order_never_opens_a_connection() {
        let (client, count, task) = server("", Duration::ZERO).await;
        assert!(
            client
                .apply_before(
                    &json!({}),
                    Instant::now().checked_sub(Duration::from_secs(1)).unwrap()
                )
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        task.abort();
    }
}

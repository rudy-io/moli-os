use crate::Config;
use http::Method;
use serde_json::Value as Json;
use std::time::{Duration, Instant};

const MAX_RESPONSE: usize = 64 * 1024;
const REQUEST_BUDGET: Duration = Duration::from_millis(3500);

pub(crate) struct Client {
    host: String,
    port: u16,
    https: Option<moli_net::PinnedHttps>,
    authorization: http::HeaderValue,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HomeAssistantClient(<redacted>)")
    }
}
impl Client {
    pub fn new(config: &Config, token: &str) -> anyhow::Result<Self> {
        let mut authorization = http::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("homeassistant.credentials")))?;
        authorization.set_sensitive(true);
        let https = match config.transport {
            crate::Transport::Http => None,
            crate::Transport::PinnedHttps => Some(moli_net::PinnedHttps::new(
                &config.host,
                config.port,
                Some(
                    config
                        .cert_sha256
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing pin"))?,
                ),
            )?),
        };
        Ok(Self {
            host: config.host.clone(),
            port: config.port,
            https,
            authorization,
        })
    }

    // One request only: no redirects, retries or reusable unauthenticated endpoints.
    // moli-net's pinned transport has its own hard 4 MiB allocation bound; parse
    // only 64 KiB here. Plain HTTP enforces 64 KiB while streaming as well.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
        deadline: Instant,
    ) -> anyhow::Result<(u16, Vec<u8>)> {
        let deadline = deadline.min(Instant::now() + REQUEST_BUDGET);
        if deadline <= Instant::now() {
            anyhow::bail!(moli_i18n::tr!("homeassistant.deadline"));
        }
        let exchange = async {
            let builder = http::Request::builder()
                .method(method)
                .uri(path)
                .header(http::header::HOST, format!("{}:{}", self.host, self.port))
                .header(http::header::AUTHORIZATION, self.authorization.clone());
            let request = match body {
                Some(body) => moli_net::json_body(builder, body)?,
                None => moli_net::empty(builder)?,
            };
            let (status, body) = if let Some(https) = &self.https {
                https.send_once(request).await?
            } else {
                moli_net::web(
                    &self.host,
                    self.port,
                    false,
                    request,
                    deadline.saturating_duration_since(Instant::now()),
                    MAX_RESPONSE,
                )
                .await?
            };
            anyhow::ensure!(body.len() <= MAX_RESPONSE, "oversized response");
            anyhow::Ok((status.as_u16(), body.to_vec()))
        };
        // Discard complete underlying errors and HTTP bodies: neither credentials
        // nor hostile server text may enter status, journal or Debug surfaces.
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), exchange)
            .await
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("homeassistant.deadline")))?
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("homeassistant.network")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
    };

    async fn server(reply: &'static str) -> (Config, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 8192];
            let mut used = 0;
            loop {
                let n = socket.read(&mut buf[used..]).await.unwrap();
                used += n;
                let text = String::from_utf8_lossy(&buf[..used]);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|s| s.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
                assert!(n > 0 && used < 8192);
            }
            socket.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8(buf[..used].to_vec()).unwrap()
        });
        let config=serde_json::from_value(json!({"host":"127.0.0.1","port":port,"transport":"http","entities":[{"native_id":"one","entity_id":"switch.example","domain":"switch","room":"Workshop","safety":"ordinary"}]})).unwrap();
        (config, handle)
    }

    #[tokio::test]
    async fn fixed_service_payload_and_token_use_one_request() {
        let (config, handle) =
            server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").await;
        let client = Client::new(&config, "synthetic-private-token").unwrap();
        let (status, body) = client
            .call(
                Method::POST,
                "/api/services/switch/turn_on",
                Some(&json!({"entity_id":"switch.example"})),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"[]");
        let request = handle.await.unwrap();
        assert!(request.starts_with("POST /api/services/switch/turn_on HTTP/1.1\r\n"));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer synthetic-private-token")
        );
        assert_eq!(
            request.split_once("\r\n\r\n").unwrap().1,
            "{\"entity_id\":\"switch.example\"}"
        );
        assert!(!format!("{client:?}").contains("synthetic-private-token"));
    }

    #[tokio::test]
    async fn returns_auth_and_redirect_status_without_following_or_leaking_body() {
        for (reply, want) in [
            (
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                401,
            ),
            (
                "HTTP/1.1 302 Found\r\nLocation: http://untrusted.example.org/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                302,
            ),
        ] {
            let (config, handle) = server(reply).await;
            let client = Client::new(&config, "synthetic-private-token").unwrap();
            assert_eq!(
                client
                    .call(
                        Method::GET,
                        "/api/states/switch.example",
                        None,
                        Instant::now() + Duration::from_secs(1)
                    )
                    .await
                    .unwrap()
                    .0,
                want
            );
            assert!(
                handle
                    .await
                    .unwrap()
                    .starts_with("GET /api/states/switch.example")
            );
        }
    }

    #[tokio::test]
    async fn oversized_response_is_rejected_without_echoing_server_content() {
        let body = "s".repeat(65537);
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .leak();
        let (config, handle) = server(reply).await;
        let client = Client::new(&config, "synthetic-private-token").unwrap();
        let error = client
            .call(
                Method::GET,
                "/api/states/switch.example",
                None,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("synthetic-private-token"));
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn transport_error_is_sanitized_and_total_deadline_is_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config:Config=serde_json::from_value(json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"transport":"http","entities":[]})).unwrap();
        let client = Client::new(&config, "synthetic-private-token").unwrap();
        let start = Instant::now();
        let err = client
            .call(
                Method::POST,
                "/api/services/switch/turn_on",
                Some(&json!({"entity_id":"switch.example"})),
                start + Duration::from_millis(40),
            )
            .await
            .unwrap_err();
        assert!(start.elapsed() < Duration::from_millis(400));
        assert!(!format!("{err:#}").contains("synthetic-private-token"));
        let (_, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        // A failed POST must not be replayed on a second connection.
        assert!(
            tokio::time::timeout(Duration::from_millis(60), listener.accept())
                .await
                .is_err()
        );
        config.port = 0;
        let client = Client::new(&config, "synthetic-private-token").unwrap();
        assert!(
            client
                .call(
                    Method::GET,
                    "/api/states/switch.example",
                    None,
                    Instant::now() + Duration::from_secs(1)
                )
                .await
                .is_err()
        );
    }
}

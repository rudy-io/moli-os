//! OpenAI-compatible endpoints (OpenAI, Mistral, local Ollama…) plus the
//! ChatGPT Codex Responses endpoint. Also handles transcription and speech.

use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::{BodyExt as _, Full};
use moli_net::Body as Bytes;
use serde_json::Value as Json;

const CHAT_LIMIT: Duration = Duration::from_secs(45);
const LISTEN_LIMIT: Duration = Duration::from_secs(30);
const SPEAK_LIMIT: Duration = Duration::from_secs(20);
/// A streamed voice: its first bytes come within this, or the house's voice speaks.
const SPEAK_HEAD: Duration = Duration::from_secs(10);
const CHECK_LIMIT: Duration = Duration::from_secs(10);
const MAX_ANSWER: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    tls: bool,
    host: String,
    port: u16,
    /// Path prefix (`/v1`), without a trailing slash; empty for none.
    base: String,
}

/// Credentials for one provider request. Codex OAuth tokens are only sent to
/// the first-party Codex endpoint, with the account routing header it expects.
pub(crate) enum RequestAuth<'a> {
    ApiKey(Option<&'a str>),
    Codex {
        access_token: &'a str,
        account_id: Option<&'a str>,
    },
}

impl Endpoint {
    pub(crate) fn parse(url: &str) -> anyhow::Result<Self> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            bail!("base_url must start with https:// or http://");
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().context("base_url: invalid port")?),
            None => (authority, if tls { 443 } else { 80 }),
        };
        if host.is_empty() {
            bail!("base_url has no host");
        }
        let path = path.trim_matches('/');
        Ok(Self {
            tls,
            host: host.to_owned(),
            port,
            base: if path.is_empty() {
                String::new()
            } else {
                format!("/{path}")
            },
        })
    }

    /// Plain HTTP only makes sense for a model on the local network: no key
    /// is required there, and none is ever sent (it would travel in clear).
    pub(crate) fn is_local(&self) -> bool {
        !self.tls
    }

    fn request(
        &self,
        key: Option<&str>,
        tail: &str,
        content_type: &str,
        accept: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<Request<Full<Bytes>>> {
        self.request_with_auth(&RequestAuth::ApiKey(key), tail, content_type, accept, body)
    }

    fn request_with_auth(
        &self,
        auth: &RequestAuth<'_>,
        tail: &str,
        content_type: &str,
        accept: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<Request<Full<Bytes>>> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(format!("{}{tail}", self.base))
            .header("content-type", content_type)
            .header("accept", accept);
        // Credentials only travel over TLS. Local HTTP models never receive
        // either an API key or a ChatGPT session token.
        if self.tls {
            match auth {
                RequestAuth::ApiKey(Some(key)) => {
                    builder = builder.header("authorization", format!("Bearer {key}"));
                }
                RequestAuth::ApiKey(None) => {}
                RequestAuth::Codex {
                    access_token,
                    account_id,
                } => {
                    builder = builder
                        .header("authorization", format!("Bearer {access_token}"))
                        .header("originator", "codex_cli_rs");
                    if let Some(account_id) = account_id {
                        builder = builder.header("ChatGPT-Account-ID", *account_id);
                    }
                }
            }
        }
        Ok(builder.body(Full::new(Bytes::from(body)))?)
    }

    /// The provider's own words, never the request (it holds the key).
    fn refused(&self, status: http::StatusCode, answer: &[u8]) -> anyhow::Error {
        let json: Json = serde_json::from_slice(answer).unwrap_or(Json::Null);
        let message = json["error"]["message"].as_str().map_or_else(
            || String::from_utf8_lossy(&answer[..answer.len().min(200)]).into_owned(),
            str::to_owned,
        );
        anyhow::anyhow!("{}: HTTP {} {message}", self.host, status.as_u16())
    }

    /// The raw answer of a POST; the provider's own error words otherwise.
    async fn send(
        &self,
        key: Option<&str>,
        tail: &str,
        content_type: &str,
        accept: &str,
        body: Vec<u8>,
        limit: Duration,
    ) -> anyhow::Result<Bytes> {
        let request = self.request(key, tail, content_type, accept, body)?;
        let (status, answer) =
            moli_net::web(&self.host, self.port, self.tls, request, limit, MAX_ANSWER).await?;
        if !status.is_success() {
            return Err(self.refused(status, &answer));
        }
        Ok(answer)
    }

    async fn send_with_auth(
        &self,
        auth: &RequestAuth<'_>,
        tail: &str,
        content_type: &str,
        accept: &str,
        body: Vec<u8>,
        limit: Duration,
    ) -> anyhow::Result<Bytes> {
        let request = self.request_with_auth(auth, tail, content_type, accept, body)?;
        let (status, answer) =
            moli_net::web(&self.host, self.port, self.tls, request, limit, MAX_ANSWER).await?;
        if !status.is_success() {
            return Err(self.refused(status, &answer));
        }
        Ok(answer)
    }

    async fn post(
        &self,
        key: Option<&str>,
        tail: &str,
        content_type: &str,
        body: Vec<u8>,
        limit: Duration,
    ) -> anyhow::Result<Json> {
        let answer = self
            .send(key, tail, content_type, "application/json", body, limit)
            .await?;
        let json: Json = serde_json::from_slice(&answer).unwrap_or(Json::Null);
        if json.is_null() {
            bail!("{}: the answer is not JSON", self.host);
        }
        Ok(json)
    }

    /// Text to speech (`/audio/speech`): MP3 bytes. `style` steers the
    /// voice (models that take instructions); empty = none. `speed` 1 = the
    /// model's own pace.
    pub(crate) async fn speech(
        &self,
        key: Option<&str>,
        model: &str,
        voice: &str,
        style: &str,
        speed: f64,
        text: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let body = speech_body(model, voice, style, speed, text, "mp3");
        let audio = self
            .send(
                key,
                "/audio/speech",
                "application/json",
                "audio/mpeg",
                body,
                SPEAK_LIMIT,
            )
            .await?;
        anyhow::ensure!(!audio.is_empty(), "{}: empty audio", self.host);
        Ok(audio.to_vec())
    }

    /// The same as it is made, in `format` (`pcm`: raw 16-bit, 24 kHz mono;
    /// `mp3`), handed over chunk by chunk, so the voice starts before the
    /// sentence is whole. Bounded here up to the answer's head; the caller
    /// bounds the rest.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn speech_stream(
        &self,
        key: Option<&str>,
        model: &str,
        voice: &str,
        style: &str,
        speed: f64,
        format: &str,
        text: &str,
    ) -> anyhow::Result<moli_net::Incoming> {
        let body = speech_body(model, voice, style, speed, text, format);
        let accept = if format == "mp3" {
            "audio/mpeg"
        } else {
            "audio/pcm"
        };
        let request = self.request(key, "/audio/speech", "application/json", accept, body)?;
        let (status, audio) =
            moli_net::web_stream(&self.host, self.port, self.tls, request, SPEAK_HEAD).await?;
        if !status.is_success() {
            let answer = tokio::time::timeout(
                SPEAK_HEAD,
                http_body_util::Limited::new(audio, 64 * 1024).collect(),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .map(http_body_util::Collected::to_bytes)
            .unwrap_or_default();
            return Err(self.refused(status, &answer));
        }
        Ok(audio)
    }

    /// Whether the provider accepts `key` (`GET /models`): `false` for a
    /// refused key, an error when the provider cannot be reached.
    pub(crate) async fn check(&self, key: &str) -> anyhow::Result<bool> {
        let request = self.models(key)?;
        let (status, _) = moli_net::web(
            &self.host,
            self.port,
            self.tls,
            request,
            CHECK_LIMIT,
            MAX_ANSWER,
        )
        .await?;
        self.accepts(status)
    }

    /// `GET /models` with `key`: over TLS only, a key never travels in clear.
    fn models(&self, key: &str) -> anyhow::Result<Request<Full<Bytes>>> {
        anyhow::ensure!(self.tls, "{}: a key is only sent over https", self.host);
        Ok(Request::builder()
            .method(Method::GET)
            .uri(format!("{}/models", self.base))
            .header("accept", "application/json")
            .header("authorization", format!("Bearer {key}"))
            .body(Full::new(Bytes::new()))?)
    }

    /// The provider's verdict on a key, by the status of its answer.
    fn accepts(&self, status: http::StatusCode) -> anyhow::Result<bool> {
        match status.as_u16() {
            200..=299 => Ok(true),
            401 | 403 => Ok(false),
            other => bail!("{}: HTTP {other}", self.host),
        }
    }

    pub(crate) async fn chat(&self, key: Option<&str>, body: &Json) -> anyhow::Result<Json> {
        self.post(
            key,
            "/chat/completions",
            "application/json",
            body.to_string().into_bytes(),
            CHAT_LIMIT,
        )
        .await
    }

    /// Responses API request with a provider-specific authentication scheme.
    pub(crate) async fn respond_with_auth(
        &self,
        auth: &RequestAuth<'_>,
        body: &Json,
    ) -> anyhow::Result<Json> {
        let answer = self
            .send_with_auth(
                auth,
                "/responses",
                "application/json",
                "text/event-stream",
                body.to_string().into_bytes(),
                CHAT_LIMIT,
            )
            .await?;
        completed_response(&self.host, &answer)
    }

    /// A small, bounded POST for the fixed OAuth endpoints used by Codex.
    /// The caller supplies only compile-time provider URLs; no request key is
    /// attached here.
    pub(crate) async fn post_raw(
        &self,
        tail: &str,
        content_type: &str,
        body: Vec<u8>,
        limit: Duration,
    ) -> anyhow::Result<(http::StatusCode, Bytes)> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{}{tail}", self.base))
            .header("content-type", content_type)
            .header("accept", "application/json")
            .body(Full::new(Bytes::from(body)))?;
        moli_net::web(&self.host, self.port, self.tls, request, limit, 64 * 1024).await
    }

    /// Speech to text (`/audio/transcriptions`), in the house's language.
    pub(crate) async fn transcribe(
        &self,
        key: Option<&str>,
        model: &str,
        audio: &[u8],
        mime: &str,
    ) -> anyhow::Result<String> {
        let boundary = format!("moli{:x}", moli_core::now_ms());
        let ext = match mime {
            "audio/ogg" => "ogg",
            "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
            "audio/mpeg" => "mp3",
            "audio/wav" | "audio/x-wav" => "wav",
            _ => "webm",
        };
        // The provider wants the two-letter code (`fr`, `en`).
        let house = moli_i18n::language();
        let language = house.split(['-', '_']).next().unwrap_or(&house);
        let mut body = Vec::with_capacity(audio.len() + 512);
        for (name, value) in [
            ("model", model),
            ("language", language),
            ("response_format", "json"),
        ] {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"voix.{ext}\"\r\nContent-Type: {mime}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(audio);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let answer = self
            .post(
                key,
                "/audio/transcriptions",
                &format!("multipart/form-data; boundary={boundary}"),
                body,
                LISTEN_LIMIT,
            )
            .await?;
        Ok(answer["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned())
    }
}

/// Codex requires Responses requests to stream. The body is buffered under
/// `MAX_ANSWER`, then reduced to the completed response for the existing
/// deterministic tool loop.
fn completed_response(host: &str, answer: &[u8]) -> anyhow::Result<Json> {
    if answer
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b'{')
    {
        return serde_json::from_slice(answer)
            .with_context(|| format!("{host}: invalid Responses API JSON"));
    }

    let text = String::from_utf8_lossy(answer);
    let mut data = String::new();
    let mut response = None;

    for line in text.lines().chain(std::iter::once("")) {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        } else if line.is_empty() && !data.is_empty() {
            if data == "[DONE]" {
                data.clear();
                continue;
            }
            let event: Json = serde_json::from_str(&data)
                .with_context(|| format!("{host}: invalid Codex stream event"))?;
            data.clear();

            match event["type"].as_str() {
                Some("response.completed" | "response.incomplete" | "response.done") => {
                    let result = event.get("response").cloned().unwrap_or_else(|| {
                        if event.get("output").is_some() {
                            event.clone()
                        } else {
                            Json::Null
                        }
                    });
                    if !result.is_null() {
                        response = Some(result);
                    }
                }
                Some("response.failed" | "error") => {
                    let message = event
                        .pointer("/response/error/message")
                        .or_else(|| event.pointer("/error/message"))
                        .or_else(|| event.get("message"))
                        .and_then(Json::as_str)
                        .unwrap_or("Codex stream failed");
                    bail!("{host}: {message}");
                }
                _ => {}
            }
        }
    }

    response.ok_or_else(|| anyhow::anyhow!("{host}: Codex stream ended without a response"))
}

/// The body of a speech request; `format` as the provider names it.
fn speech_body(
    model: &str,
    voice: &str,
    style: &str,
    speed: f64,
    text: &str,
    format: &str,
) -> Vec<u8> {
    let mut body = serde_json::json!({
        "model": model,
        "voice": voice,
        "input": text,
        "response_format": format,
    });
    if !style.is_empty() {
        body["instructions"] = serde_json::json!(style);
    }
    if (speed - 1.0).abs() > f64::EPSILON {
        body["speed"] = serde_json::json!(speed);
    }
    body.to_string().into_bytes()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Endpoint, Method};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One HTTP exchange: answers `status` with `body`, gives back the request.
    pub(crate) async fn fake_http(
        status: u16,
        body: &'static [u8],
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            answer(&mut socket, status, body).await;
            request
        });
        (url, task)
    }

    /// A provider for several requests in a row: `answers` in turn (500
    /// once they run out); keeps every request.
    pub(crate) async fn fake_provider(
        answers: Vec<String>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = asked.clone();
        tokio::spawn(async move {
            for i in 0.. {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut socket).await;
                kept.lock().unwrap().push(request);
                match answers.get(i) {
                    Some(body) => answer(&mut socket, 200, body.as_bytes()).await,
                    None => answer(&mut socket, 500, br#"{"error":{"message":"no more"}}"#).await,
                }
            }
        });
        (url, asked)
    }

    /// One request, whole (head and body).
    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw).to_string();
            if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if rest.len() >= len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        String::from_utf8_lossy(&raw).to_string()
    }

    async fn answer(socket: &mut tokio::net::TcpStream, status: u16, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
    }

    #[tokio::test]
    async fn speech_returns_the_audio() {
        let (url, task) = fake_http(200, b"ID3fake-mp3").await;
        let audio = Endpoint::parse(&url)
            .unwrap()
            .speech(
                None,
                "gpt-4o-mini-tts",
                "sage",
                "Voix posée.",
                1.2,
                "Bonsoir",
            )
            .await
            .unwrap();
        assert_eq!(audio, b"ID3fake-mp3");
        let request = task.await.unwrap();
        assert!(request.starts_with("POST /v1/audio/speech "), "{request}");
        assert!(request.contains("\"input\":\"Bonsoir\""));
        assert!(request.contains("\"instructions\":\"Voix posée.\""));
        assert!(request.contains("\"response_format\":\"mp3\""));
        assert!(request.contains("\"speed\":1.2"), "{request}");
        // The model's own pace is not asked for.
        let (url, task) = fake_http(200, b"ID3fake-mp3").await;
        Endpoint::parse(&url)
            .unwrap()
            .speech(None, "m", "v", "", 1.0, "Bonsoir")
            .await
            .unwrap();
        assert!(!task.await.unwrap().contains("speed"));
    }

    #[tokio::test]
    async fn speech_to_text_is_asked_in_the_houses_language() {
        let (url, task) = fake_http(200, br#"{"text":" Bonsoir "}"#).await;
        let text = Endpoint::parse(&url)
            .unwrap()
            .transcribe(None, "m", b"RIFFxxxx", "audio/wav")
            .await
            .unwrap();
        assert_eq!(text, "Bonsoir");
        let request = task.await.unwrap();
        assert!(
            request.contains("name=\"language\"\r\n\r\nfr\r\n"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_key_is_checked_against_the_provider() {
        let openai = Endpoint::parse("https://api.example/v1").unwrap();
        let request = openai.models("sk-good").unwrap();
        assert_eq!(*request.method(), Method::GET);
        assert_eq!(request.uri(), "/v1/models");
        assert_eq!(request.headers()["authorization"], "Bearer sk-good");
        assert!(openai.accepts(http::StatusCode::OK).unwrap());
        assert!(!openai.accepts(http::StatusCode::UNAUTHORIZED).unwrap());
        assert!(!openai.accepts(http::StatusCode::FORBIDDEN).unwrap());
        assert!(
            openai
                .accepts(http::StatusCode::INTERNAL_SERVER_ERROR)
                .is_err()
        );
    }

    /// A key never travels in clear: a plain-http model gets none, and a key
    /// is not even checked there (refused before anything is sent).
    #[tokio::test]
    async fn a_key_never_travels_in_clear() {
        let (url, task) = fake_http(200, br#"{"choices":[]}"#).await;
        let local = Endpoint::parse(&url).unwrap();
        local
            .chat(Some("sk-good"), &serde_json::json!({}))
            .await
            .unwrap();
        let request = task.await.unwrap();
        assert!(!request.contains("sk-good"), "{request}");
        assert!(
            !request.to_ascii_lowercase().contains("authorization"),
            "{request}"
        );
        let (url, task) = fake_http(200, br#"{"data":[]}"#).await;
        assert!(
            Endpoint::parse(&url)
                .unwrap()
                .check("sk-good")
                .await
                .is_err()
        );
        assert!(!task.is_finished(), "nothing was sent");
        task.abort();
        // Over TLS, the key goes with the request.
        let request = Endpoint::parse("https://api.example/v1")
            .unwrap()
            .request(
                Some("sk-good"),
                "/chat/completions",
                "a/b",
                "a/b",
                Vec::new(),
            )
            .unwrap();
        assert_eq!(request.headers()["authorization"], "Bearer sk-good");
    }

    #[tokio::test]
    async fn speech_failure_names_the_provider_error() {
        let (url, _task) = fake_http(500, br#"{"error":{"message":"boom"}}"#).await;
        let err = Endpoint::parse(&url)
            .unwrap()
            .speech(None, "m", "v", "", 1.0, "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 500 boom"), "{err}");
    }

    #[test]
    fn endpoints_parse() {
        let openai = Endpoint::parse("https://api.openai.com/v1/").unwrap();
        assert_eq!(openai.host, "api.openai.com");
        assert_eq!(openai.port, 443);
        assert_eq!(openai.base, "/v1");
        assert!(!openai.is_local());

        let ollama = Endpoint::parse("http://192.168.0.20:11434/v1").unwrap();
        assert_eq!((ollama.host.as_str(), ollama.port), ("192.168.0.20", 11434));
        assert!(ollama.is_local());

        let bare = Endpoint::parse("https://llm.example").unwrap();
        assert_eq!(bare.base, "");

        assert!(Endpoint::parse("ftp://x").is_err());
        assert!(Endpoint::parse("https://:443/v1").is_err());
    }
}

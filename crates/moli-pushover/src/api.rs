//! The Pushover API, as little of it as Moli needs: check the keys, read the
//! month's quota, send a message. Form-encoded requests to `api.pushover.net`,
//! the answers bounded. The application token and the user key go only in
//! request bodies (and, for the quota, in the query Pushover asks for): never
//! in an error, never in a log.

use std::time::Duration;

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderMap};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use moli_runtime::BoxFuture;
use serde_json::Value as Json;

pub(crate) const HOST: &str = "api.pushover.net";
const LIMIT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 64 * 1024;
/// Pushover's own explanations, a line at most.
const MAX_WHY: usize = 200;

/// One HTTPS exchange with Pushover: the network, or a test's fake.
pub(crate) trait Web: Send + Sync {
    fn send(
        &self,
        request: Request<Full<Bytes>>,
    ) -> BoxFuture<'_, anyhow::Result<(StatusCode, HeaderMap, Bytes)>>;
}

/// The real network, checked against the public CAs.
#[derive(Debug)]
pub(crate) struct Internet;

impl Web for Internet {
    fn send(
        &self,
        request: Request<Full<Bytes>>,
    ) -> BoxFuture<'_, anyhow::Result<(StatusCode, HeaderMap, Bytes)>> {
        Box::pin(moli_net::web_with_headers(
            HOST, 443, true, request, LIMIT, MAX_BODY,
        ))
    }
}

/// Why Pushover took nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Pushover cannot be reached (the cause, from the network only).
    Network(String),
    /// Pushover refuses the request (4xx other than 429): a key, a token or a
    /// device it does not know. Insisting would get the address blocked.
    Refused { status: u16, why: String },
    /// The month's messages are spent (429), until `reset` (Unix seconds).
    Quota { reset: Option<u64> },
    /// Pushover's own trouble (5xx), or an answer that is not its JSON.
    Server { status: u16 },
}

impl Failure {
    /// The line a person reads (journal, command reply).
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Network(cause) => moli_i18n::tr!("pilotes.pushover.injoignable", cause = cause),
            Self::Refused { status, why } => {
                moli_i18n::tr!("pilotes.pushover.refuse", status = status, why = why)
            }
            Self::Quota { .. } => moli_i18n::tr!("pilotes.pushover.quota_epuise"),
            Self::Server { status } => {
                moli_i18n::tr!("pilotes.pushover.erreur_serveur", status = status)
            }
        }
    }
}

/// What the month allows, as Pushover counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Quota {
    pub remaining: u64,
    /// When the count starts again (Unix seconds).
    pub reset: Option<u64>,
}

/// One message, already shaped (see `crate::message`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub title: String,
    pub body: String,
}

/// How every message of the instance goes out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub devices: Vec<String>,
    pub priority: i8,
    pub sound: Option<String>,
}

pub(crate) struct Client<'a> {
    web: &'a dyn Web,
    token: String,
    user: String,
}

impl std::fmt::Debug for Client<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl<'a> Client<'a> {
    pub(crate) fn new(web: &'a dyn Web, token: String, user: String) -> Self {
        Self { web, token, user }
    }

    /// The token and the user key are good: the user's (or group's) devices.
    pub(crate) async fn validate(&self) -> Result<Vec<String>, Failure> {
        let body = form(&[("token", &self.token), ("user", &self.user)]);
        let (_, answer) = self.exchange(post("/1/users/validate.json", body)).await?;
        Ok(answer["devices"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|d| d.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The month's quota, before any message.
    pub(crate) async fn limits(&self) -> Result<Quota, Failure> {
        let uri = format!("/1/apps/limits.json?{}", form(&[("token", &self.token)]));
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Full::new(Bytes::new()))
            .map_err(|_| Failure::Server { status: 0 })?;
        let (_, answer) = self.exchange(request).await?;
        let remaining = answer["remaining"]
            .as_u64()
            .ok_or(Failure::Server { status: 200 })?;
        Ok(Quota {
            remaining,
            reset: answer["reset"].as_u64(),
        })
    }

    /// Sends one message; the quota Pushover reports along, when it does.
    pub(crate) async fn send(
        &self,
        message: &Message,
        options: &Options,
    ) -> Result<Option<Quota>, Failure> {
        let devices = options.devices.join(",");
        let priority = options.priority.to_string();
        let mut fields: Vec<(&str, &str)> = vec![
            ("token", &self.token),
            ("user", &self.user),
            ("title", &message.title),
            ("message", &message.body),
            // The text stays text: no HTML, no link, no attachment.
            ("html", "0"),
            ("priority", &priority),
        ];
        if !devices.is_empty() {
            fields.push(("device", &devices));
        }
        if let Some(sound) = &options.sound {
            fields.push(("sound", sound));
        }
        let (headers, _) = self
            .exchange(post("/1/messages.json", form(&fields)))
            .await?;
        Ok(quota(&headers))
    }

    async fn exchange(&self, request: Request<Full<Bytes>>) -> Result<(HeaderMap, Json), Failure> {
        let (status, headers, bytes) = self
            .web
            .send(request)
            .await
            .map_err(|e| Failure::Network(e.root_cause().to_string()))?;
        let answer: Option<Json> = serde_json::from_slice(&bytes).ok();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Failure::Quota {
                reset: quota(&headers).and_then(|q| q.reset),
            });
        }
        if status.is_client_error() {
            return Err(Failure::Refused {
                status: status.as_u16(),
                why: answer.as_ref().map(why).unwrap_or_default(),
            });
        }
        match answer {
            Some(answer) if status.is_success() && answer["status"].as_i64() == Some(1) => {
                Ok((headers, answer))
            }
            // `status: 0` with a 200 is a refusal too, by the documentation.
            Some(answer) if status.is_success() => Err(Failure::Refused {
                status: status.as_u16(),
                why: why(&answer),
            }),
            _ => Err(Failure::Server {
                status: status.as_u16(),
            }),
        }
    }
}

fn post(path: &str, body: String) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default()
}

/// Pushover's explanation (`errors`), one short line, never the keys.
fn why(answer: &Json) -> String {
    let text = answer["errors"]
        .as_array()
        .map_or_else(String::new, |list| {
            list.iter()
                .filter_map(Json::as_str)
                .collect::<Vec<_>>()
                .join(" ; ")
        });
    text.chars().take(MAX_WHY).collect()
}

/// `X-Limit-App-Remaining` and `X-Limit-App-Reset`, on every message's answer.
fn quota(headers: &HeaderMap) -> Option<Quota> {
    let number =
        |name: &str| -> Option<u64> { headers.get(name)?.to_str().ok()?.trim().parse().ok() };
    Some(Quota {
        remaining: number("x-limit-app-remaining")?,
        reset: number("x-limit-app-reset"),
    })
}

/// `application/x-www-form-urlencoded`, as browsers write it.
pub(crate) fn form(fields: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (name, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        encode(name, &mut out);
        out.push('=');
        encode(value, &mut out);
    }
    out
}

fn encode(text: &str, out: &mut String) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// What a test saw leave.
    #[derive(Clone, Debug)]
    pub struct Asked {
        pub method: String,
        pub path: String,
        pub body: String,
    }

    /// One scripted answer.
    pub enum Reply {
        Json(u16, &'static str, Vec<(&'static str, &'static str)>),
        Down,
    }

    /// A scripted Pushover: answers in order, whatever was asked.
    #[derive(Default)]
    pub struct Fake {
        script: Mutex<VecDeque<Reply>>,
        asked: Mutex<Vec<Asked>>,
    }

    impl Fake {
        pub fn then(self, reply: Reply) -> Self {
            self.script.lock().unwrap().push_back(reply);
            self
        }

        pub fn ok() -> Reply {
            Reply::Json(200, r#"{"status":1,"request":"r"}"#, Vec::new())
        }

        pub fn asked(&self) -> Vec<Asked> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Web for Fake {
        fn send(
            &self,
            request: Request<Full<Bytes>>,
        ) -> BoxFuture<'_, anyhow::Result<(StatusCode, HeaderMap, Bytes)>> {
            Box::pin(async move {
                use http_body_util::BodyExt as _;
                let (head, body) = request.into_parts();
                let body = body.collect().await?.to_bytes();
                self.asked.lock().unwrap().push(Asked {
                    method: head.method.to_string(),
                    path: head.uri.to_string(),
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let reply = self.script.lock().unwrap().pop_front();
                match reply {
                    Some(Reply::Json(status, body, headers)) => {
                        let mut map = HeaderMap::new();
                        for (name, value) in headers {
                            map.insert(name, value.parse()?);
                        }
                        Ok((StatusCode::from_u16(status)?, map, Bytes::from(body)))
                    }
                    Some(Reply::Down) | None => anyhow::bail!("connection refused"),
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, Reply};
    use super::*;

    const TOKEN: &str = "faketokenfaketokenfaketoken000";
    const USER: &str = "fakeuserfakeuserfakeuser000000";

    fn message() -> Message {
        Message {
            title: "Lave-linge".into(),
            body: "Le linge est prêt & sec".into(),
        }
    }

    #[test]
    fn forms_are_encoded() {
        assert_eq!(
            form(&[("a", "b c"), ("é", "x&y=z\n")]),
            "a=b+c&%C3%A9=x%26y%3Dz%0A"
        );
    }

    #[tokio::test]
    async fn a_message_leaves_as_text_with_its_options() {
        let web = Fake::default().then(Reply::Json(
            200,
            r#"{"status":1,"request":"r"}"#,
            vec![
                ("X-Limit-App-Remaining", "7496"),
                ("X-Limit-App-Reset", "1393653600"),
            ],
        ));
        let client = Client::new(&web, TOKEN.into(), USER.into());
        let options = Options {
            devices: vec!["iphone".into(), "ipad".into()],
            priority: 1,
            sound: Some("magic".into()),
        };
        let quota = client.send(&message(), &options).await.unwrap();
        assert_eq!(
            quota,
            Some(Quota {
                remaining: 7496,
                reset: Some(1_393_653_600)
            })
        );
        let asked = web.asked();
        assert_eq!(asked[0].method, "POST");
        assert_eq!(asked[0].path, "/1/messages.json");
        let body = &asked[0].body;
        assert!(body.contains(&format!("token={TOKEN}")));
        assert!(body.contains(&format!("user={USER}")));
        assert!(body.contains("title=Lave-linge"));
        assert!(body.contains("message=Le+linge+est+pr%C3%AAt+%26+sec"));
        assert!(body.contains("html=0"));
        assert!(body.contains("priority=1"));
        assert!(body.contains("device=iphone%2Cipad"));
        assert!(body.contains("sound=magic"));
        assert!(!body.contains("url=") && !body.contains("attachment"));
    }

    #[tokio::test]
    async fn without_devices_or_sound_the_account_decides() {
        let web = Fake::default().then(Fake::ok());
        let client = Client::new(&web, TOKEN.into(), USER.into());
        assert_eq!(client.send(&message(), &Options::default()).await, Ok(None));
        let body = &web.asked()[0].body;
        assert!(!body.contains("device=") && !body.contains("sound="));
        assert!(body.contains("priority=0"));
    }

    #[tokio::test]
    async fn refusals_say_why_without_the_keys() {
        let web = Fake::default().then(Reply::Json(
            400,
            r#"{"user":"invalid","errors":["user identifier is not a valid user, group, or subscribed user key"],"status":0,"request":"r"}"#,
            Vec::new(),
        ));
        let client = Client::new(&web, TOKEN.into(), USER.into());
        let failure = client
            .send(&message(), &Options::default())
            .await
            .unwrap_err();
        assert!(matches!(failure, Failure::Refused { status: 400, .. }));
        let text = failure.text();
        assert!(text.contains("not a valid user"));
        assert!(!text.contains(TOKEN) && !text.contains(USER));
    }

    #[tokio::test]
    async fn a_spent_quota_tells_when_it_comes_back() {
        let web = Fake::default().then(Reply::Json(
            429,
            r#"{"status":0,"errors":["message limit reached"]}"#,
            vec![
                ("X-Limit-App-Remaining", "0"),
                ("X-Limit-App-Reset", "1800000000"),
            ],
        ));
        let client = Client::new(&web, TOKEN.into(), USER.into());
        assert_eq!(
            client.send(&message(), &Options::default()).await,
            Err(Failure::Quota {
                reset: Some(1_800_000_000)
            })
        );
    }

    #[tokio::test]
    async fn server_trouble_and_silence_are_not_refusals() {
        let web = Fake::default()
            .then(Reply::Json(503, "<html>busy</html>", Vec::new()))
            .then(Reply::Down)
            .then(Reply::Json(200, "not json", Vec::new()))
            .then(Reply::Json(
                200,
                r#"{"status":0,"errors":["bad"]}"#,
                Vec::new(),
            ));
        let client = Client::new(&web, TOKEN.into(), USER.into());
        let o = Options::default();
        assert_eq!(
            client.send(&message(), &o).await,
            Err(Failure::Server { status: 503 })
        );
        assert!(matches!(
            client.send(&message(), &o).await,
            Err(Failure::Network(_))
        ));
        assert_eq!(
            client.send(&message(), &o).await,
            Err(Failure::Server { status: 200 })
        );
        assert!(matches!(
            client.send(&message(), &o).await,
            Err(Failure::Refused { status: 200, .. })
        ));
    }

    #[tokio::test]
    async fn validation_lists_the_devices_and_limits_read_the_quota() {
        let web = Fake::default()
            .then(Reply::Json(
                200,
                r#"{"status":1,"group":0,"devices":["iphone","desktop"],"licenses":["iOS"],"request":"r"}"#,
                Vec::new(),
            ))
            .then(Reply::Json(
                200,
                r#"{"limit":10000,"remaining":9876,"reset":1393653600,"status":1,"request":"r"}"#,
                Vec::new(),
            ));
        let client = Client::new(&web, TOKEN.into(), USER.into());
        assert_eq!(client.validate().await.unwrap(), ["iphone", "desktop"]);
        assert_eq!(
            client.limits().await.unwrap(),
            Quota {
                remaining: 9876,
                reset: Some(1_393_653_600)
            }
        );
        let asked = web.asked();
        assert_eq!(asked[0].path, "/1/users/validate.json");
        assert_eq!(asked[1].method, "GET");
        assert_eq!(asked[1].path, format!("/1/apps/limits.json?token={TOKEN}"));
    }

    #[test]
    fn the_client_never_prints_its_keys() {
        let web = Fake::default();
        let client = Client::new(&web, TOKEN.into(), USER.into());
        let shown = format!("{client:?}");
        assert!(!shown.contains(TOKEN) && !shown.contains(USER));
    }
}

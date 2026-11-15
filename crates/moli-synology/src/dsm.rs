//! The DSM web API: one pinned HTTPS connection, one session.
//!
//! Every call is a form POST (the password and the session id never travel
//! in an address). A session the NAS dropped (its answer proves the call
//! did not run) is opened again, and the call sent once more: that holds for
//! an order too. An order (shutdown, reboot) is never repeated after a
//! failure that leaves its fate unknown: only a read is re-sent on a dropped
//! connection.

use anyhow::{Context as _, ensure};
use moli_net::{Method, PinnedHttps};
use serde_json::Value as Json;
use std::fmt::{self, Write as _};

/// The NAS refused the login (wrong or disabled account, blocked address,
/// 2-step verification…). Asking again would only get the hub blocked.
#[derive(Debug)]
pub(crate) struct Refused(pub u32);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DSM refused the login (code {})", self.0)
    }
}
impl std::error::Error for Refused {}

/// An API of the NAS answered `success: false` (no right, unknown API…).
#[derive(Debug)]
pub(crate) struct Api(pub u32);

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DSM refused the request (code {})", self.0)
    }
}
impl std::error::Error for Api {}

/// Session timed out, interrupted by another login, or unknown.
const SESSION_GONE: [u32; 3] = [106, 107, 119];

pub(crate) struct Client {
    http: PinnedHttps,
    username: String,
    password: String,
    sid: Option<String>,
}

impl Client {
    pub(crate) fn new(http: PinnedHttps, username: String, password: String) -> Self {
        Self {
            http,
            username,
            password,
            sid: None,
        }
    }

    /// A read, safe to repeat: after a dropped session, logs in again once.
    pub(crate) async fn read(
        &mut self,
        api: &str,
        version: &str,
        method: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<Json> {
        self.call(api, version, method, params, true).await
    }

    /// An order: executed at most once (see the module).
    pub(crate) async fn order(
        &mut self,
        api: &str,
        version: &str,
        method: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<Json> {
        self.call(api, version, method, params, false).await
    }

    async fn call(
        &mut self,
        api: &str,
        version: &str,
        method: &str,
        params: &[(&str, &str)],
        repeatable: bool,
    ) -> anyhow::Result<Json> {
        let mut again = true;
        loop {
            if self.sid.is_none() {
                self.login().await?;
            }
            let sid = self.sid.clone().unwrap_or_default();
            let mut pairs = vec![("api", api), ("version", version), ("method", method)];
            pairs.extend_from_slice(params);
            pairs.push(("_sid", &sid));
            let value = self
                .post("/webapi/entry.cgi", &form(&pairs), repeatable)
                .await?;
            match answer(&value) {
                Ok(data) => return Ok(data),
                Err(code) if SESSION_GONE.contains(&code) => {
                    self.sid = None;
                    if !std::mem::take(&mut again) {
                        return Err(Api(code).into());
                    }
                }
                Err(code) => return Err(Api(code).into()),
            }
        }
    }

    async fn login(&mut self) -> anyhow::Result<()> {
        let body = form(&[
            ("api", "SYNO.API.Auth"),
            ("version", "6"),
            ("method", "login"),
            ("account", &self.username),
            ("passwd", &self.password),
            ("session", "Moli"),
            ("format", "sid"),
        ]);
        let value = self.post("/webapi/entry.cgi", &body, false).await?;
        match answer(&value) {
            Ok(data) => {
                let sid = data["sid"].as_str().filter(|s| !s.is_empty());
                self.sid = Some(sid.context("DSM login without a session")?.to_owned());
                Ok(())
            }
            // 400 wrong account or password, 401 disabled, 402 no right,
            // 403 and 404 one-time code, 406/407/408… blocked, expired.
            Err(code) if (400..500).contains(&code) => Err(Refused(code).into()),
            Err(code) => Err(Api(code).into()),
        }
    }

    async fn post(&self, path: &str, body: &str, repeatable: bool) -> anyhow::Result<Json> {
        let request = moli_net::text_body(
            self.http.request(Method::POST, path),
            "application/x-www-form-urlencoded",
            body.to_owned(),
        )?;
        let (status, bytes) = if repeatable {
            self.http.send(request).await
        } else {
            self.http.send_once(request).await
        }?;
        ensure!(status.is_success(), "DSM answered {status}");
        serde_json::from_slice(&bytes).context("unreadable DSM answer")
    }
}

/// `data` of a successful answer, or the error code of a failed one.
fn answer(value: &Json) -> Result<Json, u32> {
    if value["success"].as_bool() == Some(true) {
        Ok(value["data"].clone())
    } else {
        Err(value["error"]["code"]
            .as_u64()
            .and_then(|c| u32::try_from(c).ok())
            .unwrap_or(0))
    }
}

/// `application/x-www-form-urlencoded`.
fn form(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        escape(&mut out, key);
        out.push('=');
        escape(&mut out, value);
    }
    out
}

fn escape(out: &mut String, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_form_escapes_everything_but_unreserved_characters() {
        assert_eq!(
            form(&[
                ("account", "a b&c=d"),
                ("additional", "[\"x\"]"),
                ("é", "~-._")
            ]),
            "account=a%20b%26c%3Dd&additional=%5B%22x%22%5D&%C3%A9=~-._"
        );
    }

    #[test]
    fn an_answer_is_its_data_or_its_error_code() {
        assert_eq!(
            answer(&json!({"success":true,"data":{"sid":"s"}})).unwrap()["sid"],
            "s"
        );
        assert_eq!(answer(&json!({"success":true})), Ok(Json::Null));
        assert_eq!(
            answer(&json!({"success":false,"error":{"code":400}})),
            Err(400)
        );
        assert_eq!(answer(&json!({"success":false})), Err(0));
        assert_eq!(answer(&json!({})), Err(0));
    }
}

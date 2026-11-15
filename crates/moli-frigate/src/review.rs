//! A camera's past from Frigate's authenticated API (port 8971, HTTPS with
//! Frigate's own certificate, pinned on first contact): what it saw (review
//! items), what it recorded, its zones, and their pictures and clips,
//! streamed (a clip is never held in memory). Read only: a « viewer »
//! account is enough, and nothing here writes.
//!
//! Session: `POST /api/login {user, password}` sets a cookie (the JWT); it is
//! sent with every request and renewed once on a 401.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use futures::StreamExt as _;
use http::Method;
use http_body_util::BodyExt as _;
use moli_net::PinnedHttps;
use moli_runtime::BoxFuture;
use moli_runtime::media::{MediaReply, Recorded, ReviewItem, ReviewMedia, ReviewSource, Zone};
use serde_json::Value as Json;
use tokio::sync::Mutex;

/// Frigate's API answers (JSON, pictures) and the head of a clip.
const WAIT: Duration = Duration::from_secs(15);
/// Most review items asked at once.
const LIMIT: u32 = 500;
/// Two recorded segments closer than this are one stretch (seconds).
const GAP: f64 = 2.0;

/// Frigate's API, shared by the cameras of one Frigate.
#[derive(Debug)]
pub struct Api {
    https: PinnedHttps,
    user: String,
    password: String,
    cookie: Mutex<Option<String>>,
}

impl Api {
    #[must_use]
    pub fn new(https: PinnedHttps, user: String, password: String) -> Self {
        Self {
            https,
            user,
            password,
            cookie: Mutex::new(None),
        }
    }

    async fn login(&self) -> anyhow::Result<String> {
        let body = serde_json::json!({ "user": self.user, "password": self.password }).to_string();
        let request = self.https.request(Method::POST, "/api/login");
        let request = moli_net::text_body(request, "application/json", body)?;
        let (status, headers, _) = self.https.exchange_full(request, WAIT).await?;
        if !status.is_success() {
            bail!("Frigate refused the login (HTTP {})", status.as_u16());
        }
        let cookie = headers
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(session_cookie)
            .context("Frigate set no session cookie")?;
        *self.cookie.lock().await = Some(cookie.clone());
        Ok(cookie)
    }

    async fn cookie(&self) -> anyhow::Result<String> {
        if let Some(c) = self.cookie.lock().await.clone() {
            return Ok(c);
        }
        self.login().await
    }

    /// A JSON answer, logging in again once when the session ended.
    async fn json(&self, path: &str) -> anyhow::Result<Json> {
        for attempt in 0..2 {
            let cookie = self.cookie().await?;
            let request = self
                .https
                .request(Method::GET, path)
                .header(http::header::COOKIE, cookie);
            let (status, _, body) = self
                .https
                .exchange_full(moli_net::empty(request)?, WAIT)
                .await?;
            if status == http::StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.cookie.lock().await = None;
                continue;
            }
            if !status.is_success() {
                bail!("Frigate: HTTP {} for {path}", status.as_u16());
            }
            return serde_json::from_slice(&body).context("Frigate: not JSON");
        }
        bail!("Frigate: session refused")
    }

    /// A picture or a clip, streamed, `Range` passed on.
    async fn media(&self, path: &str, range: Option<&str>) -> anyhow::Result<MediaReply> {
        for attempt in 0..2 {
            let cookie = self.cookie().await?;
            let mut request = self
                .https
                .request(Method::GET, path)
                .header(http::header::COOKIE, cookie);
            if let Some(range) = range {
                request = request.header(http::header::RANGE, range);
            }
            let response = self
                .https
                .stream_response(moli_net::empty(request)?, WAIT)
                .await?;
            let status = response.status();
            if status == http::StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.cookie.lock().await = None;
                continue;
            }
            if !(status.is_success() || status == http::StatusCode::RANGE_NOT_SATISFIABLE) {
                bail!("Frigate: HTTP {} for {path}", status.as_u16());
            }
            let headers = [
                http::header::CONTENT_TYPE,
                http::header::CONTENT_LENGTH,
                http::header::CONTENT_RANGE,
                http::header::ACCEPT_RANGES,
            ]
            .iter()
            .filter_map(|name| {
                let v = response.headers().get(name)?.to_str().ok()?;
                Some((name.as_str().to_owned(), v.to_owned()))
            })
            .collect();
            let body = response
                .into_body()
                .into_data_stream()
                .map(|chunk| chunk.map_err(std::io::Error::other))
                .boxed();
            return Ok(MediaReply {
                status: status.as_u16(),
                headers,
                body,
            });
        }
        bail!("Frigate: session refused")
    }
}

/// `frigate_token=…; Path=/; HttpOnly…` → `frigate_token=…`.
#[must_use]
pub fn session_cookie(set_cookie: &str) -> Option<String> {
    let pair = set_cookie.split(';').next()?.trim();
    let (name, value) = pair.split_once('=')?;
    (!name.is_empty() && !value.is_empty()).then(|| pair.to_owned())
}

/// One camera of that Frigate.
#[derive(Debug)]
pub struct Camera {
    pub api: Arc<Api>,
    pub name: String,
}

impl ReviewSource for Camera {
    fn review(&self, after: f64, before: f64) -> BoxFuture<'_, anyhow::Result<Vec<ReviewItem>>> {
        Box::pin(async move {
            let path = format!(
                "/api/review?cameras={}&after={after:.0}&before={before:.0}&limit={LIMIT}",
                self.name
            );
            Ok(review_items(&self.api.json(&path).await?))
        })
    }

    fn recorded(&self, after: f64, before: f64) -> BoxFuture<'_, anyhow::Result<Vec<Recorded>>> {
        Box::pin(async move {
            let path = format!(
                "/api/{}/recordings?after={after:.0}&before={before:.0}",
                self.name
            );
            Ok(stretches(&self.api.json(&path).await?))
        })
    }

    fn zones(&self) -> BoxFuture<'_, anyhow::Result<Vec<Zone>>> {
        Box::pin(async move { Ok(zones_of(&self.api.json("/api/config").await?, &self.name)) })
    }

    fn media(
        &self,
        what: ReviewMedia,
        range: Option<String>,
    ) -> BoxFuture<'_, anyhow::Result<MediaReply>> {
        Box::pin(async move {
            let path = match &what {
                ReviewMedia::EventImage(id) => {
                    format!("/api/events/{}/snapshot.jpg?bbox=1", safe(id)?)
                }
                ReviewMedia::EventThumbnail(id) => {
                    format!("/api/events/{}/thumbnail.jpg", safe(id)?)
                }
                ReviewMedia::EventClip(id) => format!("/api/events/{}/clip.mp4", safe(id)?),
                ReviewMedia::Recording { start, end } => {
                    if !(start.is_finite()
                        && end.is_finite()
                        && end > start
                        && end - start <= 3600.0)
                    {
                        bail!("a recording is asked for at most one hour");
                    }
                    format!("/api/{}/start/{start:.0}/end/{end:.0}/clip.mp4", self.name)
                }
            };
            self.api.media(&path, range.as_deref()).await
        })
    }
}

/// An id as Frigate writes them (`1731670000.123456-abcdef`): nothing else
/// reaches its paths.
fn safe(id: &str) -> anyhow::Result<&str> {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Ok(id)
    } else {
        bail!("not a Frigate event id")
    }
}

fn strings(v: &Json) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `/api/review` → items, newest first.
#[must_use]
pub fn review_items(answer: &Json) -> Vec<ReviewItem> {
    let mut items: Vec<ReviewItem> = answer
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            Some(ReviewItem {
                id: r["id"].as_str()?.to_owned(),
                start: r["start_time"].as_f64()?,
                end: r["end_time"].as_f64(),
                severity: r["severity"].as_str().unwrap_or("detection").to_owned(),
                labels: strings(&r["data"]["objects"]),
                zones: strings(&r["data"]["zones"]),
                events: strings(&r["data"]["detections"]),
            })
        })
        .collect();
    items.sort_by(|a, b| b.start.total_cmp(&a.start));
    items
}

/// `/api/<camera>/recordings` (10 s segments) → the stretches recorded.
#[must_use]
pub fn stretches(answer: &Json) -> Vec<Recorded> {
    let mut segments: Vec<(f64, f64)> = answer
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|s| Some((s["start_time"].as_f64()?, s["end_time"].as_f64()?)))
        .filter(|(a, b)| b > a)
        .collect();
    segments.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<Recorded> = Vec::new();
    for (start, end) in segments {
        match out.last_mut() {
            Some(last) if start <= last.end + GAP => last.end = last.end.max(end),
            _ => out.push(Recorded { start, end }),
        }
    }
    out
}

/// `/api/config` → this camera's zones (relative coordinates, `"x,y,x,y…"`
/// or a list of numbers).
#[must_use]
pub fn zones_of(config: &Json, camera: &str) -> Vec<Zone> {
    let Some(zones) = config["cameras"][camera]["zones"].as_object() else {
        return Vec::new();
    };
    let mut out: Vec<Zone> = zones
        .iter()
        .filter_map(|(name, z)| {
            let numbers: Vec<f64> = match &z["coordinates"] {
                Json::String(s) => s.split(',').filter_map(|n| n.trim().parse().ok()).collect(),
                Json::Array(a) => a
                    .iter()
                    .flat_map(|v| match v {
                        Json::String(s) => s
                            .split(',')
                            .filter_map(|n| n.trim().parse().ok())
                            .collect::<Vec<f64>>(),
                        other => other.as_f64().into_iter().collect(),
                    })
                    .collect(),
                _ => return None,
            };
            let points: Vec<(f64, f64)> = numbers
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[x, y]| (x, y))
                .filter(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
                .collect();
            let name = z["friendly_name"].as_str().unwrap_or(name).to_owned();
            (points.len() >= 3).then_some(Zone { name, points })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_session_cookie_is_its_first_pair() {
        assert_eq!(
            session_cookie("frigate_token=abc.def; Path=/; HttpOnly; SameSite=Lax"),
            Some("frigate_token=abc.def".to_owned())
        );
        assert_eq!(session_cookie("; Path=/"), None);
    }

    #[test]
    fn review_items_newest_first() {
        let answer = json!([
            {"id": "1", "start_time": 100.0, "end_time": 120.0, "severity": "detection",
             "data": {"objects": ["car"], "zones": [], "detections": ["e1"]}},
            {"id": "2", "start_time": 200.0, "end_time": null, "severity": "alert",
             "data": {"objects": ["person"], "zones": ["allee"], "detections": ["e2", "e3"]}},
            {"start_time": 5.0}
        ]);
        let items = review_items(&answer);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "2");
        assert_eq!(items[0].end, None);
        assert_eq!(items[0].zones, vec!["allee".to_owned()]);
        assert_eq!(items[0].events, vec!["e2".to_owned(), "e3".to_owned()]);
        assert_eq!(items[1].labels, vec!["car".to_owned()]);
    }

    #[test]
    fn segments_merge_into_stretches() {
        let answer = json!([
            {"start_time": 20.0, "end_time": 30.0},
            {"start_time": 0.0, "end_time": 10.0},
            {"start_time": 10.5, "end_time": 20.0},
            {"start_time": 100.0, "end_time": 110.0}
        ]);
        assert_eq!(
            stretches(&answer),
            vec![
                Recorded {
                    start: 0.0,
                    end: 30.0
                },
                Recorded {
                    start: 100.0,
                    end: 110.0
                }
            ]
        );
    }

    #[test]
    fn zones_from_the_config() {
        let config = json!({"cameras": {"entree": {"zones": {
            "allee": {"coordinates": "0.1,0.2,0.5,0.2,0.5,0.9", "friendly_name": "Allée"},
            "rue": {"coordinates": ["0,0", "1,0", "1,1"]},
            "abimee": {"coordinates": "0.1,0.2"}
        }}}});
        let zones = zones_of(&config, "entree");
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].name, "Allée");
        assert_eq!(zones[0].points[2], (0.5, 0.9));
        assert_eq!(zones[1].points.len(), 3);
        assert!(zones_of(&config, "jardin").is_empty());
    }

    #[test]
    fn only_frigate_ids_reach_its_paths() {
        assert!(safe("1731670000.123456-abc123").is_ok());
        assert!(safe("../config").is_err());
        assert!(safe("a?b").is_err());
    }
}

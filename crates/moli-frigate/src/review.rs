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
use moli_runtime::media::{
    Image, LiveSource, MediaReply, Recorded, ReviewItem, ReviewMedia, ReviewSource, SnapshotSource,
    Zone,
};
use serde_json::Value as Json;
use tokio::sync::Mutex;

/// Frigate's API answers (JSON, pictures) and the head of a clip.
const WAIT: Duration = Duration::from_secs(15);
/// Most review items asked at once.
const LIMIT: u32 = 500;
/// Two recorded segments closer than this are one stretch (seconds).
const GAP: f64 = 2.0;
/// Recordings are asked two hours at a time: Frigate lists every segment of
/// a few seconds, and a whole day went past what Moli reads of one answer
/// (4 MiB) on a camera recording all day.
const WINDOW: f64 = 2.0 * 3600.0;

/// Frigate's API, shared by the cameras of one Frigate.
pub struct Api {
    https: PinnedHttps,
    user: String,
    password: String,
    cookie: Mutex<Option<String>>,
    /// Set once Frigate shows another certificate than the pinned one: the
    /// driver stops on it.
    changed: tokio::sync::watch::Sender<Option<Changed>>,
    /// Each camera's live streams on Frigate's own go2rtc, read once from
    /// its configuration.
    live: Mutex<Option<std::collections::BTreeMap<String, String>>>,
}

/// The password and the session never show in a log.
impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("https", &self.https)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// Frigate showed another certificate than the pinned one.
#[derive(Clone, Debug)]
pub struct Changed {
    /// Its fingerprint, when the handshake got that far.
    pub seen: Option<String>,
}

impl Api {
    #[must_use]
    pub fn new(https: PinnedHttps, user: String, password: String) -> Self {
        Self {
            https,
            user,
            password,
            cookie: Mutex::new(None),
            changed: tokio::sync::watch::Sender::new(None),
            live: Mutex::new(None),
        }
    }

    /// Signs in and reads the cameras' live streams ahead of the first live
    /// view: the hub gives an answer 10 s in all, go2rtc alone may take 9.
    pub async fn warm(&self) {
        if let Err(e) = self.stream_of("").await {
            tracing::debug!(error = %e, "frigate review: not ready yet");
        }
    }

    /// Every camera's zones, from Frigate's configuration.
    pub async fn zones(&self) -> anyhow::Result<std::collections::BTreeMap<String, ZoneOf>> {
        Ok(zones_by_name(&self.json("/api/config").await?))
    }

    /// The stream Frigate plays for a camera: the first of its
    /// `live.streams`, else the camera's own name (what Frigate accepts on
    /// `/api/go2rtc/webrtc`). From Frigate's configuration, never a request.
    async fn stream_of(&self, camera: &str) -> anyhow::Result<String> {
        let mut live = self.live.lock().await;
        if live.is_none() {
            *live = Some(live_streams(&self.json("/api/config").await?));
        }
        Ok(live
            .as_ref()
            .and_then(|l| l.get(camera).cloned())
            .unwrap_or_else(|| camera.to_owned()))
    }

    /// Told when Frigate shows another certificate.
    pub fn certificate_changed(&self) -> tokio::sync::watch::Receiver<Option<Changed>> {
        self.changed.subscribe()
    }

    /// A refused handshake: the certificate changed. Said once to the
    /// driver, which stops; never pinned again here.
    fn noticed<T>(&self, result: anyhow::Result<T>) -> anyhow::Result<T> {
        if let Err(e) = &result
            && format!("{e:#}").contains(moli_net::PIN_MISMATCH)
        {
            let seen = self.https.seen_fingerprint();
            self.changed.send_if_modified(|was| {
                let first = was.is_none();
                if first {
                    *was = Some(Changed { seen });
                }
                first
            });
        }
        result
    }

    async fn login(&self) -> anyhow::Result<String> {
        let body = serde_json::json!({ "user": self.user, "password": self.password }).to_string();
        let request = self.https.request(Method::POST, "/api/login");
        let request = moli_net::text_body(request, "application/json", body)?;
        let (status, headers, _) = self.noticed(self.https.exchange_full(request, WAIT).await)?;
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

    /// One request in the session, logging in again once when it ended.
    /// Only the status and the body come back: what Frigate (or the go2rtc
    /// behind it) says in an error never reaches a message.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<(&str, &str)>,
        wait: Duration,
    ) -> anyhow::Result<Vec<u8>> {
        for attempt in 0..2 {
            let cookie = self.cookie().await?;
            let request = self
                .https
                .request(method.clone(), path)
                .header(http::header::COOKIE, cookie);
            let request = match body {
                Some((kind, text)) => moli_net::text_body(request, kind, text.to_owned())?,
                None => moli_net::empty(request)?,
            };
            let (status, _, body) = self.noticed(self.https.exchange_full(request, wait).await)?;
            if status == http::StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.cookie.lock().await = None;
                continue;
            }
            if !status.is_success() {
                bail!("Frigate: HTTP {} for {}", status.as_u16(), path_only(path));
            }
            return Ok(body.to_vec());
        }
        bail!("Frigate: session refused")
    }

    /// A JSON answer.
    async fn json(&self, path: &str) -> anyhow::Result<Json> {
        let body = self.send(Method::GET, path, None, WAIT).await?;
        serde_json::from_slice(&body).context("Frigate: not JSON")
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
            let response = self.noticed(
                self.https
                    .stream_response(moli_net::empty(request)?, WAIT)
                    .await,
            )?;
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

/// A path without its query (a stream name, a time) for a message.
fn path_only(path: &str) -> &str {
    path.split('?').next().unwrap_or(path)
}

/// How long the go2rtc behind Frigate may take to answer an offer.
const ANSWER_WAIT: Duration = Duration::from_secs(9);

/// One camera through Frigate's authenticated port, once `review` is set:
/// its image (`/api/<camera>/latest.jpg`) and its live video (the offer to
/// `/api/go2rtc/webrtc?src=<stream>`, a stream of Frigate's own go2rtc,
/// which Frigate checks against the account's cameras). No go2rtc is spoken
/// to directly, and `streams` (another go2rtc's names) is not used.
#[derive(Debug)]
pub struct Seen {
    pub api: Arc<Api>,
    pub camera: String,
}

impl SnapshotSource for Seen {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            let path = format!("/api/{}/latest.jpg?h=720", camera(&self.camera)?);
            let bytes = self.api.send(Method::GET, &path, None, WAIT).await?;
            if !bytes.starts_with(&[0xFF, 0xD8]) {
                bail!("Frigate: no image for {}", self.camera);
            }
            Ok(Image {
                content_type: "image/jpeg".into(),
                bytes,
            })
        })
    }
}

impl LiveSource for Seen {
    fn answer(
        &self,
        offer: String,
        viewer: Option<std::net::IpAddr>,
    ) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move {
            let stream = self.api.stream_of(&self.camera).await?;
            let path = format!("/api/go2rtc/webrtc?src={}", crate::go2rtc::encode(&stream));
            let offer = crate::go2rtc::for_go2rtc(&offer, viewer);
            let reply = self
                .api
                .send(
                    Method::POST,
                    &path,
                    Some(("application/sdp", &offer)),
                    ANSWER_WAIT,
                )
                .await?;
            let answer = String::from_utf8_lossy(&reply).into_owned();
            if !answer.starts_with("v=0") {
                bail!("Frigate: no SDP answer for {}", self.camera);
            }
            Ok(answer)
        })
    }
}

impl ReviewSource for Camera {
    fn review(&self, after: f64, before: f64) -> BoxFuture<'_, anyhow::Result<Vec<ReviewItem>>> {
        Box::pin(async move {
            let path = format!(
                "/api/review?cameras={}&after={after:.0}&before={before:.0}&limit={LIMIT}",
                camera(&self.name)?
            );
            Ok(review_items(&self.api.json(&path).await?))
        })
    }

    fn recorded(&self, after: f64, before: f64) -> BoxFuture<'_, anyhow::Result<Vec<Recorded>>> {
        Box::pin(async move {
            // Only the two times of each segment are kept, read straight as
            // numbers: a whole day as a JSON tree weighed tens of MiB, more
            // than Moli has (several cameras are asked at once).
            let mut segments: Vec<(f64, f64)> = Vec::new();
            for (from, to) in windows(after, before) {
                let path = format!(
                    "/api/{}/recordings?after={from:.0}&before={to:.0}",
                    camera(&self.name)?
                );
                let body = self.api.send(Method::GET, &path, None, WAIT).await?;
                let part: Vec<Segment> =
                    serde_json::from_slice(&body).context("Frigate: not a list of recordings")?;
                segments.extend(part.into_iter().map(|s| (s.start_time, s.end_time)));
            }
            // A segment across two windows comes twice: merged like any overlap.
            Ok(merge(segments))
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
                    format!("/api/events/{}/snapshot.jpg?bbox=1", self.event(id).await?)
                }
                ReviewMedia::EventThumbnail(id) => {
                    format!("/api/events/{}/thumbnail.jpg", self.event(id).await?)
                }
                ReviewMedia::EventClip(id) => {
                    format!("/api/events/{}/clip.mp4", self.event(id).await?)
                }
                ReviewMedia::Recording { start, end } => {
                    if !(start.is_finite()
                        && end.is_finite()
                        && end > start
                        && end - start <= 3600.0)
                    {
                        bail!("a recording is asked for at most one hour");
                    }
                    format!(
                        "/api/{}/start/{start:.0}/end/{end:.0}/clip.mp4",
                        camera(&self.name)?
                    )
                }
            };
            self.api.media(&path, range.as_deref()).await
        })
    }
}

impl Camera {
    /// An event of this camera, and of no other: its id goes into Frigate's
    /// paths only once Frigate says the event is this camera's. The right
    /// to see is checked on the camera of the route; an id read elsewhere
    /// (another camera's timeline, MQTT, a notification) opens nothing.
    async fn event<'a>(&self, id: &'a str) -> anyhow::Result<&'a str> {
        let id = safe(id)?;
        let event = self.api.json(&format!("/api/events/{id}")).await?;
        if !of_camera(&event, &self.name) {
            bail!("not an event of this camera");
        }
        Ok(id)
    }
}

/// Whether Frigate's event (`/api/events/<id>`) is this camera's.
fn of_camera(event: &Json, camera: &str) -> bool {
    event["camera"].as_str() == Some(camera)
}

/// An id as Frigate writes them (`1731670000.123456-abcdef`): nothing else
/// reaches its paths. It starts with a letter or a digit, never `..`.
fn safe(id: &str) -> anyhow::Result<&str> {
    if id.len() <= 64
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Ok(id)
    } else {
        bail!("not a Frigate event id")
    }
}

/// A camera name as Frigate allows them (`[A-Za-z0-9_-]`): the only names
/// the driver takes from MQTT, and the only ones that reach Frigate's paths
/// and queries.
#[must_use]
pub fn is_camera(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn camera(name: &str) -> anyhow::Result<&str> {
    if is_camera(name) {
        Ok(name)
    } else {
        bail!("not a Frigate camera name")
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

/// `after`..`before` in windows of [`WINDOW`] at most, in order.
#[must_use]
pub fn windows(after: f64, before: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut from = after;
    while from < before {
        let to = (from + WINDOW).min(before);
        out.push((from, to));
        from = to;
    }
    out
}

/// One recorded segment, its times only (the rest of Frigate's fields are
/// skipped while reading, never kept).
#[derive(serde::Deserialize)]
struct Segment {
    start_time: f64,
    end_time: f64,
}

/// `/api/<camera>/recordings` (10 s segments) → the stretches recorded, from
/// an answer already read as JSON (the tests' shape; the driver reads the
/// times alone, see [`Segment`]).
#[cfg(test)]
fn stretches(answer: &Json) -> Vec<Recorded> {
    merge(
        answer
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|s| Some((s["start_time"].as_f64()?, s["end_time"].as_f64()?)))
            .collect(),
    )
}

/// Segments (start, end) → the stretches they make, gaps of [`GAP`] closed.
fn merge(mut segments: Vec<(f64, f64)>) -> Vec<Recorded> {
    segments.retain(|(a, b)| b > a);
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

/// `/api/config` → each camera's first live stream (`live.streams`, a
/// label → its go2rtc stream).
#[must_use]
pub fn live_streams(config: &Json) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(cameras) = config["cameras"].as_object() {
        for (camera, c) in cameras {
            let first = c["live"]["streams"]
                .as_object()
                .and_then(|streams| streams.values().find_map(Json::as_str));
            if let Some(stream) = first {
                out.insert(camera.clone(), stream.to_owned());
            }
        }
    }
    out
}

/// A zone of Frigate's configuration: the camera it is drawn on, and the
/// name Frigate shows for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneOf {
    pub camera: String,
    pub name: String,
}

/// `/api/config` → every camera's zones, by the name Frigate publishes
/// their counts under (`<base>/<zone>/<object>`). A name Frigate would
/// refuse is left out, as for a camera; a name drawn on two cameras keeps
/// the first (Frigate counts them under one topic).
#[must_use]
pub fn zones_by_name(config: &Json) -> std::collections::BTreeMap<String, ZoneOf> {
    let mut out = std::collections::BTreeMap::new();
    let Some(cameras) = config["cameras"].as_object() else {
        return out;
    };
    for (camera, c) in cameras {
        let Some(zones) = c["zones"].as_object() else {
            continue;
        };
        for (zone, z) in zones {
            if !is_camera(camera) || !is_camera(zone) {
                continue;
            }
            out.entry(zone.clone()).or_insert_with(|| ZoneOf {
                camera: camera.clone(),
                name: z["friendly_name"]
                    .as_str()
                    .filter(|n| !n.trim().is_empty())
                    .map_or_else(|| zone.replace('_', " "), str::to_owned),
            });
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
    fn a_day_of_recordings_is_asked_two_hours_at_a_time() {
        let day = windows(0.0, 86_400.0);
        assert_eq!(day.len(), 12);
        assert_eq!(day.first(), Some(&(0.0, 7200.0)));
        assert_eq!(day.last(), Some(&(79_200.0, 86_400.0)));
        assert!(
            day.windows(2)
                .all(|w| (w[0].1 - w[1].0).abs() < f64::EPSILON)
        );
        assert_eq!(windows(10.0, 70.0), vec![(10.0, 70.0)]);
        assert!(windows(5.0, 5.0).is_empty());
        // The same segment from two windows: one stretch.
        let twice = json!([
            {"start_time": 7195.0, "end_time": 7205.0},
            {"start_time": 7195.0, "end_time": 7205.0},
            {"start_time": 7205.0, "end_time": 7215.0}
        ]);
        assert_eq!(
            stretches(&twice),
            vec![Recorded {
                start: 7195.0,
                end: 7215.0
            }]
        );
    }

    #[test]
    fn a_segment_is_read_for_its_two_times_only() {
        let body = br#"[{"id":"1700000000.0-abc","start_time":10.0,"end_time":20.0,
            "motion":12,"objects":0,"dBFS":0,"duration":10.0,"segment_size":1.3},
            {"start_time":20.5,"end_time":30.0,"region":{"x":1}}]"#;
        let part: Vec<Segment> = serde_json::from_slice(body).unwrap();
        let segments = part
            .into_iter()
            .map(|s| (s.start_time, s.end_time))
            .collect();
        assert_eq!(
            merge(segments),
            vec![Recorded {
                start: 10.0,
                end: 30.0
            }]
        );
    }

    #[test]
    fn a_camera_plays_its_own_stream_on_frigate() {
        let config = json!({"cameras": {
            "portail": {"live": {"streams": {"Principal": "portail_main", "Sous-flux": "portail_sub"}}},
            "entree": {"live": {"streams": {}}},
            "eye": {}
        }});
        let live = live_streams(&config);
        assert_eq!(
            live.get("portail").map(String::as_str),
            Some("portail_main")
        );
        // Without streams of its own, Frigate plays it by its name.
        assert!(!live.contains_key("entree") && !live.contains_key("eye"));
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
    fn zones_by_the_name_frigate_counts_them_under() {
        let config = json!({"cameras": {
            "entree": {"zones": {
                "allee_portail": {"coordinates": "0,0,1,0,1,1", "friendly_name": "Allée du portail"},
                "rue": {"coordinates": "0,0,1,0,1,1"}
            }},
            "portail": {"zones": {
                "rue": {"coordinates": "0,0,1,0,1,1"},
                "x&y": {"coordinates": "0,0,1,0,1,1"}
            }},
            "jardin": {}
        }});
        let zones = zones_by_name(&config);
        assert_eq!(
            zones.get("allee_portail"),
            Some(&ZoneOf {
                camera: "entree".into(),
                name: "Allée du portail".into()
            })
        );
        // No name of its own: its key, readable. Drawn twice: the first.
        assert_eq!(
            zones.get("rue"),
            Some(&ZoneOf {
                camera: "entree".into(),
                name: "rue".into()
            })
        );
        // A name Frigate would refuse is never a device.
        assert!(!zones.contains_key("x&y"));
        assert_eq!(zones.len(), 2);
        assert!(zones_by_name(&json!({})).is_empty());
    }

    #[test]
    fn only_frigate_ids_reach_its_paths() {
        assert!(safe("1731670000.123456-abc123").is_ok());
        assert!(safe("../config").is_err());
        assert!(safe("..").is_err());
        assert!(safe(".hidden").is_err());
        assert!(safe("").is_err());
        assert!(safe("a?b").is_err());
        assert!(safe("a/b").is_err());
    }

    #[test]
    fn an_event_opens_only_on_its_own_camera() {
        let event = json!({"id": "1731670000.1-abc", "camera": "chambre", "label": "person"});
        assert!(of_camera(&event, "chambre"));
        assert!(!of_camera(&event, "jardin"));
        assert!(!of_camera(&json!({"id": "x"}), "jardin"));
        assert!(!of_camera(&json!({"camera": null}), ""));
    }

    #[test]
    fn only_frigate_camera_names_reach_its_queries() {
        assert!(is_camera("portail") && is_camera("Jardin_2") && is_camera("rue-est"));
        assert!(!is_camera("x&cameras=chambre"));
        assert!(!is_camera("a.b") && !is_camera("..") && !is_camera(""));
        assert!(!is_camera("a b") && !is_camera("a?b") && !is_camera("a#b"));
        assert!(camera("x&cameras=chambre").is_err());
    }

    #[test]
    fn the_password_and_the_session_never_show() {
        let https = PinnedHttps::new("frigate.local", 8971, None).unwrap();
        let api = Api::new(https, "moli".into(), "s3cret-pass".into());
        *api.cookie.try_lock().unwrap() = Some("frigate_token=jwt-xyz".into());
        let shown = format!("{api:?}");
        assert!(shown.contains("moli"));
        assert!(!shown.contains("s3cret-pass") && !shown.contains("jwt-xyz"));
    }
}

//! Google Cast (TLS 8009, the TV's self-signed certificate pinned): what the
//! app on screen plays (title, picture, playing or paused) and play / pause.
//! Messages are a small protobuf envelope (after a 4-byte length) around
//! JSON. The receiver says which app runs; the app's media channel says
//! what it plays, and both speak up when something changes.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_net::PinnedTls;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::proto::{Writer, parse};

pub const PORT: u16 = 8009;
const SENDER: &str = "sender-moli";
const RECEIVER: &str = "receiver-0";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
/// Heartbeat and receiver status: the app's media is asked less often
/// (its changes come by themselves).
const POLL: Duration = Duration::from_secs(10);
const MEDIA_EVERY: u32 = 6;
/// The receiver answers every heartbeat: this long silent, it is gone.
const QUIET: Duration = Duration::from_secs(35);
const MAX_MESSAGE: usize = 256 * 1024;

/// What the screen shows, as Cast knows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playing {
    /// The app's name (« Disney+ », « YouTube »); `None`: nothing casts.
    pub app: Option<String>,
    /// `playing`, `paused`, `buffering`, `idle`.
    pub state: Option<String>,
    pub title: Option<String>,
    /// Series, artist or subtitle.
    pub subtitle: Option<String>,
    /// A picture of what plays (a public URL).
    pub image: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// Seconds, when last told.
    pub position: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Playing(Playing),
    /// The connection ended (said by whoever reconnects): nothing is known.
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Play,
    Pause,
    Stop,
}

/// The app being followed and its media session.
#[derive(Default)]
struct Follow {
    transport: Option<String>,
    session: Option<u64>,
    playing: Playing,
}

/// One connection, until it fails or `orders` closes (`Ok`).
pub async fn session(
    tls: &PinnedTls,
    host: &str,
    orders: &mut mpsc::Receiver<Order>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let stream = tls.connect(host, PORT).await.context("cast")?;
    // An order given while the TV was away is not run now, late.
    while orders.try_recv().is_ok() {}
    let (mut read, mut write) = tokio::io::split(stream);
    let (frames_tx, mut frames) = mpsc::channel::<Vec<u8>>(16);
    let reader = tokio::spawn(async move {
        while let Ok(frame) = read_frame(&mut read).await {
            if frames_tx.send(frame).await.is_err() {
                return;
            }
        }
    });
    let result: anyhow::Result<()> = async {
        let mut follow = Follow::default();
        let mut told: Option<Playing> = None;
        let mut request = 1u64;
        send(&mut write, RECEIVER, NS_CONNECTION, &json!({ "type": "CONNECT" })).await?;
        let mut tick = tokio::time::interval(POLL);
        let mut ticks = 0u32;
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    ticks += 1;
                    request += 1;
                    send(&mut write, RECEIVER, NS_HEARTBEAT, &json!({ "type": "PING" })).await?;
                    send(&mut write, RECEIVER, NS_RECEIVER, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
                    if let Some(transport) = follow.transport.clone().filter(|_| ticks.is_multiple_of(MEDIA_EVERY)) {
                        request += 1;
                        send(&mut write, &transport, NS_MEDIA, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
                    }
                }
                frame = tokio::time::timeout(QUIET, frames.recv()) => {
                    let frame = frame.context("the TV stopped answering")?.context("the TV closed the connection")?;
                    let Some((source, namespace, body)) = envelope(&frame) else { continue };
                    match (namespace.as_str(), body["type"].as_str()) {
                        (NS_HEARTBEAT, Some("PING")) => {
                            send(&mut write, &source, NS_HEARTBEAT, &json!({ "type": "PONG" })).await?;
                        }
                        (NS_RECEIVER, Some("RECEIVER_STATUS")) => {
                            if let Some(transport) = follow.receiver(&body) {
                                request += 1;
                                send(&mut write, &transport, NS_CONNECTION, &json!({ "type": "CONNECT" })).await?;
                                send(&mut write, &transport, NS_MEDIA, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
                            }
                        }
                        (NS_MEDIA, Some("MEDIA_STATUS")) => follow.media(&body),
                        (NS_CONNECTION, Some("CLOSE")) if follow.transport.as_deref() == Some(source.as_str()) => {
                            follow.transport = None;
                            follow.session = None;
                        }
                        _ => {}
                    }
                    if told.as_ref() != Some(&follow.playing) {
                        told = Some(follow.playing.clone());
                        events.send(Event::Playing(follow.playing.clone())).await?;
                    }
                }
                order = orders.recv() => {
                    let Some(order) = order else { return Ok(()) };
                    let (Some(transport), Some(session)) = (follow.transport.clone(), follow.session) else {
                        tracing::debug!("cast: nothing to control");
                        continue;
                    };
                    request += 1;
                    let kind = match order {
                        Order::Play => "PLAY",
                        Order::Pause => "PAUSE",
                        Order::Stop => "STOP",
                    };
                    send(&mut write, &transport, NS_MEDIA, &json!({ "type": kind, "mediaSessionId": session, "requestId": request })).await?;
                }
            }
        }
    }
    .await;
    reader.abort();
    result
}

impl Follow {
    /// The receiver's status: which app runs. Returns the app's transport
    /// when it is new (to connect to and ask).
    fn receiver(&mut self, body: &Json) -> Option<String> {
        let apps = body["status"]["applications"].as_array();
        let app = apps
            .into_iter()
            .flatten()
            .find(|a| a["isIdleScreen"].as_bool() != Some(true));
        let Some(app) = app else {
            *self = Self::default();
            return None;
        };
        let name = app["displayName"].as_str().map(str::to_owned);
        if self.playing.app != name {
            self.playing = Playing {
                app: name,
                ..Playing::default()
            };
        }
        let speaks_media = app["namespaces"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|n| n["name"].as_str() == Some(NS_MEDIA));
        let transport = app["transportId"]
            .as_str()
            .filter(|_| speaks_media)
            .map(str::to_owned);
        if transport == self.transport {
            return None;
        }
        self.transport.clone_from(&transport);
        self.session = None;
        transport
    }

    /// The app's media status. A status may leave out what did not change
    /// (the media itself): only what it says is updated.
    fn media(&mut self, body: &Json) {
        let Some(status) = body["status"].as_array().and_then(|s| s.first()) else {
            // Nothing loaded any more.
            self.session = None;
            self.playing = Playing {
                app: self.playing.app.take(),
                ..Playing::default()
            };
            return;
        };
        self.session = status["mediaSessionId"].as_u64().or(self.session);
        let p = &mut self.playing;
        if let Some(state) = status["playerState"].as_str() {
            p.state = Some(state.to_ascii_lowercase());
        }
        if let Some(position) = status["currentTime"].as_f64() {
            p.position = Some(position.round());
        }
        let media = &status["media"];
        if media.is_object() {
            let meta = &media["metadata"];
            let text = |key: &str| {
                meta[key]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            p.title = text("title");
            p.subtitle = text("subtitle")
                .or_else(|| text("seriesTitle"))
                .or_else(|| text("artist"));
            p.image = meta["images"][0]["url"].as_str().map(str::to_owned);
            p.duration = media["duration"]
                .as_f64()
                .filter(|d| *d > 0.0)
                .map(f64::round);
        }
    }
}

/// A message's sender, namespace and JSON payload.
fn envelope(frame: &[u8]) -> Option<(String, String, Json)> {
    let m = parse(frame)?;
    let body = serde_json::from_str(m.str(6)?).ok()?;
    Some((m.str(2)?.to_owned(), m.str(4)?.to_owned(), body))
}

async fn send<W: AsyncWrite + Unpin>(
    write: &mut W,
    destination: &str,
    namespace: &str,
    payload: &Json,
) -> anyhow::Result<()> {
    let message = Writer::new()
        .uint(1, 0)
        .str(2, SENDER)
        .str(3, destination)
        .str(4, namespace)
        .uint(5, 0)
        .str(6, &payload.to_string())
        .finish();
    let len = u32::try_from(message.len()).context("message too long")?;
    let mut frame = len.to_be_bytes().to_vec();
    frame.extend_from_slice(&message);
    write.write_all(&frame).await?;
    write.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(read: &mut R) -> anyhow::Result<Vec<u8>> {
    let len = usize::try_from(read.read_u32().await?)?;
    if len > MAX_MESSAGE {
        bail!("message of {len} bytes from the TV");
    }
    let mut frame = vec![0; len];
    read.read_exact(&mut frame).await?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(apps: &Json) -> Json {
        json!({ "type": "RECEIVER_STATUS", "status": { "applications": apps } })
    }

    #[test]
    fn follows_the_app_then_its_media() {
        let mut f = Follow::default();
        let disney = json!([{ "appId": "x", "displayName": "Disney+", "transportId": "t-1",
            "namespaces": [{ "name": NS_MEDIA }] }]);
        assert_eq!(f.receiver(&receiver(&disney)).as_deref(), Some("t-1"));
        assert_eq!(
            f.receiver(&receiver(&disney)),
            None,
            "same app: nothing new"
        );
        f.media(&json!({ "type": "MEDIA_STATUS", "status": [{
            "mediaSessionId": 7, "playerState": "PLAYING", "currentTime": 12.4,
            "media": { "duration": 496.0, "metadata": { "title": "Bluey", "seriesTitle": "Bluey S2",
                "images": [{ "url": "https://img.example/b.jpg" }] } } }] }));
        assert_eq!(
            f.playing,
            Playing {
                app: Some("Disney+".into()),
                state: Some("playing".into()),
                title: Some("Bluey".into()),
                subtitle: Some("Bluey S2".into()),
                image: Some("https://img.example/b.jpg".into()),
                duration: Some(496.0),
                position: Some(12.0),
            }
        );
        assert_eq!(f.session, Some(7));
        // A partial status (no media): the title stays.
        f.media(&json!({ "status": [{ "mediaSessionId": 7, "playerState": "PAUSED", "currentTime": 30.0 }] }));
        assert_eq!(f.playing.state.as_deref(), Some("paused"));
        assert_eq!(f.playing.title.as_deref(), Some("Bluey"));
        // Back to the home screen: nothing plays.
        let idle =
            json!([{ "displayName": "Backdrop", "isIdleScreen": true, "transportId": "t-0" }]);
        assert_eq!(f.receiver(&receiver(&idle)), None);
        assert_eq!(f.playing, Playing::default());
        assert_eq!(f.transport, None);
    }

    #[tokio::test]
    async fn envelopes_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        send(
            &mut a,
            "t-1",
            NS_MEDIA,
            &json!({ "type": "PAUSE", "mediaSessionId": 7 }),
        )
        .await
        .unwrap();
        let frame = read_frame(&mut b).await.unwrap();
        let m = parse(&frame).unwrap();
        assert_eq!(m.str(3), Some("t-1"));
        assert_eq!(m.str(4), Some(NS_MEDIA));
        // The sender is us: an envelope read back names it.
        let (source, namespace, body) = envelope(&frame).unwrap();
        assert_eq!((source.as_str(), namespace.as_str()), (SENDER, NS_MEDIA));
        assert_eq!(body["mediaSessionId"], 7);
    }
}

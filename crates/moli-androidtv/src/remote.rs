//! Android TV Remote, protocol v2: TLS on 6466 with the client certificate a
//! pairing made (Home Assistant's, reused: the TV trusts it), messages framed
//! by a varint length. The TV speaks first (configure, set active), then says
//! what changes (on or off, the app on screen, the volume) and pings every
//! 5 s; a silent connection is dead.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_net::PinnedTls;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::proto::{Fields, Writer, parse, varint};

pub const PORT: u16 = 6466;
/// What this remote does, as Home Assistant's: ping, keys, IME (the TV then
/// says which app is on screen), power, volume, app links.
const FEATURES: u64 = 1 | 2 | 4 | 32 | 64 | 512;
/// The TV pings every 5 s: three missed and the connection is gone.
const IDLE: Duration = Duration::from_secs(16);
const MAX_MESSAGE: usize = 64 * 1024;
/// `RemoteDirection.SHORT`: a press.
const SHORT: u64 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The session is up: orders can go.
    Ready,
    Power(bool),
    /// The app on screen (an Android package).
    App(String),
    Volume {
        level: u64,
        max: u64,
        muted: bool,
    },
    /// The connection ended (said by whoever reconnects): orders wait for
    /// `Ready` again.
    Lost,
    /// The TV said no to an order (what it was, in words).
    Refused(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    /// An app link (`market://launch?id=…` or any URL an app answers).
    Launch(String),
    /// An Android key code, pressed once.
    Key(u32),
}

/// What the remote launches for `target`: a link as it is, an Android
/// package through its store link (`com.netflix.ninja`).
#[must_use]
pub fn app_link(target: &str) -> String {
    if target.contains("://") {
        target.to_owned()
    } else {
        format!("market://launch?id={target}")
    }
}

/// One connection, until it fails or `orders` closes (`Ok`).
pub async fn session(
    tls: &PinnedTls,
    host: &str,
    orders: &mut mpsc::Receiver<Order>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let stream = tls.connect(host, PORT).await.context("android tv remote")?;
    // An order given while the TV was away is not run now, late.
    while orders.try_recv().is_ok() {}
    let (mut read, mut write) = tokio::io::split(stream);
    // Reading in its own task: a frame is never cut by an order arriving.
    let (frames_tx, mut frames) = mpsc::channel::<Vec<u8>>(16);
    let reader = tokio::spawn(async move {
        while let Ok(frame) = read_frame(&mut read).await {
            if frames_tx.send(frame).await.is_err() {
                return;
            }
        }
    });
    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                frame = tokio::time::timeout(IDLE, frames.recv()) => {
                    let frame = frame
                        .context("the TV went quiet")?
                        .context("the TV closed the connection")?;
                    if let Some(reply) = handle(&frame, events).await? {
                        send(&mut write, &reply).await?;
                    }
                }
                order = orders.recv() => {
                    let Some(order) = order else { return Ok(()) };
                    send(&mut write, &encode(&order)).await?;
                }
            }
        }
    }
    .await;
    reader.abort();
    result
}

/// What a message from the TV means, and what to answer.
async fn handle(frame: &[u8], events: &mpsc::Sender<Event>) -> anyhow::Result<Option<Vec<u8>>> {
    let m = parse(frame).context("unreadable message from the TV")?;
    if m.has(1) {
        let device = Writer::new()
            .uint(3, 1)
            .str(4, "1")
            .str(5, "atvremote")
            .str(6, "1.0.0");
        let configure = Writer::new().uint(1, FEATURES).msg(2, device);
        return Ok(Some(Writer::new().msg(1, configure).finish()));
    }
    if m.has(2) {
        events.send(Event::Ready).await?;
        let active = Writer::new().uint(1, FEATURES);
        return Ok(Some(Writer::new().msg(2, active).finish()));
    }
    if let Some(ping) = m.msg(8) {
        let pong = Writer::new().uint(1, ping.uint(1).unwrap_or(0));
        return Ok(Some(Writer::new().msg(9, pong).finish()));
    }
    if let Some(event) = event(&m) {
        events.send(event).await?;
    } else if m.has(3) {
        events.send(Event::Refused(refused(&m))).await?;
    }
    Ok(None)
}

/// `RemoteError { value = 1, message = 2 }`: the order the TV sends back.
fn refused(m: &Fields<'_>) -> String {
    let Some(order) = m.msg(3).and_then(|e| e.msg(2)) else {
        return "an order (not said which)".to_owned();
    };
    if let Some(link) = order.msg(90).and_then(|l| l.str(1)) {
        return format!("app link {link}");
    }
    if let Some(key) = order.msg(10).and_then(|k| k.uint(1)) {
        return format!("key {key}");
    }
    if order.has(1) {
        return "the configuration".to_owned();
    }
    if order.has(2) {
        return "set active".to_owned();
    }
    "an order".to_owned()
}

/// A change the TV reports.
fn event(m: &Fields<'_>) -> Option<Event> {
    if let Some(start) = m.msg(40) {
        // proto3: `false` is the field left out.
        return Some(Event::Power(start.uint(1) == Some(1)));
    }
    if let Some(ime) = m.msg(20) {
        let app = ime.msg(1)?.str(12)?;
        return (!app.is_empty()).then(|| Event::App(app.to_owned()));
    }
    let volume = m.msg(50)?;
    Some(Event::Volume {
        level: volume.uint(7).unwrap_or(0),
        max: volume.uint(6).unwrap_or(0),
        muted: volume.uint(8) == Some(1),
    })
}

fn encode(order: &Order) -> Vec<u8> {
    match order {
        Order::Launch(link) => Writer::new().msg(90, Writer::new().str(1, link)).finish(),
        Order::Key(code) => Writer::new()
            .msg(10, Writer::new().uint(1, u64::from(*code)).uint(2, SHORT))
            .finish(),
    }
}

async fn read_frame<R: AsyncRead + Unpin>(read: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut len = 0u64;
    for shift in (0..35).step_by(7) {
        let byte = read.read_u8().await?;
        len |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            let len = usize::try_from(len)?;
            if len > MAX_MESSAGE {
                bail!("message of {len} bytes from the TV");
            }
            let mut frame = vec![0; len];
            read.read_exact(&mut frame).await?;
            return Ok(frame);
        }
    }
    bail!("bad frame length from the TV")
}

async fn send<W: AsyncWrite + Unpin>(write: &mut W, message: &[u8]) -> anyhow::Result<()> {
    let mut frame = Vec::with_capacity(message.len() + 4);
    varint(message.len() as u64, &mut frame);
    frame.extend_from_slice(message);
    write.write_all(&frame).await?;
    write.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_the_handshake_and_pings_and_reports_changes() {
        let (tx, mut rx) = mpsc::channel(8);
        // The TV's configure: we answer with our features.
        let configure = Writer::new().msg(1, Writer::new().uint(1, 639)).finish();
        let reply = handle(&configure, &tx).await.unwrap().unwrap();
        let reply = parse(&reply).unwrap().msg(1).unwrap();
        assert_eq!(reply.uint(1), Some(FEATURES));
        assert_eq!(reply.msg(2).unwrap().str(5), Some("atvremote"));
        // Set active: ready, and the same features back.
        let active = Writer::new().msg(2, Writer::new().uint(1, 639)).finish();
        let reply = handle(&active, &tx).await.unwrap().unwrap();
        assert_eq!(
            parse(&reply).unwrap().msg(2).unwrap().uint(1),
            Some(FEATURES)
        );
        assert_eq!(rx.recv().await, Some(Event::Ready));
        // A ping is answered with its value.
        let ping = Writer::new().msg(8, Writer::new().uint(1, 42)).finish();
        let reply = handle(&ping, &tx).await.unwrap().unwrap();
        assert_eq!(parse(&reply).unwrap().msg(9).unwrap().uint(1), Some(42));
        // Changes become events.
        let off = Writer::new().msg(40, Writer::new()).finish();
        assert_eq!(handle(&off, &tx).await.unwrap(), None);
        assert_eq!(rx.recv().await, Some(Event::Power(false)));
        let app = Writer::new()
            .msg(
                20,
                Writer::new().msg(1, Writer::new().str(12, "com.netflix.ninja")),
            )
            .finish();
        handle(&app, &tx).await.unwrap();
        assert_eq!(
            rx.recv().await,
            Some(Event::App("com.netflix.ninja".into()))
        );
        let volume = Writer::new()
            .msg(50, Writer::new().uint(6, 60).uint(7, 59))
            .finish();
        handle(&volume, &tx).await.unwrap();
        assert_eq!(
            rx.recv().await,
            Some(Event::Volume {
                level: 59,
                max: 60,
                muted: false
            })
        );
    }

    #[tokio::test]
    async fn a_refusal_says_what_was_refused() {
        let (tx, mut rx) = mpsc::channel(8);
        let launch = Writer::new().msg(90, Writer::new().str(1, "market://launch?id=com.x"));
        let error = Writer::new()
            .msg(3, Writer::new().uint(1, 1).msg(2, launch))
            .finish();
        assert_eq!(handle(&error, &tx).await.unwrap(), None);
        assert_eq!(
            rx.recv().await,
            Some(Event::Refused("app link market://launch?id=com.x".into()))
        );
    }

    #[test]
    fn apps_launch_through_their_store_link() {
        assert_eq!(
            app_link("com.limelight"),
            "market://launch?id=com.limelight"
        );
        let video = "https://www.youtube.com/watch?v=NLs3LqVgpT4";
        assert_eq!(app_link(video), video);
        let launch = encode(&Order::Launch(app_link("com.netflix.ninja")));
        let m = parse(&launch).unwrap();
        assert_eq!(
            m.msg(90).unwrap().str(1),
            Some("market://launch?id=com.netflix.ninja")
        );
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let message = Writer::new().uint(40, 1).finish();
        send(&mut a, &message).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), message);
    }
}

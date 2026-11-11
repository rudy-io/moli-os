//! Android TV Remote, protocol v2: TLS on 6466 with the client certificate a
//! pairing made (Home Assistant's, reused: the TV trusts it), messages framed
//! by a varint length. The TV speaks first (configure, set active), then says
//! what changes (on or off, the app on screen, the volume) and pings every
//! 5 s; a silent connection is dead. A text field that opens on the TV
//! sends its two counters (`RemoteImeBatchEdit`); text written there goes
//! back with them.

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
/// `RemoteDirection`: a key held down (`START_LONG`), let go (`END_LONG`),
/// or pressed (`SHORT`).
const START_LONG: u64 = 1;
const END_LONG: u64 = 2;
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
    /// An Android key code held down (`true`) or let go: held, the TV
    /// repeats it as a held key on its own remote (a video seeks faster).
    Hold(u32, bool),
    /// The text of the field open on the TV (a search box…), all of it.
    Text(String),
}

/// `KEYCODE_ENTER`: what a keyboard's Enter key sends (a search starts).
pub const ENTER: u32 = 66;

/// An app people name rather than spell (« mets YouTube »).
struct KnownApp {
    /// As letters and digits only, lowercase. Brands: the same in every language.
    names: &'static [&'static str],
    package: &'static str,
    /// The link Google TVs open it by: some refuse the store link
    /// (`market://launch?id=…`). YouTube's and Netflix's were tried on a real
    /// TV; Disney+'s and Prime Video's are Home Assistant's documented ones.
    web: Option<&'static str>,
}

const KNOWN_APPS: [KnownApp; 6] = [
    KnownApp {
        names: &["netflix"],
        package: "com.netflix.ninja",
        web: Some("https://www.netflix.com/title"),
    },
    KnownApp {
        names: &["youtube"],
        package: "com.google.android.youtube.tv",
        web: Some("https://www.youtube.com"),
    },
    KnownApp {
        names: &["disney", "disneyplus"],
        package: "com.disney.disneyplus",
        web: Some("https://www.disneyplus.com"),
    },
    KnownApp {
        names: &["primevideo", "amazonprimevideo", "prime"],
        package: "com.amazon.amazonvideo.livingroom",
        web: Some("https://app.primevideo.com"),
    },
    KnownApp {
        names: &["jellyfin"],
        package: "org.jellyfin.androidtv",
        web: None,
    },
    KnownApp {
        names: &["moonlight"],
        package: "com.limelight",
        web: None,
    },
];

/// A known app, named the way people say it (« Disney+ ») or by its package.
fn known(target: &str) -> Option<&'static KnownApp> {
    let key: String = target
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    KNOWN_APPS
        .iter()
        .find(|a| a.package == target.trim() || a.names.contains(&key.as_str()))
}

/// The package of an app named the way people say it (« Disney+ », « Prime
/// Video »), when it is a known one.
#[must_use]
pub fn package_of(name: &str) -> Option<&'static str> {
    known(name).map(|a| a.package)
}

/// The links that open `target`, in the order to try: a link as it is; a
/// known app (named, or by package) by its web link, then its store link;
/// any other package by its store link (`market://launch?id=…`).
#[must_use]
pub fn app_links(target: &str) -> Vec<String> {
    let target = target.trim();
    if target.contains("://") {
        return vec![target.to_owned()];
    }
    let app = known(target);
    let package = app.map_or(target, |a| a.package);
    app.and_then(|a| a.web)
        .map(str::to_owned)
        .into_iter()
        .chain([format!("market://launch?id={package}")])
        .collect()
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
    // The text field's counters, as the TV last gave them.
    let mut field = (0, 0);
    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                frame = tokio::time::timeout(IDLE, frames.recv()) => {
                    let frame = frame
                        .context("the TV went quiet")?
                        .context("the TV closed the connection")?;
                    if let Some(counters) = text_field(&frame) {
                        field = counters;
                    }
                    if let Some(reply) = handle(&frame, events).await? {
                        send(&mut write, &reply).await?;
                    }
                }
                order = orders.recv() => {
                    let Some(order) = order else { return Ok(()) };
                    send(&mut write, &encode(&order, field)).await?;
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

/// `RemoteImeBatchEdit { ime_counter = 1, field_counter = 2 }`: the
/// counters of the text field the TV just opened or changed.
fn text_field(frame: &[u8]) -> Option<(u64, u64)> {
    let edit = parse(frame)?.msg(21)?;
    Some((edit.uint(1).unwrap_or(0), edit.uint(2).unwrap_or(0)))
}

fn encode(order: &Order, (ime, field): (u64, u64)) -> Vec<u8> {
    match order {
        Order::Launch(link) => Writer::new().msg(90, Writer::new().str(1, link)).finish(),
        Order::Key(code) => key(*code, SHORT),
        Order::Hold(code, down) => key(*code, if *down { START_LONG } else { END_LONG }),
        // `RemoteEditInfo { insert = 1, RemoteImeObject { start, end, value } }`,
        // the cursor after the last character.
        Order::Text(text) => {
            let cursor = u64::try_from(text.chars().count().saturating_sub(1)).unwrap_or(0);
            let object = Writer::new().uint(1, cursor).uint(2, cursor).str(3, text);
            let info = Writer::new().uint(1, 1).msg(2, object);
            let edit = Writer::new().uint(1, ime).uint(2, field).msg(3, info);
            Writer::new().msg(21, edit).finish()
        }
    }
}

/// `RemoteKeyInject { key_code = 1, direction = 2 }`.
fn key(code: u32, direction: u64) -> Vec<u8> {
    Writer::new()
        .msg(
            10,
            Writer::new().uint(1, u64::from(code)).uint(2, direction),
        )
        .finish()
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
    fn apps_open_by_their_web_link_then_their_store_link() {
        // A Google TV may refuse the store link: the web link comes first.
        assert_eq!(
            app_links("YouTube"),
            [
                "https://www.youtube.com",
                "market://launch?id=com.google.android.youtube.tv"
            ]
        );
        assert_eq!(
            app_links("com.netflix.ninja"),
            [
                "https://www.netflix.com/title",
                "market://launch?id=com.netflix.ninja"
            ]
        );
        // Without a web link, an unknown package: the store link only.
        assert_eq!(app_links("Moonlight"), ["market://launch?id=com.limelight"]);
        assert_eq!(
            app_links("org.example.tv"),
            ["market://launch?id=org.example.tv"]
        );
        let video = "https://www.youtube.com/watch?v=NLs3LqVgpT4";
        assert_eq!(app_links(video), [video]);
        assert_eq!(package_of("Disney+"), Some("com.disney.disneyplus"));
        assert_eq!(
            package_of("Prime Video"),
            Some("com.amazon.amazonvideo.livingroom")
        );
        assert_eq!(package_of("Une appli inconnue"), None);
        let launch = encode(
            &Order::Launch("market://launch?id=com.netflix.ninja".into()),
            (0, 0),
        );
        let m = parse(&launch).unwrap();
        assert_eq!(
            m.msg(90).unwrap().str(1),
            Some("market://launch?id=com.netflix.ninja")
        );
    }

    #[test]
    fn text_goes_back_with_the_fields_counters() {
        let from_tv = Writer::new()
            .msg(21, Writer::new().uint(1, 7).uint(2, 3))
            .finish();
        let field = text_field(&from_tv).unwrap();
        assert_eq!(field, (7, 3));
        let sent = encode(&Order::Text("Dune".into()), field);
        let edit = parse(&sent).unwrap().msg(21).unwrap();
        assert_eq!((edit.uint(1), edit.uint(2)), (Some(7), Some(3)));
        let info = edit.msg(3).unwrap();
        assert_eq!(info.uint(1), Some(1));
        let object = info.msg(2).unwrap();
        assert_eq!(object.str(3), Some("Dune"));
        assert_eq!((object.uint(1), object.uint(2)), (Some(3), Some(3)));
        assert!(text_field(&encode(&Order::Key(ENTER), field)).is_none());
    }

    #[test]
    fn a_held_key_starts_long_and_ends_long() {
        let direction = |order: Order| {
            let frame = encode(&order, (0, 0));
            let m = parse(&frame).unwrap();
            let key = m.msg(10).unwrap();
            (key.uint(1), key.uint(2))
        };
        assert_eq!(direction(Order::Key(22)), (Some(22), Some(SHORT)));
        assert_eq!(
            direction(Order::Hold(22, true)),
            (Some(22), Some(START_LONG))
        );
        assert_eq!(
            direction(Order::Hold(22, false)),
            (Some(22), Some(END_LONG))
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

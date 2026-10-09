//! The TV's Android side, beside JointSpace: the app on screen and launching
//! apps (Android TV Remote, with the pairing Home Assistant made: secrets
//! `atv_cert` / `atv_key`, PEM), what plays (Google Cast, nothing to pair).
//! Both certificates are pinned on first contact, like the TV's own.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_androidtv::{cast, remote};
use moli_core::{DeviceId, Value};
use moli_net::PinnedTls;
use moli_runtime::DriverCtx;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

pub(crate) const ATV_CERT: &str = "atv_cert";
pub(crate) const ATV_KEY: &str = "atv_key";
const ATV_PIN: &str = "atv_pin";
const CAST_PIN: &str = "cast_pin";
const RETRY_MIN: Duration = Duration::from_secs(5);
/// Deep standby closes both ports: no need to knock often (the TV coming
/// back says so at once: [`Android::tick`]).
const RETRY_MAX: Duration = Duration::from_secs(60);
/// An app asked for while the remote cannot take it (the TV waking up, the
/// remote reconnecting) is kept this long.
const HOLD: Duration = Duration::from_secs(60);
/// A TV just back may refuse an app while it starts: tried once more, later.
const RETRY_LAUNCH: Duration = Duration::from_secs(4);
/// A refusal this soon after the remote came up is the TV still starting.
const STARTING: Duration = Duration::from_secs(90);

/// News from either side.
pub(crate) enum Up {
    Remote(remote::Event),
    Cast(cast::Event),
}

/// One side: its orders, its TLS settings (for the pin), whether it is up,
/// whether its pin is stored.
struct Side<O> {
    orders: mpsc::Sender<O>,
    tls: PinnedTls,
    up: bool,
    pinned: bool,
    /// Cuts the wait before the next connection (the TV is back).
    nudge: Arc<Notify>,
}

/// An app not sent yet: from `not_before` (a retry waits a little), until
/// `until`; `retry` when it is already the second try.
struct Pending {
    target: String,
    not_before: Instant,
    until: Instant,
    retry: bool,
}

pub(crate) struct Android {
    remote: Option<Side<remote::Order>>,
    cast: Option<Side<cast::Order>>,
    playing: Option<cast::Playing>,
    pending: Option<Pending>,
    /// The last app sent: what, when, whether it was the retry.
    sent: Option<(String, Instant, bool)>,
    /// When the remote last came up.
    ready_at: Option<Instant>,
    /// The TV answered JointSpace at the last poll; when the connections were
    /// last nudged (once on the TV's return, then once a minute at most: a
    /// remote that keeps failing must keep its slower pace).
    tv_was_online: bool,
    nudged: Option<Instant>,
}

impl Android {
    /// Starts both connections (the remote only when paired).
    pub(crate) fn start(ctx: &DriverCtx, host: &str) -> (Self, mpsc::Receiver<Up>) {
        let (up, ups) = mpsc::channel(32);
        let remote = match (ctx.secret(ATV_CERT), ctx.secret(ATV_KEY)) {
            (Some(cert), Some(key)) => {
                let pin = ctx.secret(ATV_PIN);
                match PinnedTls::with_client_cert(pin.clone(), &cert, &key) {
                    Ok(tls) => {
                        let nudge = Arc::new(Notify::new());
                        Some(Side {
                            orders: keep_remote(
                                host.to_owned(),
                                tls.clone(),
                                up.clone(),
                                nudge.clone(),
                            ),
                            tls,
                            up: false,
                            pinned: pin.is_some(),
                            nudge,
                        })
                    }
                    Err(e) => {
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "android tv pairing unusable");
                        None
                    }
                }
            }
            _ => None,
        };
        let pin = ctx.secret(CAST_PIN);
        let cast = match PinnedTls::new(pin.clone()) {
            Ok(tls) => {
                let nudge = Arc::new(Notify::new());
                Some(Side {
                    orders: keep_cast(host.to_owned(), tls.clone(), up, nudge.clone()),
                    tls,
                    up: false,
                    pinned: pin.is_some(),
                    nudge,
                })
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "cast unusable");
                None
            }
        };
        (
            Self {
                remote,
                cast,
                playing: None,
                pending: None,
                sent: None,
                ready_at: None,
                tv_was_online: false,
                nudged: None,
            },
            ups,
        )
    }

    /// The remote says which app is on screen: JointSpace's word is not needed.
    pub(crate) fn knows_the_app(&self) -> bool {
        self.remote.is_some()
    }

    /// Paired: an app can be opened (worth waking the TV for).
    pub(crate) fn can_launch(&self) -> bool {
        self.remote.is_some()
    }

    /// Opens an app (Android package) or a link. The remote not ready yet
    /// (the TV waking up, the remote reconnecting): kept, and sent as soon
    /// as it is, for [`HOLD`].
    pub(crate) fn launch(&mut self, target: &str) -> anyhow::Result<()> {
        let side = self
            .remote
            .as_ref()
            .with_context(|| moli_i18n::tr!("pilotes.philips.appairage"))?;
        let target = target.trim().to_owned();
        if target.is_empty() {
            bail!(moli_i18n::tr!("pilotes.philips.quelle_appli"));
        }
        if side.up {
            // The last word wins: an app still waiting must not come after.
            self.pending = None;
            return self.send(target, false);
        }
        side.nudge.notify_one();
        let now = Instant::now();
        self.pending = Some(Pending {
            target,
            not_before: now,
            until: now + HOLD,
            retry: false,
        });
        Ok(())
    }

    fn send(&mut self, target: String, retry: bool) -> anyhow::Result<()> {
        let side = self
            .remote
            .as_ref()
            .with_context(|| moli_i18n::tr!("pilotes.philips.pas_de_telecommande"))?;
        side.orders
            .try_send(remote::Order::Launch(remote::app_link(&target)))
            .map_err(|_| {
                anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.telecommande_ne_suit_pas"))
            })?;
        self.sent = Some((target, Instant::now(), retry));
        Ok(())
    }

    /// At each poll of the TV: back on the network, the connections that are
    /// down try again now (not after their wait), and a kept app goes.
    pub(crate) fn tick(&mut self, tv_online: bool) {
        let back = tv_online && !self.tv_was_online;
        self.tv_was_online = tv_online;
        let due = back || self.nudged.is_none_or(|at| at.elapsed() >= RETRY_MAX);
        if tv_online && due {
            let mut nudged = false;
            if let Some(side) = self.remote.as_ref().filter(|s| !s.up) {
                side.nudge.notify_one();
                nudged = true;
            }
            if let Some(side) = self.cast.as_ref().filter(|s| !s.up) {
                side.nudge.notify_one();
                nudged = true;
            }
            if nudged {
                self.nudged = Some(Instant::now());
            }
        }
        self.flush();
    }

    /// Sends the kept app when the remote can take it; drops it when too old.
    fn flush(&mut self) {
        let Some(p) = &self.pending else { return };
        let now = Instant::now();
        if now >= p.until {
            tracing::warn!(app = %p.target, "android tv remote: app not opened, the remote did not come back");
            self.pending = None;
            return;
        }
        if now < p.not_before || !self.remote.as_ref().is_some_and(|s| s.up) {
            return;
        }
        let Some(p) = self.pending.take() else { return };
        if let Err(e) = self.send(p.target, p.retry) {
            tracing::warn!(error = %format!("{e:#}"), "android tv remote: app not sent");
        }
    }

    /// The TV refused an order: an app just sent to a TV still starting is
    /// tried once more a little later.
    fn refused(&mut self, what: &str) {
        let starting = self.ready_at.is_some_and(|at| at.elapsed() < STARTING);
        // Only the very app link sent (not a key, not the configuration).
        let again = match &self.sent {
            Some((target, at, false))
                if starting
                    && at.elapsed() < Duration::from_secs(3)
                    && what.contains(remote::app_link(target).as_str()) =>
            {
                Some(target.clone())
            }
            _ => None,
        };
        tracing::warn!(refused = %what, retry = again.is_some(), "android tv remote: the TV refused an order");
        if let Some(target) = again {
            let now = Instant::now();
            self.sent = None;
            self.pending = Some(Pending {
                target,
                not_before: now + RETRY_LAUNCH,
                until: now + HOLD,
                retry: true,
            });
        }
    }

    /// Play, pause or stop what Cast sees playing.
    pub(crate) fn control(&self, word: &str) -> anyhow::Result<()> {
        let order = match word {
            "play" => cast::Order::Play,
            "pause" => cast::Order::Pause,
            "stop" => cast::Order::Stop,
            other => bail!(moli_i18n::tr!(
                "pilotes.philips.play_pause_stop",
                word = format!("{other:?}")
            )),
        };
        let side = self
            .cast
            .as_ref()
            .with_context(|| moli_i18n::tr!("pilotes.philips.cast_indisponible"))?;
        if !side.up || self.playing.as_ref().is_none_or(|p| p.state.is_none()) {
            bail!(moli_i18n::tr!("pilotes.philips.rien_ne_joue"));
        }
        side.orders
            .try_send(order)
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.cast_ne_suit_pas")))
    }

    pub(crate) async fn on(&mut self, up: Up, ctx: &DriverCtx, id: &DeviceId) {
        match up {
            Up::Remote(event) => {
                let Some(side) = self.remote.as_mut() else {
                    return;
                };
                match event {
                    remote::Event::Ready => {
                        side.up = true;
                        pin(ctx, side, ATV_PIN).await;
                        self.ready_at = Some(Instant::now());
                        self.flush();
                    }
                    remote::Event::Lost => side.up = false,
                    remote::Event::Refused(what) => self.refused(&what),
                    remote::Event::App(app) => ctx.set_state(id, "app", Value::Text(app.into())),
                    remote::Event::Power(false) => ctx.set_state(id, "app", Value::Null),
                    // JointSpace says these already.
                    remote::Event::Power(true) | remote::Event::Volume { .. } => {}
                }
            }
            Up::Cast(event) => {
                let Some(side) = self.cast.as_mut() else {
                    return;
                };
                let playing = match event {
                    cast::Event::Playing(playing) => {
                        side.up = true;
                        pin(ctx, side, CAST_PIN).await;
                        playing
                    }
                    cast::Event::Lost => {
                        side.up = false;
                        cast::Playing::default()
                    }
                };
                if self.playing.as_ref() == Some(&playing) {
                    return;
                }
                for (key, value) in media_values(&playing) {
                    ctx.set_state(id, key, value);
                }
                self.playing = Some(playing);
            }
        }
    }
}

/// The certificate seen first, kept: from now on, only it.
async fn pin<O>(ctx: &DriverCtx, side: &mut Side<O>, name: &str) {
    if side.pinned {
        return;
    }
    if let Some(seen) = side.tls.pinned_fingerprint()
        && ctx.store_secret(name, &seen).await.is_ok()
    {
        side.pinned = true;
    }
}

fn text(v: Option<&String>) -> Value {
    v.map_or(Value::Null, |s| Value::Text(s.as_str().into()))
}

fn number(v: Option<f64>) -> Value {
    v.map_or(Value::Null, Value::Float)
}

pub(crate) fn media_values(p: &cast::Playing) -> [(&'static str, Value); 7] {
    [
        ("media_app", text(p.app.as_ref())),
        ("media_state", text(p.state.as_ref())),
        ("media_title", text(p.title.as_ref())),
        ("media_subtitle", text(p.subtitle.as_ref())),
        ("media_image", text(p.image.as_ref())),
        ("media_duration", number(p.duration)),
        ("media_position", number(p.position)),
    ]
}

/// The wait before the next connection, cut short by a nudge (the TV is
/// back): then the wait starts small again.
async fn wait(pause: &mut Duration, nudge: &Notify) {
    tokio::select! {
        () = tokio::time::sleep(*pause) => {}
        () = nudge.notified() => *pause = RETRY_MIN,
    }
}

/// The remote's connection, again and again while the driver lives.
fn keep_remote(
    host: String,
    tls: PinnedTls,
    up: mpsc::Sender<Up>,
    nudge: Arc<Notify>,
) -> mpsc::Sender<remote::Order> {
    let (orders_tx, mut orders) = mpsc::channel(8);
    tokio::spawn(async move {
        let (events_tx, mut events) = mpsc::channel(16);
        let forward = {
            let up = up.clone();
            tokio::spawn(async move {
                while let Some(e) = events.recv().await {
                    if up.send(Up::Remote(e)).await.is_err() {
                        return;
                    }
                }
            })
        };
        let mut pause = RETRY_MIN;
        loop {
            let started = Instant::now();
            let result = remote::session(&tls, &host, &mut orders, &events_tx).await;
            // Through the same channel as its news: never ahead of them.
            if up.is_closed()
                || result.is_ok()
                || events_tx.send(remote::Event::Lost).await.is_err()
            {
                break;
            }
            if let Err(e) = result {
                tracing::debug!(error = %format!("{e:#}"), "android tv remote: reconnecting");
            }
            pause = next_pause(pause, started);
            wait(&mut pause, &nudge).await;
        }
        forward.abort();
    });
    orders_tx
}

/// Cast's connection, the same way.
fn keep_cast(
    host: String,
    tls: PinnedTls,
    up: mpsc::Sender<Up>,
    nudge: Arc<Notify>,
) -> mpsc::Sender<cast::Order> {
    let (orders_tx, mut orders) = mpsc::channel(8);
    tokio::spawn(async move {
        let (events_tx, mut events) = mpsc::channel(16);
        let forward = {
            let up = up.clone();
            tokio::spawn(async move {
                while let Some(e) = events.recv().await {
                    if up.send(Up::Cast(e)).await.is_err() {
                        return;
                    }
                }
            })
        };
        let mut pause = RETRY_MIN;
        loop {
            let started = Instant::now();
            let result = cast::session(&tls, &host, &mut orders, &events_tx).await;
            // Through the same channel as its news: never ahead of them.
            if up.is_closed() || result.is_ok() || events_tx.send(cast::Event::Lost).await.is_err()
            {
                break;
            }
            if let Err(e) = result {
                tracing::debug!(error = %format!("{e:#}"), "cast: reconnecting");
            }
            pause = next_pause(pause, started);
            wait(&mut pause, &nudge).await;
        }
        forward.abort();
    });
    orders_tx
}

/// A connection that lasted starts again quickly; one that keeps failing
/// waits longer and longer.
fn next_pause(pause: Duration, started: Instant) -> Duration {
    if started.elapsed() > RETRY_MAX {
        RETRY_MIN
    } else {
        (pause * 2).min(RETRY_MAX)
    }
}

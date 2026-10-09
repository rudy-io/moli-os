//! The TV's Android side, beside JointSpace: the app on screen and launching
//! apps (Android TV Remote, with the pairing Home Assistant made: secrets
//! `atv_cert` / `atv_key`, PEM), what plays (Google Cast, nothing to pair).
//! Both certificates are pinned on first contact, like the TV's own.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_androidtv::{cast, remote};
use moli_core::{DeviceId, Value};
use moli_net::PinnedTls;
use moli_runtime::{CommandRequest, DriverCtx};
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
/// No refusal this long after a link: the app opened (the TV refuses within
/// a second; it does not always say which app came up).
const SETTLE: Duration = Duration::from_secs(2);
/// The hub's patience ends at the order's deadline: answered a little before.
const BEFORE_DEADLINE: Duration = Duration::from_millis(300);

/// One app link to the TV.
fn send_link(side: Option<&Side<remote::Order>>, link: &str) -> anyhow::Result<()> {
    let side = side.with_context(|| moli_i18n::tr!("pilotes.philips.pas_de_telecommande"))?;
    side.orders
        .try_send(remote::Order::Launch(link.to_owned()))
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.telecommande_ne_suit_pas")))
}

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

/// The app being opened: the link sent last, the ones left if the TV refuses
/// it, and the order waiting for the TV's word.
struct Sent {
    target: String,
    link: String,
    at: Instant,
    rest: VecDeque<String>,
    /// Whether it is already the second try (a TV that was starting).
    retry: bool,
    /// Answered once the TV takes the app or refuses every link: a refusal
    /// reaches whoever asked (the family, Moli) instead of a silent « ok ».
    waiting: Option<Waiting>,
}

/// Who waits for an app's outcome, and until when (the hub's patience).
pub(crate) struct Waiting {
    pub(crate) answer: Box<dyn FnOnce(Result<(), String>) + Send>,
    pub(crate) deadline: std::time::Instant,
}

impl Waiting {
    /// An order of the hub's.
    pub(crate) fn order(order: CommandRequest) -> Self {
        let deadline = order.deadline;
        Self {
            answer: Box::new(move |result| order.reply(result)),
            deadline,
        }
    }

    fn reply(self, result: Result<(), String>) {
        (self.answer)(result);
    }
}

impl Sent {
    fn answer(&mut self, result: Result<(), String>) {
        if let Some(waiting) = self.waiting.take() {
            waiting.reply(result);
        }
    }
}

pub(crate) struct Android {
    remote: Option<Side<remote::Order>>,
    cast: Option<Side<cast::Order>>,
    playing: Option<cast::Playing>,
    pending: Option<Pending>,
    /// The last app sent.
    sent: Option<Sent>,
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

    /// Opens an app (its name, its Android package) or a link. The remote up:
    /// sent now, and `waiting` is answered when the TV takes it or has
    /// refused every way to open it. The remote not ready yet (the TV waking
    /// up, the remote reconnecting): kept, sent as soon as it is, for
    /// [`HOLD`], and answered now (the TV may take a while).
    pub(crate) fn launch(&mut self, target: &str, waiting: Waiting) {
        let Some(side) = self.remote.as_ref() else {
            return waiting.reply(Err(moli_i18n::tr!("pilotes.philips.appairage")));
        };
        let target = target.trim().to_owned();
        if target.is_empty() {
            return waiting.reply(Err(moli_i18n::tr!("pilotes.philips.quelle_appli")));
        }
        if side.up {
            // The last word wins: an app still waiting must not come after.
            self.pending = None;
            match self.send(target, false) {
                Ok(()) => {
                    if let Some(sent) = &mut self.sent {
                        sent.waiting = Some(waiting);
                    }
                }
                Err(e) => waiting.reply(Err(format!("{e:#}"))),
            }
            return;
        }
        side.nudge.notify_one();
        let now = Instant::now();
        self.pending = Some(Pending {
            target,
            not_before: now,
            until: now + HOLD,
            retry: false,
        });
        waiting.reply(Ok(()));
    }

    fn send(&mut self, target: String, retry: bool) -> anyhow::Result<()> {
        let mut rest: VecDeque<String> = remote::app_links(&target).into();
        let link = rest.pop_front().context("no way to open it")?;
        send_link(self.remote.as_ref(), &link)?;
        // An app asked for before this one: its order is not kept waiting.
        if let Some(mut before) = self.sent.take() {
            before.answer(Ok(()));
        }
        self.sent = Some(Sent {
            target,
            link,
            at: Instant::now(),
            rest,
            retry,
            waiting: None,
        });
        Ok(())
    }

    /// When an order waiting for the TV's word is answered anyway: no
    /// refusal for [`SETTLE`], and never past the hub's patience.
    pub(crate) fn due(&self) -> Option<Instant> {
        let sent = self.sent.as_ref()?;
        let waiting = sent.waiting.as_ref()?;
        let deadline = Instant::from_std(waiting.deadline)
            .checked_sub(BEFORE_DEADLINE)
            .unwrap_or_else(Instant::now);
        Some((sent.at + SETTLE).min(deadline))
    }

    /// The TV did not refuse in time: the app opened.
    pub(crate) fn settle(&mut self) {
        if self.due().is_some_and(|due| Instant::now() >= due)
            && let Some(sent) = &mut self.sent
        {
            sent.answer(Ok(()));
        }
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

    /// The TV refused an order. The app link just sent: the next way to open
    /// the same app goes at once (a Google TV may refuse the store link and
    /// take the web one). Every way refused: a TV still starting gets the app
    /// once more a little later; otherwise whoever asked is told.
    fn refused(&mut self, what: &str) {
        // Only the very app link sent (not a key, not the configuration).
        let Some(sent) = self
            .sent
            .as_mut()
            .filter(|s| what.contains(s.link.as_str()))
        else {
            tracing::warn!(refused = %what, "android tv remote: the TV refused an order");
            return;
        };
        while let Some(next) = sent.rest.pop_front() {
            if send_link(self.remote.as_ref(), &next).is_ok() {
                tracing::info!(refused = %what, next = %next, "android tv remote: link refused, trying the next one");
                sent.link = next;
                sent.at = Instant::now();
                return;
            }
        }
        let starting = self.ready_at.is_some_and(|at| at.elapsed() < STARTING);
        let again = starting && !sent.retry && sent.at.elapsed() < Duration::from_secs(3);
        tracing::warn!(refused = %what, retry = again, "android tv remote: the TV refused an order");
        let Some(mut sent) = self.sent.take() else {
            return;
        };
        if again {
            sent.answer(Ok(()));
            let now = Instant::now();
            self.pending = Some(Pending {
                target: sent.target,
                not_before: now + RETRY_LAUNCH,
                until: now + HOLD,
                retry: true,
            });
        } else {
            let app = sent.target.clone();
            sent.answer(Err(moli_i18n::tr!(
                "pilotes.philips.appli_refusee",
                app = app
            )));
        }
    }

    /// The app on screen is the one being opened: its order is answered now.
    fn opened(&mut self, app: &str) {
        if let Some(sent) = &mut self.sent
            && (sent.target == app || remote::package_of(&sent.target) == Some(app))
        {
            sent.answer(Ok(()));
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
                    remote::Event::App(app) => {
                        self.opened(&app);
                        ctx.set_state(id, "app", Value::Text(app.into()));
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use remote::Order;
    use tokio::sync::oneshot;

    /// A remote that is up, its orders readable by the test.
    fn paired(orders: mpsc::Sender<Order>) -> Android {
        Android {
            remote: Some(Side {
                orders,
                tls: PinnedTls::new(None).unwrap(),
                up: true,
                pinned: true,
                nudge: Arc::new(Notify::new()),
            }),
            cast: None,
            playing: None,
            pending: None,
            sent: None,
            ready_at: None,
            tv_was_online: true,
            nudged: None,
        }
    }

    fn waiting() -> (Waiting, oneshot::Receiver<Result<(), String>>) {
        let (tx, rx) = oneshot::channel();
        let waiting = Waiting {
            answer: Box::new(move |result| {
                let _ = tx.send(result);
            }),
            deadline: std::time::Instant::now() + Duration::from_secs(5),
        };
        (waiting, rx)
    }

    fn link(order: Option<Order>) -> String {
        match order {
            Some(Order::Launch(link)) => link,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refused_link_tries_the_next_and_a_refusal_of_all_is_said() {
        let (tx, mut orders) = mpsc::channel(8);
        let mut android = paired(tx);
        let (answer, mut outcome) = waiting();
        android.launch("YouTube", answer);
        assert_eq!(link(orders.recv().await), "https://www.youtube.com");
        android.refused("app link https://www.youtube.com");
        let store = link(orders.recv().await);
        assert_eq!(store, "market://launch?id=com.google.android.youtube.tv");
        assert!(outcome.try_recv().is_err(), "still waiting for the TV");
        android.refused(&format!("app link {store}"));
        assert_eq!(
            outcome.await.unwrap(),
            Err("la télé a refusé d'ouvrir YouTube".into())
        );
    }

    #[tokio::test]
    async fn the_app_on_screen_answers_at_once() {
        let (tx, mut orders) = mpsc::channel(8);
        let mut android = paired(tx);
        let (answer, outcome) = waiting();
        android.launch("netflix", answer);
        assert_eq!(link(orders.recv().await), "https://www.netflix.com/title");
        android.opened("com.netflix.ninja");
        assert_eq!(outcome.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn no_refusal_means_it_opened() {
        let (tx, mut orders) = mpsc::channel(8);
        let mut android = paired(tx);
        let (answer, mut outcome) = waiting();
        android.launch("com.limelight", answer);
        assert_eq!(
            link(orders.recv().await),
            "market://launch?id=com.limelight"
        );
        android.settle();
        assert!(outcome.try_recv().is_err(), "too soon");
        tokio::time::advance(SETTLE).await;
        android.settle();
        assert_eq!(outcome.await.unwrap(), Ok(()));
        assert!(android.due().is_none());
    }

    #[tokio::test]
    async fn another_order_refused_is_not_this_app() {
        let (tx, mut orders) = mpsc::channel(8);
        let mut android = paired(tx);
        let (answer, mut outcome) = waiting();
        android.launch("YouTube", answer);
        let _ = orders.recv().await;
        android.refused("key 3");
        assert!(orders.try_recv().is_err(), "no next link for a key");
        assert!(outcome.try_recv().is_err());
    }
}

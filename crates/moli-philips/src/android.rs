//! The TV's Android side, beside JointSpace: the app on screen and launching
//! apps (Android TV Remote, with the pairing Home Assistant made: secrets
//! `atv_cert` / `atv_key`, PEM), what plays (Google Cast, nothing to pair;
//! and ADB when the TV's network debugging is on and `adb = true`: what any
//! app plays, not only what was cast). Both certificates are pinned on first
//! contact, like the TV's own; Moli's ADB key is made on the first start
//! (secret `adb_key`) and allowed once on the TV's screen.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_androidtv::{adb, cast, remote};
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
/// a fraction of a second; it does not always say which app came up, an app
/// woken from the screensaver can take seconds to show).
const SETTLE: Duration = Duration::from_secs(1);
/// The hub's patience ends at the order's deadline: answered a little before.
const BEFORE_DEADLINE: Duration = Duration::from_millis(300);
pub(crate) const ADB_KEY: &str = "adb_key";
/// What plays, asked this often through ADB.
const ADB_POLL: Duration = Duration::from_secs(3);
/// The TV asked whether to allow Moli and no one said yes: not asked again
/// before this (a question every minute on the screen would be a nuisance).
const ASK_AGAIN: Duration = Duration::from_secs(600);
/// A key held from the dashboard is let go after this, whatever happens
/// (the phone that held it gone, its « let go » lost).
const HOLD_MAX: Duration = Duration::from_secs(20);
/// How far a position may be from the one foreseen before it is told
/// again (a seek): a playing position moves on by itself on the dashboard.
const DRIFT: f64 = 3.0;
/// The keys that can be held (a video seeks faster, a list scrolls), and
/// the word that lets go.
pub(crate) const HOLD_KEYS: [&str; 7] = [
    "CursorLeft",
    "CursorRight",
    "CursorUp",
    "CursorDown",
    "FastForward",
    "Rewind",
    "release",
];

/// The Android key of a JointSpace key: the same press through the remote's
/// session, open as long as the TV is, where JointSpace goes away for half a
/// minute at times (an app starting…) while the TV plays on.
fn android_key(name: &str) -> Option<u32> {
    Some(match name {
        "CursorUp" => 19,
        "CursorDown" => 20,
        "CursorLeft" => 21,
        "CursorRight" => 22,
        "Confirm" => 23,
        "Back" => 4,
        "Home" => 3,
        "Options" => 82,
        "Info" => 165,
        "PlayPause" => 85,
        "Play" => 126,
        "Pause" => 127,
        "Stop" => 86,
        "Next" => 87,
        "Previous" => 88,
        "FastForward" => 90,
        "Rewind" => 89,
        "VolumeUp" => 24,
        "VolumeDown" => 25,
        "Mute" => 164,
        "ChannelStepUp" => 166,
        "ChannelStepDown" => 167,
        "Source" => 178,
        "WatchTV" => 170,
        _ => return None,
    })
}

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
    Adb(AdbNews),
}

/// News from the ADB side.
pub(crate) enum AdbNews {
    /// The TV asks on its screen whether to allow Moli.
    Asking,
    Ready,
    /// What plays (the app's package and what it shows), or nothing.
    Playing(Option<(String, cast::Playing)>),
    Lost,
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
    /// What Cast says plays.
    playing: Option<cast::Playing>,
    /// ADB, when on: whether it is connected, and its nudge.
    adb: Option<(bool, Arc<Notify>)>,
    /// What ADB says plays.
    adb_playing: Option<(String, cast::Playing)>,
    /// What was last published, and when.
    shown: Option<(cast::Playing, Instant)>,
    /// The key held down, since when.
    held: Option<(u32, Instant)>,
    pending: Option<Pending>,
    /// The last app sent.
    sent: Option<Sent>,
    /// When the remote last came up.
    ready_at: Option<Instant>,
    /// On or off, as the remote last said (nothing while it is down).
    power: Option<bool>,
    /// The TV answered JointSpace at the last poll; when the connections were
    /// last nudged (once on the TV's return, then once a minute at most: a
    /// remote that keeps failing must keep its slower pace).
    tv_was_online: bool,
    nudged: Option<Instant>,
}

impl Android {
    /// Starts both connections (the remote only when paired).
    pub(crate) fn start(
        ctx: &DriverCtx,
        host: &str,
        adb_key: Option<adb::Key>,
    ) -> (Self, mpsc::Receiver<Up>) {
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
        let adb = adb_key.map(|key| {
            let nudge = Arc::new(Notify::new());
            keep_adb(host.to_owned(), key, up.clone(), nudge.clone());
            (false, nudge)
        });
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
                adb,
                adb_playing: None,
                shown: None,
                held: None,
                pending: None,
                sent: None,
                ready_at: None,
                power: None,
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

    /// The remote is up and says the TV is on: JointSpace not answering is
    /// JointSpace's own outage, not the TV gone.
    pub(crate) fn says_on(&self) -> bool {
        self.remote.as_ref().is_some_and(|s| s.up) && self.power == Some(true)
    }

    /// A remote key through the Android remote while it is up; `None` when
    /// it is not (or the key has no Android twin): JointSpace takes it then.
    pub(crate) fn press(&self, name: &str) -> Option<anyhow::Result<()>> {
        let code = android_key(name)?;
        let side = self.remote.as_ref().filter(|s| s.up)?;
        Some(side.orders.try_send(remote::Order::Key(code)).map_err(|_| {
            anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.telecommande_ne_suit_pas"))
        }))
    }

    /// Holds a key down (`release` lets it go): through the Android remote,
    /// which must be up. A key already held is let go first.
    pub(crate) fn hold(&mut self, name: &str) -> anyhow::Result<()> {
        let side = self
            .remote
            .as_ref()
            .filter(|s| s.up)
            .with_context(|| moli_i18n::tr!("pilotes.philips.pas_de_telecommande"))?;
        let code = match name {
            "release" => None,
            key if HOLD_KEYS.contains(&key) => android_key(key),
            other => bail!(moli_i18n::tr!(
                "pilotes.philips.touche_inconnue",
                key = format!("{other:?}")
            )),
        };
        let mut orders = Vec::new();
        if let Some((held, _)) = self.held.take() {
            orders.push(remote::Order::Hold(held, false));
        }
        if let Some(code) = code {
            orders.push(remote::Order::Hold(code, true));
        }
        for order in orders {
            side.orders.try_send(order).map_err(|_| {
                anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.telecommande_ne_suit_pas"))
            })?;
        }
        self.held = code.map(|code| (code, Instant::now()));
        Ok(())
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
    /// Also when a held key is let go anyway ([`HOLD_MAX`]).
    pub(crate) fn due(&self) -> Option<Instant> {
        let release = self.held.map(|(_, at)| at + HOLD_MAX);
        match (release, self.answer_due()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn answer_due(&self) -> Option<Instant> {
        let sent = self.sent.as_ref()?;
        let waiting = sent.waiting.as_ref()?;
        let deadline = Instant::from_std(waiting.deadline)
            .checked_sub(BEFORE_DEADLINE)
            .unwrap_or_else(Instant::now);
        Some((sent.at + SETTLE).min(deadline))
    }

    /// The TV did not refuse in time: the app opened. A key held too long
    /// is let go.
    pub(crate) fn settle(&mut self) {
        let now = Instant::now();
        if self.held.is_some_and(|(_, at)| now >= at + HOLD_MAX) {
            tracing::debug!("android tv remote: held key let go (held too long)");
            if self.hold("release").is_err() {
                self.held = None;
            }
        }
        if self.answer_due().is_some_and(|due| now >= due)
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
            if let Some((false, nudge)) = &self.adb {
                nudge.notify_one();
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
    /// `text` into the text field open on the TV (a search box); a final
    /// line break presses Enter, which starts the search.
    pub(crate) fn write(&self, text: &str) -> anyhow::Result<()> {
        let side = self
            .remote
            .as_ref()
            .filter(|s| s.up)
            .with_context(|| moli_i18n::tr!("pilotes.philips.pas_de_telecommande"))?;
        let (line, enter) = match text.strip_suffix('\n') {
            Some(line) => (line, true),
            None => (text, false),
        };
        let mut orders = Vec::new();
        if !line.is_empty() {
            orders.push(remote::Order::Text(line.to_owned()));
        }
        if enter {
            orders.push(remote::Order::Key(remote::ENTER));
        }
        for order in orders {
            side.orders.try_send(order).map_err(|_| {
                anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.telecommande_ne_suit_pas"))
            })?;
        }
        Ok(())
    }

    /// Play, pause or stop: through Cast when Cast sees what plays,
    /// otherwise as a media key through the remote (what ADB sees playing:
    /// an app opened on the TV, not cast).
    pub(crate) fn control(&self, word: &str) -> anyhow::Result<()> {
        let (order, key) = match word {
            "play" => (cast::Order::Play, 126),
            "pause" => (cast::Order::Pause, 127),
            "stop" => (cast::Order::Stop, 86),
            other => bail!(moli_i18n::tr!(
                "pilotes.philips.play_pause_stop",
                word = format!("{other:?}")
            )),
        };
        let cast_plays = self.playing.as_ref().is_some_and(|p| p.state.is_some());
        if let Some(side) = self.cast.as_ref().filter(|s| s.up && cast_plays) {
            return side
                .orders
                .try_send(order)
                .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.cast_ne_suit_pas")));
        }
        let adb_plays = self.adb_playing.is_some();
        match self.remote.as_ref().filter(|s| s.up && adb_plays) {
            Some(side) => side.orders.try_send(remote::Order::Key(key)).map_err(|_| {
                anyhow::anyhow!(moli_i18n::tr!("pilotes.philips.telecommande_ne_suit_pas"))
            }),
            None => bail!(moli_i18n::tr!("pilotes.philips.rien_ne_joue")),
        }
    }

    /// What to show: what ADB sees when it sees a title (with Cast's
    /// picture and length when Cast sees the same), otherwise Cast's,
    /// otherwise what ADB sees without a title. ADB's leaves the app's
    /// name out: the app on screen says it.
    fn now_playing(&self) -> cast::Playing {
        let from_cast = self.playing.clone().unwrap_or_default();
        match &self.adb_playing {
            Some((_, p)) if p.title.is_some() => {
                let mut p = p.clone();
                if from_cast.title == p.title {
                    p.image.clone_from(&from_cast.image);
                    p.duration = from_cast.duration;
                }
                p
            }
            _ if from_cast.state.is_some() => from_cast,
            Some((_, p)) => p.clone(),
            None => cast::Playing::default(),
        }
    }

    /// Publishes what plays when it changed; a position only when it is not
    /// where it was foreseen (the dashboard moves it on while playing).
    fn publish(&mut self, ctx: &DriverCtx, id: &DeviceId) {
        let now = self.now_playing();
        if self
            .shown
            .as_ref()
            .is_some_and(|(shown, at)| same_but_on(shown, at.elapsed(), &now))
        {
            return;
        }
        for (key, value) in media_values(&now) {
            ctx.set_state(id, key, value);
        }
        self.shown = Some((now, Instant::now()));
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
                    remote::Event::Lost => {
                        side.up = false;
                        self.power = None;
                        self.held = None;
                    }
                    remote::Event::Refused(what) => self.refused(&what),
                    remote::Event::App(app) => {
                        self.opened(&app);
                        ctx.set_state(id, "app", Value::Text(app.into()));
                    }
                    remote::Event::Power(on) => {
                        self.power = Some(on);
                        if !on {
                            ctx.set_state(id, "app", Value::Null);
                        }
                    }
                    // JointSpace says it already.
                    remote::Event::Volume { .. } => {}
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
                self.playing = Some(playing);
                self.publish(ctx, id);
            }
            Up::Adb(news) => {
                let Some((up, _)) = self.adb.as_mut() else {
                    return;
                };
                match news {
                    AdbNews::Asking => {
                        tracing::info!(instance = %ctx.instance(), "adb: the TV asks whether to allow Moli");
                        // This branch has no driver warnings yet: the waiting status says it.
                        ctx.wait_for(moli_i18n::tr!("pilotes.philips.adb_autoriser"));
                    }
                    AdbNews::Ready => {
                        if !*up {
                            tracing::info!(instance = %ctx.instance(), "adb: connected");
                        }
                        *up = true;
                        ctx.ready();
                    }
                    AdbNews::Playing(playing) => {
                        self.adb_playing = playing;
                        self.publish(ctx, id);
                    }
                    AdbNews::Lost => {
                        *up = false;
                        self.adb_playing = None;
                        self.publish(ctx, id);
                    }
                }
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

/// `now` is `shown` `elapsed` later: the same thing, its position where it
/// was foreseen.
fn same_but_on(shown: &cast::Playing, elapsed: Duration, now: &cast::Playing) -> bool {
    let without = |p: &cast::Playing| cast::Playing {
        position: None,
        ..p.clone()
    };
    if without(shown) != without(now) {
        return false;
    }
    let playing = shown.state.as_deref() == Some("playing");
    let foreseen = shown
        .position
        .map(|p| p + if playing { elapsed.as_secs_f64() } else { 0.0 });
    match (foreseen, now.position) {
        (Some(a), Some(b)) => (a - b).abs() < DRIFT,
        (a, b) => a.is_none() && b.is_none(),
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

/// Moli's ADB key: the stored one, or a new one stored now (made off the
/// async threads: a few seconds). `None`, said in the logs or on the
/// driver, when it cannot be kept.
pub(crate) async fn adb_key(ctx: &DriverCtx) -> Option<adb::Key> {
    if let Some(stored) = ctx.secret(ADB_KEY) {
        return adb::Key::from_secret(&stored)
            .inspect_err(|e| tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "adb key unreadable"))
            .ok();
    }
    if !ctx.can_store_secrets() {
        tracing::warn!(instance = %ctx.instance(), "{}", moli_i18n::tr!("pilotes.philips.adb_sans_coffre"));
        return None;
    }
    let key = match tokio::task::spawn_blocking(adb::Key::generate).await {
        Ok(Ok(key)) => key,
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "adb key not made");
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "adb key not made");
            return None;
        }
    };
    if let Err(e) = ctx.store_secret(ADB_KEY, &key.to_secret()).await {
        tracing::warn!(error = %e, "adb key not stored");
        return None;
    }
    Some(key)
}

/// ADB's connection, again and again while the driver lives: what plays,
/// asked every [`ADB_POLL`].
fn keep_adb(host: String, key: adb::Key, up: mpsc::Sender<Up>, nudge: Arc<Notify>) {
    tokio::spawn(async move {
        let mut pause = RETRY_MIN;
        loop {
            let started = Instant::now();
            let (mut asked, mut ready) = (false, false);
            let result = adb_session(&host, &key, &up, &mut asked, &mut ready).await;
            if up.is_closed() || up.send(Up::Adb(AdbNews::Lost)).await.is_err() {
                break;
            }
            if let Err(e) = result {
                tracing::debug!(error = %format!("{e:#}"), "adb: reconnecting");
            }
            pause = if asked && !ready {
                ASK_AGAIN
            } else {
                next_pause(pause, started)
            };
            wait(&mut pause, &nudge).await;
        }
    });
}

async fn adb_session(
    host: &str,
    key: &adb::Key,
    up: &mpsc::Sender<Up>,
    asked: &mut bool,
    ready: &mut bool,
) -> anyhow::Result<()> {
    let (mut tv, _) = adb::Adb::connect(host, key, "moli-os", || {
        *asked = true;
        let _ = up.try_send(Up::Adb(AdbNews::Asking));
    })
    .await?;
    *ready = true;
    up.send(Up::Adb(AdbNews::Ready)).await?;
    loop {
        let out = tv.shell(adb::MEDIA).await?;
        up.send(Up::Adb(AdbNews::Playing(adb::playing(&out))))
            .await?;
        tokio::time::sleep(ADB_POLL).await;
    }
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
            adb: None,
            adb_playing: None,
            shown: None,
            held: None,
            pending: None,
            sent: None,
            ready_at: None,
            power: None,
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

    #[test]
    fn every_remote_key_has_its_android_key() {
        for key in crate::KEYS {
            assert!(android_key(key).is_some(), "{key}");
        }
        assert_eq!(android_key("CursorDown"), Some(20));
        assert_eq!(android_key("Confirm"), Some(23));
        assert_eq!(android_key("Standby"), None);
    }

    #[tokio::test]
    async fn keys_go_through_the_android_remote_while_it_is_up() {
        let (tx, mut orders) = mpsc::channel(8);
        let mut android = paired(tx);
        assert!(android.press("Back").is_some_and(|r| r.is_ok()));
        assert_eq!(orders.recv().await, Some(Order::Key(4)));
        // Down: JointSpace takes the key.
        android.remote.as_mut().unwrap().up = false;
        assert!(android.press("Back").is_none());
        assert!(orders.try_recv().is_err());
    }

    #[test]
    fn the_android_remote_says_whether_the_tv_is_on() {
        let (tx, _orders) = mpsc::channel(8);
        let mut android = paired(tx);
        assert!(!android.says_on(), "nothing said yet");
        android.power = Some(true);
        assert!(android.says_on());
        android.remote.as_mut().unwrap().up = false;
        assert!(!android.says_on(), "a remote that is down says nothing");
        android.remote.as_mut().unwrap().up = true;
        android.power = Some(false);
        assert!(!android.says_on());
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_key_is_let_go_by_the_dashboard_or_in_the_end() {
        let (tx, mut orders) = mpsc::channel(8);
        let mut android = paired(tx);
        android.hold("CursorRight").unwrap();
        assert_eq!(orders.recv().await, Some(Order::Hold(22, true)));
        // Another key: the first is let go before.
        android.hold("CursorLeft").unwrap();
        assert_eq!(orders.recv().await, Some(Order::Hold(22, false)));
        assert_eq!(orders.recv().await, Some(Order::Hold(21, true)));
        android.hold("release").unwrap();
        assert_eq!(orders.recv().await, Some(Order::Hold(21, false)));
        assert!(android.due().is_none());
        assert!(android.hold("Home").is_err(), "not a key to hold");
        // Never let go by the dashboard: let go anyway.
        android.hold("FastForward").unwrap();
        assert_eq!(orders.recv().await, Some(Order::Hold(90, true)));
        assert_eq!(android.due(), Some(Instant::now() + HOLD_MAX));
        tokio::time::advance(HOLD_MAX).await;
        android.settle();
        assert_eq!(orders.recv().await, Some(Order::Hold(90, false)));
        assert!(android.due().is_none());
    }

    fn plays(title: &str, state: &str, position: f64) -> cast::Playing {
        cast::Playing {
            title: Some(title.into()),
            state: Some(state.into()),
            position: Some(position),
            ..cast::Playing::default()
        }
    }

    #[test]
    fn a_position_is_told_again_only_when_it_jumps() {
        let shown = plays("Dune", "playing", 100.0);
        let ten = Duration::from_secs(10);
        assert!(same_but_on(&shown, ten, &plays("Dune", "playing", 111.0)));
        assert!(
            !same_but_on(&shown, ten, &plays("Dune", "playing", 300.0)),
            "a seek"
        );
        assert!(!same_but_on(&shown, ten, &plays("Dune", "paused", 110.0)));
        assert!(!same_but_on(
            &shown,
            ten,
            &plays("Arrival", "playing", 110.0)
        ));
        let paused = plays("Dune", "paused", 100.0);
        assert!(same_but_on(&paused, ten, &plays("Dune", "paused", 100.5)));
    }

    #[test]
    fn what_adb_sees_comes_first_with_casts_picture() {
        let (tx, _orders) = mpsc::channel(8);
        let mut android = paired(tx);
        assert_eq!(android.now_playing(), cast::Playing::default());
        android.playing = Some(cast::Playing {
            app: Some("YouTube".into()),
            image: Some("https://i.ytimg.com/vi/x/hq.jpg".into()),
            duration: Some(600.0),
            ..plays("Dune", "playing", 10.0)
        });
        assert_eq!(
            android.now_playing().app.as_deref(),
            Some("YouTube"),
            "Cast alone"
        );
        android.adb_playing = Some((
            "com.google.android.youtube.tv".into(),
            plays("Dune", "playing", 12.0),
        ));
        let now = android.now_playing();
        assert_eq!(now.app, None);
        assert_eq!(now.position, Some(12.0));
        assert_eq!(now.duration, Some(600.0));
        assert!(now.image.is_some());
        android.adb_playing = Some(("com.netflix.ninja".into(), plays("Arrival", "paused", 5.0)));
        let now = android.now_playing();
        assert_eq!((now.title.as_deref(), now.image), (Some("Arrival"), None));
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

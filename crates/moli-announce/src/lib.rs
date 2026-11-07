//! Spoken announcements, the same for every speaker that says them: the
//! words checked, a short queue said one at a time (dropped when stale), the
//! speech made by a local Piper server (Wyoming), the level an announcement
//! is said at (`announce_volume`), `quiet`, a silence that ends by itself
//! after [`QUIET_FOR`], and `speaking`, the words being said.
//!
//! How the sound reaches the speaker is the driver's own: [`Speaker::play`].
//! A driver keeps one [`Announcements`], adds its [`points`](Announcements::points)
//! to its device, hands it every command and every poll tick, and shows
//! [`speaking_changed`](Announcements::speaking_changed) as it comes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use moli_core::{Access, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::media::Image;
use moli_runtime::{BoxFuture, CommandRequest, DriverCtx, MediaPublisher};
use tokio::sync::watch;

/// An announcement is a sentence or two, not a speech.
pub const MAX_TEXT: usize = 500;
/// Announcements waiting at most: past it, a new one is refused (a burst
/// of alerts must not keep the speaker talking for minutes).
pub const MAX_WAITING: usize = 3;
/// An announcement that could not start within this delay is dropped: it
/// is no longer news.
pub const STALE_AFTER: Duration = Duration::from_secs(30);
/// A silence ends by itself after this long.
pub const QUIET_FOR: Duration = Duration::from_secs(4 * 3600);

/// A speaker that can say a sound.
pub trait Speaker: std::fmt::Debug + Send + Sync + 'static {
    /// Plays `wav` (the speech server's WAV) at `level`, 0–100, the speaker's
    /// own volume coming back afterwards, and resolves once it has been
    /// heard. [`Started::now`] when the speaker starts saying it.
    fn play<'a>(
        &'a self,
        wav: Vec<u8>,
        level: u8,
        started: &'a Started,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// Tells the house a speaker has started saying an announcement: the
/// `speaking` point holds its words until it is over. A voice satellite that
/// answers through this speaker waits for the end before listening again.
#[derive(Debug)]
pub struct Started {
    speaking: watch::Sender<Option<String>>,
    text: String,
}

impl Started {
    /// The speaker is saying it now (said again, nothing changes).
    pub fn now(&self) {
        self.speaking.send_if_modified(|said| {
            let changed = said.as_deref() != Some(self.text.as_str());
            if changed {
                *said = Some(self.text.clone());
            }
            changed
        });
    }
}

/// Where the speech is made.
#[derive(Clone, Debug)]
pub struct Speech {
    /// The Wyoming speech server (Piper), `host:port`.
    pub tts: String,
    /// Piper voice (default: the server's).
    pub voice: Option<String>,
}

/// The speech server most houses run, next to Moli.
#[must_use]
pub fn default_tts() -> String {
    "127.0.0.1:10200".into()
}

impl Speech {
    /// `text` as a WAV.
    ///
    /// # Errors
    /// `tts` is not `host:port`, or the server did not answer in time.
    pub async fn synthesize(&self, text: &str) -> anyhow::Result<Vec<u8>> {
        let (host, port) = self
            .tts
            .rsplit_once(':')
            .and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?)))
            .context("tts must be host:port")?;
        moli_net::wyoming::synthesize(
            host,
            port,
            self.voice.as_deref(),
            text,
            Duration::from_secs(30),
        )
        .await
    }
}

/// Publishes `wav` for a few minutes and gives its address as a speaker
/// reaches Moli (`media_base`, e.g. `http://192.168.0.62:8790`): for a
/// speaker that fetches the sound itself.
///
/// # Errors
/// No random name could be drawn for it.
pub fn publish_sound(
    publisher: &MediaPublisher,
    media_base: &str,
    wav: Vec<u8>,
) -> anyhow::Result<String> {
    let name = publisher
        .publish(
            "wav",
            Image {
                content_type: "audio/wav".into(),
                bytes: wav,
            },
        )
        .context("no random name for the sound")?;
    Ok(format!(
        "{}/api/media/{name}",
        media_base.trim_end_matches('/')
    ))
}

#[derive(Debug)]
pub struct Announcer {
    speaker: Box<dyn Speaker>,
    speech: Speech,
    publisher: MediaPublisher,
    /// One announcement at a time, in order.
    turn: tokio::sync::Mutex<()>,
    /// Announcements waiting or playing.
    waiting: AtomicUsize,
    /// The level an announcement is said at (the `announce_volume` point).
    level: AtomicU8,
    /// What the speaker is saying now, `None` once said (the `speaking` point).
    speaking: watch::Sender<Option<String>>,
}

impl Announcer {
    /// `level`: where `announce_volume` starts (the configured volume).
    pub fn new(
        speaker: impl Speaker,
        speech: Speech,
        level: u8,
        publisher: MediaPublisher,
    ) -> Arc<Self> {
        Arc::new(Self {
            speaker: Box::new(speaker),
            speech,
            publisher,
            turn: tokio::sync::Mutex::new(()),
            waiting: AtomicUsize::new(0),
            level: AtomicU8::new(level.min(100)),
            speaking: watch::Sender::new(None),
        })
    }

    /// The level the next announcements are said at, 0–100.
    pub fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    pub fn set_level(&self, level: u8) {
        self.level.store(level.min(100), Ordering::Relaxed);
    }

    /// Queues `text` and answers at once: the announcement itself takes
    /// seconds (speech, then the sound). Refused when too many are waiting;
    /// dropped if it could not start in time; the house is told of a failure.
    ///
    /// # Errors
    /// The queue is full.
    pub fn enqueue(self: &Arc<Self>, text: String) -> Result<(), String> {
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return Err(moli_i18n::tr!(
                "pilotes.annonces.file_pleine",
                count = MAX_WAITING
            ));
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let since = tokio::time::Instant::now();
            let outcome = {
                let _turn = this.turn.lock().await;
                if since.elapsed() > STALE_AFTER {
                    Err(anyhow::anyhow!(moli_i18n::tr!(
                        "pilotes.annonces.trop_tard",
                        seconds = STALE_AFTER.as_secs()
                    )))
                } else {
                    this.announce(&text).await
                }
            };
            this.waiting.fetch_sub(1, Ordering::SeqCst);
            if let Err(e) = outcome {
                tracing::warn!(error = %format!("{e:#}"), "announcement not played");
                this.publisher.tell(
                    &moli_i18n::tr!("pilotes.annonces.annonce_vocale"),
                    moli_i18n::tr!(
                        "pilotes.annonces.non_annoncee",
                        text = short(&text),
                        error = format!("{e:#}")
                    ),
                );
            }
        });
        Ok(())
    }

    /// Says `text` on this speaker (the caller holds the turn).
    async fn announce(&self, text: &str) -> anyhow::Result<()> {
        let wav = self.speech.synthesize(text).await?;
        let started = Started {
            speaking: self.speaking.clone(),
            text: text.to_owned(),
        };
        let played = self.speaker.play(wav, self.level(), &started).await;
        self.speaking.send_if_modified(|said| said.take().is_some());
        played
    }
}

/// A driver's announcements: its [`Announcer`] when they are configured,
/// and the silence.
#[derive(Debug, Default)]
pub struct Announcements {
    announcer: Option<Arc<Announcer>>,
    /// While set and ahead, nothing is said (ms since the epoch).
    quiet_until: Option<u64>,
    /// What the speaker is saying, as the announcer tells it.
    speaking: Option<watch::Receiver<Option<String>>>,
}

impl Announcements {
    #[must_use]
    pub fn new(announcer: Option<Arc<Announcer>>) -> Self {
        let speaking = announcer.as_ref().map(|a| a.speaking.subscribe());
        Self {
            announcer,
            quiet_until: None,
            speaking,
        }
    }

    /// The points a speaker that speaks has: none without announcements.
    #[must_use]
    pub fn points(&self) -> Vec<PointSpec> {
        if self.announcer.is_none() {
            return Vec::new();
        }
        vec![
            point(
                "quiet",
                &moli_i18n::tr!("pilotes.annonces.annonces_coupees"),
                Kind::Binary,
                Access {
                    read: true,
                    write: true,
                },
                None,
            ),
            point(
                "quiet_until",
                &moli_i18n::tr!("pilotes.annonces.annonces_coupees_jusqua"),
                Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                Access {
                    read: true,
                    write: false,
                },
                None,
            ),
            // A setting of the speaker, not its volume: « louder » is not
            // about it (hence `Other`).
            point(
                "announce_volume",
                &moli_i18n::tr!("pilotes.annonces.volume_annonce"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                Access {
                    read: true,
                    write: true,
                },
                Some(Unit::Percent),
            ),
            // Words to say, not a state: nothing to read back.
            point(
                "announce",
                &moli_i18n::tr!("pilotes.annonces.annonce"),
                Kind::Text,
                Access {
                    read: false,
                    write: true,
                },
                None,
            ),
            // What it is saying now, empty once said.
            point(
                "speaking",
                &moli_i18n::tr!("pilotes.annonces.annonce_en_cours"),
                Kind::Text,
                Access {
                    read: true,
                    write: false,
                },
                None,
            ),
        ]
    }

    /// Once the device is in the hub: a level and a silence set before
    /// survive a restart (the hub keeps the last values).
    pub fn start(&mut self, ctx: &DriverCtx, id: &DeviceId) {
        if let Some(announcer) = &self.announcer {
            if let Some(level) = ctx
                .current(id, "announce_volume")
                .as_ref()
                .and_then(level_of)
            {
                announcer.set_level(level);
            }
            ctx.set_state(id, "announce_volume", Value::Int(announcer.level().into()));
            // Nothing is being said at start, whatever was before a restart.
            ctx.set_state(id, "speaking", Value::Null);
        }
        self.quiet_until = match (ctx.current(id, "quiet"), ctx.current(id, "quiet_until")) {
            (Some(Value::Bool(true)), Some(Value::Int(until))) => u64::try_from(until).ok(),
            _ => None,
        }
        .filter(|u| *u > moli_core::now_ms());
        set_quiet(ctx, id, self.quiet_until);
    }

    /// The answer to `command` when it is about announcements, `None` when
    /// it is the driver's.
    pub fn command(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        command: &CommandRequest,
    ) -> Option<Result<(), String>> {
        match &*command.key {
            "announce" => Some(
                if self.quiet_until.is_some_and(|u| u > moli_core::now_ms()) {
                    // Silenced: nothing said, nobody bothered with an error.
                    tracing::info!("announcement not said: announcements are silenced");
                    Ok(())
                } else {
                    speak(self.announcer.as_ref(), &command.value)
                },
            ),
            "announce_volume" => {
                let result = announce_volume(self.announcer.as_ref(), &command.value);
                if let Ok(level) = result {
                    ctx.set_state(id, "announce_volume", Value::Int(level.into()));
                }
                Some(result.map(|_| ()))
            }
            "quiet" => {
                let on = matches!(command.value, Value::Bool(true));
                self.quiet_until = on.then(|| {
                    moli_core::now_ms() + u64::try_from(QUIET_FOR.as_millis()).unwrap_or(u64::MAX)
                });
                set_quiet(ctx, id, self.quiet_until);
                Some(Ok(()))
            }
            _ => None,
        }
    }

    /// Resolves when the speaker starts or stops saying an announcement
    /// (never without announcements), with its words while it says them:
    /// hand them to [`show_speaking`](Self::show_speaking). For the driver's
    /// `select!` next to its commands; cancel-safe.
    pub async fn speaking_changed(&mut self) -> Option<String> {
        let Some(speaking) = &mut self.speaking else {
            return std::future::pending().await;
        };
        if speaking.changed().await.is_err() {
            // The announcer is gone: nothing more will be said.
            return std::future::pending().await;
        }
        speaking.borrow_and_update().clone()
    }

    /// The `speaking` point: the words being said, empty once said.
    pub fn show_speaking(ctx: &DriverCtx, id: &DeviceId, said: Option<String>) {
        ctx.set_state(
            id,
            "speaking",
            said.map_or(Value::Null, |s| Value::Text(s.into())),
        );
    }

    /// On each poll: a silence forgotten on ends, and the house speaks again.
    pub fn tick(&mut self, ctx: &DriverCtx, id: &DeviceId) {
        if self.quiet_until.is_some_and(|u| u <= moli_core::now_ms()) {
            self.quiet_until = None;
            set_quiet(ctx, id, None);
        }
    }
}

fn point(key: &str, label: &str, kind: Kind, access: Access, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access,
        unit,
        semantic: Semantic::Other,
    }
}

/// The silence's two points (until: ms since epoch, 0 when not silenced).
fn set_quiet(ctx: &DriverCtx, id: &DeviceId, until: Option<u64>) {
    ctx.set_state(id, "quiet", Value::Bool(until.is_some()));
    let until = i64::try_from(until.unwrap_or(0)).unwrap_or(i64::MAX);
    ctx.set_state(id, "quiet_until", Value::Int(until));
}

/// A level, 0–100, from what was written (a slider sends a number).
#[must_use]
pub fn level_of(value: &Value) -> Option<u8> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| v.round().clamp(0.0, 100.0) as u8)
}

/// The level the next announcements are said at.
fn announce_volume(announcer: Option<&Arc<Announcer>>, value: &Value) -> Result<u8, String> {
    let announcer =
        announcer.ok_or_else(|| moli_i18n::tr!("pilotes.annonces.annonces_non_configurees"))?;
    let level = level_of(value).ok_or_else(|| moli_i18n::tr!("pilotes.annonces.volume_nombre"))?;
    announcer.set_level(level);
    Ok(level)
}

/// Queues an announcement (it takes seconds: speech, then the sound) and
/// answers at once; a failure later is told to the house.
fn speak(announcer: Option<&Arc<Announcer>>, value: &Value) -> Result<(), String> {
    let announcer =
        announcer.ok_or_else(|| moli_i18n::tr!("pilotes.annonces.annonces_non_configurees"))?;
    let Value::Text(text) = value else {
        return Err(moli_i18n::tr!("pilotes.annonces.annonce_texte"));
    };
    let text = check_text(text).map_err(|e| e.to_string())?.to_owned();
    announcer.enqueue(text)
}

/// What an announcement may say.
///
/// # Errors
/// Nothing to say, or more than [`MAX_TEXT`] characters.
pub fn check_text(text: &str) -> anyhow::Result<&str> {
    let text = text.trim();
    ensure!(
        !text.is_empty(),
        moli_i18n::tr!("pilotes.annonces.rien_a_dire")
    );
    ensure!(
        text.chars().count() <= MAX_TEXT,
        moli_i18n::tr!("pilotes.annonces.annonce_longueur", max = MAX_TEXT)
    );
    Ok(text)
}

/// The start of an announcement, for a message about it.
fn short(text: &str) -> String {
    let mut s: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_announcements_level_is_a_number_from_0_to_100() {
        assert_eq!(level_of(&Value::Int(40)), Some(40));
        assert_eq!(level_of(&Value::Float(37.6)), Some(38));
        assert_eq!(level_of(&Value::Int(250)), Some(100));
        assert_eq!(level_of(&Value::Int(-5)), Some(0));
        assert_eq!(level_of(&Value::Float(f64::NAN)), None);
        assert_eq!(level_of(&Value::Text("fort".into())), None);
    }

    #[test]
    fn what_an_announcement_may_say() {
        assert!(check_text("  ").is_err());
        assert!(check_text(&"a".repeat(MAX_TEXT + 1)).is_err());
        assert_eq!(check_text(" Quelqu'un sonne ").unwrap(), "Quelqu'un sonne");
        assert_eq!(short("court"), "court");
        assert_eq!(short(&"a".repeat(61)), format!("{}…", "a".repeat(60)));
    }

    #[test]
    fn a_speaker_without_announcements_has_none_of_their_points() {
        assert!(Announcements::new(None).points().is_empty());
    }

    /// A speaker that never makes a sound.
    #[derive(Debug)]
    struct Mute;

    impl Speaker for Mute {
        fn play<'a>(
            &'a self,
            _wav: Vec<u8>,
            _level: u8,
            _started: &'a Started,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// Without announcements, there is never anything being said.
    #[tokio::test]
    async fn a_speaker_without_announcements_never_speaks() {
        let mut none = Announcements::new(None);
        let never = tokio::time::timeout(Duration::from_millis(50), none.speaking_changed()).await;
        assert!(never.is_err());
    }

    /// A driver whose speaker starts saying something, then is done.
    #[derive(Debug)]
    struct Teller;

    impl moli_runtime::Driver for Teller {
        fn kind(&self) -> &'static str {
            "teller"
        }

        fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                let id = ctx.device_id("speaker");
                let speech = Speech {
                    tts: default_tts(),
                    voice: None,
                };
                let announcer = Announcer::new(Mute, speech, 35, ctx.media_publisher());
                let mut announcements = Announcements::new(Some(Arc::clone(&announcer)));
                ctx.upsert_device(moli_core::Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: "Teller".into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    members: Vec::new(),
                    points: announcements.points(),
                });
                announcements.start(ctx, &id);
                ctx.ready();
                // What a speaker does while it says an announcement: tells
                // it has started (twice, as a Sonos may), then the
                // announcer clears it once played.
                let speaking = announcer.speaking.clone();
                tokio::spawn(async move {
                    let started = Started {
                        speaking: speaking.clone(),
                        text: "Il fait onze degrés.".into(),
                    };
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    started.now();
                    started.now();
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    speaking.send_if_modified(|said| said.take().is_some());
                });
                loop {
                    tokio::select! {
                        said = announcements.speaking_changed() => {
                            Announcements::show_speaking(ctx, &id, said);
                        }
                        command = ctx.next_command() => {
                            let Some(command) = command else { return Ok(()) };
                            command.reply(Err("not here".into()));
                        }
                    }
                }
            })
        }
    }

    /// The `speaking` point follows the speaker: its words once it starts,
    /// nothing once it is over.
    #[tokio::test]
    async fn the_speaking_point_holds_the_words_being_said() {
        use moli_core::{InstanceId, PointId};
        use moli_runtime::{Hub, HubOptions, spawn_driver};
        use tokio_util::sync::CancellationToken;

        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let task = spawn_driver(
            &hub,
            InstanceId::from("teller"),
            Arc::new(Teller),
            cancel.clone(),
        );
        let speaking = PointId::from("teller:speaker/speaking".to_owned());
        settled(&hub, &speaking, &Value::Text("Il fait onze degrés.".into())).await;
        settled(&hub, &speaking, &Value::Null).await;
        cancel.cancel();
        task.await.unwrap();
    }

    /// The least a driver does with its announcements.
    #[derive(Debug)]
    struct Talker;

    impl moli_runtime::Driver for Talker {
        fn kind(&self) -> &'static str {
            "talker"
        }

        fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                let id = ctx.device_id("speaker");
                let speech = Speech {
                    tts: default_tts(),
                    voice: None,
                };
                let mut announcements = Announcements::new(Some(Announcer::new(
                    Mute,
                    speech,
                    35,
                    ctx.media_publisher(),
                )));
                ctx.upsert_device(moli_core::Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: "Talker".into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    members: Vec::new(),
                    points: announcements.points(),
                });
                announcements.start(ctx, &id);
                ctx.ready();
                while let Some(command) = ctx.next_command().await {
                    let result = announcements
                        .command(ctx, &id, &command)
                        .unwrap_or_else(|| Err("not an announcement".into()));
                    command.reply(result);
                }
                Ok(())
            })
        }
    }

    async fn settled(hub: &moli_runtime::Hub, point: &moli_core::PointId, want: &Value) {
        for _ in 0..200 {
            if hub.state(point).is_some_and(|s| &s.value == want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{point} never became {want:?}: {:?}", hub.state(point));
    }

    #[tokio::test]
    async fn the_level_and_the_silence_are_kept_across_a_restart() {
        use moli_core::{InstanceId, Origin, PointId};
        use moli_runtime::{Hub, HubOptions, spawn_driver};
        use tokio_util::sync::CancellationToken;

        let hub = Hub::new(HubOptions::default()).unwrap();
        let start = || {
            let cancel = CancellationToken::new();
            let task = spawn_driver(
                &hub,
                InstanceId::from("talker"),
                Arc::new(Talker),
                cancel.clone(),
            );
            (cancel, task)
        };
        let level = PointId::from("talker:speaker/announce_volume".to_owned());
        let quiet = PointId::from("talker:speaker/quiet".to_owned());
        let until = PointId::from("talker:speaker/quiet_until".to_owned());

        let (cancel, task) = start();
        // The configured level to start with, then the one written.
        settled(&hub, &level, &Value::Int(35)).await;
        settled(&hub, &quiet, &Value::Bool(false)).await;
        hub.command(&level, Value::Int(60), Origin::Ui, None)
            .await
            .unwrap();
        settled(&hub, &level, &Value::Int(60)).await;
        hub.command(&quiet, Value::Bool(true), Origin::Ui, None)
            .await
            .unwrap();
        settled(&hub, &quiet, &Value::Bool(true)).await;
        let Value::Int(ends) = hub.state(&until).unwrap().value else {
            panic!("quiet_until is not a time");
        };
        let ends = u64::try_from(ends).unwrap();
        let four_hours = u64::try_from(QUIET_FOR.as_millis()).unwrap();
        assert!(ends > moli_core::now_ms() + four_hours - 60_000, "{ends}");

        // A restart keeps both: the level written, and the silence.
        cancel.cancel();
        task.await.unwrap();
        let (cancel, task) = start();
        tokio::time::sleep(Duration::from_millis(200)).await;
        settled(&hub, &level, &Value::Int(60)).await;
        settled(&hub, &quiet, &Value::Bool(true)).await;

        // Switched off, the silence is over.
        hub.command(&quiet, Value::Bool(false), Origin::Ui, None)
            .await
            .unwrap();
        settled(&hub, &quiet, &Value::Bool(false)).await;
        settled(&hub, &until, &Value::Int(0)).await;
        cancel.cancel();
        task.await.unwrap();
    }
}

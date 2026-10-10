//! The doorbell as an interphone. Someone rings: Moli greets them through
//! the doorbell's speaker, listens for their message and hands it to the
//! house's voice brain, which passes it on to the household; then thanks
//! them. Someone is seen at the door (`by_name`): Moli listens, and a
//! sentence that calls it by name is a message too. A sentence said to the
//! door from the house, soon after a visit, is followed by listening for the
//! answer.
//!
//! Moli's brain decides what a sentence is; this side only speaks and
//! listens, one session at a time through the station.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use moli_audio::pcm::wav;
use moli_runtime::voice::{AtDoor, VoiceBrain};
use serde::Deserialize;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::listen;
use crate::rtsp::Station;
use crate::talk;

/// After a visit, a sentence said to the door from the house is followed by
/// listening for the answer, this long.
const REPLY_WINDOW: Duration = Duration::from_secs(180);

/// `[driver.options.interphone]`: the doorbell answers when someone rings.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interphone {
    /// Said when someone rings (default: the catalogue's).
    #[serde(default)]
    pub greeting: Option<String>,
    /// Said once their message is passed on.
    #[serde(default)]
    pub thanks: Option<String>,
    /// How long Moli waits for the visitor to speak, in seconds.
    #[serde(default = "default_wait")]
    pub wait_s: u64,
    /// Someone seen at the door can call Moli by name (« Hey Moli, … »).
    #[serde(default)]
    pub by_name: bool,
    /// How long Moli listens for its name once someone is seen, in seconds.
    #[serde(default = "default_watch")]
    pub watch_s: u64,
    /// After a sentence said to the door from the house (within a few
    /// minutes of a visit), how long Moli listens for the answer, in seconds
    /// (0: it does not).
    #[serde(default = "default_reply")]
    pub reply_s: u64,
}

fn default_wait() -> u64 {
    8
}

fn default_watch() -> u64 {
    30
}

fn default_reply() -> u64 {
    10
}

/// One station's doors: who speaks and listens, and when.
#[derive(Debug)]
pub(crate) struct Doors {
    station: Station,
    interphone: Option<Interphone>,
    /// One sentence at a time through the station.
    turn: tokio::sync::Mutex<()>,
    /// One visit (or watch) at a time.
    visit: tokio::sync::Mutex<()>,
    /// The button: a watch stops listening, the visit starts.
    rang: Notify,
    ringing: AtomicBool,
    last_visit: Mutex<Option<Instant>>,
}

impl Doors {
    pub(crate) fn new(station: Station, interphone: Option<Interphone>) -> Self {
        Self {
            station,
            interphone,
            turn: tokio::sync::Mutex::new(()),
            visit: tokio::sync::Mutex::new(()),
            rang: Notify::new(),
            ringing: AtomicBool::new(false),
            last_visit: Mutex::new(None),
        }
    }

    pub(crate) fn station(&self) -> &Station {
        &self.station
    }

    pub(crate) fn answers(&self) -> bool {
        self.interphone.is_some()
    }

    pub(crate) fn by_name(&self) -> bool {
        self.interphone.as_ref().is_some_and(|i| i.by_name)
    }

    /// Says `text` through `channel`'s speaker (after any sentence before).
    pub(crate) async fn say(
        &self,
        brain: &Arc<dyn VoiceBrain>,
        channel: u64,
        text: &str,
    ) -> anyhow::Result<()> {
        let _turn = self.turn.lock().await;
        let pcm = brain.voice_pcm(text).await.map_err(anyhow::Error::msg)?;
        #[allow(clippy::cast_precision_loss)]
        let seconds = pcm.samples.len() as f64 / f64::from(pcm.rate.max(1));
        let limit = Duration::from_secs_f64(seconds) + Duration::from_secs(15);
        tokio::time::timeout(limit, talk::say(&self.station, channel, &pcm))
            .await
            .context("the doorbell took too long")?
    }

    /// A sentence from the house: said, then, soon after a visit, the
    /// visitor's answer listened for and passed on.
    pub(crate) async fn house_says(
        &self,
        brain: &Arc<dyn VoiceBrain>,
        door: &str,
        channel: u64,
        text: &str,
    ) -> anyhow::Result<()> {
        self.say(brain, channel, text).await?;
        let recent = self
            .last_visit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|at| at.elapsed() < REPLY_WINDOW);
        let Some(i) = self.interphone.as_ref().filter(|i| recent && i.reply_s > 0) else {
            return Ok(());
        };
        let _visit = self.visit.lock().await;
        let quiet = Notify::new();
        let heard = listen::sentence(
            &self.station,
            channel,
            Duration::from_secs(i.reply_s),
            &quiet,
        )
        .await?;
        if let Some(pcm) = heard {
            self.pass_on(brain, door, channel, Some(pcm), true).await?;
        }
        Ok(())
    }

    /// Someone rang at `channel`.
    pub(crate) async fn rang(
        &self,
        brain: &Arc<dyn VoiceBrain>,
        door: &str,
        channel: u64,
    ) -> anyhow::Result<()> {
        let Some(i) = &self.interphone else {
            return Ok(());
        };
        // A watch listening for Moli's name gives way.
        self.ringing.store(true, Ordering::SeqCst);
        self.rang.notify_waiters();
        let _visit = self.visit.lock().await;
        self.ringing.store(false, Ordering::SeqCst);
        *self
            .last_visit
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        let greeting = i
            .greeting
            .clone()
            .unwrap_or_else(|| moli_i18n::tr!("pilotes.reolink.accueil"));
        // Whatever fails on the way, the household still hears of the ring.
        if let Err(e) = self.say(brain, channel, &greeting).await {
            tracing::warn!(channel, error = %format!("{e:#}"), "doorbell greeting failed");
        }
        let quiet = Notify::new();
        let heard = listen::sentence(
            &self.station,
            channel,
            Duration::from_secs(i.wait_s),
            &quiet,
        )
        .await
        .unwrap_or_default();
        self.pass_on(brain, door, channel, heard, true).await
    }

    /// Someone is seen at `channel`: Moli listens a while for its name.
    pub(crate) async fn seen(
        &self,
        brain: &Arc<dyn VoiceBrain>,
        door: &str,
        channel: u64,
    ) -> anyhow::Result<()> {
        let Some(i) = self.interphone.as_ref().filter(|i| i.by_name) else {
            return Ok(());
        };
        // Already in a visit: nothing more to do.
        let Ok(_visit) = self.visit.try_lock() else {
            return Ok(());
        };
        let until = Instant::now() + Duration::from_secs(i.watch_s);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if self.ringing.load(Ordering::SeqCst) {
                return Ok(());
            }
            let Some(pcm) = listen::sentence(&self.station, channel, left, &self.rang).await?
            else {
                return Ok(());
            };
            if self.ringing.load(Ordering::SeqCst) {
                return Ok(());
            }
            let wav = wav(&pcm, listen::RATE);
            match brain.at_door(door.to_owned(), Some(wav), false).await {
                Ok(AtDoor::Nothing) => {}
                Ok(AtDoor::Called) => {
                    self.say(brain, channel, &moli_i18n::tr!("pilotes.reolink.oui"))
                        .await?;
                    let quiet = Notify::new();
                    let heard = listen::sentence(
                        &self.station,
                        channel,
                        Duration::from_secs(i.wait_s),
                        &quiet,
                    )
                    .await?;
                    if heard.is_some() {
                        self.pass_on(brain, door, channel, heard, true).await?;
                    }
                    return Ok(());
                }
                Ok(AtDoor::Relayed(_)) => return self.thank(brain, channel).await,
                Err(e) => anyhow::bail!(e),
            }
        }
        Ok(())
    }

    /// What was heard (`None`: nothing) to the brain, then thanks for a
    /// message passed on.
    async fn pass_on(
        &self,
        brain: &Arc<dyn VoiceBrain>,
        door: &str,
        channel: u64,
        heard: Option<Vec<i16>>,
        rang: bool,
    ) -> anyhow::Result<()> {
        let wav = heard.map(|pcm| wav(&pcm, listen::RATE));
        match brain
            .at_door(door.to_owned(), wav, rang)
            .await
            .map_err(anyhow::Error::msg)?
        {
            AtDoor::Relayed(message) if !message.is_empty() => self.thank(brain, channel).await,
            _ => Ok(()),
        }
    }

    async fn thank(&self, brain: &Arc<dyn VoiceBrain>, channel: u64) -> anyhow::Result<()> {
        let thanks = self
            .interphone
            .as_ref()
            .and_then(|i| i.thanks.clone())
            .unwrap_or_else(|| moli_i18n::tr!("pilotes.reolink.merci"));
        self.say(brain, channel, &thanks).await
    }
}

//! Listening at a doorbell: its microphone's track (AAC over RTP, RFC 3640)
//! decoded to 16 kHz, until someone has spoken a sentence (a voice, then a
//! silence) — or nobody did in time.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_audio::aac::{Decoder, Format};
use moli_audio::pcm::{bytes, resample};
use moli_audio::vad::{Edge, Vad};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::rtsp::{self, Incoming, Rtsp, Station, Way};

/// What the transcription gets.
pub(crate) const RATE: u32 = 16_000;
/// A sentence never runs longer than this.
const LONGEST: Duration = Duration::from_secs(20);
/// Kept before the voice was found: its first syllable.
const BEFORE: Duration = Duration::from_millis(500);

/// A sentence said at `channel`'s microphone, as 16 kHz samples; `None`
/// when nobody spoke within `wait`, or when `stop` is notified.
pub(crate) async fn sentence(
    station: &Station,
    channel: u64,
    wait: Duration,
    stop: &Notify,
) -> anyhow::Result<Option<Vec<i16>>> {
    let mut rtsp = Rtsp::open(station).await?;
    let base = station.uri(channel);
    let mut session = None;
    let result = tokio::select! {
        heard = listen(&mut rtsp, &base, &mut session, wait) => heard,
        () = stop.notified() => Ok(None),
    };
    #[allow(clippy::cast_precision_loss)]
    let seconds = |pcm: &Vec<i16>| pcm.len() as f64 / f64::from(RATE);
    match &result {
        Ok(heard) => {
            tracing::info!(channel, sentence_s = ?heard.as_ref().map(seconds), "door listened");
        }
        Err(e) => tracing::warn!(channel, error = %format!("{e:#}"), "door listening failed"),
    }
    rtsp.finish(&base, session.as_deref()).await;
    result
}

async fn listen(
    rtsp: &mut Rtsp,
    base: &str,
    session: &mut Option<String>,
    wait: Duration,
) -> anyhow::Result<Option<Vec<i16>>> {
    let track = rtsp
        .describe(base)
        .await?
        .into_iter()
        .find(|t| t.audio && t.way == Way::In && t.codec.starts_with("MPEG4-GENERIC"))
        .context("this camera has no microphone Moli can read")?;
    let format = Format::parse(&track.fmtp).context("the microphone's format is unknown")?;
    let rate = format.rate().unwrap_or(RATE);
    let mut decoder = Decoder::new(format)?;
    let id = rtsp.setup(&track, 0).await?;
    rtsp.play(base, &id).await?;
    *session = Some(id);

    let mut vad = Vad::default();
    let mut heard: Vec<i16> = Vec::new();
    let mut started: Option<usize> = None;
    let mut deadline = Instant::now() + wait;
    loop {
        let incoming = tokio::time::timeout_at(deadline, rtsp.next()).await;
        let Ok(incoming) = incoming else {
            // Nobody spoke, or the sentence ran to its longest.
            let h = vad.heard();
            tracing::info!(
                best = h.best,
                voice_db = ?h.voice_db(),
                quiet_db = ?h.quiet_db(),
                spoke = started.is_some(),
                "door microphone"
            );
            return Ok(started.map(|at| heard[at..].to_vec()));
        };
        let Incoming::Packet(0, packet) = incoming? else {
            continue;
        };
        let Some(payload) = rtsp::payload_of(&packet) else {
            continue;
        };
        let samples = resample(&decoder.push(payload), rate, RATE);
        heard.extend_from_slice(&samples);
        match vad.push(&bytes(&samples)) {
            Some(Edge::Start) if started.is_none() => {
                let back = usize::try_from(u128::from(RATE) * BEFORE.as_millis() / 1000)?;
                let speech_at = vad.heard().speech_at.map_or(heard.len(), samples_in);
                started = Some(speech_at.saturating_sub(back).min(heard.len()));
                deadline = Instant::now() + LONGEST;
            }
            Some(Edge::End) => {
                if let Some(at) = started {
                    return Ok(Some(heard[at..].to_vec()));
                }
            }
            _ => {}
        }
        if heard.len() > samples_in(wait + LONGEST + LONGEST) {
            bail!("listened too long");
        }
    }
}

/// Samples at [`RATE`] in `d`.
fn samples_in(d: Duration) -> usize {
    usize::try_from(u128::from(RATE) * d.as_millis() / 1000).unwrap_or(usize::MAX)
}

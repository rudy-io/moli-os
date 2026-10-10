//! A conversation on a voice satellite. The device detects its wake word,
//! then streams what it hears; Moli finds the end of the sentence (the
//! device never does), has it transcribed, lets Moli answer, gives the device
//! the answer's voice to play, and asks it to listen again: the conversation
//! stays open, without the wake word, until a silence of `window`, a closing
//! phrase (« merci », « c'est tout »), a double press, or an error. Its end
//! is marked by a chime.

use std::sync::Arc;
use std::time::Duration;

use moli_core::{DeviceId, Value};
use moli_runtime::DriverCtx;
use moli_runtime::media::Image;
use moli_runtime::voice::{Answer, Said, Speaker, VoiceBrain};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::api::{self, VoiceEvent, VoiceRequest};
use crate::vad::{self, Edge, Vad};
use crate::{Outbox, sound};

const RATE: u32 = 16_000;
/// A sentence longer than this is cut and heard as it is.
const MAX_UTTERANCE: Duration = Duration::from_secs(20);
/// After an answer has played, the device asks to listen again at once:
/// not within this, the conversation is over.
const CONTINUE_WAIT: Duration = Duration::from_secs(5);

/// What the thinking task found, step by step.
#[derive(Debug)]
pub(crate) enum Thought {
    Heard(String),
    Answered(Answer),
    Failed(String),
}

/// One request of the device: the audio so far and where its sentence is.
#[derive(Debug)]
struct Run {
    vad: Vad,
    pcm: Vec<u8>,
    length: Duration,
    /// The sentence must start before this (`None` once it has).
    speech_by: Option<Instant>,
    /// The sentence ended: the microphone is off, Moli thinks.
    ended: bool,
}

impl Run {
    /// What the microphone gave, and how the listening ended: the levels to
    /// tune the detection by.
    fn log(&self, ctx: &DriverCtx, ended_by: &str) {
        let heard = self.vad.heard();
        tracing::info!(
            instance = %ctx.instance(),
            ended_by,
            length_ms = self.length.as_millis(),
            speech_at_ms = heard.speech_at.map(|d| d.as_millis()),
            best = heard.best,
            voice_db = heard.voice_db(),
            quiet_db = heard.quiet_db(),
            "satellite listened"
        );
    }
}

/// The voice of one satellite.
#[derive(Debug)]
pub(crate) struct Voice {
    window: Duration,
    media_base: Option<String>,
    run: Option<Run>,
    conversation: Vec<Said>,
    conversation_id: String,
    /// The last answer asked the device to listen again; it must, by then.
    continuing: Option<Instant>,
    /// The last answer closed the conversation: when the device listens
    /// again (it does, once the words have played), the chime ends it.
    closing: bool,
    /// Results of a thinking task are for this generation only (a stop or a
    /// new request makes the old ones stale).
    generation: u64,
    thoughts_tx: mpsc::Sender<(u64, Thought)>,
}

/// Where the thinking task's steps arrive (kept apart: the session waits on it).
pub(crate) type Thoughts = mpsc::Receiver<(u64, Thought)>;

type Sent = anyhow::Result<()>;

impl Voice {
    pub(crate) fn new(window: Duration, media_base: Option<String>) -> (Self, Thoughts) {
        let (thoughts_tx, thoughts) = mpsc::channel(8);
        let voice = Self {
            window,
            media_base: media_base.map(|b| b.trim_end_matches('/').to_owned()),
            run: None,
            conversation: Vec::new(),
            conversation_id: String::new(),
            continuing: None,
            closing: false,
            generation: 0,
            thoughts_tx,
        };
        (voice, thoughts)
    }

    /// When something must happen even if the device says nothing.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.run
            .as_ref()
            .and_then(|r| r.speech_by)
            .or(self.continuing)
    }

    fn state(ctx: &DriverCtx, id: &DeviceId, state: &str) {
        ctx.set_state(id, "state", Value::Text(state.into()));
    }

    async fn event(tx: &mut Outbox, event: VoiceEvent, data: &[(&str, &str)]) -> Sent {
        tx.send(api::VOICE_EVENT, &api::voice_event(event, data))
            .await
    }

    /// The device starts listening (or stops).
    pub(crate) async fn on_request(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        tx: &mut Outbox,
        request: &VoiceRequest,
    ) -> Sent {
        self.generation += 1;
        if !request.start {
            self.forget();
            Self::state(ctx, id, "idle");
            return Ok(());
        }
        if ctx.voice_brain().is_none() || self.media_base.is_none() {
            tracing::warn!(instance = %ctx.instance(), "voice request: no assistant or no media_base");
            Self::state(ctx, id, "idle");
            return tx
                .send(api::VOICE_RESPONSE, &api::voice_response(true))
                .await;
        }
        let continued = self
            .continuing
            .take()
            .is_some_and(|by| Instant::now() <= by);
        if !continued {
            self.conversation.clear();
            self.conversation_id = new_id();
            if !request.wake_word.is_empty() {
                ctx.set_state(
                    id,
                    "wake_word",
                    Value::Text(request.wake_word.as_str().into()),
                );
            }
        }
        tx.send(api::VOICE_RESPONSE, &api::voice_response(false))
            .await?;
        Self::event(tx, VoiceEvent::RunStart, &[]).await?;
        if continued && std::mem::take(&mut self.closing) {
            return self.end(ctx, id, tx, true).await;
        }
        Self::event(tx, VoiceEvent::SttStart, &[]).await?;
        self.run = Some(Run {
            vad: Vad::default(),
            pcm: Vec::new(),
            length: Duration::ZERO,
            speech_by: Some(Instant::now() + self.window),
            ended: false,
        });
        Self::state(ctx, id, "listening");
        Ok(())
    }

    /// What the microphone heard: the sentence's start and end.
    pub(crate) async fn on_audio(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        tx: &mut Outbox,
        pcm: &[u8],
    ) -> Sent {
        let Some(run) = self.run.as_mut().filter(|r| !r.ended) else {
            return Ok(());
        };
        run.pcm.extend_from_slice(pcm);
        run.length += vad::length(pcm);
        let edge = run.vad.push(pcm);
        if edge == Some(Edge::Start) {
            run.speech_by = None;
            Self::event(tx, VoiceEvent::SttVadStart, &[]).await?;
        }
        let too_long = run.length >= MAX_UTTERANCE;
        if edge != Some(Edge::End) && !too_long {
            return Ok(());
        }
        run.log(ctx, if too_long { "cut" } else { "silence" });
        run.ended = true;
        run.speech_by = None;
        let wav = sound::wav(&std::mem::take(&mut run.pcm), RATE);
        Self::event(tx, VoiceEvent::SttVadEnd, &[]).await?;
        Self::state(ctx, id, "thinking");
        let Some(brain) = ctx.voice_brain() else {
            return self.fail(ctx, id, tx, "no assistant").await;
        };
        self.think(brain, wav);
        Ok(())
    }

    /// Hear, then (unless it closes the conversation) answer: in the
    /// background, the device's messages keep flowing meanwhile.
    fn think(&self, brain: Arc<dyn VoiceBrain>, wav: Vec<u8>) {
        let generation = self.generation;
        let conversation = self.conversation.clone();
        let tx = self.thoughts_tx.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let text = match brain.hear(wav).await {
                Ok(text) => text.trim().to_owned(),
                Err(e) => {
                    let _ = tx.send((generation, Thought::Failed(e))).await;
                    return;
                }
            };
            let heard_ms = started.elapsed().as_millis();
            let _ = tx.send((generation, Thought::Heard(text.clone()))).await;
            if text.is_empty() || brain.is_goodbye(&text) {
                return;
            }
            let mut lines = conversation;
            lines.push(Said {
                who: Speaker::Person,
                text,
            });
            let thought = match brain.answer(lines).await {
                Ok(answer) => Thought::Answered(answer),
                Err(e) => Thought::Failed(e),
            };
            tracing::info!(
                heard_ms,
                answered_ms = started.elapsed().as_millis() - heard_ms,
                "satellite turn"
            );
            let _ = tx.send((generation, thought)).await;
        });
    }

    /// A step of the thinking task.
    pub(crate) async fn on_thought(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        tx: &mut Outbox,
        generation: u64,
        thought: Thought,
    ) -> Sent {
        if generation != self.generation {
            return Ok(());
        }
        match thought {
            Thought::Heard(text) => {
                let brain = ctx.voice_brain();
                Self::event(tx, VoiceEvent::SttEnd, &[("text", &text)]).await?;
                // Nothing understood, or a closing phrase: the conversation ends.
                if text.is_empty()
                    && let Some(brain) = &brain
                {
                    brain.heard_nothing();
                }
                if text.is_empty() || brain.is_some_and(|b| b.is_goodbye(&text)) {
                    return self.end(ctx, id, tx, false).await;
                }
                self.conversation.push(Said {
                    who: Speaker::Person,
                    text,
                });
                Self::event(tx, VoiceEvent::IntentStart, &[]).await
            }
            Thought::Answered(Answer {
                text: reply,
                closes,
            }) => {
                // Closed without words: the chime, now.
                if closes && reply.trim().is_empty() {
                    return self.end(ctx, id, tx, false).await;
                }
                let url = match (ctx.voice_brain(), &self.media_base) {
                    (Some(brain), Some(base)) => brain.voice_url(base, &reply),
                    _ => None,
                };
                let Some(url) = url else {
                    return self.fail(ctx, id, tx, "no voice for the answer").await;
                };
                self.conversation.push(Said {
                    who: Speaker::Moli,
                    text: reply.clone(),
                });
                let conversation_id = self.conversation_id.clone();
                Self::event(
                    tx,
                    VoiceEvent::IntentEnd,
                    &[
                        ("conversation_id", &conversation_id),
                        ("continue_conversation", "1"),
                    ],
                )
                .await?;
                Self::event(tx, VoiceEvent::TtsStart, &[("text", &reply)]).await?;
                Self::event(tx, VoiceEvent::TtsEnd, &[("url", &url)]).await?;
                Self::event(tx, VoiceEvent::RunEnd, &[]).await?;
                self.run = None;
                // Listening again is the device's move once the answer has played
                // (closing too: its new request is answered with the chime).
                self.continuing = Some(Instant::now() + Duration::from_secs(60));
                self.closing = closes;
                Self::state(ctx, id, "speaking");
                Ok(())
            }
            Thought::Failed(why) => self.fail(ctx, id, tx, &why).await,
        }
    }

    /// The answer (or the chime) has played.
    pub(crate) fn on_played(&mut self, ctx: &DriverCtx, id: &DeviceId) {
        if self.continuing.is_some() {
            // The device asks to listen again now, or the conversation is over.
            self.continuing = Some(Instant::now() + CONTINUE_WAIT);
            Self::state(ctx, id, "listening");
        } else if self.run.is_none() {
            Self::state(ctx, id, "idle");
        }
    }

    /// Nothing came in time: no sentence in the window, or no new request
    /// after an answer.
    pub(crate) async fn on_deadline(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        tx: &mut Outbox,
    ) -> Sent {
        let now = Instant::now();
        if let Some(run) = self
            .run
            .as_ref()
            .filter(|r| r.speech_by.is_some_and(|by| now >= by))
        {
            run.log(ctx, "no voice");
            if let Some(brain) = ctx.voice_brain() {
                brain.heard_nothing();
            }
            return self.end(ctx, id, tx, true).await;
        }
        if self.continuing.is_some_and(|by| now >= by) {
            self.forget();
            Self::state(ctx, id, "idle");
        }
        Ok(())
    }

    /// The button's double press: the conversation ends now.
    pub(crate) async fn on_button(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        tx: &mut Outbox,
        event: &str,
    ) -> Sent {
        if event != "double_press" || (self.run.is_none() && self.continuing.is_none()) {
            return Ok(());
        }
        let listening = self.run.as_ref().is_some_and(|r| !r.ended);
        if self.run.is_some() {
            self.end(ctx, id, tx, listening).await
        } else {
            self.forget();
            Self::state(ctx, id, "idle");
            Ok(())
        }
    }

    /// The end of a conversation: the device stops listening and plays the chime.
    async fn end(
        &mut self,
        ctx: &DriverCtx,
        id: &DeviceId,
        tx: &mut Outbox,
        stop_mic: bool,
    ) -> Sent {
        self.generation += 1;
        if stop_mic {
            Self::event(tx, VoiceEvent::SttVadEnd, &[]).await?;
        }
        let conversation_id = self.conversation_id.clone();
        Self::event(
            tx,
            VoiceEvent::IntentEnd,
            &[
                ("conversation_id", &conversation_id),
                ("continue_conversation", "0"),
            ],
        )
        .await?;
        let chime = self.media_base.as_ref().and_then(|base| {
            let name = ctx.media_publisher().publish(
                "wav",
                Image {
                    content_type: "audio/wav".into(),
                    bytes: sound::chime(),
                },
            )?;
            Some(format!("{base}/api/media/{name}"))
        });
        if let Some(url) = &chime {
            Self::event(tx, VoiceEvent::TtsEnd, &[("url", url)]).await?;
        }
        Self::event(tx, VoiceEvent::RunEnd, &[]).await?;
        self.forget();
        Self::state(ctx, id, if chime.is_some() { "speaking" } else { "idle" });
        tracing::info!(instance = %ctx.instance(), "satellite conversation over");
        Ok(())
    }

    /// Something failed: the device is told (it stops), the conversation forgotten.
    async fn fail(&mut self, ctx: &DriverCtx, id: &DeviceId, tx: &mut Outbox, why: &str) -> Sent {
        tracing::warn!(instance = %ctx.instance(), error = why, "satellite turn failed");
        self.generation += 1;
        Self::event(
            tx,
            VoiceEvent::Error,
            &[("code", "moli-error"), ("message", why)],
        )
        .await?;
        Self::event(tx, VoiceEvent::RunEnd, &[]).await?;
        self.forget();
        Self::state(ctx, id, "idle");
        Ok(())
    }

    fn forget(&mut self) {
        self.run = None;
        self.continuing = None;
        self.closing = false;
        self.conversation.clear();
    }
}

/// A conversation's id: random enough to never mix two.
fn new_id() -> String {
    use ring::rand::SecureRandom as _;
    let mut raw = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut raw);
    raw.iter().fold(String::with_capacity(16), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    })
}

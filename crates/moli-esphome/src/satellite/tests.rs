//! A satellite whose announcement player only decodes FLAC. Voice answers
//! and the closing sound take that same player, including in conversation.

use std::io::Cursor;

use moli_core::InstanceId;
use moli_net::proto;
use moli_runtime::{BoxFuture, Driver, Hub, HubOptions, spawn_driver};
use symphonia_bundle_flac::{FlacDecoder, FlacReader};
use symphonia_core::codecs::audio::{AudioDecoder as _, AudioDecoderOptions};
use symphonia_core::formats::{FormatOptions, FormatReader as _, TrackType};
use symphonia_core::io::{MediaSourceStream, MediaSourceStreamOptions};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{frame, noise};

#[derive(Debug)]
struct Brain;

impl VoiceBrain for Brain {
    fn hear(&self, _: Vec<u8>) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async { panic!("no microphone or model in this playback test") })
    }

    fn answer(
        &self,
        _: &moli_core::DeviceId,
        _: Vec<Said>,
    ) -> BoxFuture<'_, Result<Answer, String>> {
        Box::pin(async { panic!("the answer is already known") })
    }

    fn voice_url(&self, base: &str, _: &str) -> Option<String> {
        Some(format!("{base}/answer.wav"))
    }

    fn announcement_url(&self, base: &str, _: &str) -> Option<String> {
        Some(format!("{base}/answer.flac"))
    }

    fn is_goodbye(&self, _: &str) -> bool {
        false
    }
}

#[derive(Debug)]
struct Playback {
    answer: bool,
    urls: mpsc::Sender<String>,
}

impl Driver for Playback {
    fn kind(&self) -> &'static str {
        "esphome-playback-test"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let client = TcpStream::connect(listener.local_addr()?).await?;
            let (device, _) = listener.accept().await?;
            let (initiator, first) = noise::Initiator::start(&[0; 32], noise::PROLOGUE)?;
            let (second, responder) = noise::respond(&[0; 32], noise::PROLOGUE, &first)?;
            let session = initiator.finish(&second)?;
            let (_, write) = client.into_split();
            let mut tx = frame::Sender::new(write, session.send);
            let mut rx = frame::Receiver::new(device, responder.receive);
            let (mut voice, _) = Voice::new(
                Duration::from_secs(8),
                Some("http://media.example.test".to_owned()),
                moli_core::DeviceId::from("esphome:aabbccddeeff"),
                None,
            );
            let id = ctx.device_id("aabbccddeeff");
            if self.answer {
                voice
                    .on_thought(
                        ctx,
                        &id,
                        &mut tx,
                        0,
                        Thought::Answered(Answer {
                            text: "The weather is fine.".to_owned(),
                            closes: false,
                        }),
                    )
                    .await?;
            } else {
                voice.end(ctx, &id, &mut tx, false).await?;
            }
            loop {
                let (kind, payload) = rx.next().await?;
                assert_eq!(kind, api::VOICE_EVENT);
                let event = proto::parse(&payload).unwrap();
                if event.uint(1) == Some(VoiceEvent::TtsEnd as u64) {
                    let url = event
                        .all_msgs(2)
                        .find(|arg| arg.str(1) == Some("url"))
                        .unwrap()
                        .str(2)
                        .unwrap()
                        .to_owned();
                    self.urls.send(url).await?;
                    break;
                }
            }
            ctx.cancelled().await;
            Ok(())
        })
    }
}

/// Decode the media selected by the actual encrypted TTS_END event. Like a
/// firmware without WAV, this player rejects that format before playback.
async fn play(answer: bool) -> (u32, Vec<i16>) {
    let hub = Hub::new(HubOptions::default()).unwrap();
    hub.set_voice_brain(Arc::new(Brain));
    let (urls, mut received) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("voice"),
        Arc::new(Playback { answer, urls }),
        cancel.clone(),
    );
    let url = tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .expect("a TTS_END event")
        .unwrap();
    cancel.cancel();
    assert!(
        url.rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("flac")),
        "the player cannot decode WAV: {url}"
    );
    let bytes = if answer {
        assert_eq!(url, "http://media.example.test/answer.flac");
        // A prepared answer, supplied without calling a speech provider.
        moli_audio::flac::encode(&[0, 1000, -1000, 500, 0], 24_000)
    } else {
        let media = hub
            .published_media(url.rsplit('/').next().unwrap())
            .unwrap();
        assert_eq!(media.content_type, "audio/flac");
        media.bytes
    };
    let source = MediaSourceStream::new(
        Box::new(Cursor::new(bytes)),
        MediaSourceStreamOptions::default(),
    );
    let mut reader = FlacReader::try_new(source, FormatOptions::default()).unwrap();
    let params = reader
        .default_track(TrackType::Audio)
        .unwrap()
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap()
        .clone();
    let mut decoder = FlacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = reader.next_packet().unwrap() {
        let mut chunk: Vec<i16> = Vec::new();
        decoder
            .decode(&packet)
            .unwrap()
            .copy_to_vec_interleaved(&mut chunk);
        samples.extend(chunk);
    }
    (params.sample_rate.unwrap(), samples)
}

/// The device's button stops a request; what the device receives then.
#[derive(Debug)]
struct Stopped {
    thinking: bool,
    events: mpsc::Sender<u64>,
}

impl Driver for Stopped {
    fn kind(&self) -> &'static str {
        "esphome-stop-test"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let client = TcpStream::connect(listener.local_addr()?).await?;
            let (device, _) = listener.accept().await?;
            let (initiator, first) = noise::Initiator::start(&[0; 32], noise::PROLOGUE)?;
            let (second, responder) = noise::respond(&[0; 32], noise::PROLOGUE, &first)?;
            let session = initiator.finish(&second)?;
            let (_, write) = client.into_split();
            let mut tx = frame::Sender::new(write, session.send);
            let mut rx = frame::Receiver::new(device, responder.receive);
            let (mut voice, _) = Voice::new(
                Duration::from_secs(8),
                Some("http://media.example.test".to_owned()),
                moli_core::DeviceId::from("esphome:aabbccddeeff"),
                None,
            );
            let id = ctx.device_id("aabbccddeeff");
            voice.run = Some(Run {
                vad: Vad::default(),
                pcm: Vec::new(),
                length: Duration::ZERO,
                speech_by: None,
                ended: self.thinking,
            });
            let stop = VoiceRequest::default();
            voice.on_request(ctx, &id, &mut tx, &stop).await?;
            assert!(voice.run.is_none());
            let (kind, payload) = rx.next().await?;
            assert_eq!(kind, api::VOICE_EVENT);
            let event = proto::parse(&payload).unwrap().uint(1).unwrap();
            self.events.send(event).await?;
            ctx.cancelled().await;
            Ok(())
        })
    }
}

async fn event_after_a_stop(thinking: bool) -> u64 {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let (events, mut received) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("voice"),
        Arc::new(Stopped { thinking, events }),
        cancel.clone(),
    );
    let event = tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .expect("an event")
        .unwrap();
    cancel.cancel();
    event
}

/// The firmware leaves « waiting for a response » only on RUN_END: without
/// it, the device blinks for good and no longer hears its wake word.
#[tokio::test]
async fn a_stop_while_thinking_ends_the_run_for_the_device() {
    assert_eq!(event_after_a_stop(true).await, VoiceEvent::RunEnd as u64);
}

/// RUN_END also resets the lights of a stop while listening.
#[tokio::test]
async fn a_stop_while_listening_ends_the_run_for_the_device() {
    assert_eq!(event_after_a_stop(false).await, VoiceEvent::RunEnd as u64);
}

/// An answer through a speaker: the device shows it answers, waits, then
/// plays a silence once the speaker said it, or the answer if it did not.
#[derive(Debug)]
struct ThroughSpeaker {
    said: bool,
    events: mpsc::Sender<(u64, Option<String>)>,
}

impl Driver for ThroughSpeaker {
    fn kind(&self) -> &'static str {
        "esphome-speaker-test"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let client = TcpStream::connect(listener.local_addr()?).await?;
            let (device, _) = listener.accept().await?;
            let (initiator, first) = noise::Initiator::start(&[0; 32], noise::PROLOGUE)?;
            let (second, responder) = noise::respond(&[0; 32], noise::PROLOGUE, &first)?;
            let session = initiator.finish(&second)?;
            let (_, write) = client.into_split();
            let mut tx = frame::Sender::new(write, session.send);
            let mut rx = frame::Receiver::new(device, responder.receive);
            let (mut voice, _) = Voice::new(
                Duration::from_secs(8),
                Some("http://media.example.test".to_owned()),
                moli_core::DeviceId::from("esphome:aabbccddeeff"),
                Some("speaker:one/announce".to_owned()),
            );
            let id = ctx.device_id("aabbccddeeff");
            let answer = Thought::Answered(Answer {
                text: "The weather is fine.".to_owned(),
                closes: false,
            });
            voice.on_thought(ctx, &id, &mut tx, 0, answer).await?;
            assert!(voice.spoken_by_speaker.is_some(), "waits for the speaker");
            let spoken = Thought::Spoken(self.said);
            voice.on_thought(ctx, &id, &mut tx, 0, spoken).await?;
            loop {
                let (kind, payload) = rx.next().await?;
                assert_eq!(kind, api::VOICE_EVENT);
                let event = proto::parse(&payload).unwrap();
                let url = event
                    .all_msgs(2)
                    .find(|arg| arg.str(1) == Some("url"))
                    .and_then(|arg| arg.str(2))
                    .map(str::to_owned);
                let kind = event.uint(1).unwrap();
                self.events.send((kind, url)).await?;
                if kind == VoiceEvent::RunEnd as u64 {
                    break;
                }
            }
            ctx.cancelled().await;
            Ok(())
        })
    }
}

async fn through_speaker(said: bool) -> Vec<(u64, Option<String>)> {
    let hub = Hub::new(HubOptions::default()).unwrap();
    hub.set_voice_brain(Arc::new(Brain));
    let (events, mut received) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("voice"),
        Arc::new(ThroughSpeaker { said, events }),
        cancel.clone(),
    );
    let mut all = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(3), received.recv()).await
    {
        let end = event.0 == VoiceEvent::RunEnd as u64;
        all.push(event);
        if end {
            break;
        }
    }
    cancel.cancel();
    all
}

#[tokio::test]
async fn a_speaker_says_the_answer_and_the_device_plays_a_silence() {
    let events = through_speaker(true).await;
    let kinds: Vec<u64> = events.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        [
            VoiceEvent::IntentEnd as u64,
            VoiceEvent::TtsStart as u64,
            VoiceEvent::TtsEnd as u64,
            VoiceEvent::RunEnd as u64
        ]
    );
    let url = events[2].1.clone().unwrap();
    assert!(
        url.starts_with("http://media.example.test/api/media/"),
        "a silence, not the answer: {url}"
    );
}

#[tokio::test]
async fn the_device_says_the_answer_when_the_speaker_did_not() {
    let events = through_speaker(false).await;
    let url = events
        .iter()
        .find(|(kind, _)| *kind == VoiceEvent::TtsEnd as u64)
        .and_then(|(_, url)| url.clone())
        .unwrap();
    assert_eq!(url, "http://media.example.test/answer.flac");
}

#[tokio::test]
async fn a_conversation_answer_reaches_a_flac_only_player() {
    let (rate, samples) = play(true).await;
    assert_eq!(rate, 24_000);
    assert_eq!(samples, [0, 1000, -1000, 500, 0]);
}

#[tokio::test]
async fn the_closing_chime_reaches_a_flac_only_player() {
    let (rate, samples) = play(false).await;
    assert_eq!(rate, 22_050);
    assert_eq!(samples.len(), 7276, "the complete 330 ms chime");
    assert_eq!(samples[0], 0);
    assert!(samples.iter().any(|sample| sample.abs() > 1000));
}

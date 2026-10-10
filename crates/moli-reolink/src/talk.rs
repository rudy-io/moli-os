//! Talking through a doorbell (or any camera with a speaker): the ONVIF back
//! channel the station's RTSP server offers when asked for it, fed with
//! Moli's voice as G.711 µ-law at 8 kHz, 20 ms per RTP packet, in real time.
//!
//! A doorbell whose back channel stays open stays in talk mode (its ring
//! goes blue and its chime stops ringing): every session ends with a
//! TEARDOWN and a closed connection, whatever happened in between.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_audio::pcm::{resample, ulaw};
use moli_runtime::voice::Pcm;

use crate::rtsp::{self, Rtsp, Station, Way};

/// The back channel's sound: G.711 at 8 kHz.
const RATE: u32 = 8_000;
/// 20 ms of sound per packet.
const PACKET: usize = 160;
/// Packets sent at once before keeping time, so the speaker never starves.
const AHEAD: usize = 3;
/// After the last packet, the speaker's own buffer plays out.
const PLAY_OUT: Duration = Duration::from_millis(400);

/// Whether `channel` has a speaker Moli can talk through.
pub(crate) async fn offers(station: &Station, channel: u64) -> anyhow::Result<bool> {
    let mut rtsp = Rtsp::open(station).await?;
    let base = station.uri(channel);
    let tracks = rtsp.describe(&base).await;
    rtsp.finish(&base, None).await;
    Ok(tracks?.iter().any(|t| t.audio && t.way == Way::Out))
}

/// Says `pcm` through `channel`'s speaker.
pub(crate) async fn say(station: &Station, channel: u64, pcm: &Pcm) -> anyhow::Result<()> {
    let sound: Vec<u8> = resample(&pcm.samples, pcm.rate, RATE)
        .into_iter()
        .map(ulaw)
        .collect();
    let mut rtsp = Rtsp::open(station).await?;
    let base = station.uri(channel);
    let mut session = None;
    let result = async {
        let track = rtsp
            .describe(&base)
            .await?
            .into_iter()
            .find(|t| t.audio && t.way == Way::Out)
            .context("this camera offers no back channel")?;
        if track.payload != 0 {
            bail!(
                "the back channel wants payload {}, not µ-law",
                track.payload
            );
        }
        let id = rtsp.setup(&track, 0).await?;
        rtsp.play(&base, &id).await?;
        session = Some(id);
        send_sound(&mut rtsp, &sound, track.payload).await
    }
    .await;
    // Always: a back channel left open keeps the doorbell in talk mode.
    rtsp.finish(&base, session.as_deref()).await;
    result
}

/// The sound, packet by packet in real time; what the station sends
/// meanwhile (reports) is read and dropped.
async fn send_sound(rtsp: &mut Rtsp, sound: &[u8], payload: u8) -> anyhow::Result<()> {
    let ssrc = rtsp::nanos();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    for (i, chunk) in sound.chunks(PACKET).enumerate() {
        if i >= AHEAD {
            tick.tick().await;
        }
        #[allow(clippy::cast_possible_truncation)]
        let packet = rtsp::rtp(payload, i as u16, (i * PACKET) as u32, ssrc, i == 0, chunk);
        rtsp.send(0, &packet).await?;
        rtsp.drain()?;
    }
    tokio::time::sleep(PLAY_OUT).await;
    rtsp.drain()
}

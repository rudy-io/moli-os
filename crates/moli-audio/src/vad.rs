//! Is someone speaking? A small neural network (earshot, pure Rust, 40 KiB
//! of weights) scores every 16 ms of audio for a human voice; a sentence
//! starts after `start` of voice and ends after `end` without. Loudness plays
//! no part: an energy threshold did, and a room's hum above it (the Voice
//! PE's processed microphone, a fan) kept every sentence running to its cap.

use std::time::Duration;

use earshot::Detector;

/// What a frame changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// The detector's frame: 256 samples of 16 kHz audio.
const FRAME_SAMPLES: usize = 256;
const FRAME: Duration = Duration::from_millis(16);
const RATE: f64 = 16_000.0;
/// A frame scoring this or more holds a voice (earshot's own default).
const VOICE: f32 = 0.5;
/// A fresh detector takes the first few hundred milliseconds of a room's
/// noise for a voice (the jump from its silent buffer): they cannot start a
/// sentence. Nothing is lost, the audio is kept whole for the transcription.
const WARM_UP: Duration = Duration::from_millis(400);

/// Where a sentence starts and ends, from each frame's verdict.
#[derive(Clone, Debug)]
struct Timing {
    start: Duration,
    end: Duration,
    speaking: bool,
    voiced: Duration,
    unvoiced: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            start: Duration::from_millis(100),
            // 700 ms without a voice ends a sentence (a pause inside one is shorter).
            end: Duration::from_millis(700),
            speaking: false,
            voiced: Duration::ZERO,
            unvoiced: Duration::ZERO,
        }
    }
}

impl Timing {
    fn push(&mut self, voice: bool, frame: Duration) -> Option<Edge> {
        if !self.speaking {
            self.voiced = if voice {
                self.voiced + frame
            } else {
                Duration::ZERO
            };
            if self.voiced >= self.start {
                self.speaking = true;
                self.unvoiced = Duration::ZERO;
                return Some(Edge::Start);
            }
            return None;
        }
        self.unvoiced = if voice {
            Duration::ZERO
        } else {
            self.unvoiced + frame
        };
        if self.unvoiced >= self.end {
            self.speaking = false;
            self.voiced = Duration::ZERO;
            return Some(Edge::End);
        }
        None
    }
}

/// The mean power of some frames.
#[derive(Clone, Copy, Debug, Default)]
struct Power {
    squares: f64,
    samples: u64,
}

impl Power {
    fn add(&mut self, frame: &[i16]) {
        self.squares += frame
            .iter()
            .map(|s| {
                let s = f64::from(*s) / 32768.0;
                s * s
            })
            .sum::<f64>();
        self.samples += frame.len() as u64;
    }

    /// In dBFS, to a tenth (`None` without a frame).
    fn db(self) -> Option<f32> {
        if self.samples == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let mean = self.squares / self.samples as f64;
        let db = (10.0 * mean.max(1e-12).log10() * 10.0).round() / 10.0;
        #[allow(clippy::cast_possible_truncation)]
        Some(db as f32)
    }
}

/// What a request's audio was like: logged, to tune by.
#[derive(Clone, Copy, Debug, Default)]
pub struct Heard {
    /// The audio scored so far.
    pub length: Duration,
    /// When the sentence was found to start.
    pub speech_at: Option<Duration>,
    /// The highest frame score, 0 to 1.
    pub best: f32,
    voice: Power,
    quiet: Power,
}

impl Heard {
    /// The frames with a voice, in dBFS.
    #[must_use]
    pub fn voice_db(&self) -> Option<f32> {
        self.voice.db()
    }

    /// The frames without, in dBFS: the room.
    #[must_use]
    pub fn quiet_db(&self) -> Option<f32> {
        self.quiet.db()
    }
}

pub struct Vad {
    detector: Box<Detector>,
    timing: Timing,
    /// Samples short of a whole frame, for the next push.
    pending: Vec<i16>,
    heard: Heard,
}

impl std::fmt::Debug for Vad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vad")
            .field("timing", &self.timing)
            .field("heard", &self.heard)
            .finish_non_exhaustive()
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self {
            detector: Box::new(Detector::default()),
            timing: Timing::default(),
            pending: Vec::with_capacity(2 * FRAME_SAMPLES),
            heard: Heard::default(),
        }
    }
}

impl Vad {
    /// More 16-bit little-endian mono samples at 16 kHz, in chunks of any length.
    pub fn push(&mut self, pcm: &[u8]) -> Option<Edge> {
        self.pending.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b)),
        );
        let mut edge = None;
        let mut used = 0;
        while self.pending.len() - used >= FRAME_SAMPLES {
            let frame = &self.pending[used..used + FRAME_SAMPLES];
            used += FRAME_SAMPLES;
            let score = self.detector.predict_i16(frame);
            let voice = score >= VOICE;
            self.heard.length += FRAME;
            self.heard.best = self.heard.best.max(score);
            if voice {
                self.heard.voice.add(frame);
            } else {
                self.heard.quiet.add(frame);
            }
            let warm = self.heard.length > WARM_UP;
            if let Some(found) = self.timing.push(voice && warm, FRAME) {
                if found == Edge::Start {
                    self.heard.speech_at = Some(self.heard.length);
                }
                edge = Some(found);
            }
        }
        self.pending.drain(..used);
        edge
    }

    #[must_use]
    pub fn heard(&self) -> Heard {
        self.heard
    }
}

/// How long 16-bit mono samples at 16 kHz last.
#[must_use]
pub fn length(pcm: &[u8]) -> Duration {
    #[allow(clippy::cast_precision_loss)]
    Duration::from_secs_f64((pcm.len() / 2) as f64 / RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(timing: &mut Timing, voice: bool, frames: usize) -> Vec<Edge> {
        (0..frames)
            .filter_map(|_| timing.push(voice, FRAME))
            .collect()
    }

    #[test]
    fn silence_speech_then_the_end_of_the_sentence() {
        let mut timing = Timing::default();
        assert!(run(&mut timing, false, 60).is_empty());
        assert_eq!(run(&mut timing, true, 30), [Edge::Start]);
        assert!(run(&mut timing, false, 40).is_empty());
        assert_eq!(run(&mut timing, false, 10), [Edge::End]);
    }

    #[test]
    fn a_click_is_not_speech() {
        let mut timing = Timing::default();
        assert!(run(&mut timing, true, 3).is_empty());
        assert!(run(&mut timing, false, 60).is_empty());
    }

    #[test]
    fn a_pause_inside_a_sentence_does_not_end_it() {
        let mut timing = Timing::default();
        run(&mut timing, true, 30);
        assert!(run(&mut timing, false, 35).is_empty());
        assert!(run(&mut timing, true, 10).is_empty());
        assert_eq!(run(&mut timing, false, 50), [Edge::End]);
    }

    /// Samples from a fixed seed: the same noise on every run.
    fn noise(samples: usize, amplitude: f64, seed: u32) -> Vec<i16> {
        let mut state = seed;
        (0..samples)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let unit = f64::from(state >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0;
                #[allow(clippy::cast_possible_truncation)]
                let sample = (unit * amplitude * 32767.0) as i16;
                sample
            })
            .collect()
    }

    fn bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn a_hum_is_not_a_voice_even_while_the_detector_warms_up() {
        // Two seconds of a fan's noise, far above the old energy threshold.
        for (amplitude, seed) in [(0.05, 1), (0.05, 7), (0.1, 42), (0.2, 7)] {
            let mut vad = Vad::default();
            let pcm = bytes(&noise(32_000, amplitude, seed));
            let edges: Vec<Edge> = pcm.chunks(1024).filter_map(|c| vad.push(c)).collect();
            let heard = vad.heard();
            assert!(edges.is_empty(), "{amplitude} {seed}: {edges:?} {heard:?}");
            assert_eq!(heard.length, Duration::from_secs(2));
            assert!(heard.quiet_db().is_some_and(|db| db > -40.0), "{heard:?}");
        }
    }

    /// A vowel: a 200 Hz voice through the formants of « a », rising and
    /// falling four times a second like syllables.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn vowel(samples: usize, amplitude: f64) -> Vec<i16> {
        use std::f64::consts::PI;
        let rate = 16_000.0;
        let mut out: Vec<f64> = (0..samples)
            .map(|n| if n % 80 == 0 { 1.0 } else { 0.0 })
            .collect();
        for (formant, width) in [(700.0, 110.0), (1200.0, 120.0), (2500.0, 160.0)] {
            let r = (-PI * width / rate).exp();
            let c = 2.0 * r * (2.0 * PI * formant / rate).cos();
            let (mut y1, mut y2) = (0.0, 0.0);
            for x in &mut out {
                let y = *x + c * y1 - r * r * y2;
                (y2, y1) = (y1, y);
                *x = y;
            }
        }
        for (n, x) in out.iter_mut().enumerate() {
            *x *= 0.6 + 0.4 * (2.0 * PI * 4.0 * n as f64 / rate).sin();
        }
        let peak = out.iter().fold(0f64, |m, x| m.max(x.abs()));
        out.iter()
            .map(|x| (x / peak * amplitude * 32767.0) as i16)
            .collect()
    }

    #[test]
    fn a_voice_over_a_hum_starts_then_ends_a_sentence() {
        let hum = noise(56_000, 0.02, 9);
        let mut audio = hum[..16_000].to_vec();
        let voice = vowel(24_000, 0.3);
        audio.extend(
            voice
                .iter()
                .zip(&hum[16_000..])
                .map(|(v, h)| v.saturating_add(*h)),
        );
        audio.extend_from_slice(&hum[40_000..]);
        let mut vad = Vad::default();
        let mut edges = Vec::new();
        for chunk in bytes(&audio).chunks(1024) {
            edges.extend(vad.push(chunk));
        }
        let heard = vad.heard();
        assert_eq!(edges, [Edge::Start, Edge::End], "{heard:?}");
        // The voice starts at 1 s and lasts 1.5 s.
        assert!(
            heard
                .speech_at
                .is_some_and(|at| at > Duration::from_secs(1) && at < Duration::from_millis(1_300)),
            "{heard:?}"
        );
        assert!(heard.voice_db() > heard.quiet_db(), "{heard:?}");
    }

    #[test]
    fn chunks_of_any_length_make_the_same_frames() {
        let pcm = bytes(&noise(4_000, 0.05, 3));
        let mut whole = Vad::default();
        whole.push(&pcm);
        let mut pieces = Vad::default();
        for chunk in pcm.chunks(300) {
            pieces.push(chunk);
        }
        // 4000 samples: 15 frames, 160 samples left for later.
        assert_eq!(whole.heard().length, Duration::from_millis(16 * 15));
        assert_eq!(pieces.heard().length, whole.heard().length);
        assert!((pieces.heard().best - whole.heard().best).abs() < 1e-6);
        assert_eq!(pieces.pending.len(), 160);
    }

    #[test]
    fn digital_silence_is_quiet() {
        let mut vad = Vad::default();
        assert_eq!(vad.push(&vec![0u8; 16_000]), None);
        assert_eq!(vad.heard().quiet_db(), Some(-120.0));
        assert_eq!(length(&[0u8; 1024]), Duration::from_millis(32));
    }
}

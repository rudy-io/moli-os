//! Is someone speaking? Energy against an adaptive noise floor, frame by
//! frame: the dashboard's `vad.js`, same thresholds, in milliseconds so
//! that any frame length works (the Voice PE sends 32 ms).

use std::time::Duration;

/// What a frame changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

#[derive(Clone, Debug)]
pub struct Vad {
    factor: f32,
    min_level: f32,
    start: Duration,
    end: Duration,
    floor: f32,
    speaking: bool,
    loud: Duration,
    quiet: Duration,
}

impl Default for Vad {
    fn default() -> Self {
        Self {
            factor: 3.0,
            min_level: 0.012,
            start: Duration::from_millis(150),
            // 700 ms of silence ends a sentence (a pause inside one is shorter).
            end: Duration::from_millis(700),
            floor: 0.004,
            speaking: false,
            loud: Duration::ZERO,
            quiet: Duration::ZERO,
        }
    }
}

impl Vad {
    fn threshold(&self) -> f32 {
        (self.floor * self.factor).max(self.min_level)
    }

    /// One frame's level (RMS, 0 to 1) and length.
    pub fn push(&mut self, rms: f32, frame: Duration) -> Option<Edge> {
        let loud = rms > self.threshold();
        if !self.speaking {
            if !loud {
                self.floor = self.floor * 0.97 + rms * 0.03;
            }
            self.loud = if loud {
                self.loud + frame
            } else {
                Duration::ZERO
            };
            if self.loud >= self.start {
                self.speaking = true;
                self.quiet = Duration::ZERO;
                return Some(Edge::Start);
            }
            return None;
        }
        self.quiet = if loud {
            Duration::ZERO
        } else {
            self.quiet + frame
        };
        if self.quiet >= self.end {
            self.speaking = false;
            self.loud = Duration::ZERO;
            return Some(Edge::End);
        }
        None
    }
}

/// 16-bit little-endian mono samples: their RMS, 0 to 1, and how long they last at `rate`.
#[must_use]
pub fn level(pcm: &[u8], rate: u32) -> (f32, Duration) {
    let samples = pcm.len() / 2;
    if samples == 0 {
        return (0.0, Duration::ZERO);
    }
    let sum: f64 = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            let s = f64::from(i16::from_le_bytes(*b)) / 32768.0;
            s * s
        })
        .sum();
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let rms = (sum / samples as f64).sqrt() as f32;
    #[allow(clippy::cast_precision_loss)]
    let length = Duration::from_secs_f64(samples as f64 / f64::from(rate));
    (rms, length)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(20);

    fn run(vad: &mut Vad, level: f32, frames: usize) -> Vec<Edge> {
        (0..frames).filter_map(|_| vad.push(level, FRAME)).collect()
    }

    #[test]
    fn silence_speech_then_the_end_of_the_sentence() {
        let mut vad = Vad::default();
        assert!(run(&mut vad, 0.002, 50).is_empty());
        assert_eq!(run(&mut vad, 0.08, 20), [Edge::Start]);
        assert!(run(&mut vad, 0.002, 30).is_empty());
        assert_eq!(run(&mut vad, 0.002, 30), [Edge::End]);
    }

    #[test]
    fn a_click_is_not_speech() {
        let mut vad = Vad::default();
        run(&mut vad, 0.002, 50);
        assert!(run(&mut vad, 0.3, 3).is_empty());
        assert!(run(&mut vad, 0.002, 50).is_empty());
    }

    #[test]
    fn a_pause_inside_a_sentence_does_not_end_it() {
        let mut vad = Vad::default();
        run(&mut vad, 0.002, 50);
        run(&mut vad, 0.08, 20);
        assert!(run(&mut vad, 0.002, 25).is_empty());
        assert!(run(&mut vad, 0.08, 10).is_empty());
        assert_eq!(run(&mut vad, 0.002, 60), [Edge::End]);
    }

    #[test]
    fn the_noise_floor_follows_a_steady_hum() {
        let mut vad = Vad::default();
        assert!(run(&mut vad, 0.008, 400).is_empty());
        assert!(vad.threshold() > 0.02);
        assert_eq!(run(&mut vad, 0.1, 20), [Edge::Start]);
    }

    #[test]
    fn levels_of_pcm() {
        let half: Vec<u8> = std::iter::repeat_n(16_384i16.to_le_bytes(), 512)
            .flatten()
            .collect();
        let (rms, length) = level(&half, 16_000);
        assert!((rms - 0.5).abs() < 1e-3, "{rms}");
        assert_eq!(length, Duration::from_millis(32));
        assert_eq!(level(&[], 16_000), (0.0, Duration::ZERO));
    }
}

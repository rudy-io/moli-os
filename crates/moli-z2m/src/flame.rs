//! A flame the driver plays itself. No IO.
//!
//! Most Zigbee colour lamps have no animation of their own (the Zigbee
//! `effect` is a blink to find the lamp, not a look). The driver plays this
//! one for them: every second or two, a slow fade to a slightly different
//! amber and level around the brightness the person chose. It is gentle on
//! purpose: a glow that lives, not a strobe, and one order at a time so the
//! Zigbee network stays free for everything else.

use std::time::Duration;

/// The value of `effect` that starts the flame.
pub const FLAME: &str = "flame";
/// The value of `effect` that stops it (the word Hue uses).
pub const NO_EFFECT: &str = "no_effect";

/// Pause after a fade, before the next one.
const REST: Duration = Duration::from_millis(300);

/// One lamp's flame.
#[derive(Debug)]
pub struct Flame {
    /// Percent: the level the flame moves around.
    base: f64,
    seed: u64,
}

/// One fade: where the lamp goes next, and how slowly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Percent.
    pub brightness: f64,
    /// Degrees.
    pub hue: u64,
    /// Percent.
    pub saturation: u64,
    pub transition: Duration,
}

impl Frame {
    /// How long until the next frame.
    #[must_use]
    pub fn period(&self) -> Duration {
        self.transition + REST
    }
}

impl Flame {
    /// A flame around `base` percent. `seed`: anything that differs from one
    /// lamp to the next, so two lamps never move together.
    #[must_use]
    pub fn new(base: f64, seed: u64) -> Self {
        Self {
            base: base.clamp(1.0, 100.0),
            // xorshift never leaves zero.
            seed: seed | 1,
        }
    }

    #[must_use]
    pub fn base(&self) -> f64 {
        self.base
    }

    /// The person moved the brightness: the flame follows.
    pub fn set_base(&mut self, base: f64) {
        self.base = base.clamp(1.0, 100.0);
    }

    /// A number in `0.0..1.0` (xorshift64).
    #[allow(clippy::cast_precision_loss)]
    fn roll(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The next fade.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn frame(&mut self) -> Frame {
        // One time in ten the flame sinks a little, as a real one does.
        let factor = if self.roll() < 0.1 {
            0.68 + 0.12 * self.roll()
        } else {
            0.82 + 0.40 * self.roll()
        };
        Frame {
            brightness: (self.base * factor).clamp(1.0, 100.0),
            hue: 24 + (self.roll() * 9.0) as u64,
            saturation: 85 + (self.roll() * 11.0) as u64,
            transition: Duration::from_millis(1000 + (self.roll() * 1001.0) as u64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flame_stays_gentle_and_amber() {
        let mut flame = Flame::new(22.0, 42);
        let mut low = f64::MAX;
        let mut high = 0.0_f64;
        for _ in 0..2000 {
            let fade = flame.frame();
            low = low.min(fade.brightness);
            high = high.max(fade.brightness);
            assert!((24..=32).contains(&fade.hue), "{}", fade.hue);
            assert!((85..=95).contains(&fade.saturation), "{}", fade.saturation);
            assert!(fade.transition >= Duration::from_secs(1));
            assert!(fade.period() <= Duration::from_millis(2300));
        }
        // Around 22 %: never below 14 %, never above 27 %, and it does move.
        assert!((22.0 * 0.68..18.0).contains(&low), "{low}");
        assert!(high > 25.0 && high <= 22.0 * 1.22, "{high}");
    }

    #[test]
    fn it_follows_the_brightness_and_stays_in_range() {
        let mut flame = Flame::new(250.0, 0);
        assert!((flame.base() - 100.0).abs() < f64::EPSILON);
        assert!((0..200).all(|_| flame.frame().brightness <= 100.0));
        flame.set_base(0.0);
        assert!((0..200).all(|_| flame.frame().brightness >= 1.0));
    }

    #[test]
    fn two_lamps_do_not_move_together() {
        let (mut a, mut b) = (Flame::new(50.0, 1), Flame::new(50.0, 2));
        assert!((0..20).any(|_| a.frame() != b.frame()));
    }
}

//! Samples from one form to another: another rate, G.711 µ-law, a WAV file.

/// The resampling filter's length.
const TAPS: usize = 63;

/// `samples` at `to` per second: a windowed-sinc low-pass under the new
/// Nyquist frequency when the rate drops, then linear interpolation.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let filtered: Vec<f64> = if from > to {
        let cutoff = 0.45 / ratio; // of the input rate
        let middle = (TAPS / 2) as f64;
        let kernel: Vec<f64> = (0..TAPS)
            .map(|n| {
                let x = n as f64 - middle;
                let sinc = if x == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let window =
                    0.54 - 0.46 * (2.0 * std::f64::consts::PI * n as f64 / (TAPS - 1) as f64).cos();
                sinc * window
            })
            .collect();
        (0..samples.len())
            .map(|i| {
                kernel
                    .iter()
                    .enumerate()
                    .map(|(k, h)| {
                        (i + k)
                            .checked_sub(TAPS / 2)
                            .and_then(|j| samples.get(j))
                            .map_or(0.0, |s| f64::from(*s) * h)
                    })
                    .sum()
            })
            .collect()
    } else {
        samples.iter().map(|s| f64::from(*s)).collect()
    };
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let at = i as f64 * ratio;
            let j = at.floor() as usize;
            let frac = at - j as f64;
            let a = filtered[j.min(filtered.len() - 1)];
            let b = filtered[(j + 1).min(filtered.len() - 1)];
            (a + (b - a) * frac)
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// G.711 µ-law of one sample.
#[must_use]
pub fn ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32_635;
    let mut s = i32::from(sample);
    let sign = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    s = s.min(CLIP) + BIAS;
    let mut exponent = 7;
    let mut mask = 0x4000;
    while s & mask == 0 && exponent > 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (s >> (exponent + 3)) & 0x0F;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = !(sign | (exponent << 4) | mantissa) as u8;
    byte
}

/// A 16-bit mono WAV file of `samples`.
#[must_use]
pub fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX - 36);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Samples as 16-bit little-endian bytes.
#[must_use]
pub fn bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mu_law_matches_g711() {
        assert_eq!(ulaw(0), 0xFF);
        assert_eq!(ulaw(i16::MAX), 0x80);
        assert_eq!(ulaw(i16::MIN), 0x00);
        assert_eq!(ulaw(-1), 0x7F);
        assert_eq!(ulaw(1000) & 0x80, 0x80, "positive keeps the sign bit");
    }

    #[test]
    fn the_voice_comes_down_to_eight_kilohertz() {
        let second = vec![1000i16; 24_000];
        let out = resample(&second, 24_000, 8_000);
        assert_eq!(out.len(), 8_000);
        // A steady level stays (the filter's gain is 1), away from the edges.
        assert!((i32::from(out[4_000]) - 1000).abs() < 20, "{}", out[4_000]);
        // A tone above 4 kHz is filtered out, one below stays.
        let tone = |hz: f64| -> Vec<i16> {
            (0..24_000)
                .map(|i| {
                    let x = 2.0 * std::f64::consts::PI * hz * f64::from(i) / 24_000.0;
                    #[allow(clippy::cast_possible_truncation)]
                    let s = (10_000.0 * x.sin()) as i16;
                    s
                })
                .collect()
        };
        let peak = |s: &[i16]| {
            s[1000..7000]
                .iter()
                .map(|x| x.unsigned_abs())
                .max()
                .unwrap()
        };
        assert!(peak(&resample(&tone(6_000.0), 24_000, 8_000)) < 1_500);
        assert!(peak(&resample(&tone(500.0), 24_000, 8_000)) > 9_000);
    }

    #[test]
    fn a_wav_says_its_rate_and_length() {
        let w = wav(&[1, -1, 2], 16_000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(w.len(), 50);
        assert_eq!(bytes(&[1, -1]), [1, 0, 0xff, 0xff]);
    }
}

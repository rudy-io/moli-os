//! Sounds the satellite needs from Moli: what it heard, as a WAV for the
//! transcription, and the end-of-conversation chime, made here (no file).

/// 16-bit mono PCM in a WAV container.
#[must_use]
pub fn wav(pcm: &[u8], rate: u32) -> Vec<u8> {
    let data = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&data.saturating_add(36).to_le_bytes());
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
    out.extend_from_slice(pcm);
    out
}

/// Two soft notes going down (A5 then E5): the conversation is over.
#[must_use]
pub fn chime() -> Vec<u8> {
    const RATE: u32 = 22_050;
    let mut pcm = Vec::new();
    for (freq, ms) in [(880.0_f64, 130_u32), (659.25, 200)] {
        let n = RATE * ms / 1000;
        for i in 0..n {
            let t = f64::from(i) / f64::from(RATE);
            let pos = f64::from(i) / f64::from(n);
            // A quick attack, then a decay: no click at either end.
            let envelope = (pos * 20.0).min(1.0) * (1.0 - pos).powi(2);
            let s = (2.0 * std::f64::consts::PI * freq * t).sin() * envelope * 0.25;
            #[allow(clippy::cast_possible_truncation)]
            let sample = (s * f64::from(i16::MAX)) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    wav(&pcm, RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_chime() {
        let w = wav(&[1, 0, 2, 0], 16_000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 4);
        assert_eq!(w.len(), 48);
        let c = chime();
        // 330 ms at 22 050 Hz, 2 bytes a sample, plus the header.
        assert_eq!(
            c.len(),
            44 + 2 * (22_050 * 130 / 1000 + 22_050 * 200 / 1000) as usize
        );
        let first = i16::from_le_bytes([c[44], c[45]]);
        assert_eq!(first, 0, "starts silent");
    }
}

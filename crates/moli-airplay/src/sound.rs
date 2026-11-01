//! What RAOP carries: 44.1 kHz, 16-bit, stereo, little-endian (not the
//! big-endian of RTP's L16: the receiver plays that as noise).

use anyhow::{Context as _, bail, ensure};

/// The rate a RAOP receiver plays.
pub const RATE: u32 = 44_100;
/// The rates a WAV may come at: anything else is not speech (and a rate
/// of 1 Hz would make the resampling grow out of memory).
const RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
/// Silence after the words: the end of a sentence must never sit where the
/// session's close can take it.
const CUSHION_MS: u32 = 1_500;

/// A 16-bit PCM WAV (mono or stereo, any rate: Piper's is 22 050 Hz mono)
/// as RAOP's bytes, the cushion of silence included.
///
/// # Errors
/// Not a 16-bit PCM WAV, or one with no sound in it.
pub fn from_wav(wav: &[u8]) -> anyhow::Result<Vec<u8>> {
    let (channels, rate, data) = parse(wav)?;
    let mono: Vec<i16> = data
        .chunks_exact(2 * usize::from(channels))
        .map(|frame| {
            let left = i16::from_le_bytes([frame[0], frame[1]]);
            if channels == 1 {
                left
            } else {
                let right = i16::from_le_bytes([frame[2], frame[3]]);
                // The mean of two i16 fits in an i16.
                #[allow(clippy::cast_possible_truncation)]
                let mean = i32::midpoint(i32::from(left), i32::from(right)) as i16;
                mean
            }
        })
        .collect();
    ensure!(!mono.is_empty(), "the WAV holds no sound");
    let samples = if rate == RATE {
        mono
    } else {
        moli_audio::pcm::resample(&mono, rate, RATE)
    };
    let cushion = (RATE * CUSHION_MS / 1000) as usize;
    let mut out = Vec::with_capacity((samples.len() + cushion) * 4);
    for s in samples {
        let b = s.to_le_bytes();
        out.extend_from_slice(&[b[0], b[1], b[0], b[1]]);
    }
    out.resize(out.len() + cushion * 4, 0);
    Ok(out)
}

/// Channels, rate and the sample bytes of a 16-bit PCM WAV.
fn parse(wav: &[u8]) -> anyhow::Result<(u16, u32, &[u8])> {
    ensure!(
        wav.len() >= 12 && &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE",
        "not a WAV file"
    );
    let mut format = None;
    let mut at = 12;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let size = u32::from_le_bytes(wav[at + 4..at + 8].try_into()?) as usize;
        let body = wav
            .get(at + 8..(at + 8).saturating_add(size).min(wav.len()))
            .context("truncated WAV")?;
        match id {
            b"fmt " => {
                ensure!(body.len() >= 16, "short WAV format");
                let kind = u16::from_le_bytes([body[0], body[1]]);
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let rate = u32::from_le_bytes(body[4..8].try_into()?);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                ensure!(
                    kind == 1 && bits == 16 && (channels == 1 || channels == 2),
                    "only 16-bit PCM WAV, mono or stereo"
                );
                ensure!(RATES.contains(&rate), "a WAV at {rate} Hz is not speech");
                format = Some((channels, rate));
            }
            b"data" => {
                let (channels, rate) = format.context("WAV data before its format")?;
                return Ok((channels, rate, body));
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        at = at
            .checked_add(8)
            .and_then(|a| a.checked_add(size))
            .and_then(|a| a.checked_add(size & 1))
            .context("a WAV chunk too large")?;
    }
    bail!("the WAV has no data")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut w = b"RIFF".to_vec();
        w.extend_from_slice(&u32::try_from(36 + data.len()).unwrap().to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 2 * u32::from(channels)).to_le_bytes());
        w.extend_from_slice(&(2 * channels).to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        w.extend_from_slice(&data);
        w
    }

    #[test]
    fn a_44k_mono_sample_becomes_two_little_endian_channels_then_silence() {
        let out = from_wav(&wav(1, RATE, &[0x0102, -2])).unwrap();
        assert_eq!(&out[..8], &[0x02, 0x01, 0x02, 0x01, 0xFE, 0xFF, 0xFE, 0xFF]);
        let cushion = (RATE * CUSHION_MS / 1000) as usize * 4;
        assert_eq!(out.len(), 8 + cushion);
        assert!(out[8..].iter().all(|b| *b == 0));
    }

    #[test]
    fn stereo_is_averaged_and_pipers_rate_is_doubled() {
        let out = from_wav(&wav(2, RATE, &[100, 300])).unwrap();
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 200);
        let piper = from_wav(&wav(1, 22_050, &[1000; 2205])).unwrap();
        let cushion = (RATE * CUSHION_MS / 1000) as usize * 4;
        let frames = (piper.len() - cushion) / 4;
        assert!((4400..=4420).contains(&frames), "{frames}");
    }

    #[test]
    fn what_is_not_16_bit_pcm_is_refused() {
        assert!(from_wav(b"nothing").is_err());
        let mut eight_bit = wav(1, RATE, &[1, 2]);
        eight_bit[34] = 8;
        assert!(from_wav(&eight_bit).is_err());
        assert!(from_wav(&wav(1, RATE, &[])).is_err());
        assert!(from_wav(&wav(1, 1, &[1, 2])).is_err(), "1 Hz is not speech");
        assert!(from_wav(&wav(1, 400_000, &[1, 2])).is_err());
    }

    #[test]
    fn a_chunk_that_claims_more_than_memory_holds_is_refused() {
        let mut w = wav(1, RATE, &[1, 2]);
        // An unknown chunk of 4 GiB before the format.
        let mut huge = b"junk".to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        w.splice(12..12, huge);
        assert!(from_wav(&w).is_err());
    }
}

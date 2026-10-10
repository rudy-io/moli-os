//! AAC as cameras send it over RTP (`MPEG4-GENERIC`, RFC 3640, `AAC-hbr`):
//! each packet holds AU headers (each unit's size), then the units, which
//! are raw AAC frames; decoded to 16-bit samples by Symphonia's decoder,
//! from the stream's `AudioSpecificConfig` (the SDP's `config=`).

use anyhow::{Context as _, bail};
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder as _, AudioDecoderOptions};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, Timestamp};

/// Samples in an AAC frame.
const FRAME: u64 = 1024;
/// A unit split over packets never grows past this.
const MAX_UNIT: usize = 16 * 1024;

/// What the SDP's `a=fmtp:` says about the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Format {
    /// The `AudioSpecificConfig`.
    pub config: Vec<u8>,
    /// Bits of an AU header giving the unit's size (13 for `AAC-hbr`).
    pub size_length: u32,
    /// Bits after it (3 for `AAC-hbr`).
    pub index_length: u32,
}

impl Format {
    /// From an `a=fmtp:` line (or just its parameters).
    #[must_use]
    pub fn parse(fmtp: &str) -> Option<Self> {
        let params = fmtp.split_once(' ').map_or(fmtp, |(_, p)| p);
        let mut format = Self {
            config: Vec::new(),
            size_length: 13,
            index_length: 3,
        };
        for pair in params.split(';') {
            let Some((key, value)) = pair.trim().split_once('=') else {
                continue;
            };
            match key.trim().to_ascii_lowercase().as_str() {
                "config" => format.config = hex(value.trim())?,
                "sizelength" => format.size_length = value.trim().parse().ok()?,
                "indexlength" => format.index_length = value.trim().parse().ok()?,
                _ => {}
            }
        }
        (!format.config.is_empty()).then_some(format)
    }

    /// The sample rate the config says (`None` for an unusual one).
    #[must_use]
    pub fn rate(&self) -> Option<u32> {
        const RATES: [u32; 13] = [
            96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
            8_000, 7_350,
        ];
        let index = ((self.config.first()? & 0x07) << 1) | (self.config.get(1)? >> 7);
        RATES.get(usize::from(index)).copied()
    }
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 == 1 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// RTP payloads in, 16-bit mono samples out.
pub struct Decoder {
    aac: Box<AacDecoder>,
    format: Format,
    /// A unit that started in an earlier packet: its size and what came.
    partial: Option<(usize, Vec<u8>)>,
    frames: u64,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("format", &self.format)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl Decoder {
    /// # Errors
    /// When the config is not an AAC one Symphonia decodes.
    pub fn new(format: Format) -> anyhow::Result<Self> {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_extra_data(format.config.clone().into_boxed_slice());
        // Every frame kept: the stream has no delay to trim.
        let mut options = AudioDecoderOptions::default();
        options.gapless = false;
        let aac = AacDecoder::try_new(&params, &options).context("AAC decoder")?;
        Ok(Self {
            aac: Box::new(aac),
            format,
            partial: None,
            frames: 0,
        })
    }

    /// One RTP payload: the samples of every whole unit it completes (a
    /// unit that does not decode is skipped, the stream goes on).
    pub fn push(&mut self, payload: &[u8]) -> Vec<i16> {
        let mut out = Vec::new();
        let units = match self.partial.take() {
            Some((size, mut so_far)) => {
                so_far.extend_from_slice(payload);
                if so_far.len() < size && so_far.len() < MAX_UNIT {
                    self.partial = Some((size, so_far));
                    return out;
                }
                so_far.truncate(size);
                vec![so_far]
            }
            None => match units(payload, &self.format) {
                Ok((whole, partial)) => {
                    self.partial = partial;
                    whole
                }
                Err(_) => return out,
            },
        };
        for unit in units {
            let packet = Packet::new(
                0,
                Timestamp::new(i64::try_from(self.frames * FRAME).unwrap_or(i64::MAX)),
                Duration::new(FRAME),
                unit,
            );
            self.frames += 1;
            if let Ok(decoded) = self.aac.decode(&packet) {
                let mut samples: Vec<i16> = Vec::new();
                decoded.copy_to_vec_interleaved(&mut samples);
                out.extend(samples);
            }
        }
        out
    }
}

/// A payload's whole units, and a unit it only starts (its size, its
/// first bytes).
#[allow(clippy::type_complexity)]
fn units(
    payload: &[u8],
    format: &Format,
) -> anyhow::Result<(Vec<Vec<u8>>, Option<(usize, Vec<u8>)>)> {
    let per_unit = format.size_length + format.index_length;
    if payload.len() < 2 || per_unit == 0 || per_unit > 32 {
        bail!("not an AAC-hbr payload");
    }
    let all_headers = u32::from(u16::from_be_bytes([payload[0], payload[1]]));
    let headers_bytes = usize::try_from(all_headers.div_ceil(8))?;
    let count = all_headers / per_unit;
    let headers = payload
        .get(2..2 + headers_bytes)
        .context("AU headers cut short")?;
    let mut data = payload.get(2 + headers_bytes..).unwrap_or_default();
    let mut whole = Vec::new();
    for i in 0..count {
        let size = usize::try_from(bits(headers, i * per_unit, format.size_length))?;
        if size <= data.len() {
            whole.push(data[..size].to_vec());
            data = &data[size..];
        } else {
            // The unit goes on in the next packets (one unit per packet then).
            return Ok((whole, Some((size, data.to_vec()))));
        }
    }
    Ok((whole, None))
}

/// `count` bits from bit `at` (big-endian).
fn bits(bytes: &[u8], at: u32, count: u32) -> u32 {
    (0..count).fold(0, |value, i| {
        let bit = at + i;
        let byte = bytes
            .get(usize::try_from(bit / 8).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(0);
        (value << 1) | u32::from((byte >> (7 - bit % 8)) & 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sdp_gives_the_stream_format() {
        let f = Format::parse(
            "97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1408",
        )
        .unwrap();
        assert_eq!(f.config, [0x14, 0x08]);
        assert_eq!((f.size_length, f.index_length), (13, 3));
        assert_eq!(f.rate(), Some(16_000));
        assert_eq!(Format::parse("97 mode=AAC-hbr"), None);
    }

    #[test]
    fn units_are_cut_by_their_headers() {
        let f = Format::parse("config=1408").unwrap();
        // Two headers (32 bits): sizes 3 and 2, index 0.
        let payload = [0x00, 0x20, 0x00, 0x18, 0x00, 0x10, 1, 2, 3, 4, 5];
        let (whole, partial) = units(&payload, &f).unwrap();
        assert_eq!(whole, vec![vec![1, 2, 3], vec![4, 5]]);
        assert!(partial.is_none());
        // One unit of 6 bytes, 2 here: the rest comes next.
        let start = [0x00, 0x10, 0x00, 0x30, 9, 9];
        let (whole, partial) = units(&start, &f).unwrap();
        assert!(whole.is_empty());
        assert_eq!(partial, Some((6, vec![9, 9])));
    }

    #[test]
    fn a_decoder_starts_from_the_cameras_config() {
        let mut d = Decoder::new(Format::parse("config=1408").unwrap()).unwrap();
        // Garbage does not stop the stream: whole frames or nothing.
        for _ in 0..3 {
            let out = d.push(&[0x00, 0x10, 0x00, 0x18, 0xff, 0xff, 0xff]);
            assert_eq!(out.len() % 1024, 0, "{}", out.len());
        }
        assert!(d.push(&[0x00]).is_empty(), "too short for AU headers");
    }
}

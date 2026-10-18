//! ASH frame codec: CRC, data randomization, byte stuffing, frame parsing.

/// A decoded or to-be-encoded ASH frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// EZSP payload (randomized on the wire; here in the clear).
    Data {
        frm: u8,
        re_tx: bool,
        ack: u8,
        payload: Vec<u8>,
    },
    Ack {
        ack: u8,
        not_ready: bool,
    },
    Nak {
        ack: u8,
        not_ready: bool,
    },
    Rst,
    RstAck {
        version: u8,
        reset_code: u8,
    },
    Error {
        version: u8,
        code: u8,
    },
}

/// Why a frame was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AshError {
    /// The CRC did not match, or the frame was corrupted on the line
    /// (substitute byte, oversize).
    Crc,
    /// Fewer bytes than a control byte and a CRC.
    TooShort,
    /// Unknown control byte, or a body that does not fit its kind.
    BadControl(u8),
    /// A cancel byte interrupted a frame in progress.
    Cancelled,
}

const FLAG: u8 = 0x7E;
const ESCAPE: u8 = 0x7D;
const XON: u8 = 0x11;
const XOFF: u8 = 0x13;
const SUBSTITUTE: u8 = 0x18;
pub(super) const CANCEL: u8 = 0x1A;
/// Bytes that must not appear bare on the wire.
const RESERVED: [u8; 6] = [FLAG, ESCAPE, XON, XOFF, SUBSTITUTE, CANCEL];
/// Longest frame body kept (control byte, data and CRC, unstuffed). An ASH
/// frame is far smaller; anything bigger is line noise.
const MAX_FRAME: usize = 256;

/// CRC-CCITT (init `0xFFFF`, polynomial `0x1021`, most significant bit first).
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}

/// XOR `data` with the ASH pseudo-random sequence; applying it twice restores it.
pub fn randomize(data: &mut [u8]) {
    let mut rand: u8 = 0x42;
    for byte in data {
        *byte ^= rand;
        rand = if rand & 1 == 0 {
            rand >> 1
        } else {
            (rand >> 1) ^ 0xB8
        };
    }
}

/// Append the CRC to a frame body, byte-stuff it and end it with the flag byte.
fn seal(body: &[u8]) -> Vec<u8> {
    let mut wire = Vec::with_capacity(body.len() + 4);
    for byte in body.iter().copied().chain(crc16(body).to_be_bytes()) {
        if RESERVED.contains(&byte) {
            wire.extend_from_slice(&[ESCAPE, byte ^ 0x20]);
        } else {
            wire.push(byte);
        }
    }
    wire.push(FLAG);
    wire
}

impl Frame {
    /// Wire form: control byte, data, CRC, all byte-stuffed, then the flag byte.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            Self::Data {
                frm,
                re_tx,
                ack,
                payload,
            } => {
                body.push((frm & 7) << 4 | u8::from(*re_tx) << 3 | (ack & 7));
                let start = body.len();
                body.extend_from_slice(payload);
                randomize(&mut body[start..]);
            }
            Self::Ack { ack, not_ready } => {
                body.push(0x80 | u8::from(*not_ready) << 3 | (ack & 7));
            }
            Self::Nak { ack, not_ready } => {
                body.push(0xA0 | u8::from(*not_ready) << 3 | (ack & 7));
            }
            Self::Rst => body.push(0xC0),
            Self::RstAck {
                version,
                reset_code,
            } => body.extend_from_slice(&[0xC1, *version, *reset_code]),
            Self::Error { version, code } => body.extend_from_slice(&[0xC2, *version, *code]),
        }
        seal(&body)
    }

    /// Parse an unstuffed frame body (control byte, data, CRC).
    fn parse(raw: &[u8]) -> Result<Self, AshError> {
        if raw.len() < 3 {
            return Err(AshError::TooShort);
        }
        let (body, crc) = raw.split_at(raw.len() - 2);
        if crc16(body) != u16::from_be_bytes([crc[0], crc[1]]) {
            return Err(AshError::Crc);
        }
        let (control, rest) = (body[0], &body[1..]);
        let ack = control & 7;
        let not_ready = control & 8 != 0;
        match control {
            0x00..=0x7F => {
                let mut payload = rest.to_vec();
                randomize(&mut payload);
                Ok(Self::Data {
                    frm: control >> 4 & 7,
                    re_tx: not_ready,
                    ack,
                    payload,
                })
            }
            0x80..=0x9F if control & 0x10 == 0 && rest.is_empty() => {
                Ok(Self::Ack { ack, not_ready })
            }
            0xA0..=0xBF if control & 0x10 == 0 && rest.is_empty() => {
                Ok(Self::Nak { ack, not_ready })
            }
            0xC0 if rest.is_empty() => Ok(Self::Rst),
            0xC1 if rest.len() == 2 => Ok(Self::RstAck {
                version: rest[0],
                reset_code: rest[1],
            }),
            0xC2 if rest.len() == 2 => Ok(Self::Error {
                version: rest[0],
                code: rest[1],
            }),
            _ => Err(AshError::BadControl(control)),
        }
    }
}

/// Incremental frame decoder: feed it the serial bytes one at a time.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Unstuffed bytes of the frame in progress.
    buf: Vec<u8>,
    /// The previous byte was an escape.
    escaped: bool,
    /// The frame in progress is corrupt: skip to the next flag.
    corrupt: bool,
}

impl Decoder {
    /// Consume one byte; returns a frame (or why it failed) when a flag byte
    /// or a cancel ends one.
    pub fn push(&mut self, byte: u8) -> Option<Result<Frame, AshError>> {
        match byte {
            FLAG => {
                self.escaped = false;
                let raw = std::mem::take(&mut self.buf);
                if std::mem::take(&mut self.corrupt) {
                    Some(Err(AshError::Crc))
                } else if raw.is_empty() {
                    None
                } else {
                    Some(Frame::parse(&raw))
                }
            }
            CANCEL => {
                let in_progress = !self.buf.is_empty() || self.corrupt;
                self.buf.clear();
                self.escaped = false;
                self.corrupt = false;
                in_progress.then_some(Err(AshError::Cancelled))
            }
            SUBSTITUTE => {
                self.buf.clear();
                self.escaped = false;
                self.corrupt = true;
                None
            }
            XON | XOFF => None,
            ESCAPE => {
                self.escaped = true;
                None
            }
            _ => {
                let byte = if std::mem::take(&mut self.escaped) {
                    byte ^ 0x20
                } else {
                    byte
                };
                if !self.corrupt {
                    if self.buf.len() < MAX_FRAME {
                        self.buf.push(byte);
                    } else {
                        self.buf.clear();
                        self.corrupt = true;
                    }
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(dec: &mut Decoder, bytes: &[u8]) -> Vec<Result<Frame, AshError>> {
        bytes.iter().filter_map(|&b| dec.push(b)).collect()
    }

    fn oks(results: Vec<Result<Frame, AshError>>) -> Vec<Frame> {
        results.into_iter().filter_map(Result::ok).collect()
    }

    const RSTACK: [u8; 6] = [0xC1, 0x02, 0x02, 0x9B, 0x7B, 0x7E];

    fn rstack() -> Frame {
        Frame::RstAck {
            version: 2,
            reset_code: 2,
        }
    }

    #[test]
    fn crc_vectors() {
        assert_eq!(crc16(&[0xC0]), 0x38BC);
        assert_eq!(crc16(&[0x81]), 0x6059);
        assert_eq!(crc16(&[0xA6]), 0x34DC);
        assert_eq!(crc16(&[0xC1, 0x02, 0x02]), 0x9B7B);
    }

    #[test]
    fn random_sequence() {
        let mut zeros = [0u8; 8];
        randomize(&mut zeros);
        assert_eq!(zeros, [0x42, 0x21, 0xA8, 0x54, 0x2A, 0x15, 0xB2, 0x59]);
        randomize(&mut zeros);
        assert_eq!(zeros, [0u8; 8]);
    }

    #[test]
    fn encode_control_frames() {
        assert_eq!(Frame::Rst.encode(), [0xC0, 0x38, 0xBC, 0x7E]);
        assert_eq!(
            Frame::Ack {
                ack: 1,
                not_ready: false
            }
            .encode(),
            [0x81, 0x60, 0x59, 0x7E]
        );
        assert_eq!(
            Frame::Nak {
                ack: 6,
                not_ready: false
            }
            .encode(),
            [0xA6, 0x34, 0xDC, 0x7E]
        );
    }

    #[test]
    fn encode_legacy_version_request() {
        let frame = Frame::Data {
            frm: 0,
            re_tx: false,
            ack: 0,
            payload: vec![0x00, 0x00, 0x00, 0x0D],
        };
        assert_eq!(
            frame.encode(),
            [0x00, 0x42, 0x21, 0xA8, 0x59, 0x7C, 0x05, 0x7E]
        );
    }

    #[test]
    fn control_bits_round_trip() {
        let frames = [
            Frame::Data {
                frm: 5,
                re_tx: true,
                ack: 3,
                payload: vec![1, 2, 3],
            },
            Frame::Ack {
                ack: 7,
                not_ready: true,
            },
            Frame::Nak {
                ack: 2,
                not_ready: true,
            },
            Frame::Rst,
            rstack(),
            Frame::Error {
                version: 2,
                code: 0x51,
            },
        ];
        let mut dec = Decoder::default();
        for frame in frames {
            assert_eq!(oks(feed(&mut dec, &frame.encode())), [frame]);
        }
    }

    #[test]
    fn decode_rstack() {
        let mut dec = Decoder::default();
        let out = feed(&mut dec, &RSTACK);
        assert_eq!(out, [Ok(rstack())]);
    }

    #[test]
    fn stuffing_round_trip() {
        // The clear payload is chosen so the randomized one holds every reserved byte.
        let mut payload = vec![0x7E, 0x11, 0x7D, 0x13, 0x18, 0x1A];
        randomize(&mut payload);
        let frame = Frame::Data {
            frm: 1,
            re_tx: false,
            ack: 2,
            payload,
        };
        let wire = frame.encode();
        // Only the final flag may be a bare 7E; no bare cancel/substitute/xon/xoff.
        assert_eq!(wire.iter().position(|&b| b == 0x7E), Some(wire.len() - 1));
        for reserved in [0x11, 0x13, 0x18, 0x1A] {
            assert!(!wire.contains(&reserved));
        }
        let mut dec = Decoder::default();
        assert_eq!(oks(feed(&mut dec, &wire)), [frame]);
    }

    #[test]
    fn bad_crc_is_rejected() {
        let mut wire = RSTACK;
        wire[4] ^= 0x01;
        let mut dec = Decoder::default();
        assert_eq!(feed(&mut dec, &wire), [Err(AshError::Crc)]);
        // The decoder is ready for the next frame.
        assert_eq!(feed(&mut dec, &RSTACK), [Ok(rstack())]);
    }

    #[test]
    fn too_short_and_bad_control() {
        let mut dec = Decoder::default();
        assert_eq!(
            feed(&mut dec, &[0xC0, 0x38, 0x7E]),
            [Err(AshError::TooShort)]
        );
        // Unknown control byte 0xC3 with a valid CRC.
        assert_eq!(
            feed(&mut dec, &seal(&[0xC3])),
            [Err(AshError::BadControl(0xC3))]
        );
        // RSTACK missing its reset code.
        assert_eq!(
            feed(&mut dec, &seal(&[0xC1, 0x02])),
            [Err(AshError::BadControl(0xC1))]
        );
    }

    #[test]
    fn empty_frames_between_flags_are_ignored() {
        let mut dec = Decoder::default();
        assert!(feed(&mut dec, &[0x7E, 0x7E, 0x7E]).is_empty());
        assert_eq!(feed(&mut dec, &RSTACK), [Ok(rstack())]);
    }

    #[test]
    fn cancel_byte_resets() {
        let mut dec = Decoder::default();
        let mut out = feed(&mut dec, &[0xC1, 0x02, 0x1A]);
        out.extend(feed(&mut dec, &RSTACK));
        assert_eq!(oks(out), [rstack()]);
    }

    #[test]
    fn substitute_drops_frame() {
        let mut dec = Decoder::default();
        let out = feed(&mut dec, &[0xC1, 0x18, 0x02, 0x02, 0x9B, 0x7B, 0x7E]);
        assert!(oks(out).is_empty());
        assert_eq!(oks(feed(&mut dec, &RSTACK)), [rstack()]);
    }

    #[test]
    fn xon_xoff_are_ignored_outside_escape() {
        let mut dec = Decoder::default();
        let out = feed(&mut dec, &[0xC1, 0x11, 0x02, 0x13, 0x02, 0x9B, 0x7B, 0x7E]);
        assert_eq!(out, [Ok(rstack())]);
    }

    #[test]
    fn oversize_frame_is_dropped() {
        let mut dec = Decoder::default();
        let mut wire = vec![0x00; 1000];
        wire.push(0x7E);
        assert!(oks(feed(&mut dec, &wire)).is_empty());
        assert_eq!(oks(feed(&mut dec, &RSTACK)), [rstack()]);
    }
}

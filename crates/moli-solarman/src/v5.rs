//! Solarman's V5 protocol (TCP 8899): a Modbus RTU frame wrapped in the
//! logger's envelope (pure, no I/O).
//!
//! Request: `A5`, payload length (u16 LE), control code `0x4510`, sequence
//! (u16 LE), logger serial (u32 LE), payload = frame type `02`, sensor type
//! (2 bytes), three time fields (12 bytes), the Modbus RTU frame; then a
//! checksum (sum of every byte after `A5`, low byte) and `15`.
//! Answer: control code `0x1510`, payload = frame type, status, three time
//! fields, then the Modbus answer. The logger may also send its own frames
//! (heartbeats): skipped.
//!
//! Written from the public description of the protocol (pysolarmanv5, MIT)
//! and checked against real loggers.

use anyhow::{bail, ensure};

const START: u8 = 0xA5;
const END: u8 = 0x15;
const REQUEST: u16 = 0x4510;
const ANSWER: u16 = 0x1510;
/// Envelope before the payload: start, length, control code, sequence, serial.
const HEADER: usize = 11;
/// Answer payload before the Modbus frame.
const ANSWER_PREFIX: usize = 14;
/// Read holding registers: the only function this driver ever sends.
const READ: u8 = 0x03;

/// Modbus CRC-16 (polynomial 0xA001, start 0xFFFF).
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

fn checksum(frame: &[u8]) -> u8 {
    frame
        .iter()
        .skip(1)
        .fold(0u8, |sum, b| sum.wrapping_add(*b))
}

/// A request reading `count` registers from `start` on Modbus slave `slave`.
#[must_use]
pub fn read_request(serial: u32, sequence: u16, slave: u8, start: u16, count: u16) -> Vec<u8> {
    let mut rtu = vec![slave, READ];
    rtu.extend_from_slice(&start.to_be_bytes());
    rtu.extend_from_slice(&count.to_be_bytes());
    rtu.extend_from_slice(&crc16(&rtu).to_le_bytes());
    let mut payload = vec![0x02, 0, 0];
    payload.extend_from_slice(&[0; 12]);
    payload.extend_from_slice(&rtu);
    let mut frame = vec![START];
    frame.extend_from_slice(
        &u16::try_from(payload.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    frame.extend_from_slice(&REQUEST.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&serial.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame.push(checksum(&frame));
    frame.push(END);
    frame
}

/// What reading the start of `buf` gave.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// Not a whole frame yet.
    Partial,
    /// A whole frame that is not the answer (a heartbeat…): `n` bytes to drop.
    Other(usize),
    /// The registers asked for, and the bytes this answer took.
    Registers(Vec<u16>, usize),
}

/// Why an answer carries no registers.
#[derive(Debug, PartialEq, Eq)]
pub enum NoData {
    /// The logger answered, the inverter behind it did not (asleep at night).
    Asleep,
    /// The inverter refused (Modbus exception code).
    Refused(u8),
}

impl std::fmt::Display for NoData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Asleep => f.write_str("the inverter does not answer"),
            Self::Refused(code) => write!(f, "the inverter refused (modbus exception {code})"),
        }
    }
}

impl std::error::Error for NoData {}

/// Reads the answer to a request from `serial` with `sequence`, `count`
/// registers.
pub fn parse(buf: &[u8], serial: u32, sequence: u16, count: u16) -> anyhow::Result<Frame> {
    // Garbage before a frame: skip to the next start byte.
    let Some(at) = buf.iter().position(|&b| b == START) else {
        return Ok(if buf.is_empty() {
            Frame::Partial
        } else {
            Frame::Other(buf.len())
        });
    };
    if at > 0 {
        return Ok(Frame::Other(at));
    }
    if buf.len() < HEADER {
        return Ok(Frame::Partial);
    }
    let length = usize::from(u16::from_le_bytes([buf[1], buf[2]]));
    let total = HEADER + length + 2;
    ensure!(length <= 1024, "frame too long");
    if buf.len() < total {
        return Ok(Frame::Partial);
    }
    let frame = &buf[..total];
    ensure!(frame[total - 1] == END, "bad frame end");
    ensure!(
        checksum(&frame[..total - 2]) == frame[total - 2],
        "bad frame checksum"
    );
    let control = u16::from_le_bytes([frame[3], frame[4]]);
    let echoed = frame[5];
    let from = u32::from_le_bytes([frame[7], frame[8], frame[9], frame[10]]);
    #[allow(clippy::cast_possible_truncation)]
    if control != ANSWER || from != serial || echoed != sequence as u8 {
        return Ok(Frame::Other(total));
    }
    let payload = &frame[HEADER..total - 2];
    let Some(rtu) = payload.get(ANSWER_PREFIX..).filter(|r| r.len() >= 5) else {
        return Err(NoData::Asleep.into());
    };
    if rtu[1] == READ | 0x80 {
        return Err(NoData::Refused(rtu[2]).into());
    }
    ensure!(rtu[1] == READ, "unexpected modbus function {:#x}", rtu[1]);
    let bytes = usize::from(rtu[2]);
    ensure!(rtu.len() >= 3 + bytes + 2, "short modbus answer");
    let crc = u16::from_le_bytes([rtu[3 + bytes], rtu[4 + bytes]]);
    ensure!(crc16(&rtu[..3 + bytes]) == crc, "bad modbus crc");
    if bytes != usize::from(count) * 2 {
        bail!("asked {count} registers, got {bytes} bytes");
    }
    let registers = rtu[3..3 + bytes]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|w| u16::from_be_bytes(*w))
        .collect();
    Ok(Frame::Registers(registers, total))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn modbus_crc_matches_the_reference() {
        // 01 03 00 00 00 0A → C5 CD (the classic example of the Modbus spec).
        assert_eq!(
            crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]).to_le_bytes(),
            [0xC5, 0xCD]
        );
    }

    #[test]
    fn a_request_is_framed_like_pysolarmanv5() {
        let frame = read_request(1_234_567_890, 1, 1, 0x003B, 0x3A);
        assert_eq!(frame[0], START);
        assert_eq!(*frame.last().unwrap(), END);
        assert_eq!(u16::from_le_bytes([frame[1], frame[2]]), 23);
        assert_eq!(&frame[3..5], &[0x10, 0x45]);
        assert_eq!(&frame[7..11], &1_234_567_890u32.to_le_bytes());
        assert_eq!(&frame[26..32], &[0x01, 0x03, 0x00, 0x3B, 0x00, 0x3A]);
        assert_eq!(frame[frame.len() - 2], checksum(&frame[..frame.len() - 2]));
    }

    /// The answers captured on real loggers (anonymised).
    pub(crate) fn fixture(name: &str) -> Vec<(u16, u16, u16, Vec<u8>)> {
        let path = format!(
            "{}/../../integrations/solarman/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        items
            .iter()
            .map(|i| {
                let n = |k: &str| u16::try_from(i[k].as_u64().unwrap()).unwrap();
                (
                    n("start"),
                    n("count"),
                    n("sequence"),
                    hex(i["answer"].as_str().unwrap()),
                )
            })
            .collect()
    }

    #[test]
    fn a_real_answer_reads_as_registers() {
        for name in ["micro-2-inputs", "micro-4-inputs"] {
            for (_, count, sequence, bytes) in fixture(name) {
                match parse(&bytes, 1_234_567_890, sequence, count).unwrap() {
                    Frame::Registers(r, used) => {
                        assert_eq!(r.len(), usize::from(count));
                        assert_eq!(used, bytes.len());
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
    }

    #[test]
    fn heartbeats_and_garbage_are_skipped_and_silence_is_sleep() {
        assert_eq!(
            parse(&[0x00, 0x01, START], 1, 1, 1).unwrap(),
            Frame::Other(2)
        );
        assert_eq!(parse(&[START, 0x10], 1, 1, 1).unwrap(), Frame::Partial);
        // An answer whose payload stops before the Modbus frame.
        let mut frame = vec![START, 2, 0, 0x10, 0x15, 1, 0];
        frame.extend_from_slice(&7u32.to_le_bytes());
        frame.extend_from_slice(&[0x02, 0x01]);
        let sum = checksum(&frame);
        frame.extend_from_slice(&[sum, END]);
        let e = parse(&frame, 7, 1, 1).unwrap_err();
        assert_eq!(e.downcast_ref::<NoData>(), Some(&NoData::Asleep));
        // The same from another logger: not ours.
        assert_eq!(parse(&frame, 8, 1, 1).unwrap(), Frame::Other(frame.len()));
    }
}

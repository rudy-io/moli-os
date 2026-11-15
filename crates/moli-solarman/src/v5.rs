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
//!
//! TSUN's TITAN micro-inverters put their own frame in the same envelope
//! instead of Modbus RTU ([`native_request`], [`native_parse`]): written
//! from tsun-gen3-proxy's description (BSD-3-Clause, see `NOTICE`), never
//! checked against a real logger.

use anyhow::{bail, ensure};

const START: u8 = 0xA5;
const END: u8 = 0x15;
const REQUEST: u16 = 0x4510;
const ANSWER: u16 = 0x1510;
/// Envelope before the payload: start, length, control code, sequence, serial.
const HEADER: usize = 11;
/// Answer payload before the Modbus frame.
const ANSWER_PREFIX: usize = 14;
/// Read holding registers (Deye, TSUN GEN4, ZCS).
pub const HOLDING: u8 = 0x03;
/// Read input registers (Solis). Reads only: nothing else is ever sent.
pub const INPUT: u8 = 0x04;
/// What a logger puts before a frame in TSUN's own framing (TITAN).
const NATIVE_START: u8 = 0x7E;
/// A TSUN native answer that carries data.
const NATIVE_OK: u8 = 0x01;

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

/// A request reading `count` registers from `start` on Modbus slave `slave`,
/// with `function` ([`HOLDING`] or [`INPUT`]).
#[must_use]
pub fn read_request(
    serial: u32,
    sequence: u16,
    slave: u8,
    function: u8,
    start: u16,
    count: u16,
) -> Vec<u8> {
    let mut rtu = vec![slave, function];
    rtu.extend_from_slice(&start.to_be_bytes());
    rtu.extend_from_slice(&count.to_be_bytes());
    rtu.extend_from_slice(&crc16(&rtu).to_le_bytes());
    request(serial, sequence, &rtu)
}

/// A request reading `count` registers from `start` in TSUN's own framing
/// (TITAN micro-inverters): read code and sub-code, a zero, the first
/// register, `0002`, the count, then the Modbus CRC high byte first. Only
/// the read codes are ever sent.
#[must_use]
pub fn native_request(
    serial: u32,
    sequence: u16,
    (code, sub): (u8, u8),
    start: u16,
    count: u16,
) -> Vec<u8> {
    let mut body = vec![code, sub, 0];
    body.extend_from_slice(&start.to_be_bytes());
    body.extend_from_slice(&[0, 2]);
    body.extend_from_slice(&count.to_be_bytes());
    body.extend_from_slice(&crc16(&body).to_be_bytes());
    request(serial, sequence, &body)
}

/// The logger's envelope around a frame for the inverter.
fn request(serial: u32, sequence: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x02, 0, 0];
    payload.extend_from_slice(&[0; 12]);
    payload.extend_from_slice(body);
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

/// The inverter's part of the answer to the request from `serial` with
/// `sequence` at the start of `buf`, and the bytes the answer took; or what
/// else the start of `buf` is.
fn answer(buf: &[u8], serial: u32, sequence: u16) -> anyhow::Result<Result<(&[u8], usize), Frame>> {
    // Garbage before a frame: skip to the next start byte.
    let Some(at) = buf.iter().position(|&b| b == START) else {
        return Ok(Err(if buf.is_empty() {
            Frame::Partial
        } else {
            Frame::Other(buf.len())
        }));
    };
    if at > 0 {
        return Ok(Err(Frame::Other(at)));
    }
    if buf.len() < HEADER {
        return Ok(Err(Frame::Partial));
    }
    let length = usize::from(u16::from_le_bytes([buf[1], buf[2]]));
    let total = HEADER + length + 2;
    ensure!(length <= 1024, "frame too long");
    if buf.len() < total {
        return Ok(Err(Frame::Partial));
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
        return Ok(Err(Frame::Other(total)));
    }
    let payload = &frame[HEADER..total - 2];
    Ok(Ok((payload.get(ANSWER_PREFIX..).unwrap_or(&[]), total)))
}

/// Reads the answer to a request from `serial` with `sequence`, `count`
/// registers read with `function`.
pub fn parse(
    buf: &[u8],
    serial: u32,
    sequence: u16,
    function: u8,
    count: u16,
) -> anyhow::Result<Frame> {
    let (rtu, total) = match answer(buf, serial, sequence)? {
        Ok(found) => found,
        Err(frame) => return Ok(frame),
    };
    if rtu.len() < 5 {
        return Err(NoData::Asleep.into());
    }
    if rtu[1] == function | 0x80 {
        return Err(NoData::Refused(rtu[2]).into());
    }
    ensure!(
        rtu[1] == function,
        "unexpected modbus function {:#x}",
        rtu[1]
    );
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

/// Reads the answer to a [`native_request`]: the read code, its sub-code
/// with the high bit set, a status, the first register, the bytes of data,
/// the registers low byte first, then the CRC high byte first.
pub fn native_parse(
    buf: &[u8],
    serial: u32,
    sequence: u16,
    code: u8,
    start: u16,
    count: u16,
) -> anyhow::Result<Frame> {
    let (body, total) = match answer(buf, serial, sequence)? {
        Ok(found) => found,
        Err(frame) => return Ok(frame),
    };
    let body = body.strip_prefix(&[NATIVE_START]).unwrap_or(body);
    if body.len() < 9 {
        return Err(NoData::Asleep.into());
    }
    ensure!(body[0] == code, "unexpected native function {:#x}", body[0]);
    if body[2] != NATIVE_OK {
        return Err(NoData::Refused(body[2]).into());
    }
    let bytes = usize::from(u16::from_be_bytes([body[5], body[6]]));
    ensure!(body.len() >= 7 + bytes + 2, "short native answer");
    let crc = u16::from_be_bytes([body[7 + bytes], body[8 + bytes]]);
    ensure!(crc16(&body[..7 + bytes]) == crc, "bad native crc");
    let first = u16::from_be_bytes([body[3], body[4]]);
    ensure!(first == start, "asked register {start}, got {first}");
    if bytes != usize::from(count) * 2 {
        bail!("asked {count} registers, got {bytes} bytes");
    }
    let registers = body[7..7 + bytes]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|w| u16::from_le_bytes(*w))
        .collect();
    Ok(Frame::Registers(registers, total))
}

/// Modbus TCP (some loggers, a Solis stick among them, speak it on the
/// same port instead of V5): a request with transaction `tid`.
#[must_use]
pub fn tcp_request(tid: u16, slave: u8, function: u8, start: u16, count: u16) -> Vec<u8> {
    let mut frame = Vec::with_capacity(12);
    frame.extend_from_slice(&tid.to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0, 6, slave, function]);
    frame.extend_from_slice(&start.to_be_bytes());
    frame.extend_from_slice(&count.to_be_bytes());
    frame
}

/// The Modbus TCP answer to transaction `tid`, `count` registers read with
/// `function`.
pub fn tcp_parse(
    buf: &[u8],
    tid: u16,
    slave: u8,
    function: u8,
    count: u16,
) -> anyhow::Result<Frame> {
    if buf.len() < 9 {
        return Ok(Frame::Partial);
    }
    let length = usize::from(u16::from_be_bytes([buf[4], buf[5]]));
    ensure!((2..=260).contains(&length), "bad modbus tcp length");
    let total = 6 + length;
    if buf.len() < total {
        return Ok(Frame::Partial);
    }
    if u16::from_be_bytes([buf[0], buf[1]]) != tid || buf[2..4] != [0, 0] || buf[6] != slave {
        return Ok(Frame::Other(total));
    }
    let pdu = &buf[7..total];
    if pdu[0] == function | 0x80 {
        return Err(NoData::Refused(pdu.get(1).copied().unwrap_or(0)).into());
    }
    ensure!(pdu.len() >= 2, "short modbus answer");
    ensure!(
        pdu[0] == function,
        "unexpected modbus function {:#x}",
        pdu[0]
    );
    let bytes = usize::from(pdu[1]);
    ensure!(pdu.len() >= 2 + bytes, "short modbus answer");
    if bytes != usize::from(count) * 2 {
        bail!("asked {count} registers, got {bytes} bytes");
    }
    let registers = pdu[2..2 + bytes]
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
    fn modbus_tcp_reads_registers() {
        let req = tcp_request(0x10, 1, INPUT, 3004, 2);
        assert_eq!(req, [0, 0x10, 0, 0, 0, 6, 1, 4, 0x0B, 0xBC, 0, 2]);
        let answer = [0, 0x10, 0, 0, 0, 7, 1, 4, 4, 0, 0, 0, 0x32];
        assert_eq!(
            tcp_parse(&answer, 0x10, 1, INPUT, 2).unwrap(),
            Frame::Registers(vec![0, 50], 13)
        );
        assert_eq!(
            tcp_parse(&answer[..8], 0x10, 1, INPUT, 2).unwrap(),
            Frame::Partial
        );
        assert_eq!(
            tcp_parse(&answer, 0x11, 1, INPUT, 2).unwrap(),
            Frame::Other(13)
        );
        // Another unit's answer is not ours.
        assert_eq!(
            tcp_parse(&answer, 0x10, 2, INPUT, 2).unwrap(),
            Frame::Other(13)
        );
        let refused = [0, 0x10, 0, 0, 0, 3, 1, 0x84, 2];
        assert!(tcp_parse(&refused, 0x10, 1, INPUT, 2).is_err());
        // A length of 2: the function alone, no byte count. Refused, no panic.
        let short = [0, 0x10, 0, 0, 0, 2, 1, 4, 0];
        assert!(tcp_parse(&short, 0x10, 1, INPUT, 2).is_err());
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
        let frame = read_request(1_234_567_890, 1, 1, HOLDING, 0x003B, 0x3A);
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

    /// The answers of a fixture written for a register map, read the way
    /// its asks say: each block asked and the registers it gave.
    pub(crate) fn replay(name: &str) -> Vec<(crate::regs::Ask, Vec<u16>)> {
        use crate::regs::Ask;
        let path = format!(
            "{}/../../integrations/solarman/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        items
            .iter()
            .map(|i| {
                let n = |v: &serde_json::Value| u16::try_from(v.as_u64().unwrap()).unwrap();
                let (start, count, sequence) = (n(&i["start"]), n(&i["count"]), n(&i["sequence"]));
                let bytes = hex(i["answer"].as_str().unwrap());
                let (ask, frame) = match i.get("native") {
                    Some(native) => {
                        let code = u8::try_from(n(&native[0])).unwrap();
                        let sub = u8::try_from(n(&native[1])).unwrap();
                        (
                            Ask::native(code, sub, start, count),
                            native_parse(&bytes, 1_234_567_890, sequence, code, start, count),
                        )
                    }
                    None => (
                        Ask::holding(start, count),
                        parse(&bytes, 1_234_567_890, sequence, HOLDING, count),
                    ),
                };
                match frame.unwrap() {
                    Frame::Registers(words, used) => {
                        assert_eq!(used, bytes.len(), "{name} {start}");
                        (ask, words)
                    }
                    other => panic!("{name} {start}: {other:?}"),
                }
            })
            .collect()
    }

    /// A logger's answer around what the inverter said.
    pub(crate) fn wrapped(serial: u32, sequence: u8, body: &[u8]) -> Vec<u8> {
        let mut payload = vec![0x02, 0x01];
        payload.extend_from_slice(&[0; 12]);
        payload.extend_from_slice(body);
        let mut frame = vec![START];
        frame.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_le_bytes());
        frame.extend_from_slice(&ANSWER.to_le_bytes());
        frame.extend_from_slice(&[sequence, 0]);
        frame.extend_from_slice(&serial.to_le_bytes());
        frame.extend_from_slice(&payload);
        frame.push(checksum(&frame));
        frame.push(END);
        frame
    }

    #[test]
    fn a_native_request_is_framed_like_the_public_description() {
        // tsun-gen3-proxy's example: 32 registers from 3000 with A1 01.
        let body = [0xA1, 0x01, 0x00, 0x0B, 0xB8, 0x00, 0x02, 0x00, 0x20];
        assert_eq!(crc16(&body), 0xD33F);
        let frame = native_request(1_234_567_890, 1, (0xA1, 0x01), 3000, 32);
        assert_eq!(u16::from_le_bytes([frame[1], frame[2]]), 26);
        assert_eq!(&frame[3..5], &[0x10, 0x45]);
        assert_eq!(&frame[26..35], &body);
        // The CRC high byte first, unlike Modbus RTU.
        assert_eq!(&frame[35..37], &[0xD3, 0x3F]);
        assert_eq!(frame[frame.len() - 2], checksum(&frame[..frame.len() - 2]));
    }

    #[test]
    fn a_native_answer_reads_low_byte_first() {
        let mut body = vec![0xA1, 0x81, 0x01, 0x0B, 0xB8, 0x00, 0x04];
        body.extend_from_slice(&[0x3C, 0x09, 0x01, 0x00]);
        body.extend_from_slice(&crc16(&body).to_be_bytes());
        let read = |frame: &[u8], start| native_parse(frame, 7, 1, 0xA1, start, 2);
        // With the 7E some loggers put before it, or without.
        let mut marked = vec![NATIVE_START];
        marked.extend_from_slice(&body);
        for inner in [&body, &marked] {
            let frame = wrapped(7, 1, inner);
            assert_eq!(
                read(&frame, 3000).unwrap(),
                Frame::Registers(vec![0x093C, 1], frame.len())
            );
            assert_eq!(read(&frame[..20], 3000).unwrap(), Frame::Partial);
            assert!(read(&frame, 3600).is_err(), "another block's answer");
            assert_eq!(
                native_parse(&frame, 8, 1, 0xA1, 3000, 2).unwrap(),
                Frame::Other(frame.len()),
                "another logger's"
            );
            assert!(native_parse(&frame, 7, 1, 0xA1, 3000, 3).is_err());
            assert!(native_parse(&frame, 7, 1, 0xA3, 3000, 2).is_err());
        }
        // A wrong CRC, or one written low byte first.
        let mut wrong = body.clone();
        wrong.swap(11, 12);
        assert!(read(&wrapped(7, 1, &wrong), 3000).is_err());
        // The inverter says no (unknown address): refused, not asleep.
        let mut no = vec![0xA1, 0x81, 0x11, 0x0B, 0xB8, 0x00, 0x02, 0x00, 0x00];
        no.extend_from_slice(&crc16(&no).to_be_bytes());
        let e = read(&wrapped(7, 1, &no), 3000).unwrap_err();
        assert_eq!(e.downcast_ref::<NoData>(), Some(&NoData::Refused(0x11)));
        // Nothing behind the logger: asleep.
        let e = read(&wrapped(7, 1, &[]), 3000).unwrap_err();
        assert_eq!(e.downcast_ref::<NoData>(), Some(&NoData::Asleep));
    }

    #[test]
    fn a_real_answer_reads_as_registers() {
        for name in ["micro-2-inputs", "micro-4-inputs"] {
            for (_, count, sequence, bytes) in fixture(name) {
                match parse(&bytes, 1_234_567_890, sequence, HOLDING, count).unwrap() {
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
            parse(&[0x00, 0x01, START], 1, 1, HOLDING, 1).unwrap(),
            Frame::Other(2)
        );
        assert_eq!(
            parse(&[START, 0x10], 1, 1, HOLDING, 1).unwrap(),
            Frame::Partial
        );
        // An answer whose payload stops before the Modbus frame.
        let mut frame = vec![START, 2, 0, 0x10, 0x15, 1, 0];
        frame.extend_from_slice(&7u32.to_le_bytes());
        frame.extend_from_slice(&[0x02, 0x01]);
        let sum = checksum(&frame);
        frame.extend_from_slice(&[sum, END]);
        let e = parse(&frame, 7, 1, HOLDING, 1).unwrap_err();
        assert_eq!(e.downcast_ref::<NoData>(), Some(&NoData::Asleep));
        // The same from another logger: not ours.
        assert_eq!(
            parse(&frame, 8, 1, HOLDING, 1).unwrap(),
            Frame::Other(frame.len())
        );
    }
}

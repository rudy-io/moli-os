//! The stations' own protocol, on the home network (UDP 32108): one short
//! session per order — local lookup, `CHECK_CAM` / `CAM_ID`, the command,
//! its result, `END`. Ported from eufy-security-client (`src/p2p`, MIT,
//! see NOTICE).
//!
//! Commands that the app encrypts are encrypted at level 1 (AES-128-ECB, a
//! key made of the station's serial and P2P id), the library's fallback
//! when the station's RSA key is not used.

use std::net::SocketAddr;
use std::time::Duration;

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt as _, BlockEncrypt as _, KeyInit as _};
use anyhow::{Context as _, bail, ensure};
use tokio::net::UdpSocket;
use tokio::time::Instant;

const PORT: u16 = 32108;
const LOCAL_LOOKUP: [u8; 2] = [0xF1, 0x30];
const LOCAL_LOOKUP_RESP: [u8; 2] = [0xF1, 0x41];
const CHECK_CAM: [u8; 2] = [0xF1, 0x41];
const CAM_ID: [u8; 2] = [0xF1, 0x42];
const PING: [u8; 2] = [0xF1, 0xE0];
const PONG: [u8; 2] = [0xF1, 0xE1];
const DATA: [u8; 2] = [0xF1, 0xD0];
const ACK: [u8; 2] = [0xF1, 0xD1];
const END: [u8; 2] = [0xF1, 0xF0];
/// The data channel of a station (as opposed to video, control…).
const DATA_TYPE: [u8; 2] = [0xD1, 0x00];
const MAGIC: &[u8; 4] = b"XZYH";
/// A station's own channel.
pub const STATION: u8 = 255;

pub const CMD_SET_ARMING: u16 = 1224;
pub const CMD_SET_PAYLOAD: u16 = 1350;

/// Our command again if the station has not acknowledged it.
const RESEND: Duration = Duration::from_millis(500);

/// Commands the app encrypts (`isP2PCommandEncrypted`): those Moli sends.
fn encrypted(command: u16) -> bool {
    matches!(command, CMD_SET_ARMING | CMD_SET_PAYLOAD)
}

/// Where and who a station is.
#[derive(Clone, Debug)]
pub struct Station {
    pub serial: String,
    pub ip: String,
    pub p2p_did: String,
}

/// `ABCDEFG-123456-HIJKL` as 20 bytes: letters padded to 8, number in 4.
fn did_bytes(did: &str) -> anyhow::Result<[u8; 20]> {
    let mut parts = did.split('-');
    let (Some(head), Some(number), Some(tail)) = (parts.next(), parts.next(), parts.next()) else {
        bail!("not a P2P id");
    };
    ensure!(head.len() <= 8 && tail.len() <= 8, "not a P2P id");
    let number: u32 = number.parse().context("not a P2P id")?;
    let mut out = [0u8; 20];
    out[..head.len()].copy_from_slice(head.as_bytes());
    out[8..12].copy_from_slice(&number.to_be_bytes());
    out[12..12 + tail.len()].copy_from_slice(tail.as_bytes());
    Ok(out)
}

/// The P2P id a `LOCAL_LOOKUP_RESP` names.
fn did_of(message: &[u8]) -> Option<String> {
    let text = |b: &[u8]| -> Option<String> {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        std::str::from_utf8(&b[..end]).ok().map(str::to_owned)
    };
    let head = text(message.get(4..12)?)?;
    let number = u32::from_be_bytes(message.get(12..16)?.try_into().ok()?);
    let tail = text(message.get(16..24)?)?;
    Some(format!("{head}-{number:06}-{tail}"))
}

fn frame(id: [u8; 2], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&id);
    out.extend_from_slice(
        &u16::try_from(payload.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(payload);
    out
}

/// Level 1: the serial's last 7 characters and 9 of the P2P id from its
/// first dash.
pub fn level1_key(serial: &str, did: &str) -> Option<[u8; 16]> {
    let tail = serial.get(serial.len().checked_sub(7)?..)?;
    let dash = did.find('-')?;
    let part = did.get(dash..dash + 9)?;
    format!("{tail}{part}").as_bytes().try_into().ok()
}

fn ecb(key: &[u8; 16], data: &mut [u8], encrypt: bool) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for block in data.as_chunks_mut::<16>().0 {
        if encrypt {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        } else {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
    }
}

/// A command's body as the library builds it: length, magic, channel and
/// encryption level, then the data (padded and encrypted when it must be).
fn body(key: Option<&[u8; 16]>, command: u16, data: &[u8], channel: u8) -> Vec<u8> {
    let key = key.filter(|_| encrypted(command));
    let mut data = data.to_vec();
    if let Some(key) = key {
        data.resize(data.len().div_ceil(16).max(1) * 16, 0);
        ecb(key, &mut data, true);
    }
    let mut out = Vec::with_capacity(10 + data.len());
    out.extend_from_slice(&u16::try_from(data.len()).unwrap_or(u16::MAX).to_le_bytes());
    out.extend_from_slice(&[0, 0, 1, 0, channel, u8::from(key.is_some()), 0, 0]);
    out.extend_from_slice(&data);
    out
}

/// A text padded with zeros to a multiple of 128 bytes (`stringWithLength`).
fn padded(text: &str) -> Vec<u8> {
    let mut out = text.as_bytes().to_vec();
    out.resize(out.len().div_ceil(128).max(1) * 128, 0);
    out
}

/// `sendCommandWithInt`: a number, then maybe a text.
pub fn int_command(
    key: Option<&[u8; 16]>,
    command: u16,
    value: u32,
    text: &str,
    channel: u8,
) -> Vec<u8> {
    let mut data = value.to_le_bytes().to_vec();
    if !text.is_empty() {
        data.extend(padded(text));
    }
    body(key, command, &data, channel)
}

/// `sendCommandWithStringPayload`: a JSON text.
pub fn string_command(key: Option<&[u8; 16]>, command: u16, json: &str, channel: u8) -> Vec<u8> {
    body(key, command, json.as_bytes(), channel)
}

fn data_message(seq: u16, command: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(10 + body.len());
    payload.extend_from_slice(&DATA_TYPE);
    payload.extend_from_slice(&seq.to_be_bytes());
    payload.extend_from_slice(MAGIC);
    payload.extend_from_slice(&command.to_le_bytes());
    payload.extend_from_slice(body);
    frame(DATA, &payload)
}

fn ack(data_type: [u8; 2], seq: u16) -> Vec<u8> {
    let mut payload = data_type.to_vec();
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.extend_from_slice(&seq.to_be_bytes());
    frame(ACK, &payload)
}

/// A result the station sent on the data channel: for which command, and
/// its return code (0: done).
fn result(message: &[u8], key: Option<&[u8; 16]>) -> Option<(u16, i32)> {
    let body = message.get(8..)?;
    if body.get(..4)? != MAGIC {
        return None;
    }
    let command = u16::from_le_bytes(body.get(4..6)?.try_into().ok()?);
    let length = usize::try_from(u32::from_le_bytes(body.get(6..10)?.try_into().ok()?)).ok()?;
    let (sign, kind) = (*body.get(13)?, *body.get(14)?);
    if kind != 1 {
        return None;
    }
    let mut data = body
        .get(16..16 + length.min(body.len().saturating_sub(16)))?
        .to_vec();
    if sign > 0
        && !data.is_empty()
        && data.len().is_multiple_of(16)
        && let Some(key) = key
    {
        ecb(key, &mut data, false);
    }
    let code = i32::from_le_bytes(data.get(..4)?.try_into().ok()?);
    Some((command, code))
}

/// How far an exchange went: said when the station stops answering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Lookup,
    Check,
    Command,
    Result,
}

/// One command to a station, on the home network: its return code, within
/// `limit` (the hub waits a few seconds, no more).
pub async fn send(
    station: &Station,
    command: u16,
    body: Vec<u8>,
    key: Option<[u8; 16]>,
    limit: Duration,
) -> anyhow::Result<i32> {
    let mut phase = Phase::Lookup;
    let outcome = tokio::time::timeout(
        limit,
        exchange(station, command, &body, key.as_ref(), &mut phase),
    )
    .await;
    if let Ok(result) = outcome {
        return result;
    }
    tracing::info!(station = %station.serial, ?phase, "eufy p2p: no answer");
    bail!(moli_i18n::tr!("pilotes.eufy.station_muette"))
}

async fn exchange(
    station: &Station,
    command: u16,
    body: &[u8],
    key: Option<&[u8; 16]>,
    phase: &mut Phase,
) -> anyhow::Result<i32> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
    socket.set_broadcast(true)?;
    // The address the cloud knows may be stale: asked there and on the
    // whole network, the station answers with its P2P id.
    let mut targets: Vec<SocketAddr> = vec![SocketAddr::from(([255, 255, 255, 255], PORT))];
    if let Ok(known) = format!("{}:{PORT}", station.ip).parse() {
        targets.push(known);
    }
    let did = did_bytes(&station.p2p_did)?;
    let mut buf = vec![0u8; 2048];
    let mut peer: Option<SocketAddr> = None;
    let mut connected = false;
    let message = data_message(0, command, body);
    let mut acknowledged = false;
    let mut next_send = Instant::now();
    loop {
        if Instant::now() >= next_send {
            match (peer, connected) {
                (None, _) => {
                    for target in &targets {
                        // A broadcast may be refused (no route): the other goes.
                        let _ = socket.send_to(&frame(LOCAL_LOOKUP, &[0, 0]), target).await;
                    }
                }
                (Some(peer), false) => {
                    let mut check = did.to_vec();
                    check.extend_from_slice(&[0, 0, 0]);
                    socket.send_to(&frame(CHECK_CAM, &check), peer).await?;
                }
                (Some(peer), true) if !acknowledged => {
                    socket.send_to(&message, peer).await?;
                }
                _ => {}
            }
            next_send = Instant::now() + RESEND;
        }
        let Ok(read) = tokio::time::timeout_at(next_send, socket.recv_from(&mut buf)).await else {
            continue;
        };
        let (n, from) = read?;
        let msg = &buf[..n];
        let Some(id) = msg.get(..2) else {
            continue;
        };
        // Only the station we look for (a lookup answer names itself).
        if peer.is_some_and(|p| p.ip() != from.ip()) {
            continue;
        }
        match [id[0], id[1]] {
            LOCAL_LOOKUP_RESP if peer.is_none() => {
                if did_of(msg).as_deref() == Some(station.p2p_did.as_str()) {
                    peer = Some(from);
                    *phase = Phase::Check;
                    next_send = Instant::now();
                }
            }
            CAM_ID if !connected => {
                peer = Some(from);
                connected = true;
                *phase = Phase::Command;
                next_send = Instant::now();
            }
            PING => {
                socket.send_to(&frame(PONG, &[]), from).await?;
            }
            ACK if connected => {
                if msg.get(4..6) == Some(&DATA_TYPE[..]) && msg.get(8..10) == Some(&[0, 0][..]) {
                    acknowledged = true;
                    *phase = Phase::Result;
                }
            }
            DATA if connected => {
                let (Some(kind), Some(seq)) = (msg.get(4..6), msg.get(6..8)) else {
                    continue;
                };
                let kind: [u8; 2] = kind.try_into()?;
                socket
                    .send_to(&ack(kind, u16::from_be_bytes(seq.try_into()?)), from)
                    .await?;
                if kind == DATA_TYPE
                    && let Some((answered, code)) = result(msg, key)
                    && answered == command
                {
                    let _ = socket.send_to(&frame(END, &[]), from).await;
                    return Ok(code);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_p2p_id_is_written_and_read_like_the_station() {
        let bytes = did_bytes("ABCDEFG-012345-HIJKL").unwrap();
        assert_eq!(&bytes[..8], b"ABCDEFG\0");
        assert_eq!(&bytes[8..12], &12345u32.to_be_bytes());
        assert_eq!(&bytes[12..], b"HIJKL\0\0\0");
        let mut answer = frame(LOCAL_LOOKUP_RESP, &bytes);
        answer.extend_from_slice(&[0; 4]);
        assert_eq!(did_of(&answer).as_deref(), Some("ABCDEFG-012345-HIJKL"));
        assert!(did_bytes("nope").is_err());
    }

    #[test]
    fn the_level1_key_is_serial_and_id() {
        assert_eq!(
            &level1_key("T8010P0000001234", "ABCDEFG-012345-HIJKL").unwrap(),
            b"0001234-012345-H"
        );
        assert!(level1_key("T80", "ABCDEFG-012345-HIJKL").is_none());
    }

    #[test]
    fn an_arming_command_is_built_like_the_library() {
        let key = level1_key("T8010P0000001234", "ABCDEFG-012345-HIJKL").unwrap();
        let plain = int_command(None, CMD_SET_ARMING, 63, "", STATION);
        assert_eq!(plain, [4, 0, 0, 0, 1, 0, 255, 0, 0, 0, 63, 0, 0, 0]);
        let sealed = int_command(Some(&key), CMD_SET_ARMING, 63, "user", STATION);
        // 4 + 128 bytes, padded to 144, encrypted: level 1 in the header.
        assert_eq!(&sealed[..10], &[144, 0, 0, 0, 1, 0, 255, 1, 0, 0]);
        let mut data = sealed[10..].to_vec();
        ecb(&key, &mut data, false);
        assert_eq!(&data[..8], &[63, 0, 0, 0, b'u', b's', b'e', b'r']);
        let message = data_message(7, CMD_SET_ARMING, &plain);
        assert_eq!(&message[..2], &DATA);
        assert_eq!(&message[4..12], &[0xD1, 0, 0, 7, b'X', b'Z', b'Y', b'H']);
        assert_eq!(&message[12..14], &CMD_SET_ARMING.to_le_bytes());
    }

    #[test]
    fn the_station_s_result_gives_its_code() {
        let key = level1_key("T8010P0000001234", "ABCDEFG-012345-HIJKL").unwrap();
        let mut data = (-133i32).to_le_bytes().to_vec();
        data.resize(16, 0);
        ecb(&key, &mut data, true);
        let mut body = MAGIC.to_vec();
        body.extend_from_slice(&CMD_SET_PAYLOAD.to_le_bytes());
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&[0, 0, 255, 1, 1, 0]);
        body.extend_from_slice(&data);
        let mut message = vec![0xF1, 0xD0, 0, 0, 0xD1, 0, 0, 3];
        message.extend_from_slice(&body);
        assert_eq!(result(&message, Some(&key)), Some((CMD_SET_PAYLOAD, -133)));
        // A notice (type 0), not a result.
        message[8 + 14] = 0;
        assert_eq!(result(&message, Some(&key)), None);
    }
}

//! Matter application protocol header codec (Matter Core Specification
//! §4.4.5).
//!
//! The bytes inside every encrypted payload that carry Exchange Flags,
//! Opcode, Exchange ID, Protocol ID, and optional Acknowledged Message
//! Counter / Secure Extensions / Vendor data. Pure codec — no state, no
//! clock.
//!
//! # Wire layout (byte-parity with matter.js)
//!
//! ```text
//!   off  size  field
//!   0    1     exchange_flags    (I / A / R / SX / V bits; reserved bits must be 0)
//!   1    1     opcode
//!   2    2     exchange_id       (u16 LE)
//!   4    2     protocol_short_id (u16 LE)
//!   6    2     vendor_id         (u16 LE) — present iff V=1
//!   ...  4     ack_counter       (u32 LE) — present iff A=1
//!   ...  2+n   sx_data           — present iff SX=1, decoder skips
//!   ...  2+n   v_data            — UNUSED (no separate v_data bytes; V bit
//!                                  governs vendor_id presence only)
//! ```
//!
//! Two surprises relative to the Matter spec text:
//!
//! 1. **No `vendor_id` bytes when V=0.** Spec text describes a fixed 8-byte
//!    header (`vendor_id` always present); matter.js's encoder writes the
//!    `vendor_id` only when `vendor != 0` and uses the V (`HasVendorId`)
//!    flag bit to signal its presence. We match matter.js's behaviour for
//!    byte-parity — verified in `tests/protocol_header_byte_parity.rs`.
//!
//! 2. **Order is `protocol_short_id` THEN `vendor_id`.** Spec §4.4.5 says
//!    "Protocol ID (16 bits)" with vendor optional; matter.js packs both
//!    into a single u32 (`(vendor<<16) | protocol_short`) and writes it
//!    as u32 LE — which puts `protocol_short` in the low bytes (first on
//!    the wire) and `vendor_id` in the high bytes (second on the wire).
//!    Our encoder writes them in the same order so the bytes match.

use bitflags::bitflags;

use crate::error::{Error, Result};
use crate::framing::MessageCounter;

bitflags! {
    /// First byte of the Matter application protocol header. Bit
    /// positions match matter.js's `PayloadHeaderFlag` enum (verified
    /// byte-parity in `tests/protocol_header_byte_parity.rs`).
    ///
    /// - Bit 0: `I` — initiator-of-exchange.
    /// - Bit 1: `A` — acknowledged message counter present.
    /// - Bit 2: `R` — reliable (requires ack).
    /// - Bit 3: `SX` — secure extensions present (data length-prefix-skipped on decode).
    /// - Bit 4: `V` — vendor ID present (2-byte vendor field follows protocol_short).
    /// - Bits 5..7: reserved (rejected on decode).
    ///
    /// `A` and `V` are derived bits — the encoder overrides whatever
    /// the caller sets to match `ack_counter.is_some()` and `vendor != 0`
    /// respectively. Callers should not set these manually.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ExchangeFlags: u8 {
        /// We initiated this exchange.
        const INITIATOR        = 0b0000_0001;
        /// Header carries an acknowledged message counter. Derived from
        /// `ack_counter.is_some()` by the encoder.
        const ACK              = 0b0000_0010;
        /// Message requires an MRP ack from the peer.
        const RELIABLE         = 0b0000_0100;
        /// Secure-extension data present; decoder skips past it.
        const SECURE_EXTENSION = 0b0000_1000;
        /// Vendor ID field is present on the wire. Derived from
        /// `protocol_id.vendor != 0` by the encoder.
        const VENDOR           = 0b0001_0000;
    }
}

const RESERVED_FLAG_BITS: u8 = 0b1110_0000;

/// Matter protocol identifier: 32-bit `(vendor_id, protocol_id)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolId {
    /// Vendor ID (CSA-assigned). Zero for spec-standard protocols.
    pub vendor: u16,
    /// Protocol short ID within the vendor namespace.
    pub protocol: u16,
}

impl ProtocolId {
    /// Secure Channel protocol — opcode 0x10 is `STANDALONE_ACK`.
    pub const SECURE_CHANNEL: Self = Self {
        vendor: 0,
        protocol: 0x0000,
    };
    /// Interaction Model (Read/Write/Invoke/Subscribe).
    pub const INTERACTION_MODEL: Self = Self {
        vendor: 0,
        protocol: 0x0001,
    };
    /// Bulk Data Exchange (OTA, etc.).
    pub const BDX: Self = Self {
        vendor: 0,
        protocol: 0x0002,
    };
}

/// Spec-defined opcodes by protocol.
pub mod opcode {
    /// Opcodes within `ProtocolId::SECURE_CHANNEL`.
    pub mod secure_channel {
        /// MRP standalone acknowledgement (Matter Core Spec §4.11.5).
        /// Empty payload, A=1, R=0.
        pub const STANDALONE_ACK: u8 = 0x10;

        /// Status Report (Matter 1.4 Core §4.11.1, Table 18): a common
        /// status format (Appendix D) that every protocol uses, always sent
        /// under the Secure Channel protocol id.
        pub const STATUS_REPORT: u8 = 0x40;
    }
}

/// Parsed view of the Matter application protocol header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolHeader {
    /// Exchange-control flag bits.
    pub exchange_flags: ExchangeFlags,
    /// Protocol opcode (interpreted within `protocol_id`'s namespace).
    pub opcode: u8,
    /// Exchange identifier — uniquely identifies a request/response pair.
    pub exchange_id: u16,
    /// Protocol identifier.
    pub protocol_id: ProtocolId,
    /// Present iff `exchange_flags.contains(ACK)`.
    pub ack_counter: Option<MessageCounter>,
}

/// Encode a [`ProtocolHeader`] into the output buffer.
///
/// `ExchangeFlags::VENDOR` is auto-derived from `protocol_id.vendor`:
/// caller-supplied `VENDOR` bit is overwritten and the vendor field is
/// emitted iff `vendor != 0` (matter.js semantics — see module docs).
///
/// `ExchangeFlags::ACK` is similarly derived from `ack_counter.is_some()`.
///
/// `SECURE_EXTENSION` data is never emitted — the SX bit, if set by the
/// caller, is passed through to the wire but no extension payload is
/// written. (Decoder skips SX data on read; this asymmetry is the
/// agreed forward-compat point.)
pub fn encode_protocol_header(h: &ProtocolHeader, out: &mut Vec<u8>) {
    let has_vendor = h.protocol_id.vendor != 0;
    let has_ack = h.ack_counter.is_some();

    let mut flags = h.exchange_flags;
    flags.set(ExchangeFlags::VENDOR, has_vendor);
    flags.set(ExchangeFlags::ACK, has_ack);

    out.push(flags.bits());
    out.push(h.opcode);
    out.extend_from_slice(&h.exchange_id.to_le_bytes());
    // matter.js writes `(vendor<<16)|protocol_short` as u32 LE when V=1
    // (== protocol_short LE then vendor LE), or just `protocol_short`
    // as u16 LE when V=0. We mirror that exactly.
    out.extend_from_slice(&h.protocol_id.protocol.to_le_bytes());
    if has_vendor {
        out.extend_from_slice(&h.protocol_id.vendor.to_le_bytes());
    }
    if let Some(MessageCounter(c)) = h.ack_counter {
        out.extend_from_slice(&c.to_le_bytes());
    }
}

/// Decode a [`ProtocolHeader`] from the start of `bytes`. Returns the
/// parsed header and the remaining bytes (the application payload tail).
///
/// SX data, when present, is length-prefix-skipped and discarded; the SX
/// bit stays set in `ExchangeFlags` so callers can detect that
/// extensions were present.
///
/// # Errors
///
/// Returns [`Error::MalformedProtocolHeader`] if:
/// - the fixed 6-byte portion (flags + opcode + `exchange_id` +
///   `protocol_short`) is truncated;
/// - `V=1` but the 2-byte vendor field is truncated;
/// - `A=1` but the 4-byte ack counter is truncated;
/// - `SX=1` but the 2-byte length prefix or the announced data length is
///   truncated;
/// - any reserved flag bit (5..=7) is set.
pub fn decode_protocol_header(bytes: &[u8]) -> Result<(ProtocolHeader, &[u8])> {
    if bytes.len() < 6 {
        return Err(Error::MalformedProtocolHeader(bytes.len()));
    }
    let flag_byte = bytes[0];
    if flag_byte & RESERVED_FLAG_BITS != 0 {
        return Err(Error::MalformedProtocolHeader(0));
    }
    let flags = ExchangeFlags::from_bits_retain(flag_byte);
    let opcode = bytes[1];
    let exchange_id = u16::from_le_bytes([bytes[2], bytes[3]]);
    // matter.js wire order: protocol_short LE first, vendor LE second
    // (vendor only present when V=1).
    let protocol = u16::from_le_bytes([bytes[4], bytes[5]]);

    let mut offset = 6;

    let vendor = if flags.contains(ExchangeFlags::VENDOR) {
        if bytes.len() < offset + 2 {
            return Err(Error::MalformedProtocolHeader(offset));
        }
        let v = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        offset += 2;
        v
    } else {
        0
    };
    let protocol_id = ProtocolId { vendor, protocol };

    let ack_counter = if flags.contains(ExchangeFlags::ACK) {
        if bytes.len() < offset + 4 {
            return Err(Error::MalformedProtocolHeader(offset));
        }
        let value = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        offset += 4;
        Some(MessageCounter(value))
    } else {
        None
    };

    if flags.contains(ExchangeFlags::SECURE_EXTENSION) {
        offset = skip_length_prefixed(bytes, offset)?;
    }

    let parsed = ProtocolHeader {
        exchange_flags: flags,
        opcode,
        exchange_id,
        protocol_id,
        ack_counter,
    };
    Ok((parsed, &bytes[offset..]))
}

/// Skip a 2-byte-LE-length-prefixed blob. Returns the new offset.
fn skip_length_prefixed(bytes: &[u8], offset: usize) -> Result<usize> {
    if bytes.len() < offset + 2 {
        return Err(Error::MalformedProtocolHeader(offset));
    }
    let len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    let data_offset = offset + 2;
    if bytes.len() < data_offset + len {
        return Err(Error::MalformedProtocolHeader(data_offset));
    }
    Ok(data_offset + len)
}

/// Build a [`ProtocolHeader`] for a standalone-ack message. Used by
/// `SessionManager` when piggyback drain misses the 200ms deadline OR a
/// duplicate reliable inbound is detected.
///
/// Flags = `A | (I if is_local_initiator)`. Opcode =
/// [`opcode::secure_channel::STANDALONE_ACK`]. Protocol = `SecureChannel`.
/// Exchange ID and `ack_counter` come from the caller.
#[must_use]
pub fn build_standalone_ack_header(
    exchange_id: u16,
    ack_counter: MessageCounter,
    is_local_initiator: bool,
) -> ProtocolHeader {
    let mut flags = ExchangeFlags::ACK;
    if is_local_initiator {
        flags |= ExchangeFlags::INITIATOR;
    }
    ProtocolHeader {
        exchange_flags: flags,
        opcode: opcode::secure_channel::STANDALONE_ACK,
        exchange_id,
        protocol_id: ProtocolId::SECURE_CHANNEL,
        ack_counter: Some(ack_counter),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn minimal_header_roundtrip() {
        let header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x20,
            exchange_id: 0x4242,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter: None,
        };
        let mut out = Vec::new();
        encode_protocol_header(&header, &mut out);
        assert_eq!(
            out.len(),
            6,
            "6-byte fixed header when vendor=0 / no optional fields"
        );
        // Flags(1) | opcode(1) | exchange_id LE(2) | protocol_short LE(2)
        assert_eq!(out, vec![0x01, 0x20, 0x42, 0x42, 0x01, 0x00]);
        let (parsed, rest) = decode_protocol_header(&out).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_with_ack_counter() {
        let header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR | ExchangeFlags::ACK,
            opcode: 0x10,
            exchange_id: 0x0001,
            protocol_id: ProtocolId::SECURE_CHANNEL,
            ack_counter: Some(MessageCounter(0xAABB_CCDD)),
        };
        let mut out = Vec::new();
        encode_protocol_header(&header, &mut out);
        assert_eq!(out.len(), 10, "6 fixed (V=0) + 4 ack_counter");
        let (parsed, rest) = decode_protocol_header(&out).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_reliable_initiator() {
        let header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            opcode: 0x30,
            exchange_id: 0x1234,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter: None,
        };
        let mut out = Vec::new();
        encode_protocol_header(&header, &mut out);
        assert_eq!(out[0], 0b0000_0101, "I=1 R=1");
        let (parsed, rest) = decode_protocol_header(&out).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_preserves_payload_tail() {
        let header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x20,
            exchange_id: 7,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter: None,
        };
        let mut out = Vec::new();
        encode_protocol_header(&header, &mut out);
        out.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let (_, rest) = decode_protocol_header(&out).unwrap();
        assert_eq!(rest, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn decode_truncated_fixed_portion() {
        // Minimum is 6 bytes (V=0); 5 bytes is short.
        let bytes = [0x01, 0x20, 0x42, 0x42, 0x00];
        let err = decode_protocol_header(&bytes).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MalformedProtocolHeader(_)
        ));
    }

    #[test]
    fn decode_truncated_ack_counter() {
        // Flags = I | A (V=0), 6-byte fixed portion, then only 2 of 4 bytes
        // of ack_counter present.
        let mut bytes = vec![0b0000_0011, 0x20, 0x01, 0x00, 0x01, 0x00];
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        let err = decode_protocol_header(&bytes).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MalformedProtocolHeader(_)
        ));
    }

    #[test]
    fn decode_skips_sx_data() {
        // Flags = I | SX (V=0, A=0). 6-byte fixed portion, then SX data
        // (2-byte LE length=3, then 3 bytes), then 4 bytes app payload.
        let bytes = vec![
            0b0000_1001, // I=1 SX=1
            0x20,        // opcode
            0x01,
            0x00, // exchange_id = 1
            0x01,
            0x00, // protocol_short = 1
            0x03,
            0x00, // SX length = 3
            0x11,
            0x22,
            0x33, // SX data (discarded)
            0xDE,
            0xAD,
            0xBE,
            0xEF, // app payload
        ];
        let (parsed, rest) = decode_protocol_header(&bytes).unwrap();
        assert!(parsed
            .exchange_flags
            .contains(ExchangeFlags::SECURE_EXTENSION));
        assert_eq!(rest, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn decode_reads_vendor_when_v_set() {
        // Flags = I | V. Wire order: flags, opcode, exchange_id LE,
        // protocol_short LE, vendor LE, then app payload.
        let bytes = vec![
            0b0001_0001, // I=1 V=1
            0x20,
            0x01,
            0x00, // exchange_id = 1
            0x78,
            0x56, // protocol_short = 0x5678
            0x34,
            0x12, // vendor = 0x1234
            0xCA,
            0xFE,
            0x42, // app payload
        ];
        let (parsed, rest) = decode_protocol_header(&bytes).unwrap();
        assert!(parsed.exchange_flags.contains(ExchangeFlags::VENDOR));
        assert_eq!(parsed.protocol_id.vendor, 0x1234);
        assert_eq!(parsed.protocol_id.protocol, 0x5678);
        assert_eq!(rest, &[0xCA, 0xFE, 0x42]);
    }

    #[test]
    fn decode_truncated_vendor() {
        // Flags = I | V, but only 1 byte where vendor needs 2.
        let bytes = vec![
            0b0001_0001, // I=1 V=1
            0x20,
            0x01,
            0x00, // exchange_id
            0x01,
            0x00, // protocol_short
            0x34, // truncated vendor (1 of 2 bytes)
        ];
        let err = decode_protocol_header(&bytes).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MalformedProtocolHeader(_)
        ));
    }

    #[test]
    fn encode_decode_with_vendor_roundtrip() {
        // Caller supplies vendor != 0; encoder auto-sets V and emits
        // vendor on the wire. Decoder reads it back.
        let header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x33,
            exchange_id: 0xABCD,
            protocol_id: ProtocolId {
                vendor: 0x1234,
                protocol: 0x5678,
            },
            ack_counter: None,
        };
        let mut out = Vec::new();
        encode_protocol_header(&header, &mut out);
        assert_eq!(out.len(), 8, "6 fixed + 2 vendor when V=1");
        assert_eq!(
            out[0] & ExchangeFlags::VENDOR.bits(),
            ExchangeFlags::VENDOR.bits(),
            "encoder auto-sets V when vendor != 0",
        );
        let (parsed, rest) = decode_protocol_header(&out).unwrap();
        assert!(parsed.exchange_flags.contains(ExchangeFlags::VENDOR));
        assert_eq!(parsed.protocol_id.vendor, 0x1234);
        assert_eq!(parsed.protocol_id.protocol, 0x5678);
        assert_eq!(rest, b"");
    }

    #[test]
    fn decode_truncated_sx_length_prefix() {
        // Flags = I | SX (V=0). 6-byte fixed + truncated SX length prefix.
        let bytes = vec![
            0b0000_1001, // I=1 SX=1
            0x20,
            0x01,
            0x00, // exchange_id
            0x01,
            0x00, // protocol_short
            0x03, // truncated length prefix (1 of 2 bytes)
        ];
        let err = decode_protocol_header(&bytes).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MalformedProtocolHeader(_)
        ));
    }

    #[test]
    fn decode_truncated_sx_data() {
        // Flags = I | SX (V=0). SX length says 10 bytes but only 3 follow.
        let bytes = vec![
            0b0000_1001, // I=1 SX=1
            0x20,
            0x01,
            0x00, // exchange_id
            0x01,
            0x00, // protocol_short
            0x0A,
            0x00, // SX length = 10
            0xAA,
            0xBB,
            0xCC, // only 3 of 10 bytes
        ];
        let err = decode_protocol_header(&bytes).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MalformedProtocolHeader(_)
        ));
    }

    #[test]
    fn decode_reserved_flag_bits_rejected() {
        // Bit 5 set is reserved per Matter spec §4.4.5.1.
        let bytes = [0b0010_0001, 0x20, 0x01, 0x00, 0x01, 0x00];
        let err = decode_protocol_header(&bytes).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MalformedProtocolHeader(_)
        ));
    }

    #[test]
    fn build_standalone_ack_header_initiator() {
        let h = build_standalone_ack_header(
            0x4242,
            MessageCounter(100),
            /* is_local_initiator */ true,
        );
        assert_eq!(h.opcode, opcode::secure_channel::STANDALONE_ACK);
        assert_eq!(h.exchange_id, 0x4242);
        assert_eq!(h.protocol_id, ProtocolId::SECURE_CHANNEL);
        assert_eq!(h.ack_counter, Some(MessageCounter(100)));
        assert!(h.exchange_flags.contains(ExchangeFlags::INITIATOR));
        assert!(h.exchange_flags.contains(ExchangeFlags::ACK));
        assert!(!h.exchange_flags.contains(ExchangeFlags::RELIABLE));
    }

    #[test]
    fn build_standalone_ack_header_responder() {
        let h = build_standalone_ack_header(
            0x4242,
            MessageCounter(100),
            /* is_local_initiator */ false,
        );
        assert!(!h.exchange_flags.contains(ExchangeFlags::INITIATOR));
        assert!(h.exchange_flags.contains(ExchangeFlags::ACK));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod proptest_module {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn roundtrip_any_header(
            initiator in any::<bool>(),
            ack in any::<bool>(),
            reliable in any::<bool>(),
            opcode in any::<u8>(),
            exchange_id in any::<u16>(),
            vendor in any::<u16>(),
            protocol in any::<u16>(),
            ack_value in any::<u32>(),
        ) {
            // The V and A bits are derived by the encoder from
            // `vendor != 0` and `ack_counter.is_some()` respectively.
            // To roundtrip, the input flag set must already include
            // these derived bits — otherwise the decoded header won't
            // compare equal to the input.
            let mut flags = ExchangeFlags::empty();
            if initiator { flags |= ExchangeFlags::INITIATOR; }
            if ack { flags |= ExchangeFlags::ACK; }
            if reliable { flags |= ExchangeFlags::RELIABLE; }
            if vendor != 0 { flags |= ExchangeFlags::VENDOR; }

            let header = ProtocolHeader {
                exchange_flags: flags,
                opcode,
                exchange_id,
                protocol_id: ProtocolId { vendor, protocol },
                ack_counter: if ack { Some(MessageCounter(ack_value)) } else { None },
            };

            let mut out = Vec::new();
            encode_protocol_header(&header, &mut out);
            let (parsed, rest) = decode_protocol_header(&out).unwrap();
            prop_assert_eq!(parsed, header);
            prop_assert!(rest.is_empty());
        }
    }
}

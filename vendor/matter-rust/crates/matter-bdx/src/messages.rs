//! BDX message codecs (Matter Core §11.21). All integers little-endian; the
//! Transfer Control byte packs `version` in the low nibble and the drive-mode
//! flags in the high bits. Metadata is an opaque passthrough (empty for OTA).

#![forbid(unsafe_code)]

use crate::error::BdxError;

/// BDX protocol version implemented here (Matter Core §11.21).
pub const BDX_VERSION: u8 = 0;

const VERSION_MASK: u8 = 0x0F;

bitflags::bitflags! {
    /// Transfer Control flags (high bits of the Transfer Control byte; the low
    /// nibble holds the version, handled separately).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TransferControl: u8 {
        /// Sender drives the transfer (pushes blocks).
        const SENDER_DRIVE = 0x10;
        /// Receiver drives the transfer (pulls blocks). OTA uses this.
        const RECEIVER_DRIVE = 0x20;
        /// Asynchronous mode (unused here).
        const ASYNC = 0x40;
    }
}

bitflags::bitflags! {
    /// Range Control flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RangeControl: u8 {
        /// A definite transfer length is present.
        const DEF_LEN = 0x01;
        /// A non-zero start offset is present.
        const START_OFFSET = 0x02;
        /// Offsets/lengths are 8 bytes (else 4).
        const WIDE_RANGE = 0x10;
    }
}

/// Little-endian cursor with bounds checks (no panics, no `unwrap`).
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub(crate) fn u8(&mut self) -> Result<u8, BdxError> {
        let b = *self.buf.get(self.pos).ok_or(BdxError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }
    pub(crate) fn u16_le(&mut self) -> Result<u16, BdxError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    pub(crate) fn u32_le(&mut self) -> Result<u32, BdxError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub(crate) fn u64_le(&mut self) -> Result<u64, BdxError> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_le_bytes(a))
    }
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], BdxError> {
        let end = self.pos.checked_add(n).ok_or(BdxError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(BdxError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let r = &self.buf[self.pos..];
        self.pos = self.buf.len();
        r
    }
}

/// Append a 4- or 8-byte little-endian range value (low 4 bytes of the u64 when
/// not wide — correct since LE puts the low word first).
fn put_range_value(buf: &mut Vec<u8>, value: u64, wide: bool) {
    let le = value.to_le_bytes();
    if wide {
        buf.extend_from_slice(&le);
    } else {
        buf.extend_from_slice(&le[..4]);
    }
}

/// `SendInit` / `ReceiveInit` — identical wire format (a proposed transfer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferInit {
    /// Proposed transfer-control flags (drive mode).
    pub control: TransferControl,
    /// Highest BDX version the proposer supports.
    pub version: u8,
    /// Proposed maximum block size.
    pub max_block_size: u16,
    /// Proposed start offset (0 = none).
    pub start_offset: u64,
    /// Proposed definite length (0 = indefinite).
    pub max_length: u64,
    /// File designator (for OTA, the `ImageURI` path).
    pub file_designator: Vec<u8>,
    /// Optional TLV metadata, opaque here (empty for OTA).
    pub metadata: Vec<u8>,
}

impl TransferInit {
    /// Encode to the BDX message body (raw little-endian).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let wide = self.start_offset > u64::from(u32::MAX) || self.max_length > u64::from(u32::MAX);
        let mut range = RangeControl::empty();
        range.set(RangeControl::DEF_LEN, self.max_length > 0);
        range.set(RangeControl::START_OFFSET, self.start_offset > 0);
        range.set(RangeControl::WIDE_RANGE, wide);

        let mut buf = Vec::with_capacity(16 + self.file_designator.len() + self.metadata.len());
        buf.push((self.version & VERSION_MASK) | self.control.bits());
        buf.push(range.bits());
        buf.extend_from_slice(&self.max_block_size.to_le_bytes());
        if self.start_offset > 0 {
            put_range_value(&mut buf, self.start_offset, wide);
        }
        if self.max_length > 0 {
            put_range_value(&mut buf, self.max_length, wide);
        }
        let fd_len = u16::try_from(self.file_designator.len()).unwrap_or(u16::MAX);
        buf.extend_from_slice(&fd_len.to_le_bytes());
        buf.extend_from_slice(&self.file_designator);
        buf.extend_from_slice(&self.metadata);
        buf
    }

    /// Decode a BDX `TransferInit` body.
    ///
    /// # Errors
    /// Returns [`BdxError::Truncated`] if the body ends before a fixed field.
    pub fn decode(body: &[u8]) -> Result<Self, BdxError> {
        let mut r = Reader::new(body);
        let ctl = r.u8()?;
        let range = RangeControl::from_bits_retain(r.u8()?);
        let max_block_size = r.u16_le()?;
        let version = ctl & VERSION_MASK;
        let control = TransferControl::from_bits_retain(ctl & !VERSION_MASK);
        let wide = range.contains(RangeControl::WIDE_RANGE);
        let start_offset = if range.contains(RangeControl::START_OFFSET) {
            if wide {
                r.u64_le()?
            } else {
                u64::from(r.u32_le()?)
            }
        } else {
            0
        };
        let max_length = if range.contains(RangeControl::DEF_LEN) {
            if wide {
                r.u64_le()?
            } else {
                u64::from(r.u32_le()?)
            }
        } else {
            0
        };
        let fd_len = usize::from(r.u16_le()?);
        let file_designator = r.take(fd_len)?.to_vec();
        let metadata = r.rest().to_vec();
        Ok(Self {
            control,
            version,
            max_block_size,
            start_offset,
            max_length,
            file_designator,
            metadata,
        })
    }
}

/// `ReceiveAccept` — the receiver-drive accept an OTA Provider sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveAccept {
    /// Agreed transfer-control flags (drive mode).
    pub control: TransferControl,
    /// Agreed BDX version.
    pub version: u8,
    /// Chosen maximum block size.
    pub max_block_size: u16,
    /// Chosen start offset (0 = none).
    pub start_offset: u64,
    /// Definite transfer length (0 = indefinite).
    pub length: u64,
    /// Optional TLV metadata, opaque here.
    pub metadata: Vec<u8>,
}

impl ReceiveAccept {
    /// Encode to the BDX message body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let wide = self.start_offset > u64::from(u32::MAX) || self.length > u64::from(u32::MAX);
        let mut range = RangeControl::empty();
        range.set(RangeControl::DEF_LEN, self.length > 0);
        range.set(RangeControl::START_OFFSET, self.start_offset > 0);
        range.set(RangeControl::WIDE_RANGE, wide);

        let mut buf = Vec::with_capacity(16 + self.metadata.len());
        buf.push((self.version & VERSION_MASK) | self.control.bits());
        buf.push(range.bits());
        buf.extend_from_slice(&self.max_block_size.to_le_bytes());
        if self.start_offset > 0 {
            put_range_value(&mut buf, self.start_offset, wide);
        }
        if self.length > 0 {
            put_range_value(&mut buf, self.length, wide);
        }
        buf.extend_from_slice(&self.metadata);
        buf
    }

    /// Decode a BDX `ReceiveAccept` body.
    ///
    /// # Errors
    /// Returns [`BdxError::Truncated`] if the body ends before a fixed field.
    pub fn decode(body: &[u8]) -> Result<Self, BdxError> {
        let mut r = Reader::new(body);
        let ctl = r.u8()?;
        let range = RangeControl::from_bits_retain(r.u8()?);
        let max_block_size = r.u16_le()?;
        let version = ctl & VERSION_MASK;
        let control = TransferControl::from_bits_retain(ctl & !VERSION_MASK);
        let wide = range.contains(RangeControl::WIDE_RANGE);
        let start_offset = if range.contains(RangeControl::START_OFFSET) {
            if wide {
                r.u64_le()?
            } else {
                u64::from(r.u32_le()?)
            }
        } else {
            0
        };
        let length = if range.contains(RangeControl::DEF_LEN) {
            if wide {
                r.u64_le()?
            } else {
                u64::from(r.u32_le()?)
            }
        } else {
            0
        };
        let metadata = r.rest().to_vec();
        Ok(Self {
            control,
            version,
            max_block_size,
            start_offset,
            length,
            metadata,
        })
    }
}

/// `SendAccept` — accept of a `SendInit` (no range/offset/length on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendAccept {
    /// Agreed transfer-control flags.
    pub control: TransferControl,
    /// Agreed BDX version.
    pub version: u8,
    /// Chosen maximum block size.
    pub max_block_size: u16,
    /// Optional TLV metadata, opaque here.
    pub metadata: Vec<u8>,
}

impl SendAccept {
    /// Encode to the BDX message body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(3 + self.metadata.len());
        buf.push((self.version & VERSION_MASK) | self.control.bits());
        buf.extend_from_slice(&self.max_block_size.to_le_bytes());
        buf.extend_from_slice(&self.metadata);
        buf
    }

    /// Decode a BDX `SendAccept` body.
    ///
    /// # Errors
    /// Returns [`BdxError::Truncated`] if the body ends before a fixed field.
    pub fn decode(body: &[u8]) -> Result<Self, BdxError> {
        let mut r = Reader::new(body);
        let ctl = r.u8()?;
        let max_block_size = r.u16_le()?;
        let version = ctl & VERSION_MASK;
        let control = TransferControl::from_bits_retain(ctl & !VERSION_MASK);
        let metadata = r.rest().to_vec();
        Ok(Self {
            control,
            version,
            max_block_size,
            metadata,
        })
    }
}

/// `BlockQuery` / `BlockAck` / `BlockAckEOF` — a bare block counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterMessage {
    /// The block counter.
    pub block_counter: u32,
}

impl CounterMessage {
    /// Encode to the BDX message body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.block_counter.to_le_bytes().to_vec()
    }
    /// Decode a BDX counter-message body.
    ///
    /// # Errors
    /// Returns [`BdxError::Truncated`] if fewer than 4 bytes.
    pub fn decode(body: &[u8]) -> Result<Self, BdxError> {
        let mut r = Reader::new(body);
        Ok(Self {
            block_counter: r.u32_le()?,
        })
    }
}

/// `Block` / `BlockEOF` — a block counter followed by the block data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataBlock {
    /// The block counter.
    pub block_counter: u32,
    /// The block payload.
    pub data: Vec<u8>,
}

impl DataBlock {
    /// Encode to the BDX message body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + self.data.len());
        buf.extend_from_slice(&self.block_counter.to_le_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }
    /// Decode a BDX data-block body.
    ///
    /// # Errors
    /// Returns [`BdxError::Truncated`] if fewer than 4 bytes.
    pub fn decode(body: &[u8]) -> Result<Self, BdxError> {
        let mut r = Reader::new(body);
        let block_counter = r.u32_le()?;
        Ok(Self {
            block_counter,
            data: r.rest().to_vec(),
        })
    }
}

/// A decoded BDX message, tagged by its [`MessageType`](crate::MessageType).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BdxMessage {
    /// `ReceiveInit` (0x04).
    ReceiveInit(TransferInit),
    /// `SendInit` (0x01).
    SendInit(TransferInit),
    /// `ReceiveAccept` (0x05).
    ReceiveAccept(ReceiveAccept),
    /// `SendAccept` (0x02).
    SendAccept(SendAccept),
    /// `BlockQuery` (0x10).
    BlockQuery(CounterMessage),
    /// `Block` (0x11).
    Block(DataBlock),
    /// `BlockEOF` (0x12).
    BlockEof(DataBlock),
    /// `BlockAck` (0x13).
    BlockAck(CounterMessage),
    /// `BlockAckEOF` (0x14).
    BlockAckEof(CounterMessage),
}

impl BdxMessage {
    /// Decode a BDX message body given its [`MessageType`](crate::MessageType).
    ///
    /// # Errors
    /// Returns [`BdxError::Truncated`] on a short body.
    pub fn decode(message_type: crate::MessageType, body: &[u8]) -> Result<Self, BdxError> {
        use crate::MessageType as Mt;
        Ok(match message_type {
            Mt::ReceiveInit => Self::ReceiveInit(TransferInit::decode(body)?),
            Mt::SendInit => Self::SendInit(TransferInit::decode(body)?),
            Mt::ReceiveAccept => Self::ReceiveAccept(ReceiveAccept::decode(body)?),
            Mt::SendAccept => Self::SendAccept(SendAccept::decode(body)?),
            Mt::BlockQuery => Self::BlockQuery(CounterMessage::decode(body)?),
            Mt::Block => Self::Block(DataBlock::decode(body)?),
            Mt::BlockEof => Self::BlockEof(DataBlock::decode(body)?),
            Mt::BlockAck => Self::BlockAck(CounterMessage::decode(body)?),
            Mt::BlockAckEof => Self::BlockAckEof(CounterMessage::decode(body)?),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    // ReceiveInit, field-by-field (all LE):
    //   20            TransferControl: version 0 | ReceiverDrive(0x20)
    //   00            RangeControl: no DefLen, no StartOffset, not wide
    //   00 04         MaxBlockSize u16 = 0x0400 = 1024
    //   06 00         FileDesignatorLength u16 = 6
    //   66 77 2e 6f 74 61   "fw.ota"
    const RECEIVE_INIT_HEX: &str = "20000004060066772e6f7461";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn transfer_init_encodes_to_frozen_bytes() {
        let init = TransferInit {
            control: TransferControl::RECEIVER_DRIVE,
            version: 0,
            max_block_size: 1024,
            start_offset: 0,
            max_length: 0,
            file_designator: b"fw.ota".to_vec(),
            metadata: Vec::new(),
        };
        assert_eq!(init.encode(), unhex(RECEIVE_INIT_HEX));
    }

    #[test]
    fn transfer_init_roundtrips() {
        let init = TransferInit {
            control: TransferControl::RECEIVER_DRIVE,
            version: 0,
            max_block_size: 1024,
            start_offset: 0,
            max_length: 0,
            file_designator: b"fw.ota".to_vec(),
            metadata: Vec::new(),
        };
        assert_eq!(TransferInit::decode(&init.encode()).unwrap(), init);
    }

    #[test]
    fn transfer_init_wide_range_offset_and_length_roundtrip() {
        // Offset/length above u32::MAX force the WideRange (8-byte) encoding.
        let init = TransferInit {
            control: TransferControl::RECEIVER_DRIVE,
            version: 0,
            max_block_size: 256,
            start_offset: 0x1_0000_0000,
            max_length: 0x2_0000_0000,
            file_designator: b"x".to_vec(),
            metadata: Vec::new(),
        };
        let decoded = TransferInit::decode(&init.encode()).unwrap();
        assert_eq!(decoded, init);
    }

    #[test]
    fn transfer_init_decode_truncated_errs() {
        assert_eq!(TransferInit::decode(&[0x20]), Err(BdxError::Truncated));
    }

    // ReceiveAccept, field-by-field (all LE):
    //   20            TransferControl: version 0 | ReceiverDrive
    //   01            RangeControl: DefLen (Length present), no offset, not wide
    //   00 04         MaxBlockSize u16 = 1024
    //   0a 00 00 00   Length u32 = 10
    const RECEIVE_ACCEPT_HEX: &str = "200100040a000000";

    #[test]
    fn receive_accept_encodes_to_frozen_bytes() {
        let acc = ReceiveAccept {
            control: TransferControl::RECEIVER_DRIVE,
            version: 0,
            max_block_size: 1024,
            start_offset: 0,
            length: 10,
            metadata: Vec::new(),
        };
        assert_eq!(acc.encode(), unhex(RECEIVE_ACCEPT_HEX));
        assert_eq!(ReceiveAccept::decode(&acc.encode()).unwrap(), acc);
    }

    #[test]
    fn send_accept_roundtrips() {
        let acc = SendAccept {
            control: TransferControl::RECEIVER_DRIVE,
            version: 0,
            max_block_size: 512,
            metadata: Vec::new(),
        };
        assert_eq!(acc.encode(), unhex("200002")); // 20 | 00 02 (512 LE)
        assert_eq!(SendAccept::decode(&acc.encode()).unwrap(), acc);
    }

    #[test]
    fn counter_message_roundtrips_and_freezes() {
        let q = CounterMessage { block_counter: 0 };
        assert_eq!(q.encode(), unhex("00000000"));
        assert_eq!(CounterMessage::decode(&q.encode()).unwrap(), q);
        let ack = CounterMessage { block_counter: 2 };
        assert_eq!(ack.encode(), unhex("02000000"));
        assert_eq!(
            CounterMessage::decode(&[0x00, 0x00]),
            Err(BdxError::Truncated)
        );
    }

    #[test]
    fn data_block_roundtrips_and_freezes() {
        // Block counter 0, data "HELL".
        let b = DataBlock {
            block_counter: 0,
            data: b"HELL".to_vec(),
        };
        assert_eq!(b.encode(), unhex("0000000048454c4c"));
        assert_eq!(DataBlock::decode(&b.encode()).unwrap(), b);
        // BlockEOF counter 2, data "RLD".
        let eof = DataBlock {
            block_counter: 2,
            data: b"RLD".to_vec(),
        };
        assert_eq!(eof.encode(), unhex("02000000524c44"));
    }

    #[test]
    fn bdx_message_dispatch_decodes_by_type() {
        use crate::MessageType;
        let body = CounterMessage { block_counter: 5 }.encode();
        match BdxMessage::decode(MessageType::BlockQuery, &body).unwrap() {
            BdxMessage::BlockQuery(c) => assert_eq!(c.block_counter, 5),
            other => panic!("expected BlockQuery, got {other:?}"),
        }
        let blk = DataBlock {
            block_counter: 1,
            data: b"xy".to_vec(),
        }
        .encode();
        match BdxMessage::decode(MessageType::BlockEof, &blk).unwrap() {
            BdxMessage::BlockEof(d) => {
                assert_eq!(d.block_counter, 1);
                assert_eq!(d.data, b"xy");
            }
            other => panic!("expected BlockEof, got {other:?}"),
        }
    }
}

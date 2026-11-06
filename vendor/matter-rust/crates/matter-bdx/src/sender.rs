//! Receiver-driven BDX **sender** state machine (Matter Core §11.21) — the role
//! an OTA Provider plays serving a firmware image. Sans-I/O: it consumes decoded
//! inbound messages and emits the next outbound message (or an abort). The
//! caller frames each [`OutgoingMessage`] over `ProtocolId::BDX`.
//!
//! Flow: accept a `ReceiveInit` → `ReceiveAccept`; answer each `BlockQuery` with
//! the next `Block`/`BlockEOF`; finish on `BlockAckEOF`. Block counters start at
//! 0 and increment per block; a block is `BlockEOF` once the cumulative bytes
//! reach the (definite) image length — so the last full block is itself
//! `BlockEOF` (chip `BdxOtaSender` semantics), with no trailing empty block.

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::message_type::{BdxStatusCode, MessageType};
use crate::messages::BDX_VERSION;
use crate::messages::{CounterMessage, ReceiveAccept, TransferControl, TransferInit};

/// A BDX message the state machine wants sent, tagged with its opcode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    /// The BDX message type (its value is the protocol-header opcode).
    pub message_type: MessageType,
    /// The encoded BDX message body.
    pub payload: Vec<u8>,
}

/// The result of feeding the state machine an inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderOutcome {
    /// Send this message to the receiver.
    Send(OutgoingMessage),
    /// The transfer completed successfully (final ack received).
    Done,
    /// Abort the transfer with this BDX status (caller sends a `StatusReport`).
    Abort(BdxStatusCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitingInit,
    Sending,
    AwaitingEofAck,
    Complete,
    Aborted,
}

/// Receiver-driven BDX sender serving `image` to a pulling receiver.
#[derive(Debug, Clone)]
pub struct BlockSender {
    image: Arc<[u8]>,
    max_block_size: u16,
    offset: usize,
    next_block_counter: u32,
    last_block_counter: u32,
    state: State,
}

impl BlockSender {
    /// Create a sender for `image`, proposing up to `max_block_size` bytes per
    /// block (the negotiated size is `min(max_block_size, receiver proposal)`).
    /// Copies `image` into a shared allocation; use [`Self::from_shared`] to
    /// serve an image you already hold in an `Arc` without copying.
    #[must_use]
    pub fn new(image: Vec<u8>, max_block_size: u16) -> Self {
        Self::from_shared(Arc::from(image), max_block_size)
    }

    /// Create a sender serving a shared image. Clones of the `Arc` share one
    /// allocation, so a provider serving the same image across many transfers
    /// (or re-arming mid-flow) holds exactly one copy of the bytes.
    #[must_use]
    pub fn from_shared(image: Arc<[u8]>, max_block_size: u16) -> Self {
        Self {
            image,
            max_block_size,
            offset: 0,
            next_block_counter: 0,
            last_block_counter: 0,
            state: State::AwaitingInit,
        }
    }

    /// Whether the transfer has completed successfully.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state == State::Complete
    }

    /// Accept an inbound `ReceiveInit`, producing the `ReceiveAccept` to send.
    ///
    /// Aborts (returns [`SenderOutcome::Abort`]) if the proposal is not
    /// receiver-drive or arrives out of state — it never panics.
    pub fn accept_receive_init(&mut self, init: &TransferInit) -> SenderOutcome {
        if self.state != State::AwaitingInit {
            return self.abort(BdxStatusCode::UnexpectedMessage);
        }
        if !init.control.contains(TransferControl::RECEIVER_DRIVE) {
            return self.abort(BdxStatusCode::TransferMethodNotSupported);
        }
        let negotiated = init.max_block_size.min(self.max_block_size);
        self.max_block_size = negotiated;
        let length = u64::try_from(self.image.len()).unwrap_or(u64::MAX);
        let accept = ReceiveAccept {
            control: TransferControl::RECEIVER_DRIVE,
            // Agreed version = min(our version, proposed). BDX_VERSION is 0 (the
            // only spec version), so the negotiation always lands on it.
            version: BDX_VERSION,
            max_block_size: negotiated,
            start_offset: 0,
            length,
            metadata: Vec::new(),
        };
        self.state = State::Sending;
        SenderOutcome::Send(OutgoingMessage {
            message_type: MessageType::ReceiveAccept,
            payload: accept.encode(),
        })
    }

    /// Answer an inbound `BlockQuery` with the next `Block`/`BlockEOF`.
    ///
    /// Aborts on an out-of-state query or a block-counter mismatch.
    pub fn handle_block_query(&mut self, query: &CounterMessage) -> SenderOutcome {
        if self.state != State::Sending {
            return self.abort(BdxStatusCode::UnexpectedMessage);
        }
        if query.block_counter != self.next_block_counter {
            return self.abort(BdxStatusCode::BadBlockCounter);
        }
        let end = self
            .offset
            .saturating_add(usize::from(self.max_block_size))
            .min(self.image.len());
        let is_eof = end == self.image.len();
        let counter = self.next_block_counter;
        // counter LE ++ block bytes — exactly DataBlock::encode's layout,
        // built once from the shared image (no intermediate Vec).
        let mut payload = Vec::with_capacity(4 + (end - self.offset));
        payload.extend_from_slice(&counter.to_le_bytes());
        payload.extend_from_slice(&self.image[self.offset..end]);

        self.offset = end;
        self.last_block_counter = counter;
        self.next_block_counter = self.next_block_counter.wrapping_add(1);

        let message_type = if is_eof {
            self.state = State::AwaitingEofAck;
            MessageType::BlockEof
        } else {
            MessageType::Block
        };
        SenderOutcome::Send(OutgoingMessage {
            message_type,
            payload,
        })
    }

    /// Process the final `BlockAckEOF`, completing the transfer.
    ///
    /// Aborts on an out-of-state ack or a block-counter mismatch.
    pub fn handle_block_ack_eof(&mut self, ack: &CounterMessage) -> SenderOutcome {
        if self.state != State::AwaitingEofAck {
            return self.abort(BdxStatusCode::UnexpectedMessage);
        }
        if ack.block_counter != self.last_block_counter {
            return self.abort(BdxStatusCode::BadBlockCounter);
        }
        self.state = State::Complete;
        SenderOutcome::Done
    }

    fn abort(&mut self, code: BdxStatusCode) -> SenderOutcome {
        self.state = State::Aborted;
        SenderOutcome::Abort(code)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use crate::messages::{DataBlock, ReceiveAccept, TransferControl, TransferInit};
    use std::sync::Arc;

    #[test]
    fn from_shared_serves_without_copying_the_image() {
        let image: Arc<[u8]> = Arc::from(&b"0123456789ABCDEF"[..]); // 16 bytes
        let mut sender = BlockSender::from_shared(Arc::clone(&image), 8);
        // The sender holds a handle, not a copy.
        assert_eq!(Arc::strong_count(&image), 2);

        let _ = sender.accept_receive_init(&receiver_init(8));

        // Block 0
        let SenderOutcome::Send(o0) =
            sender.handle_block_query(&CounterMessage { block_counter: 0 })
        else {
            panic!()
        };
        assert_eq!(o0.message_type, MessageType::Block);
        let d0 = DataBlock::decode(&o0.payload).unwrap();
        assert_eq!(d0.data, b"01234567");

        // Block 1 -> BlockEOF (exact multiple: 16 bytes / 8 = 2 blocks)
        let SenderOutcome::Send(o1) =
            sender.handle_block_query(&CounterMessage { block_counter: 1 })
        else {
            panic!()
        };
        assert_eq!(o1.message_type, MessageType::BlockEof);
        let d1 = DataBlock::decode(&o1.payload).unwrap();
        assert_eq!(d1.data, b"89ABCDEF");

        let mut reassembled = d0.data.clone();
        reassembled.extend_from_slice(&d1.data);
        assert_eq!(reassembled.as_slice(), &image[..]);

        assert!(!sender.is_complete());
        assert_eq!(
            sender.handle_block_ack_eof(&CounterMessage { block_counter: 1 }),
            SenderOutcome::Done
        );
        assert!(sender.is_complete());

        // Still just the two handles (image + sender's clone) — no copy made.
        assert_eq!(Arc::strong_count(&image), 2);
    }

    fn receiver_init(max_block_size: u16) -> TransferInit {
        TransferInit {
            control: TransferControl::RECEIVER_DRIVE,
            version: 0,
            max_block_size,
            start_offset: 0,
            max_length: 0,
            file_designator: b"fw.ota".to_vec(),
            metadata: Vec::new(),
        }
    }

    #[test]
    fn accept_negotiates_min_block_size_and_definite_length() {
        let mut s = BlockSender::new(b"HELLO WORLD".to_vec(), 1024); // 11 bytes
        let SenderOutcome::Send(out) = s.accept_receive_init(&receiver_init(4)) else {
            panic!("expected Send(ReceiveAccept)");
        };
        assert_eq!(out.message_type, MessageType::ReceiveAccept);
        let acc = ReceiveAccept::decode(&out.payload).unwrap();
        assert!(acc.control.contains(TransferControl::RECEIVER_DRIVE));
        assert_eq!(acc.max_block_size, 4); // min(4, 1024)
        assert_eq!(acc.length, 11); // definite length = image size
        assert_eq!(acc.version, 0);
    }

    #[test]
    fn rejects_non_receiver_drive_init() {
        let mut s = BlockSender::new(b"x".to_vec(), 16);
        let mut init = receiver_init(16);
        init.control = TransferControl::SENDER_DRIVE;
        assert_eq!(
            s.accept_receive_init(&init),
            SenderOutcome::Abort(BdxStatusCode::TransferMethodNotSupported)
        );
    }

    #[test]
    fn query_with_wrong_counter_aborts() {
        let mut s = BlockSender::new(b"HELLO WORLD".to_vec(), 4);
        let _ = s.accept_receive_init(&receiver_init(4));
        assert_eq!(
            s.handle_block_query(&CounterMessage { block_counter: 7 }),
            SenderOutcome::Abort(BdxStatusCode::BadBlockCounter)
        );
    }

    #[test]
    fn full_three_block_transfer() {
        let mut s = BlockSender::new(b"HELLO WORLD".to_vec(), 4); // 11 bytes -> 4,4,3
        let _ = s.accept_receive_init(&receiver_init(4));

        // Block 0
        let SenderOutcome::Send(o0) = s.handle_block_query(&CounterMessage { block_counter: 0 })
        else {
            panic!()
        };
        assert_eq!(o0.message_type, MessageType::Block);
        assert_eq!(DataBlock::decode(&o0.payload).unwrap().data, b"HELL");
        // Block 1
        let SenderOutcome::Send(o1) = s.handle_block_query(&CounterMessage { block_counter: 1 })
        else {
            panic!()
        };
        assert_eq!(o1.message_type, MessageType::Block);
        assert_eq!(DataBlock::decode(&o1.payload).unwrap().data, b"O WO");
        // Block 2 -> BlockEOF (short final block)
        let SenderOutcome::Send(o2) = s.handle_block_query(&CounterMessage { block_counter: 2 })
        else {
            panic!()
        };
        assert_eq!(o2.message_type, MessageType::BlockEof);
        assert_eq!(DataBlock::decode(&o2.payload).unwrap().data, b"RLD");

        assert!(!s.is_complete());
        assert_eq!(
            s.handle_block_ack_eof(&CounterMessage { block_counter: 2 }),
            SenderOutcome::Done
        );
        assert!(s.is_complete());
    }

    #[test]
    fn exact_multiple_marks_last_full_block_as_eof() {
        let mut s = BlockSender::new(b"ABCDEFGH".to_vec(), 4); // 8 bytes -> 4,4 (exact)
        let _ = s.accept_receive_init(&receiver_init(4));
        let _ = s.handle_block_query(&CounterMessage { block_counter: 0 }); // Block "ABCD"
        let SenderOutcome::Send(o1) = s.handle_block_query(&CounterMessage { block_counter: 1 })
        else {
            panic!()
        };
        assert_eq!(o1.message_type, MessageType::BlockEof); // last full block IS EOF
        assert_eq!(DataBlock::decode(&o1.payload).unwrap().data, b"EFGH");
    }

    #[test]
    fn out_of_state_query_before_accept_aborts() {
        let mut s = BlockSender::new(b"x".to_vec(), 8);
        assert_eq!(
            s.handle_block_query(&CounterMessage { block_counter: 0 }),
            SenderOutcome::Abort(BdxStatusCode::UnexpectedMessage)
        );
    }
}

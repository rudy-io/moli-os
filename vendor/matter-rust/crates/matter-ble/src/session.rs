//! Sans-IO BTP session engine — RX reassembly + acknowledgement generation and
//! TX segmentation + flow control.
//!
//! Byte-grounded against connectedhomeip `src/ble/BtpEngine.cpp`
//! (`HandleCharacteristicReceived`, `HandleCharacteristicSend`,
//! `GetAndIncrementNextTxSeqNum`, `GetAndRecordRxAckSeqNum`, `HandleAckReceived`,
//! `IsValidAck`, `EncodeStandAloneAck`) and `BLEEndPoint.cpp` send-path /
//! window / timer semantics (`DriveSending`, `SendNextMessage`,
//! `ContinueMessageSend`, `PrepareNextFragment`, `AdjustRemoteReceiveWindow`).
//! Vectors: `test-vectors/btp/segments_rx.json`,
//! `test-vectors/btp/segments_tx.json`, `test-vectors/btp/standalone_ack.json`.
//!
//! # TX segmentation and flow control
//! [`BtpSession::queue_message`] hands one whole Matter SDU to the fragmenter;
//! [`BtpSession::next_gatt_write`] emits the next data fragment (respecting the
//! remote receive window and the one-GATT-op-in-flight rule) and
//! [`BtpSession::gatt_write_completed`] releases the in-flight latch. This
//! mirrors chip's `DriveSending`: a fragment is withheld while a GATT write is
//! outstanding, while the remote window is 0, or while it is 1 and there is no
//! pending acknowledgement to piggyback (chip
//! `BTP_WINDOW_NO_ACK_SEND_THRESHOLD`). Each departing data fragment debits the
//! remote window, consumes a TX sequence number, and (if not already awaiting
//! one) arms the 15 s ack-timeout — the exact registration a standalone ack
//! performs. A departing fragment piggybacks any pending receive ack (chip
//! `PrepareNextFragment` → `GetAndRecordRxAckSeqNum`): the `0x08` flag and ack
//! byte are added, the pending-ack state is cleared, and the local receive
//! window is reset to its maximum. An inbound ack credits the remote window per
//! chip `AdjustRemoteReceiveWindow`, re-opening a window that had closed.
//!
//! # Sans-IO discipline
//! [`BtpSession::on_indication`] NEVER returns outbound bytes. When a received
//! fragment makes an acknowledgement due (2.5 s send-ack timer, or immediately
//! when the local receive window is exhausted) it records that state so
//! [`BtpSession::poll_timeout`] surfaces the deadline and
//! [`BtpSession::handle_timeout`] emits the standalone ack. This mirrors
//! `matter-transport`'s MRP engine. A standalone ack is a full outbound
//! fragment: it consumes a TX sequence number and registers itself as an
//! outstanding unacknowledged fragment (chip `EncodeStandAloneAck` →
//! `GetAndIncrementNextTxSeqNum`, then `DoSendStandAloneAck` →
//! `StartAckReceivedTimer`), so it is itself subject to the 15 s ack-timeout
//! and a subsequent inbound ack for it is valid.
//!
//! # RX error mapping (chip error name → [`BtpError`])
//! Each RX vector maps to exactly one variant:
//! - `reassembler_incorrect_state` (a `Begin`/data fragment while a message is
//!   already mid-reassembly) → [`BtpError::ReassemblerInvalidState`]. We also
//!   fold chip's idle-non-`Begin` case (chip `INVALID_BTP_HEADER_FLAGS`) into
//!   this variant — a documented simplification; both kill the session.
//! - `invalid_btp_sequence_number` (a fragment/ack whose own sequence byte
//!   != `RxNext`) → [`BtpError::InvalidSequence`]. Chip checks this AFTER the
//!   piggyback ack, so a valid ack in the same packet is applied before the
//!   sequence check fails (`handle_ack_received_incorrect_sequence`).
//! - `invalid_ack` (standalone/piggyback ack when none is outstanding, or an
//!   ack for a sequence number outside the outstanding TX interval) →
//!   [`BtpError::InvalidAck`].
//!
//! # Fused receive+take (BLEEndPoint-stack semantics)
//! Chip's *engine* holds a completed message in `kState_Complete` until the
//! upper layer calls `TakeRxPacket`, which resets it to `kState_Idle`
//! (`BtpEngine.cpp:395-397`). In chip's *real* stack the take is synchronous:
//! `BLEEndPoint::HandleCharacteristicReceived` calls `TakeRxPacket` in the same
//! turn it observes `kState_Complete` (`BLEEndPoint.cpp:1278`), so a fresh
//! `Begin` for the next message is legal immediately after a completed one —
//! BTP flow control is fragment-based, so a peer may send up to `window`
//! fragments (hence two back-to-back single-fragment messages) before any ack.
//! Our [`BtpSession::on_indication`] fuses receive-and-take: it returns the
//! reassembled SDU and resets reassembly to idle in the same call, matching the
//! stack. A `Begin` arriving *mid-reassembly* (before the `End`) is still an
//! error. The `handle_characteristic_received_incorrect_sequence` vector
//! documents the engine-ISOLATION semantics (chip's unit test never calls
//! `TakeRxPacket`), which do not apply to a fused receive+take; its test is
//! rewritten to exercise the mid-reassembly `Begin` that IS reachable here.

use std::time::{Duration, Instant};

use crate::handshake::MIN_SEGMENT_SIZE;
use crate::BtpError;

/// Time a receiver may buffer a pending acknowledgement before it must emit a
/// standalone ack (chip `BTP_ACK_SEND_TIMEOUT_MS`).
pub const ACK_SEND_TIMEOUT: Duration = Duration::from_millis(2500);
/// Time a sender waits for an acknowledgement of an outstanding fragment
/// before declaring the session dead (chip `BTP_ACK_TIMEOUT_MS`).
pub const ACK_TIMEOUT: Duration = Duration::from_secs(15);
/// When the local receive window drops to this many free slots (with nothing
/// outbound to piggyback on), an acknowledgement is sent immediately
/// (chip `BLE_CONFIG_IMMEDIATE_ACK_WINDOW_THRESHOLD`).
pub const IMMEDIATE_ACK_THRESHOLD: u8 = 1;
/// Remote-window level at or below which a data fragment is withheld unless the
/// same fragment also carries a piggyback acknowledgement (which re-opens the
/// remote's window). Chip `BTP_WINDOW_NO_ACK_SEND_THRESHOLD`.
pub const WINDOW_NO_ACK_SEND_THRESHOLD: u8 = 1;
/// Upper bound on a single reassembled BTP SDU (one chip pbuf).
pub const RX_REASSEMBLY_CAP: usize = 2048;

const FLAG_START: u8 = 0x01;
const FLAG_CONTINUE: u8 = 0x02;
const FLAG_END: u8 = 0x04;
const FLAG_ACK: u8 = 0x08;

/// Which side of the BTP connection this engine models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The commissioner (GATT client). Receives the handshake response as an
    /// implicit sequence 0, arming the 2.5 s send-ack timer at construction.
    Central,
    /// The commissionee (GATT server). Starts with its handshake response
    /// (sequence 0) outstanding and awaiting acknowledgement.
    Peripheral,
}

/// Sans-IO BTP session engine (chip `BtpEngine` + `BLEEndPoint` timer
/// semantics). All time is injected via [`Instant`] — no tokio, no clocks.
#[derive(Debug)]
pub struct BtpSession {
    role: Role,
    segment_size: u16,
    local_window_max: u8,
    local_window: u8,

    // RX reassembly.
    rx_next_seq: u8,
    rx_buf: Vec<u8>,
    rx_expected_len: Option<u16>,
    unacked_rx: Option<u8>,
    ack_due_at: Option<Instant>,

    // TX outstanding-acknowledgement tracking (shared by the send path and the
    // inbound-ack validator).
    tx_next_seq: u8,
    tx_oldest_unacked: u8,
    tx_newest_unacked: u8,
    expecting_ack: bool,
    tx_unacked_since: Option<Instant>,

    // TX flow control and segmentation.
    /// Free slots in the remote peer's receive window: how many more data
    /// fragments we may put on the wire before we must wait for an ack. Debited
    /// per departing fragment, credited by inbound acks (chip
    /// `mRemoteReceiveWindowSize`). Both directions share the negotiated window,
    /// so `local_window_max` is also the remote window's maximum.
    remote_window: u8,
    /// The whole SDU currently being fragmented, or `None` when the fragmenter
    /// is idle. `Some` with `tx_sent < len` means fragments remain unsent
    /// (chip `kState_InProgress`); it is cleared the moment the final fragment
    /// departs (fused with chip's `TakeTxPacket`).
    tx_pending: Option<Vec<u8>>,
    /// Payload bytes of `tx_pending` already emitted (chip advances `mTxBuf` /
    /// shrinks `mTxLength`; we track the forward offset instead).
    tx_sent: usize,
    /// A GATT write we issued is awaiting its completion callback; no further
    /// fragment may depart until [`BtpSession::gatt_write_completed`]
    /// (chip `ConnectionStateFlag::kGattOperationInFlight`).
    gatt_in_flight: bool,
}

impl BtpSession {
    /// Construct a session for `role` with the negotiated `segment_size` and
    /// receive `window`.
    ///
    /// Central construction arms the 2.5 s send-ack timer for sequence 0 (the
    /// handshake response indication implicitly occupies sequence 0 — chip
    /// `BLEEndPoint.cpp:1084-1091`) and debits the local receive window by 1.
    /// Peripheral construction starts with TX sequence 0 outstanding (15 s ack
    /// deadline).
    #[must_use]
    pub fn new(role: Role, segment_size: u16, window: u8, now: Instant) -> Self {
        let segment_size = segment_size.max(MIN_SEGMENT_SIZE);
        let window = window.max(1);
        match role {
            Role::Central => Self {
                role,
                segment_size,
                local_window_max: window,
                local_window: window.saturating_sub(1),
                rx_next_seq: 1,
                rx_buf: Vec::new(),
                rx_expected_len: None,
                unacked_rx: Some(0),
                ack_due_at: Some(now + ACK_SEND_TIMEOUT),
                tx_next_seq: 0,
                tx_oldest_unacked: 0,
                tx_newest_unacked: 0,
                expecting_ack: false,
                tx_unacked_since: None,
                remote_window: window,
                tx_pending: None,
                tx_sent: 0,
                gatt_in_flight: false,
            },
            Role::Peripheral => Self {
                role,
                segment_size,
                local_window_max: window,
                local_window: window,
                rx_next_seq: 0,
                rx_buf: Vec::new(),
                rx_expected_len: None,
                unacked_rx: None,
                ack_due_at: None,
                tx_next_seq: 1,
                tx_oldest_unacked: 0,
                tx_newest_unacked: 0,
                expecting_ack: true,
                tx_unacked_since: Some(now),
                remote_window: window,
                tx_pending: None,
                tx_sent: 0,
                gatt_in_flight: false,
            },
        }
    }

    /// Feed one C2 indication (central) / C1 write (peripheral, tests only).
    ///
    /// Returns `Ok(Some(sdu))` when a Matter message completes reassembly,
    /// `Ok(None)` when more fragments are expected or the packet was a pure
    /// standalone ack. Never returns outbound bytes (see the module-level
    /// sans-IO note); a due acknowledgement is surfaced via [`Self::poll_timeout`].
    ///
    /// # Errors
    /// - [`BtpError::PacketTooShort`]: fewer bytes than the flags imply.
    /// - [`BtpError::InvalidAck`]: a piggyback ack outside the outstanding TX range.
    /// - [`BtpError::InvalidSequence`]: the fragment's sequence byte != `RxNext`.
    /// - [`BtpError::ReassemblerInvalidState`]: a fragment illegal for the
    ///   current reassembly state.
    /// - [`BtpError::ReassemblyOverflow`]: a declared or accumulated length
    ///   exceeding [`RX_REASSEMBLY_CAP`].
    pub fn on_indication(
        &mut self,
        packet: &[u8],
        now: Instant,
    ) -> Result<Option<Vec<u8>>, BtpError> {
        // Truncate to the negotiated fragment size: the BLE characteristic may
        // be larger than what was negotiated (chip truncates to mRxFragmentSize).
        let packet = &packet[..packet.len().min(self.segment_size as usize)];

        let flags = *packet.first().ok_or(BtpError::PacketTooShort)?;
        let mut idx = 1usize;

        // Piggyback / standalone ack — parsed and validated BEFORE the sequence
        // number, matching chip's HandleCharacteristicReceived ordering.
        if flags & FLAG_ACK != 0 {
            let ack = *packet.get(idx).ok_or(BtpError::PacketTooShort)?;
            idx += 1;
            self.receive_ack(ack)?;
        }

        // Sequence number: must equal RxNext exactly (chip
        // BLE_ERROR_INVALID_BTP_SEQUENCE_NUMBER).
        let seq = *packet.get(idx).ok_or(BtpError::PacketTooShort)?;
        idx += 1;
        if seq != self.rx_next_seq {
            return Err(BtpError::InvalidSequence {
                expected: self.rx_next_seq,
                got: seq,
            });
        }
        self.rx_next_seq = self.rx_next_seq.wrapping_add(1);

        let is_data = flags & (FLAG_START | FLAG_CONTINUE | FLAG_END) != 0;
        if !is_data {
            // Pure standalone ack. Chip still debits the local receive window on
            // every accepted characteristic (`BLEEndPoint.cpp:1198`) and, having
            // advanced `RxNext` past the ack's own sequence number, now owes an
            // ack-of-ack (`HasUnackedData` becomes true — chip `BtpEngine.cpp`
            // GetAndRecordRxAckSeqNum / HandleCharacteristicReceived). Mirror
            // that: debit the window, record this seq as owed, arm the send-ack
            // timer.
            self.local_window = self.local_window.saturating_sub(1);
            self.unacked_rx = Some(seq);
            self.arm_ack_timer(now);
            return Ok(None);
        }

        let begin = flags & FLAG_START != 0;
        let end = flags & FLAG_END != 0;
        let cont = flags & FLAG_CONTINUE != 0;

        if self.rx_expected_len.is_some() {
            // Reassembly in progress: a new Begin, or a fragment carrying
            // neither Continue nor End, is illegal.
            if begin || !(cont || end) {
                return Err(BtpError::ReassemblerInvalidState);
            }
            let payload = &packet[idx..];
            if self.rx_buf.len() + payload.len() > RX_REASSEMBLY_CAP {
                return Err(BtpError::ReassemblyOverflow);
            }
            self.rx_buf.extend_from_slice(payload);
        } else {
            // Idle: only a Begin fragment may start a new message. A completed
            // message was already taken (fused receive+take resets to idle in
            // the same call it returned the SDU), so a fresh Begin here is legal
            // — no completed-but-unacked guard.
            if !begin {
                return Err(BtpError::ReassemblerInvalidState);
            }
            let len_bytes = packet.get(idx..idx + 2).ok_or(BtpError::PacketTooShort)?;
            let msg_len = u16::from_le_bytes([len_bytes[0], len_bytes[1]]);
            idx += 2;
            if msg_len as usize > RX_REASSEMBLY_CAP {
                return Err(BtpError::ReassemblyOverflow);
            }
            self.rx_expected_len = Some(msg_len);
            self.rx_buf.clear();
            self.rx_buf.extend_from_slice(&packet[idx..]);
        }

        // Every received data fragment counts against the local receive window
        // and becomes something we owe an acknowledgement for.
        self.local_window = self.local_window.saturating_sub(1);
        self.unacked_rx = Some(seq);
        self.arm_ack_timer(now);

        if !end {
            return Ok(None);
        }

        // End fragment: trim sender-declared padding, verify completeness.
        let expected = self.rx_expected_len.take().unwrap_or(0) as usize;
        if self.rx_buf.len() < expected {
            // Missing data (chip BLE_ERROR_REASSEMBLER_MISSING_DATA).
            return Err(BtpError::ReassemblerInvalidState);
        }
        self.rx_buf.truncate(expected);
        // Fused receive+take: hand the SDU to the caller and reset reassembly to
        // idle in the same call (chip `BLEEndPoint` TakeRxPacket →
        // `BtpEngine.cpp:395-397` kState_Complete → kState_Idle). `rx_expected_len`
        // was already cleared by `.take()` above; `mem::take` empties `rx_buf`.
        Ok(Some(std::mem::take(&mut self.rx_buf)))
    }

    /// The role this session was constructed for.
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// Earliest deadline among the send-ack (2.5 s) and ack-timeout (15 s)
    /// timers. Returns an [`Instant`] in the past (the indication time) when an
    /// acknowledgement is due immediately. `None` when idle.
    #[must_use]
    pub fn poll_timeout(&self) -> Option<Instant> {
        let ack_timeout = if self.expecting_ack {
            self.tx_unacked_since.map(|since| since + ACK_TIMEOUT)
        } else {
            None
        };
        match (self.ack_due_at, ack_timeout) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Validate and apply an inbound acknowledgement (chip `HandleAckReceived`
    /// with `IsValidAck`): `ack` must fall within the outstanding TX interval
    /// from `tx_oldest_unacked` to `tx_newest_unacked` (which may wrap mod 256).
    fn receive_ack(&mut self, ack: u8) -> Result<(), BtpError> {
        if !self.expecting_ack {
            return Err(BtpError::InvalidAck(ack));
        }
        let valid = if self.tx_newest_unacked >= self.tx_oldest_unacked {
            ack <= self.tx_newest_unacked && ack >= self.tx_oldest_unacked
        } else {
            // Interval wraps past 255.
            ack <= self.tx_newest_unacked || ack >= self.tx_oldest_unacked
        };
        if !valid {
            return Err(BtpError::InvalidAck(ack));
        }
        if self.tx_newest_unacked == ack {
            // Everything outstanding is now acknowledged.
            self.tx_oldest_unacked = ack;
            self.expecting_ack = false;
            self.tx_unacked_since = None;
        } else {
            self.tx_oldest_unacked = ack.wrapping_add(1);
        }
        // Re-open the remote peer's receive window by the sequence numbers it
        // just acknowledged (chip `AdjustRemoteReceiveWindow` /
        // `BLEEndPoint.cpp:1232`), letting `next_gatt_write` resume if it was
        // paused on a closed window. This credit is also one of the two events
        // that unblock a standalone ack whose emission `handle_timeout` deferred
        // because the remote window was 0 (the other is `gatt_write_completed`):
        // the deferred ack stays pending, so the caller's next `handle_timeout`
        // after this credit emits it. (On an error path later in the same
        // `on_indication` the session is torn down, so the credit is moot.)
        self.remote_window = self.adjust_remote_window(ack);
        Ok(())
    }

    /// Recompute the remote receive-window credit after acknowledgement `ack`
    /// (chip `AdjustRemoteReceiveWindow`): the window boundary sits at
    /// `ack + max_window`; the credit is the distance from the newest fragment
    /// we have sent to that boundary, with the same wrap correction chip applies
    /// when the boundary would exceed 255 but the newest sent seq has already
    /// wrapped. `local_window_max` is the negotiated window, shared by both
    /// directions.
    fn adjust_remote_window(&self, ack: u8) -> u8 {
        let boundary = u16::from(ack) + u16::from(self.local_window_max);
        let newest = u16::from(self.tx_newest_unacked);
        let credit = if boundary > u16::from(u8::MAX) && newest < u16::from(ack) {
            boundary.wrapping_sub(newest + u16::from(u8::MAX))
        } else {
            boundary.wrapping_sub(newest)
        };
        // Low 8 bits — chip casts the u16 boundary difference back to a
        // SequenceNumber_t (uint8, mod 256).
        (credit & 0xff) as u8
    }

    /// Register a just-emitted outbound sequence number as outstanding-unacked
    /// (chip `GetAndIncrementNextTxSeqNum`): begin expecting an ack (anchoring
    /// the 15 s ack-timeout at `now`) if we were not already, advance the
    /// newest-unacked marker, and increment the next-TX counter (wrapping u8).
    /// Shared by the standalone-ack emitter and the data-fragment send path so
    /// both register identically.
    fn register_outbound_seq(&mut self, seq: u8, now: Instant) {
        if !self.expecting_ack {
            self.expecting_ack = true;
            self.tx_oldest_unacked = seq;
            self.tx_unacked_since = Some(now);
        }
        self.tx_newest_unacked = seq;
        self.tx_next_seq = self.tx_next_seq.wrapping_add(1);
    }

    /// Queue one whole Matter message SDU for segmentation.
    ///
    /// # Errors
    /// - [`BtpError::MessageInFlight`]: a previously queued SDU still has unsent
    ///   fragments (chip fragments one message at a time; a new whole message
    ///   waits until the prior one is fully on the wire).
    /// - [`BtpError::ReassemblyOverflow`]: `sdu` is longer than the 16-bit BTP
    ///   message-length field can encode (`u16::MAX`).
    pub fn queue_message(&mut self, sdu: &[u8]) -> Result<(), BtpError> {
        if self.tx_pending.is_some() {
            return Err(BtpError::MessageInFlight);
        }
        if sdu.len() > usize::from(u16::MAX) {
            return Err(BtpError::ReassemblyOverflow);
        }
        self.tx_pending = Some(sdu.to_vec());
        self.tx_sent = 0;
        Ok(())
    }

    /// The next GATT write (one BTP data fragment) to put on the wire, or `None`
    /// when nothing may be sent right now.
    ///
    /// Returns `None` while a GATT write is still in flight (until
    /// [`Self::gatt_write_completed`]), while the remote receive window is 0,
    /// while it is at [`WINDOW_NO_ACK_SEND_THRESHOLD`] with no pending ack to
    /// piggyback, or when the fragmenter is idle (nothing queued). Otherwise it
    /// builds the next fragment: `flags [ack] seq [msgLen_LE] payload`, total
    /// length ≤ the negotiated segment size. A departing fragment debits the
    /// remote window, registers its sequence for the 15 s ack-timeout, latches
    /// the one-op-in-flight flag, and — if a receive ack is pending — piggybacks
    /// it (OR-ing the `0x08` flag, inserting the ack byte, clearing the pending
    /// ack, and resetting the local receive window to its maximum).
    #[must_use]
    pub fn next_gatt_write(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.gatt_in_flight {
            return None;
        }
        let msg = self.tx_pending.as_ref()?;
        let msg_len = msg.len();

        // Window gate — chip `DriveSending` (`BLEEndPoint.cpp:898`).
        let have_ack = self.unacked_rx.is_some();
        if self.remote_window == 0 {
            return None;
        }
        if self.remote_window <= WINDOW_NO_ACK_SEND_THRESHOLD && !have_ack {
            return None;
        }

        let first = self.tx_sent == 0;
        let send_ack = have_ack;
        // Header cursor size = flags + [ack] + seq + [msgLen] (chip `cursor`).
        let header_len = 1 + usize::from(send_ack) + 1 + if first { 2 } else { 0 };
        let segment = self.segment_size as usize;
        let max_payload = segment.saturating_sub(header_len);
        let remaining = msg_len - self.tx_sent;
        // chip: `(mTxLength + cursor) <= mTxFragmentSize` ⇒ this fragment ends
        // the message and carries all remaining bytes; otherwise it fills the
        // segment.
        let (payload_len, end) = if remaining + header_len <= segment {
            (remaining, true)
        } else {
            (max_payload, false)
        };
        let payload: Vec<u8> = msg[self.tx_sent..self.tx_sent + payload_len].to_vec();

        let seq = self.tx_next_seq;
        let mut flags = if first { FLAG_START } else { FLAG_CONTINUE };
        if end {
            flags |= FLAG_END;
        }
        if send_ack {
            flags |= FLAG_ACK;
        }

        let mut packet = Vec::with_capacity(header_len + payload_len);
        packet.push(flags);
        if send_ack {
            // Piggyback the pending receive ack (chip `PrepareNextFragment` +
            // `GetAndRecordRxAckSeqNum`): emit the newest unacked receive seq,
            // clear the pending-ack state, and reset the local receive window.
            let ack = self.unacked_rx.take().unwrap_or(0);
            packet.push(ack);
            self.ack_due_at = None;
            self.local_window = self.local_window_max;
        }
        packet.push(seq);
        if first {
            // msgLen u16 LE. `queue_message` bounds `msg_len` to `u16::MAX`, so
            // the conversion never saturates in practice.
            let len = u16::try_from(msg_len).unwrap_or(u16::MAX);
            packet.extend_from_slice(&len.to_le_bytes());
        }
        packet.extend_from_slice(&payload);

        // Post-send bookkeeping (chip `GetAndIncrementNextTxSeqNum` +
        // `SendCharacteristic` window debit + `SendWrite` GATT latch).
        self.register_outbound_seq(seq, now);
        self.remote_window = self.remote_window.saturating_sub(1);
        self.gatt_in_flight = true;
        if end {
            // Final fragment departed: the fragmenter is idle again (fused with
            // chip `TakeTxPacket`), so a new message may be queued.
            self.tx_pending = None;
            self.tx_sent = 0;
        } else {
            self.tx_sent += payload_len;
        }
        Some(packet)
    }

    /// Signal that the GATT write returned by [`Self::next_gatt_write`] has
    /// completed, releasing the one-op-in-flight latch so the next fragment may
    /// depart (chip `HandleGattSendConfirmation` clears
    /// `kGattOperationInFlight`). Completion is time-independent in this engine;
    /// `_now` is accepted for signature symmetry with the other send verbs.
    ///
    /// This is also one of the two events that unblock a standalone ack whose
    /// emission [`Self::handle_timeout`] deferred because the GATT slot was busy
    /// (the other is a remote-window credit from `receive_ack`): the
    /// deferred ack stays pending with its send-ack deadline in the past, so the
    /// caller's next [`Self::handle_timeout`] after this call emits it.
    pub fn gatt_write_completed(&mut self, _now: Instant) {
        self.gatt_in_flight = false;
    }

    /// Mark that an acknowledgement is due. Immediately when the local receive
    /// window has reached the immediate-ack threshold; otherwise on the 2.5 s
    /// send-ack timer. The earliest of any already-armed deadline is kept.
    fn arm_ack_timer(&mut self, now: Instant) {
        // Queued outbound data does not suppress the immediate deadline here:
        // a data fragment that leaves through `next_gatt_write` first
        // piggybacks the ack and clears the deadline.
        let candidate = if self.local_window <= IMMEDIATE_ACK_THRESHOLD {
            now
        } else {
            now + ACK_SEND_TIMEOUT
        };
        self.ack_due_at = Some(match self.ack_due_at {
            Some(existing) => existing.min(candidate),
            None => candidate,
        });
    }

    /// Fire any timers due at `now`.
    ///
    /// Returns `Ok(Some(packet))` with a standalone ack to send now, or
    /// `Ok(None)` when nothing is due.
    ///
    /// # Standalone-ack flow control (chip `DriveSending` → `DoSendStandAloneAck`)
    /// Chip does not emit a due standalone ack unconditionally: it routes it
    /// through `DriveSending`, which withholds *any* send while the remote
    /// receive window is 0 or a GATT operation is in flight
    /// (`BLEEndPoint.cpp:898-921`), and `DoSendStandAloneAck → SendCharacteristic`
    /// debits `mRemoteReceiveWindowSize` and latches the GATT slot like any other
    /// characteristic write (`BLEEndPoint.cpp:570,883`). We mirror this: when the
    /// send-ack timer is due but `gatt_in_flight` is set or `remote_window == 0`,
    /// the ack is **deferred, not dropped** — `unacked_rx` and `ack_due_at` are
    /// left untouched (the deadline is now in the past, so [`Self::poll_timeout`]
    /// keeps reporting it due). The deferral is cleared by whichever comes first:
    /// a queued data fragment piggybacking it in [`Self::next_gatt_write`], or the
    /// caller's next `handle_timeout` after the GATT slot frees
    /// ([`Self::gatt_write_completed`]) or the remote window is credited
    /// (`receive_ack`) — the two natural unblock events. The 15 s
    /// ack-timeout check runs *before* this gate, so a blocked standalone ack
    /// never starves the session-death deadline.
    ///
    /// # Errors
    /// [`BtpError::AckTimedOut`]: an outstanding TX fragment went 15 s without
    /// acknowledgement; the session is dead and the caller must close it.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<Option<Vec<u8>>, BtpError> {
        // A TX fragment that has gone unacknowledged for 15 s kills the session.
        // Checked FIRST so a standalone ack blocked on flow control (below) can
        // never starve this deadline.
        if self.expecting_ack {
            if let Some(since) = self.tx_unacked_since {
                if now >= since + ACK_TIMEOUT {
                    return Err(BtpError::AckTimedOut);
                }
            }
        }

        // Send-ack timer: emit a standalone ack for the newest received seq.
        if let Some(due) = self.ack_due_at {
            if now >= due {
                if let Some(ack_num) = self.unacked_rx {
                    // Flow-control gate (chip `DriveSending`): a standalone ack is
                    // a C1 characteristic write like any other, so it may not go
                    // out while a GATT op is in flight or the remote window is
                    // closed. Defer WITHOUT clearing `ack_due_at`/`unacked_rx`:
                    // the deadline stays in the past so `poll_timeout` remains due,
                    // and the next `handle_timeout` after `gatt_write_completed` or
                    // a `receive_ack` window credit (or a piggyback in
                    // `next_gatt_write`) delivers it.
                    if self.gatt_in_flight || self.remote_window == 0 {
                        return Ok(None);
                    }
                    self.ack_due_at = None;
                    // Standalone ack: 08 | ack_num | our-own-tx-seq.
                    let seq = self.tx_next_seq;
                    let packet = vec![FLAG_ACK, ack_num, seq];
                    // Full chip GetAndIncrementNextTxSeqNum (BtpEngine.cpp:107-125):
                    // the standalone ack is itself an outbound fragment that
                    // becomes outstanding-unacknowledged. Register it (identically
                    // to a data fragment) so (a) it is subject to the 15 s
                    // ack-timeout and (b) a valid inbound ack for `seq` passes
                    // IsValidAck instead of hitting expecting_ack==false.
                    // DoSendStandAloneAck then starts the ack-received timer if it
                    // is not already running.
                    self.register_outbound_seq(seq, now);
                    self.unacked_rx = None;
                    // The ack write debits the remote receive window and occupies
                    // the single GATT slot exactly like a data fragment (chip
                    // `SendCharacteristic` window debit + `kStandAloneAckInFlight`).
                    self.remote_window = self.remote_window.saturating_sub(1);
                    self.gatt_in_flight = true;
                    // Acknowledging frees the local receive window again
                    // (chip DoSendStandAloneAck resets mLocalReceiveWindowSize).
                    self.local_window = self.local_window_max;
                    return Ok(Some(packet));
                }
                // Timer due but nothing to acknowledge: clear the spurious deadline.
                self.ack_due_at = None;
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn central(t0: Instant) -> BtpSession {
        BtpSession::new(Role::Central, 20, 6, t0)
    }

    // ---- segments_rx.json ---------------------------------------------------

    // Vector: handle_characteristic_received_one_packet
    #[test]
    fn handle_characteristic_received_one_packet() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(
            s.on_indication(&hex("05010100ff"), t0).unwrap(),
            Some(hex("ff"))
        );
    }

    // Vector: handle_characteristic_received_two_packet
    #[test]
    fn handle_characteristic_received_two_packet() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(s.on_indication(&hex("01010200fe"), t0).unwrap(), None);
        assert_eq!(
            s.on_indication(&hex("0402ff"), t0).unwrap(),
            Some(hex("feff"))
        );
    }

    // Vector: handle_characteristic_received_three_packet
    #[test]
    fn handle_characteristic_received_three_packet() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(s.on_indication(&hex("01010300fd"), t0).unwrap(), None);
        assert_eq!(s.on_indication(&hex("0202fe"), t0).unwrap(), None);
        assert_eq!(
            s.on_indication(&hex("0403ff"), t0).unwrap(),
            Some(hex("fdfeff"))
        );
    }

    // Vector: handle_characteristic_received_with_padding
    #[test]
    fn handle_characteristic_received_with_padding() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Declared msgLen 1; 5 trailing 0x00 padding bytes must be ignored.
        assert_eq!(
            s.on_indication(&hex("05010100ff0000000000"), t0).unwrap(),
            Some(hex("ff"))
        );
    }

    // Vector: handle_characteristic_received_incorrect_sequence
    // DIVERGENCE: the vector encodes chip's engine-ISOLATION semantics (its unit
    // test never calls TakeRxPacket, so a second Begin after a completed message
    // errors). Our on_indication fuses receive+take (BLEEndPoint-stack
    // semantics): a completed message is taken and reassembly reset in the same
    // call, so a fresh Begin after completion is LEGAL (see
    // `begin_after_complete_accepted_under_fused_take`). We instead exercise the
    // reassembler_incorrect_state that IS reachable in the fused model — a Begin
    // arriving MID-reassembly (before the End) — against the same error.
    #[test]
    fn handle_characteristic_received_incorrect_sequence() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Begin, seq 1, msgLen 3, one payload byte -> reassembly in progress.
        assert_eq!(s.on_indication(&hex("01010300fd"), t0).unwrap(), None);
        // A second Begin (seq 2) while mid-reassembly is illegal.
        assert_eq!(
            s.on_indication(&hex("05020100ff"), t0),
            Err(BtpError::ReassemblerInvalidState)
        );
    }

    // Fused receive+take: two back-to-back single-fragment messages with NO ack
    // between them are legal (window permitting). The vector's engine-isolation
    // scenario that used to error is now the accepted path.
    #[test]
    fn begin_after_complete_accepted_under_fused_take() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(
            s.on_indication(&hex("05010100ff"), t0).unwrap(),
            Some(hex("ff"))
        );
        // Next message's Begin arrives immediately, no interposed ack.
        assert_eq!(
            s.on_indication(&hex("05020100ee"), t0).unwrap(),
            Some(hex("ee"))
        );
    }

    // A Continue|End (flags 0x06) fragment completes an in-progress message.
    #[test]
    fn continue_end_flag_completes_reassembly() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Begin, seq 1, msgLen 3, payload "fd".
        assert_eq!(s.on_indication(&hex("01010300fd"), t0).unwrap(), None);
        // Continue|End (0x06), seq 2, payload "feff" -> total 3 bytes, complete.
        assert_eq!(
            s.on_indication(&hex("0602feff"), t0).unwrap(),
            Some(hex("fdfeff"))
        );
    }

    // Vector: handle_characteristic_received_unexpected_standalone_ack
    // chip invalid_ack: standalone ack (ack_num 0) with nothing outstanding.
    #[test]
    fn handle_characteristic_received_unexpected_standalone_ack() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(
            s.on_indication(&hex("080001"), t0),
            Err(BtpError::InvalidAck(0))
        );
    }

    // Vector: handle_ack_received_incorrect_sequence
    // chip invalid_btp_sequence_number: the ack (0x00) is VALID and applied,
    // then the packet's own seq byte (0x02) fails against RxNext (1).
    // Precondition: 2 fragments sent (seq 0,1), expecting ack, tx_next at 2.
    #[test]
    fn handle_ack_received_incorrect_sequence() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Inject the TX-outstanding precondition directly, not via the send path.
        s.expecting_ack = true;
        s.tx_oldest_unacked = 0;
        s.tx_newest_unacked = 1;
        s.tx_next_seq = 2;
        s.tx_unacked_since = Some(t0);
        assert_eq!(
            s.on_indication(&hex("080002"), t0),
            Err(BtpError::InvalidSequence {
                expected: 1,
                got: 2
            })
        );
        // The ack byte (0x00) was VALID and applied BEFORE the seq check failed:
        // 0 != newest(1), so oldest advances to 1 and we still expect the ack
        // for seq 1 (vector `expecting_ack_after: 1`).
        assert!(s.expecting_ack, "ack 0 applied but still awaiting seq 1");
        assert_eq!(s.tx_oldest_unacked, 1, "oldest advanced past acked seq 0");
    }

    // Vector: handle_characteristic_received_sequence_wraparound_invalid_ack
    // chip invalid_ack: after a full tx wraparound, ack_num 95 (0x5f) is not in
    // the outstanding interval [250..=255, 0].
    #[test]
    fn handle_characteristic_received_sequence_wraparound_invalid_ack() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Outstanding interval after 257 sends w/ acks every 10th: oldest 250,
        // newest 0 (wrapped), tx_next 1. 95 falls outside -> InvalidAck(95).
        s.expecting_ack = true;
        s.tx_oldest_unacked = 250;
        s.tx_newest_unacked = 0;
        s.tx_next_seq = 1;
        s.tx_unacked_since = Some(t0);
        assert_eq!(
            s.on_indication(&hex("085f1a"), t0),
            Err(BtpError::InvalidAck(95))
        );
    }

    // ---- standalone_ack.json ------------------------------------------------

    // Vector: encode_standalone_ack_one_packet -> 08 01 00
    #[test]
    fn encode_standalone_ack_one_packet() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(
            s.on_indication(&hex("05010100ff"), t0).unwrap(),
            Some(hex("ff"))
        );
        let due = s.poll_timeout().unwrap();
        assert_eq!(s.handle_timeout(due).unwrap(), Some(hex("080100")));
    }

    // Vector: encode_standalone_ack_no_unacked_data -> 08 00 00
    #[test]
    fn encode_standalone_ack_no_unacked_data() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // seq 0 (handshake response) is the newest unacked; ack_num 0.
        assert_eq!(
            s.handle_timeout(t0 + ACK_SEND_TIMEOUT).unwrap(),
            Some(hex("080000"))
        );
    }

    // Vector: encode_standalone_ack_multi_fragment_message -> 08 03 00
    #[test]
    fn encode_standalone_ack_multi_fragment_message() {
        let t0 = Instant::now();
        let mut s = central(t0);
        assert_eq!(s.on_indication(&hex("01010300fd"), t0).unwrap(), None);
        assert_eq!(s.on_indication(&hex("0202fe"), t0).unwrap(), None);
        assert_eq!(
            s.on_indication(&hex("0403ff"), t0).unwrap(),
            Some(hex("fdfeff"))
        );
        let due = s.poll_timeout().unwrap();
        assert_eq!(s.handle_timeout(due).unwrap(), Some(hex("080300")));
    }

    // ---- behaviour ----------------------------------------------------------

    #[test]
    fn central_arms_seq0_ack_timer_at_construction() {
        let t0 = Instant::now();
        let s = central(t0);
        assert_eq!(s.poll_timeout(), Some(t0 + ACK_SEND_TIMEOUT));
    }

    #[test]
    fn ack_timer_fires_standalone_ack_for_seq0() {
        let t0 = Instant::now();
        let mut s = central(t0);
        let pkt = s.handle_timeout(t0 + ACK_SEND_TIMEOUT).unwrap().unwrap();
        assert_eq!(pkt, vec![0x08, 0x00, 0x00]);
    }

    // Regression: a standalone ack we emit registers itself as an outstanding
    // TX fragment (full GetAndIncrementNextTxSeqNum), so (a) a valid inbound
    // ack for that seq is accepted rather than hitting expecting_ack==false ->
    // InvalidAck, and (b) it is subject to the 15 s ack-timeout.
    #[test]
    fn emitted_standalone_ack_is_registered_as_outstanding_tx() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Emit the seq-0 standalone ack; it consumes tx seq 0.
        assert_eq!(
            s.handle_timeout(t0 + ACK_SEND_TIMEOUT).unwrap(),
            Some(hex("080000"))
        );
        assert!(s.expecting_ack, "emitted ack must be outstanding");
        assert_eq!(s.tx_oldest_unacked, 0);
        assert_eq!(s.tx_newest_unacked, 0);
        // It is now subject to the 15 s ack-timeout.
        assert_eq!(s.poll_timeout(), Some(t0 + ACK_SEND_TIMEOUT + ACK_TIMEOUT));
        // A valid inbound ack for tx seq 0 (packet 08 00 01: ack=0, own seq=1)
        // must be ACCEPTED (not InvalidAck) and clear the outstanding fragment.
        assert_eq!(s.on_indication(&hex("080001"), t0).unwrap(), None);
        assert!(!s.expecting_ack, "inbound ack for the emitted seq accepted");
    }

    // An unacked emitted standalone ack times out the session after 15 s.
    #[test]
    fn emitted_standalone_ack_times_out_unacked() {
        let t0 = Instant::now();
        let mut s = central(t0);
        let emit_at = t0 + ACK_SEND_TIMEOUT;
        assert_eq!(s.handle_timeout(emit_at).unwrap(), Some(hex("080000")));
        assert_eq!(
            s.handle_timeout(emit_at + ACK_TIMEOUT),
            Err(BtpError::AckTimedOut)
        );
    }

    #[test]
    fn window_exhaustion_forces_immediate_ack() {
        // window 2: handshake debit leaves 1 free slot; the next data fragment
        // drops the local window to the immediate-ack threshold.
        let t0 = Instant::now();
        let mut s = BtpSession::new(Role::Central, 20, 2, t0);
        // seq must be 1 (RxNext=1 for Central): 05 01 0100 78.
        let _ = s.on_indication(&hex("0501010078"), t0).unwrap();
        assert_eq!(s.poll_timeout(), Some(t0)); // due now
    }

    #[test]
    fn seq_wraps_mod_256() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Central RxNext starts at 1. Drive single-fragment messages seq
        // 1,2,...,255,0 (256 messages) back-to-back with NO interposed ack —
        // fused receive+take resets reassembly to idle after each, so there is
        // no completed-unacked guard to clear. The seq 0 message (RxNext wrapped
        // 255 -> 0) must be accepted.
        for i in 0u16..256 {
            let seq = ((1 + i) & 0xff) as u8;
            let pkt = format!("05{seq:02x}0100{seq:02x}");
            let out = s.on_indication(&hex(&pkt), t0).unwrap();
            assert_eq!(out, Some(vec![seq]), "message seq {seq} should complete");
        }
    }

    #[test]
    fn reassembly_cap_enforced() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // Begin (flags 01, seq 01, msgLen 4000 = 0x0fa0 LE) > RX_REASSEMBLY_CAP.
        assert_eq!(
            s.on_indication(&hex("0101a00f"), t0),
            Err(BtpError::ReassemblyOverflow)
        );
    }

    #[test]
    fn peripheral_arms_15s_ack_timeout() {
        let t0 = Instant::now();
        let s = BtpSession::new(Role::Peripheral, 20, 6, t0);
        assert_eq!(s.poll_timeout(), Some(t0 + ACK_TIMEOUT));
    }

    #[test]
    fn peripheral_ack_timeout_kills_session() {
        let t0 = Instant::now();
        let mut s = BtpSession::new(Role::Peripheral, 20, 6, t0);
        assert_eq!(
            s.handle_timeout(t0 + ACK_TIMEOUT),
            Err(BtpError::AckTimedOut)
        );
    }

    // ---- segments_tx.json (TX segmentation) ---------------------------------

    /// A Central just after its post-handshake ack has been flushed: TX seq 0,
    /// no receive ack pending, remote window open. This matches the bare-engine
    /// precondition of the `segments_tx.json` vectors (chip
    /// `Init(nullptr, false)`, `mTxNextSeqNum` starts at 0, nothing to ack).
    fn tx_central(t0: Instant) -> BtpSession {
        let mut s = central(t0);
        s.unacked_rx = None;
        s.ack_due_at = None;
        s
    }

    // The 40-byte sequential SDU 0x00..0x27 shared by the multi-fragment vectors.
    fn sdu40() -> Vec<u8> {
        hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f2021222324252627")
    }

    // Vector: one_byte_sdu_frag20_no_ack -> 05 00 0100 78
    #[test]
    fn one_byte_sdu_frag20_no_ack() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.queue_message(&hex("78")).unwrap();
        assert_eq!(s.next_gatt_write(t0), Some(hex("0500010078")));
    }

    // Vector: one_byte_sdu_frag20_piggyback_ack1 -> 0d 01 00 0100 78
    #[test]
    fn one_byte_sdu_frag20_piggyback_ack1() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        // Feed one inbound single-fragment message (seq 1 = Central RxNext) so a
        // receive ack (ack_num 1) is pending to piggyback.
        assert_eq!(
            s.on_indication(&hex("0501010042"), t0).unwrap(),
            Some(hex("42"))
        );
        s.queue_message(&hex("78")).unwrap();
        // flags 0d = Start|Ack|End, ack 01, seq 00, msgLen 0001 LE, payload 78.
        assert_eq!(s.next_gatt_write(t0), Some(hex("0d0100010078")));
    }

    // Vector: forty_byte_sdu_frag20 -> three fragments, no ack.
    #[test]
    fn forty_byte_sdu_frag20() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.queue_message(&sdu40()).unwrap();
        assert_eq!(
            s.next_gatt_write(t0),
            Some(hex("01002800000102030405060708090a0b0c0d0e0f"))
        );
        s.gatt_write_completed(t0);
        assert_eq!(
            s.next_gatt_write(t0),
            Some(hex("0201101112131415161718191a1b1c1d1e1f2021"))
        );
        s.gatt_write_completed(t0);
        // Final fragment flags 0x06 = Continue|End (chip ORs End onto the
        // Continue base — BtpEngine.cpp:499,514 — it is NOT bare 0x04).
        assert_eq!(s.next_gatt_write(t0), Some(hex("0602222324252627")));
    }

    // Vector: forty_byte_sdu_frag20_midstream_ack -> ack piggybacked on frag 2.
    #[test]
    fn forty_byte_sdu_frag20_midstream_ack() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.queue_message(&sdu40()).unwrap();
        // Fragment 0: no ack pending yet (identical to the no-ack vector).
        assert_eq!(
            s.next_gatt_write(t0),
            Some(hex("01002800000102030405060708090a0b0c0d0e0f"))
        );
        s.gatt_write_completed(t0);
        // A pending rx ack (ack_num 2) becomes available before fragment 2: feed
        // two inbound single-fragment messages (seq 1 then seq 2), legal
        // back-to-back under fused receive+take.
        assert_eq!(
            s.on_indication(&hex("0501010011"), t0).unwrap(),
            Some(hex("11"))
        );
        assert_eq!(
            s.on_indication(&hex("0502010022"), t0).unwrap(),
            Some(hex("22"))
        );
        // Fragment 1: flags 0a = Continue|Ack, ack 02, seq 01, payload SDU[16:33]
        // (17 bytes — the ack byte grows the mid-fragment header 2->3).
        assert_eq!(
            s.next_gatt_write(t0),
            Some(hex("0a0201101112131415161718191a1b1c1d1e1f20"))
        );
        s.gatt_write_completed(t0);
        // Fragment 2: flags 0x06 = Continue|End, seq 02, payload SDU[33:40].
        assert_eq!(s.next_gatt_write(t0), Some(hex("060221222324252627")));
    }

    // ---- TX behaviour -------------------------------------------------------

    // One GATT op in flight: no further fragment departs until completion.
    #[test]
    fn tx_one_gatt_op_in_flight_blocks_next_fragment() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.queue_message(&sdu40()).unwrap();
        assert!(s.next_gatt_write(t0).is_some(), "fragment 0 departs");
        assert_eq!(
            s.next_gatt_write(t0),
            None,
            "blocked while the write is in flight"
        );
        s.gatt_write_completed(t0);
        assert!(
            s.next_gatt_write(t0).is_some(),
            "fragment 1 departs after ack"
        );
    }

    // Remote window 0: nothing may be sent.
    #[test]
    fn tx_remote_window_zero_blocks() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.remote_window = 0;
        s.queue_message(&hex("78")).unwrap();
        assert_eq!(s.next_gatt_write(t0), None);
    }

    // Remote window 1 with no pending ack: withheld (keep one slot in reserve).
    #[test]
    fn tx_remote_window_one_without_ack_blocks() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.remote_window = 1;
        s.queue_message(&hex("78")).unwrap();
        assert_eq!(s.next_gatt_write(t0), None);
    }

    // Remote window 1 WITH a pending ack: sends (the piggyback re-opens the
    // peer's window), consuming the last slot.
    #[test]
    fn tx_remote_window_one_with_pending_ack_sends() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        assert_eq!(
            s.on_indication(&hex("0501010042"), t0).unwrap(),
            Some(hex("42"))
        );
        s.remote_window = 1;
        s.queue_message(&hex("78")).unwrap();
        assert_eq!(s.next_gatt_write(t0), Some(hex("0d0100010078")));
        assert_eq!(s.remote_window, 0, "the send consumed the last window slot");
    }

    // Queueing a new SDU while the prior one still has unsent fragments errors.
    #[test]
    fn tx_queue_while_fragments_unsent_is_message_in_flight() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.queue_message(&sdu40()).unwrap();
        let _ = s.next_gatt_write(t0).unwrap(); // frag 0; frags 1,2 still unsent.
        assert_eq!(s.queue_message(&hex("99")), Err(BtpError::MessageInFlight));
    }

    // A data fragment unacknowledged for 15 s kills the session (the ack-timeout
    // is armed by the departing fragment, exactly as for a standalone ack).
    #[test]
    fn tx_data_fragment_ack_timeout_kills_session() {
        let t0 = Instant::now();
        let mut s = tx_central(t0);
        s.queue_message(&hex("78")).unwrap();
        let _ = s.next_gatt_write(t0).unwrap();
        assert_eq!(s.poll_timeout(), Some(t0 + ACK_TIMEOUT));
        assert_eq!(
            s.handle_timeout(t0 + ACK_TIMEOUT),
            Err(BtpError::AckTimedOut)
        );
    }

    // An inbound ack credits the remote window (chip AdjustRemoteReceiveWindow)
    // and disarms the 15 s deadline.
    #[test]
    fn tx_inbound_ack_credits_remote_window_and_disarms_deadline() {
        let t0 = Instant::now();
        let mut s = BtpSession::new(Role::Central, 20, 4, t0);
        s.unacked_rx = None;
        s.ack_due_at = None;
        s.queue_message(&hex("78")).unwrap();
        let _ = s.next_gatt_write(t0).unwrap(); // seq 0 departs; remote 4 -> 3.
        assert_eq!(s.remote_window, 3);
        s.gatt_write_completed(t0);
        // Inbound ack for tx seq 0 (08 00 01: ack=0, own seq=1 == RxNext).
        assert_eq!(s.on_indication(&hex("080001"), t0).unwrap(), None);
        // AdjustRemoteReceiveWindow(ack=0, max=4, newest=0) = 4.
        assert_eq!(
            s.remote_window, 4,
            "inbound ack re-opened the remote window"
        );
        assert!(!s.expecting_ack, "outstanding fragment acknowledged");
        // The 15 s ack-timeout is gone; only the receive-ack timer remains (the
        // inbound ack packet itself now owes an ack-of-ack).
        assert_eq!(s.poll_timeout(), Some(t0 + ACK_SEND_TIMEOUT));
    }

    // ---- standalone-ack flow control ----------------------------------------

    // Regression: chip routes standalone acks through DriveSending ->
    // DoSendStandAloneAck -> SendCharacteristic, gated on GATT-op in flight. A
    // due send-ack timer with a GATT write outstanding must DEFER (emit
    // nothing, leave the ack pending, keep the timer due), then deliver on the
    // next handle_timeout once gatt_write_completed frees the slot.
    #[test]
    fn standalone_ack_deferred_while_gatt_in_flight_then_emitted() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // A GATT write is outstanding (e.g. a data fragment already on the wire).
        s.gatt_in_flight = true;
        let due = s.poll_timeout().unwrap(); // seq-0 send-ack deadline.
                                             // Blocked: nothing emitted, the seq-0 ack stays pending, timer stays due.
        assert_eq!(s.handle_timeout(due).unwrap(), None);
        assert_eq!(s.unacked_rx, Some(0), "ack still pending");
        assert_eq!(s.poll_timeout(), Some(due), "deadline unchanged, still due");
        assert!(!s.expecting_ack, "nothing registered as outstanding TX");
        // The write completes -> the natural unblock event -> next handle_timeout
        // emits the identical standalone ack.
        s.gatt_write_completed(due);
        assert_eq!(s.handle_timeout(due).unwrap(), Some(hex("080000")));
    }

    // Same deferral when the remote receive window is closed; a window credit
    // (chip AdjustRemoteReceiveWindow) is the other natural unblock event.
    #[test]
    fn standalone_ack_deferred_while_remote_window_zero_then_emitted_on_credit() {
        let t0 = Instant::now();
        let mut s = central(t0);
        s.remote_window = 0;
        let due = s.poll_timeout().unwrap();
        assert_eq!(
            s.handle_timeout(due).unwrap(),
            None,
            "blocked: window closed"
        );
        assert_eq!(s.unacked_rx, Some(0), "ack still pending");
        assert_eq!(s.poll_timeout(), Some(due), "deadline unchanged, still due");
        // A window credit arrives; the next handle_timeout emits.
        s.remote_window = 1;
        assert_eq!(s.handle_timeout(due).unwrap(), Some(hex("080000")));
        assert_eq!(
            s.remote_window, 0,
            "the emitted ack debited the credited slot"
        );
    }

    // An emitted standalone ack is a characteristic write: it debits the remote
    // window and latches the GATT slot (chip SendCharacteristic +
    // kStandAloneAckInFlight), so a queued message cannot depart until the ack
    // write completes.
    #[test]
    fn emitted_standalone_ack_debits_remote_window_and_latches_gatt() {
        let t0 = Instant::now();
        let mut s = central(t0); // window 6 -> remote_window 6, slot free.
        let emit_at = t0 + ACK_SEND_TIMEOUT;
        assert_eq!(s.handle_timeout(emit_at).unwrap(), Some(hex("080000")));
        assert_eq!(
            s.remote_window, 5,
            "standalone ack debited the remote window"
        );
        assert!(
            s.gatt_in_flight,
            "standalone ack occupies the single GATT slot"
        );
        // The GATT slot is busy: a queued message is withheld until completion.
        s.queue_message(&hex("78")).unwrap();
        assert_eq!(
            s.next_gatt_write(emit_at),
            None,
            "queued fragment withheld behind the in-flight ack write"
        );
        s.gatt_write_completed(emit_at);
        assert!(
            s.next_gatt_write(emit_at).is_some(),
            "fragment departs once the ack write completes"
        );
    }

    // The 15 s AckTimedOut check must fire even when standalone-ack emission is
    // blocked on flow control — the timeout check runs before the send-ack gate
    // and must not be starved by the blocked-emission early return.
    #[test]
    fn blocked_standalone_ack_does_not_starve_ack_timeout() {
        let t0 = Instant::now();
        let mut s = central(t0);
        // A TX fragment is outstanding (15 s deadline live) AND a GATT op is in
        // flight, so the due send-ack emission is blocked.
        s.expecting_ack = true;
        s.tx_unacked_since = Some(t0);
        s.tx_oldest_unacked = 0;
        s.tx_newest_unacked = 0;
        s.tx_next_seq = 1;
        s.gatt_in_flight = true;
        // At 15 s both timers are due; the ack-timeout must win (checked first)
        // despite the blocked standalone ack.
        assert_eq!(
            s.handle_timeout(t0 + ACK_TIMEOUT),
            Err(BtpError::AckTimedOut)
        );
    }
}

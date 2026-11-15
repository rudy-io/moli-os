//! Matter secured-message framing (Matter Core Specification §4.4) plus the
//! reception-side replay window (§4.6.5).
//!
//! The module holds the header codec, AES-CCM payload encryption and
//! decryption (unicast and group), and the replay window.

use bitflags::bitflags;

use crate::error::{Error, Result};

bitflags! {
    /// First byte of the secured-message header. The bit layout follows
    /// matter.js's `PacketHeaderFlag` enum (`@matter/protocol`
    /// `codec/MessageCodec.ts`), cross-verified byte-for-byte against
    /// captured fixtures in `test-vectors/transport/`.
    ///
    /// - Bit 0: `DSIZ` low bit — set if destination is a unicast Node ID.
    /// - Bit 1: `DSIZ` high bit — set if destination is a 16-bit Group ID.
    ///   (`DSIZ = 0b11` is reserved.)
    /// - Bit 2: `S` — source node ID present in header.
    /// - Bit 3: reserved (must be `0`).
    /// - Bits 4..=7: protocol version (must be `0` for current spec).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SecuredMessageFlags: u8 {
        /// `S = 1` — header carries an 8-byte source node ID.
        const SOURCE_PRESENT = 0b0000_0100;
        /// `DSIZ = 0b01` — header carries an 8-byte unicast destination node ID.
        const DEST_UNICAST   = 0b0000_0001;
        /// `DSIZ = 0b10` — header carries a 2-byte group ID instead.
        const DEST_GROUP     = 0b0000_0010;
        // Version field (bits 4..=7) and reserved (bit 3) are zero in all
        // currently spec-defined messages — we surface no bitflag constants
        // for them; reads/writes round-trip the raw bits via `bits()`.
    }

    /// Second-section byte of the secured-message header. Bit layout
    /// follows matter.js's `SecurityFlag` enum.
    ///
    /// - Bits 0..=1: session type (`SessionTypeMask`). `0` unicast,
    ///   `1` group; others reserved.
    /// - Bits 2..=4: reserved.
    /// - Bit 5: `MX` — message extensions present.
    /// - Bit 6: `C` — control message (Secure Channel protocol message).
    /// - Bit 7: `P` — privacy enhancements applied to the message header.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SecurityFlags: u8 {
        /// `P` — privacy enhancements applied.
        const PRIVACY            = 0b1000_0000;
        /// `C` — control message.
        const CONTROL            = 0b0100_0000;
        /// `MX` — message extensions present.
        const EXTENSIONS_PRESENT = 0b0010_0000;
        /// Session type bits set to "group" (`0b01` in the low two bits).
        const SESSION_TYPE_GROUP = 0b0000_0001;
    }
}

const DSIZ_MASK: u8 = 0b0000_0011;
const DSIZ_NONE: u8 = 0b0000_0000;
const DSIZ_UNICAST: u8 = 0b0000_0001;
const DSIZ_GROUP: u8 = 0b0000_0010;
const DSIZ_RESERVED: u8 = 0b0000_0011;

/// Bits of the first header byte that MUST be zero on the wire: bit 3
/// (reserved) and bits 4..=7 (the message-format version, which is `0` for
/// the current Matter Core Spec §4.4.1). Only bits 0..=2 (DSIZ + `S`) carry
/// meaning; any set bit in this mask marks an unsupported/malformed header.
const FLAGS_MUST_BE_ZERO_MASK: u8 = 0b1111_1000;

/// Peer-allocated session identifier carried at byte offset 1 of the
/// header (little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u16);

/// 32-bit monotonic message counter. Matter Core Spec §4.6.1.1 requires
/// every message counter to start at `Crypto_DRBG(len = 28) + 1`, a CSPRNG
/// value in `[1, 2^28]`; a secure session's outbound counter does (see
/// `Session::outbound_counter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageCounter(pub u32);

/// 64-bit Node ID used in source/destination header fields and the
/// AES-CCM nonce composition (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u64);

/// Destination address: either a unicast 64-bit Node ID or a 16-bit
/// Group ID.
///
/// `#[non_exhaustive]`: the `DSIZ` field reserves a `0b11` encoding the spec
/// may later define; marking this lets a future variant land without a semver
/// break. Downstream `match`es must include a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DestNodeId {
    /// Unicast — `DSIZ = 0b01`.
    Node(NodeId),
    /// Multicast group — `DSIZ = 0b10`: the group id a group message is
    /// addressed to.
    Group(u16),
}

/// Parsed view of the secured-message header (everything before the
/// encrypted payload). See [`encode_header`] and [`decode_header`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecuredMessageHeader {
    /// Top-byte flags (S, DSIZ, version).
    pub flags: SecuredMessageFlags,
    /// Peer-allocated session identifier.
    pub session_id: SessionId,
    /// Security-flags byte (P, C, MX, session type).
    pub security_flags: SecurityFlags,
    /// 32-bit message counter.
    pub message_counter: MessageCounter,
    /// Optional source Node ID. Presence MUST match the `S` bit in
    /// [`Self::flags`] — [`encode_header`] returns
    /// [`Error::MalformedHeader`] on mismatch.
    pub source_node_id: Option<NodeId>,
    /// Optional destination address. Presence MUST match the `DSIZ` bits
    /// in [`Self::flags`].
    pub destination_node_id: Option<DestNodeId>,
}

/// Largest possible encoded header: 8 fixed bytes + 8 source node id +
/// 8 unicast destination node id (a 2-byte group destination is smaller).
/// Pinned by `header_with_source_and_destination`, which asserts the
/// maximal shape encodes to exactly 24 bytes.
const MAX_HEADER_LEN: usize = 24;

/// Encode a [`SecuredMessageHeader`] to its on-the-wire byte sequence.
pub fn encode_header(header: &SecuredMessageHeader) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAX_HEADER_LEN);
    encode_header_into(header, &mut out);
    out
}

/// Append the on-the-wire header bytes to `out`.
///
/// The body of [`encode_header`], factored out so the secured-message
/// encoder can serialise the header straight into the pre-sized output
/// buffer instead of allocating a separate header `Vec` and copying it.
/// Both callers therefore share one definition of the byte layout.
fn encode_header_into(header: &SecuredMessageHeader, out: &mut Vec<u8>) {
    out.push(header.flags.bits());
    out.extend_from_slice(&header.session_id.0.to_le_bytes());
    out.push(header.security_flags.bits());
    out.extend_from_slice(&header.message_counter.0.to_le_bytes());
    if let Some(node) = header.source_node_id {
        out.extend_from_slice(&node.0.to_le_bytes());
    }
    match header.destination_node_id {
        None => {}
        Some(DestNodeId::Node(NodeId(n))) => out.extend_from_slice(&n.to_le_bytes()),
        Some(DestNodeId::Group(g)) => out.extend_from_slice(&g.to_le_bytes()),
    }
}

/// Decode the header from the start of `bytes`. On success returns the
/// parsed header and the remainder of the input (i.e. the encrypted
/// payload + auth tag).
///
/// # Errors
///
/// Returns [`Error::MalformedHeader`] if:
/// - the fixed 8-byte portion is truncated;
/// - the version field (bits 4..=7 of byte 0) is non-zero, or the reserved
///   bit 3 is set (both must be `0` per Matter Core Spec §4.4.1);
/// - the `S` bit is set but only a partial source Node ID is present;
/// - `DSIZ` is set but only a partial destination is present;
/// - `DSIZ` has the reserved `0b11` value.
pub fn decode_header(bytes: &[u8]) -> Result<(SecuredMessageHeader, &[u8])> {
    if bytes.len() < 8 {
        return Err(Error::MalformedHeader(bytes.len()));
    }
    let flags = SecuredMessageFlags::from_bits_retain(bytes[0]);

    // The message-format version (bits 4..=7) must be the supported value
    // (`0`) and the reserved bit 3 must be clear — Matter Core Spec §4.4.1.
    // Rejecting here keeps a future-version or corrupt datagram from being
    // silently mis-parsed as a current-spec message.
    if (bytes[0] & FLAGS_MUST_BE_ZERO_MASK) != 0 {
        return Err(Error::MalformedHeader(0));
    }

    if (bytes[0] & DSIZ_MASK) == DSIZ_RESERVED {
        return Err(Error::MalformedHeader(0));
    }

    let session_id = SessionId(u16::from_le_bytes([bytes[1], bytes[2]]));
    let security_flags = SecurityFlags::from_bits_retain(bytes[3]);
    let message_counter =
        MessageCounter(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));

    let mut offset = 8;

    let source_node_id = if flags.contains(SecuredMessageFlags::SOURCE_PRESENT) {
        if bytes.len() < offset + 8 {
            return Err(Error::MalformedHeader(offset));
        }
        let bs: [u8; 8] = bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| Error::MalformedHeader(offset))?;
        offset += 8;
        Some(NodeId(u64::from_le_bytes(bs)))
    } else {
        None
    };

    let destination_node_id = match bytes[0] & DSIZ_MASK {
        DSIZ_NONE => None,
        DSIZ_UNICAST => {
            if bytes.len() < offset + 8 {
                return Err(Error::MalformedHeader(offset));
            }
            let bs: [u8; 8] = bytes[offset..offset + 8]
                .try_into()
                .map_err(|_| Error::MalformedHeader(offset))?;
            offset += 8;
            Some(DestNodeId::Node(NodeId(u64::from_le_bytes(bs))))
        }
        DSIZ_GROUP => {
            if bytes.len() < offset + 2 {
                return Err(Error::MalformedHeader(offset));
            }
            let bs: [u8; 2] = bytes[offset..offset + 2]
                .try_into()
                .map_err(|_| Error::MalformedHeader(offset))?;
            offset += 2;
            Some(DestNodeId::Group(u16::from_le_bytes(bs)))
        }
        // DSIZ_RESERVED already rejected above; mask covers all 4 values.
        _ => return Err(Error::MalformedHeader(0)),
    };

    let parsed = SecuredMessageHeader {
        flags,
        session_id,
        security_flags,
        message_counter,
        source_node_id,
        destination_node_id,
    };
    Ok((parsed, &bytes[offset..]))
}

/// Sliding-window dedup for inbound message counters on a secure unicast
/// session, per Matter Core Specification §4.6.5 (the message reception
/// state of §4.6.5.1).
///
/// Tracks the highest counter seen (always counted as seen) plus a 32-bit
/// bitmap of the 32 counters immediately behind it, `highest_seen - 1`
/// down to `highest_seen - 32`, which is the spec's window of
/// `MSG_COUNTER_WINDOW_SIZE` = 32 and chip's `PeerMessageCounter` layout.
/// Counters further behind (below `highest_seen - 32`) are rejected as too
/// old; the highest itself, and counters in the window that have already
/// been seen, are rejected as duplicates; everything else is accepted and
/// recorded.
///
/// A new window starts synced at counter 0, so 0 is the highest and counts
/// as seen, and a peer's counter 0 is always rejected. §4.14.1.2 (PASE) and
/// §4.14.2.3 (CASE) set the peer's synchronized `max_message_counter` to 0
/// when a session is established, a secure session's counters start at 1
/// or higher (§4.6.1.1), and chip starts every secure session's reception
/// state the same way (`PeerMessageCounter::SetCounter(
/// kInitialSyncValue)` in `PairingSession::ActivateSecureSession`). Every
/// counter from 1 up is accepted as a first counter.
#[derive(Debug, Clone)]
pub struct ReplayWindow {
    /// The highest counter accepted (0 when nothing has been). Always counts
    /// as seen.
    highest_seen: u32,
    /// Bit `n - 1` set ⇔ `highest_seen - n` has been observed, for `n` in
    /// `1..=32`.
    bitmap: u32,
}

impl ReplayWindow {
    /// Number of counters tracked behind the highest one (bits in the
    /// bitmap): the spec's `MSG_COUNTER_WINDOW_SIZE`.
    pub const WIDTH: u32 = 32;

    /// Create a window for a new secure session: synced at counter 0, as
    /// chip's `PeerMessageCounter::SetCounter(0)` is. Any counter from 1 up
    /// is novel until recorded; counter 0 is the highest, so it is rejected
    /// as a duplicate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            highest_seen: 0,
            bitmap: 0,
        }
    }

    /// Classify `counter` against the current window WITHOUT mutating any
    /// state. Both [`Self::check_and_record`] and [`Self::would_reject`]
    /// delegate here so the accept/reject decision can never drift between
    /// the mutating and non-mutating paths.
    ///
    /// Returns `Err` for the same conditions [`Self::check_and_record`]
    /// documents; otherwise `Ok`.
    fn classify(&self, counter: u32) -> Result<()> {
        let highest = self.highest_seen;
        if counter > highest {
            // Forward jump — always novel.
            return Ok(());
        }
        if counter == highest {
            // The highest always counts as seen.
            return Err(Error::ReplayedCounter { counter });
        }
        // 1 <= offset here, since counter < highest.
        let offset = highest - counter;
        if offset > Self::WIDTH {
            return Err(Error::CounterTooOld {
                counter,
                window_low: highest.saturating_sub(Self::WIDTH),
                window_high: highest,
            });
        }
        if self.bitmap & (1u32 << (offset - 1)) != 0 {
            return Err(Error::ReplayedCounter { counter });
        }
        Ok(())
    }

    /// Check whether `counter` WOULD be rejected, without recording it or
    /// otherwise mutating the window. Takes `&self` and never writes.
    ///
    /// **Not used to gate the inbound path** (see [`decode_secured`]):
    /// classifying a replay from the *unauthenticated* header — before the
    /// AES-CCM tag is verified — let a forged datagram drive the caller's MRP
    /// duplicate-ack logic. Replay classification on inbound traffic is done
    /// only by [`Self::check_and_record`], after authentication. This
    /// non-mutating classifier remains for callers that genuinely want a
    /// read-only window query (and for tests); it must never be the basis for
    /// an ack, a counter increment, or any other side effect on an
    /// unauthenticated message.
    ///
    /// # Errors
    ///
    /// - [`Error::ReplayedCounter`] if `counter` is the highest seen, or is
    ///   inside the window and has already been observed.
    /// - [`Error::CounterTooOld`] if `counter` is older than
    ///   `highest_seen - 32`.
    pub fn would_reject(&self, counter: u32) -> Result<()> {
        self.classify(counter)
    }

    /// Validate `counter` against the window and, on success, record it.
    ///
    /// # Errors
    ///
    /// - [`Error::ReplayedCounter`] if `counter` is the highest seen, or is
    ///   inside the window and has already been observed.
    /// - [`Error::CounterTooOld`] if `counter` is older than
    ///   `highest_seen - 32`.
    pub fn check_and_record(&mut self, counter: u32) -> Result<()> {
        // Decide first (no mutation); only commit once the counter is
        // accepted. Keeps the reject logic identical to `would_reject`.
        self.classify(counter)?;

        let highest = self.highest_seen;
        if counter > highest {
            // Forward jump (chip `CommitWithPosition`). The previous highest
            // becomes `shift` behind the new one, so it is marked at bit
            // `shift - 1`; a jump past the window clears it. `checked_shl`
            // because a shift of 32 would overflow `u32 <<`.
            let shift = counter - highest;
            self.bitmap = if shift > Self::WIDTH {
                0
            } else {
                self.bitmap.checked_shl(shift).unwrap_or(0) | (1u32 << (shift - 1))
            };
            self.highest_seen = counter;
        } else {
            // In-window, novel (classify already ruled out the highest,
            // replays and too-old counters, so 1 <= offset <= 32).
            let offset = highest - counter;
            self.bitmap |= 1u32 << (offset - 1);
        }
        Ok(())
    }
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// Hard cap on encrypted payload size (in bytes). Matter Core Spec §4.4.4
/// recommends staying well under MTU; we additionally cap at 1280 (the
/// IPv6 minimum MTU) minus header (24 bytes max) minus AES-CCM tag
/// (16 bytes) ≈ 1240. We round to 1024 for headroom — large messages use
/// TCP transport (deferred post-1.0) or BDX.
const MAX_PAYLOAD_LEN: usize = 1024;

/// Encode + encrypt a Matter secured message.
///
/// The output layout is `header bytes || AES-CCM(payload) || 16-byte tag`,
/// matching matter.js's `MessageCodec.encodePayload(...)` byte-for-byte.
///
/// `nonce_source_node_id` is the SENDER's node id mixed into the AES-CCM
/// nonce (spec §4.8.2): the sender's *operational* node id on CASE sessions
/// — even though the wire header omits it — and `0` on PASE sessions.
/// Real devices silently drop CASE frames encrypted with the wrong nonce
/// node id (observed: Tapo P110M, M6.6.5 validation).
///
/// # Errors
///
/// - [`Error::PayloadTooLarge`] if `payload.len() > MAX_PAYLOAD_LEN`.
/// - [`Error::Crypto`] if the underlying AES-CCM cipher fails (not
///   expected in practice for spec-bounded message sizes).
pub fn encode_secured(
    header: &SecuredMessageHeader,
    payload: &[u8],
    keys: &crate::session::SessionKeys,
    role: crate::session::SessionRole,
    nonce_source_node_id: u64,
) -> Result<Vec<u8>> {
    // Outbound traffic is encrypted with OUR sending key: i2r when we are
    // the initiator, r2i when we are the responder.
    let key = match role {
        crate::session::SessionRole::Initiator => &keys.i2r_key,
        crate::session::SessionRole::Responder => &keys.r2i_key,
    };
    encode_secured_with_cipher(
        header,
        payload,
        &matter_crypto::aead::SessionAead::new(key),
        nonce_source_node_id,
    )
}

/// [`encode_secured`] with the AES-CCM cipher supplied by the caller
/// instead of rebuilt from raw key bytes.
///
/// Identical output to [`encode_secured`] for the same
/// header/payload/key/nonce — pinned by
/// `with_cipher_paths_match_public_functions`. The point is that
/// [`matter_crypto::aead::SessionAead`] caches the AES-128 key schedule, so a
/// caller holding one per session (see [`crate::session::Session`]) pays
/// key expansion once per session rather than once per packet. `cipher` MUST
/// be built from the key [`encode_secured`] would have selected for `role`;
/// the direction choice lives in the two callers, not here.
///
/// # Errors
///
/// Same as [`encode_secured`]: [`Error::PayloadTooLarge`] and
/// [`Error::Crypto`].
pub(crate) fn encode_secured_with_cipher(
    header: &SecuredMessageHeader,
    payload: &[u8],
    cipher: &matter_crypto::aead::SessionAead,
    nonce_source_node_id: u64,
) -> Result<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(Error::PayloadTooLarge {
            len: payload.len(),
            max: MAX_PAYLOAD_LEN,
        });
    }

    // One allocation for the finished frame, sized for the worst-case header
    // up front: previously the header `Vec` was allocated at 24 bytes and
    // then GUARANTEED to reallocate when the ciphertext (payload + 16-byte
    // tag) was appended — a realloc plus a full copy of every outbound
    // packet. `MAX_HEADER_LEN` over-reserves by at most 16 bytes for the
    // common S=0/DSIZ=0 header, which is cheaper than the realloc it avoids.
    let mut out =
        Vec::with_capacity(MAX_HEADER_LEN + payload.len() + matter_crypto::aead::AEAD_TAG_LEN);
    encode_header_into(header, &mut out);
    let nonce = build_nonce(header, nonce_source_node_id);
    // AAD is the header bytes just written — the same bytes the old code
    // passed as a standalone `Vec`. `cipher.encrypt` allocates its output
    // pre-sized to `payload.len() + tag` internally (aead 0.5's blanket
    // `Aead::encrypt`), so it does not realloc either.
    let ciphertext = cipher.encrypt(&nonce, &out, payload)?;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt + decode a Matter secured message.
///
/// On success returns the parsed header and the decrypted payload.
///
/// Replay is classified ONLY after the message authenticates: the header
/// (session id, counter) is unauthenticated plaintext, so both advancing the
/// window *and reporting a replay* before verifying the AES-CCM tag are
/// unsafe. Advancing early would let a forged far-future counter poison the
/// window (stranding later genuine traffic — a remote single-packet `DoS`);
/// reporting a replay early would let a forged datagram carrying a
/// previously-seen reliable counter drive the caller's MRP duplicate-ack path
/// into emitting an encrypted ack and burning a counter. We therefore
/// authenticate first, then call [`ReplayWindow::check_and_record`] — the sole
/// replay gate on this path. A forged datagram fails the AEAD tag and returns
/// [`Error::DecryptionFailed`] before any replay/ack decision is reached.
///
/// # Errors
///
/// - [`Error::MalformedHeader`] if the header bytes are truncated or
///   reserved-value bits are set.
/// - [`Error::ReplayedCounter`] / [`Error::CounterTooOld`] per
///   [`ReplayWindow::check_and_record`].
/// - [`Error::DecryptionFailed`] if the AES-CCM tag does not verify.
///
/// `nonce_source_node_id` mirrors [`encode_secured`]: the SENDER'S (here:
/// the peer's) operational node id on CASE sessions, `0` on PASE sessions.
pub fn decode_secured(
    bytes: &[u8],
    keys: &crate::session::SessionKeys,
    role: crate::session::SessionRole,
    replay_window: &mut ReplayWindow,
    nonce_source_node_id: u64,
) -> Result<(SecuredMessageHeader, Vec<u8>)> {
    let (header, rest) = decode_header(bytes)?;
    let header_len = bytes.len() - rest.len();

    // We're decoding inbound from the peer; the peer's outbound key is
    // the opposite of ours.
    let key = match role {
        crate::session::SessionRole::Initiator => &keys.r2i_key,
        crate::session::SessionRole::Responder => &keys.i2r_key,
    };

    let plaintext = decrypt_secured_with_header(
        bytes,
        &header,
        header_len,
        &matter_crypto::aead::SessionAead::new(key),
        nonce_source_node_id,
    )?;
    // Authenticated, so it is now safe to COMMIT the counter to the
    // replay window. A forged datagram never reaches this point.
    replay_window.check_and_record(header.message_counter.0)?;
    Ok((header, plaintext))
}

/// The decrypt-and-authenticate half of [`decode_secured`], with the header
/// already parsed and the AES-CCM cipher supplied by the caller. It does not
/// touch a replay window: the caller checks the counter afterwards, and must
/// never before, so that only an authenticated message is
/// classified and only an authenticated duplicate can earn an ACK.
///
/// `header` MUST be the result of [`decode_header`] on `bytes`, and
/// `header_len` the number of leading bytes it consumed (i.e.
/// `bytes.len() - rest.len()`); `cipher` MUST be built from the key
/// [`decode_secured`] would have selected for the session role. The two
/// exist so [`crate::session::SessionManager::decode_inbound`], which must
/// parse the header anyway to demux onto a session, parses it exactly once
/// per inbound packet and reuses that session's cached cipher instead of
/// re-running AES key expansion per packet.
///
/// Returns the decrypted payload only — the caller already owns the header.
///
/// # Errors
///
/// - [`Error::DecryptionFailed`] if the AES-CCM tag does not verify.
/// - [`Error::MalformedHeader`] if `header_len` is past the end of `bytes`
///   (a caller-contract violation; we bound-check rather than panic).
pub(crate) fn decrypt_secured_with_header(
    bytes: &[u8],
    header: &SecuredMessageHeader,
    header_len: usize,
    cipher: &matter_crypto::aead::SessionAead,
    nonce_source_node_id: u64,
) -> Result<Vec<u8>> {
    // Replay classification happens AFTER authentication, never
    // before, and is the caller's step. The header (session id, counter) is
    // unauthenticated cleartext, so there is no pre-decrypt reject here — an
    // attacker who replays a previously-seen reliable counter with any
    // ciphertext would otherwise be reported as `ReplayedCounter`, which
    // drives the MRP duplicate-ack path (`SessionManager::decode_inbound`)
    // into emitting an encrypted standalone ack and burning an outbound
    // counter for a datagram that never authenticated. We decrypt first; the
    // caller checks the window only after this returns Ok. (chip does the
    // same: decrypt+verify before message-counter verification,
    // `SessionManager.cpp:963`.)

    // The AAD is the on-the-wire header — the leading `header_len` bytes of
    // `bytes`, i.e. byte-for-byte the header the sender authenticated. We
    // slice it directly rather than re-encoding via `encode_header(header)`:
    // the slice and the re-encode are identical bytes (pinned by
    // `aad_slice_equals_reencoded_header`), and the slice avoids an
    // allocation + re-serialisation on every inbound packet.
    let (aad, rest) = bytes
        .split_at_checked(header_len)
        .ok_or(Error::MalformedHeader(header_len))?;
    let nonce = build_nonce(header, nonce_source_node_id);

    cipher
        .decrypt(&nonce, aad, rest)
        .map_err(|_| Error::DecryptionFailed)
}

/// Encode + encrypt a Matter **group** secured message (Matter Core Spec
/// §4.15 group messaging, framing per §4.4 / §4.8.2).
///
/// A group message differs from the unicast secured path in six ways, all
/// of which we mirror byte-for-byte against connectedhomeip (the full-frame
/// KAT below comes from `TestSessionManagerDispatch.cpp`, "private group
/// message"; the P=0 decode direction is additionally covered by the
/// matter.js vector `test-vectors/transport/group-message.json`):
///
/// 1. **Key.** AES-128-CCM uses the *operational group key* directly — there
///    is no per-session `i2r`/`r2i` split and no [`crate::session::Session`],
///    so the caller supplies the 16-byte key.
/// 2. **Security flags.** [`SecurityFlags::SESSION_TYPE_GROUP`] plus
///    [`SecurityFlags::PRIVACY`] — chip's `SessionManager` sets the `P` flag
///    unconditionally for group TX; the wire `securityFlags` byte is `0x81`.
/// 3. **Message flags / destination.** [`SecuredMessageFlags::DEST_GROUP`]
///    (`DSIZ = 0b10`) plus [`SecuredMessageFlags::SOURCE_PRESENT`] — a group
///    header carries BOTH the 8-byte source node id and the 2-byte
///    destination group id (`msgFlags = 0x06`).
/// 4. **Nonce.** `SecurityFlags(1) || MessageCounter(4 LE) || SourceNodeId(8
///    LE)`, where the node id is OUR (the sender's) operational node id.
///    The flags byte is the RAW security-flags byte — `P` bit **included**
///    (`0x81`), exactly as in the header/AAD: chip `CryptoContext::BuildNonce`
///    consumes `PacketHeader::GetSecurityFlags()`, the raw byte. See the
///    private `build_nonce` helper.
/// 5. **Privacy obfuscation.** After AEAD encryption, the mutable header
///    region — bytes `[4..header_len]`: message counter, source node id,
///    destination group id — is XOR-masked with an AES-CTR keystream under the
///    *privacy key* (derived from the operational key,
///    [`matter_crypto::derive_group_privacy_key`]) and the privacy nonce
///    `SessionId(2 BE) || MIC[5..16]` ([`build_group_privacy_nonce`]).
/// 6. **No MRP.** Group sends are unacknowledged; the caller owns the group
///    message counter (there is no session counter to advance).
///
/// `counter` is the group message counter the caller has allocated.
/// `protocol_header` and `app_payload` form the plaintext exactly as on the
/// unicast path: `encode_protocol_header(protocol_header) || app_payload`.
///
/// The output layout is `header bytes (privacy-obfuscated from byte 4) ||
/// AES-CCM(plaintext) || 16-byte tag`, byte-for-byte the full group wire
/// message as chip emits it.
///
/// # Errors
///
/// - [`Error::PayloadTooLarge`] if the encoded plaintext (protocol header +
///   `app_payload`) exceeds the framing payload cap.
/// - [`Error::Crypto`] if the underlying AES-CCM cipher or the privacy-key
///   HKDF fails (not expected in practice for spec-bounded message sizes).
pub fn encode_group_secured(
    operational_group_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
    group_session_id: u16,
    source_node_id: u64,
    group_id: u16,
    counter: u32,
    protocol_header: &crate::protocol_header::ProtocolHeader,
    app_payload: &[u8],
) -> Result<Vec<u8>> {
    // Derive the privacy key here, then run the ONE encode path below. A
    // caller that sends repeatedly under a long-lived group key should
    // pre-derive once and call the variant directly.
    let privacy_key =
        matter_crypto::derive_group_privacy_key(operational_group_key).map_err(Error::Crypto)?;
    encode_group_secured_with_privacy_key(
        operational_group_key,
        &privacy_key,
        group_session_id,
        source_node_id,
        group_id,
        counter,
        protocol_header,
        app_payload,
    )
}

/// [`encode_group_secured`] with the **privacy key supplied by the caller**
/// instead of derived per packet.
///
/// Identical wire output — this is the same code path, and
/// [`encode_group_secured`] is a thin wrapper that derives `privacy_key` and
/// calls straight through. Use this when you hold a long-lived group key set:
/// derive once with [`matter_crypto::derive_group_privacy_key`], cache it
/// alongside the operational group key, and skip an HKDF per outbound packet.
///
/// `privacy_key` MUST be `derive_group_privacy_key(operational_group_key)`.
/// Any other value produces a frame no spec-conformant receiver can
/// de-obfuscate (the AAD would not match, so the AEAD tag check fails there) —
/// it is not a second, independent secret.
///
/// # Errors
///
/// Same as [`encode_group_secured`], minus the privacy-key HKDF (already
/// done by the caller): [`Error::PayloadTooLarge`] for an over-cap plaintext,
/// [`Error::Crypto`] if the underlying AES-CCM cipher fails.
// One key more than `encode_group_secured`, which is already at the limit;
// the parameters are the group header fields and cannot be bundled without
// inventing a type the caller would only ever fill in inline.
#[allow(clippy::too_many_arguments)]
pub fn encode_group_secured_with_privacy_key(
    operational_group_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
    privacy_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
    group_session_id: u16,
    source_node_id: u64,
    group_id: u16,
    counter: u32,
    protocol_header: &crate::protocol_header::ProtocolHeader,
    app_payload: &[u8],
) -> Result<Vec<u8>> {
    // Plaintext = protocol header || app payload, identical to the unicast
    // path's `prepared.wire_payload`.
    let mut plaintext = Vec::with_capacity(16 + app_payload.len());
    crate::protocol_header::encode_protocol_header(protocol_header, &mut plaintext);
    plaintext.extend_from_slice(app_payload);

    if plaintext.len() > MAX_PAYLOAD_LEN {
        return Err(Error::PayloadTooLarge {
            len: plaintext.len(),
            max: MAX_PAYLOAD_LEN,
        });
    }

    let header = SecuredMessageHeader {
        // Group header carries BOTH the source node id (S=1) and the 2-byte
        // group id (DSIZ=0b10) → msgFlags = 0x06.
        flags: SecuredMessageFlags::SOURCE_PRESENT | SecuredMessageFlags::DEST_GROUP,
        session_id: SessionId(group_session_id),
        // P is set from the start: the AAD (and the wire) carry 0x81. Only
        // the AEAD nonce masks the P bit (see build_nonce).
        security_flags: SecurityFlags::SESSION_TYPE_GROUP | SecurityFlags::PRIVACY,
        message_counter: MessageCounter(counter),
        source_node_id: Some(NodeId(source_node_id)),
        destination_node_id: Some(DestNodeId::Group(group_id)),
    };

    // AAD = the exact encoded packet-header bytes (spec §4.8.2), P bit
    // included. Nonce mixes the source node id; `build_nonce` reads it from
    // the header (which carries it for group messages).
    let aad = encode_header(&header);
    let nonce = build_nonce(&header, source_node_id);

    // Reuse the shared AES-CCM routine — never hand-roll the cipher. One
    // `SessionAead` per call: group sends have no `Session` to hang a cached
    // cipher off, so the key schedule is built here (it is not cached across
    // calls).
    let ciphertext = matter_crypto::aead::SessionAead::new(operational_group_key)
        .encrypt(&nonce, &aad, &plaintext)?;

    // Privacy obfuscation (§4.8.3): CTR over the header's mutable region
    // under the privacy key, keyed by the (plaintext) session id + MIC.
    let mic = ciphertext
        .get(ciphertext.len() - matter_crypto::aead::AEAD_TAG_LEN..)
        .ok_or(Error::Crypto(matter_crypto::Error::EncryptionFailed))?;
    let privacy_nonce = build_group_privacy_nonce(group_session_id, mic)?;
    let mut out = aad;
    let obfuscated = matter_crypto::aead::SessionAead::new(privacy_key)
        .ctr_apply(&privacy_nonce, &out[PRIVACY_REGION_START..])?;
    out.truncate(PRIVACY_REGION_START);
    out.extend_from_slice(&obfuscated);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// First obfuscated byte of a privacy-enhanced header: the flags, session id
/// and security-flags bytes (`[0..4]`) stay in the clear — the receiver needs
/// them to locate the key — while everything after (message counter, source
/// node id, destination group id) is obfuscated (Matter Core Spec §4.8.3;
/// chip `PacketHeader::PrivacyEncrypt`).
const PRIVACY_REGION_START: usize = 4;

/// Compose the 13-byte group **privacy nonce** (Matter Core Spec §4.8.3;
/// chip `CryptoContext::BuildPrivacyNonce`):
/// `SessionId(2 BE) || MIC[5..16]` — note the session id is **big-endian**
/// here, unlike its little-endian wire encoding.
///
/// `mic` is the 16-byte AEAD tag trailing the message ciphertext.
///
/// # Errors
///
/// Returns [`Error::MalformedHeader`] if `mic` is not exactly 16 bytes.
pub fn build_group_privacy_nonce(
    session_id: u16,
    mic: &[u8],
) -> Result<[u8; matter_crypto::aead::AEAD_NONCE_LEN]> {
    if mic.len() != matter_crypto::aead::AEAD_TAG_LEN {
        return Err(Error::MalformedHeader(mic.len()));
    }
    let mut nonce = [0u8; matter_crypto::aead::AEAD_NONCE_LEN];
    nonce[0..2].copy_from_slice(&session_id.to_be_bytes());
    nonce[2..13].copy_from_slice(&mic[5..16]);
    Ok(nonce)
}

/// Decrypt + decode a Matter **group** secured message produced by
/// [`encode_group_secured`], chip, or a matter.js group sender.
///
/// Handles both privacy-enhanced (`P = 1`, chip's unconditional group TX
/// format) and plain (`P = 0`, e.g. matter.js) frames: when `P` is set, the
/// header's mutable region is first de-obfuscated with the privacy key
/// derived from `operational_group_key` (§4.8.3), then the message is
/// decrypted as usual.
///
/// On success returns the parsed (de-obfuscated) header and the decrypted
/// plaintext (protocol header || app payload). The caller recovers the
/// source node id and group id from the returned [`SecuredMessageHeader`].
///
/// Unlike [`decode_secured`], there is no replay window: group messaging is
/// stateless on the receive side here, and the caller owns any per-group
/// replay tracking. The nonce node id is taken from the header's source node
/// id (always present on group messages).
///
/// # Errors
///
/// - [`Error::MalformedHeader`] if the header bytes are truncated or
///   reserved-value bits are set (including a `P = 1` frame too short to
///   hold the privacy region + MIC).
/// - [`Error::DecryptionFailed`] if the AES-CCM tag does not verify (wrong
///   group key, tampered ciphertext, or mismatched AAD/nonce).
pub fn decode_group_secured(
    bytes: &[u8],
    operational_group_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
) -> Result<(SecuredMessageHeader, Vec<u8>)> {
    // `None` = derive the privacy key on demand, and only if the frame
    // actually carries P = 1 (a plain matter.js frame needs no HKDF at all).
    decode_group_secured_inner(bytes, operational_group_key, None)
}

/// [`decode_group_secured`] with the **privacy key supplied by the caller**
/// instead of derived per packet.
///
/// Same code path and same acceptance behaviour — [`decode_group_secured`]
/// differs only in deriving `privacy_key` itself (and only for `P = 1`
/// frames). Use this when you hold a long-lived group key set: derive once
/// with [`matter_crypto::derive_group_privacy_key`], cache it alongside the
/// operational group key, and skip an HKDF per inbound packet.
///
/// `privacy_key` MUST be `derive_group_privacy_key(operational_group_key)`;
/// a wrong value de-obfuscates the header to garbage and the frame then fails
/// its AEAD tag check ([`Error::DecryptionFailed`]). It is ignored entirely
/// for `P = 0` frames, which carry no obfuscation.
///
/// # Errors
///
/// Same as [`decode_group_secured`].
pub fn decode_group_secured_with_privacy_key(
    bytes: &[u8],
    operational_group_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
    privacy_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
) -> Result<(SecuredMessageHeader, Vec<u8>)> {
    decode_group_secured_inner(bytes, operational_group_key, Some(privacy_key))
}

/// The single group-decode implementation behind both public entry points.
///
/// `privacy_key`: `Some` = pre-derived by the caller, `None` = derive it here
/// (lazily, inside the `P = 1` branch only).
fn decode_group_secured_inner(
    bytes: &[u8],
    operational_group_key: &[u8; matter_crypto::aead::AEAD_KEY_LEN],
    privacy_key: Option<&[u8; matter_crypto::aead::AEAD_KEY_LEN]>,
) -> Result<(SecuredMessageHeader, Vec<u8>)> {
    // The first 4 bytes (flags, session id LE, security flags) are always in
    // the clear — peek at the P bit before any header parse, because a
    // privacy-enhanced header's counter/source/group bytes are obfuscated and
    // would otherwise mis-parse.
    let sec_byte = *bytes.get(3).ok_or(Error::MalformedHeader(bytes.len()))?;
    let deobfuscated: Vec<u8>;
    let wire = if SecurityFlags::from_bits_retain(sec_byte).contains(SecurityFlags::PRIVACY) {
        // Privacy region length = header length for this flags byte:
        // 4 + source(8, S bit) + destination(8 / 2 by DSIZ).
        let flags = SecuredMessageFlags::from_bits_retain(bytes[0]);
        let mut header_len = PRIVACY_REGION_START + 4; // counter always present
        if flags.contains(SecuredMessageFlags::SOURCE_PRESENT) {
            header_len += 8;
        }
        header_len += match bytes[0] & DSIZ_MASK {
            DSIZ_UNICAST => 8,
            DSIZ_GROUP => 2,
            _ => 0,
        };
        let min_len = header_len + matter_crypto::aead::AEAD_TAG_LEN;
        if bytes.len() < min_len {
            return Err(Error::MalformedHeader(bytes.len()));
        }
        let session_id = u16::from_le_bytes([bytes[1], bytes[2]]);
        let mic = &bytes[bytes.len() - matter_crypto::aead::AEAD_TAG_LEN..];
        let privacy_nonce = build_group_privacy_nonce(session_id, mic)?;
        // Borrow the caller's key, or derive one that lives just long enough
        // for the de-obfuscation below.
        let derived;
        let privacy_key = if let Some(key) = privacy_key {
            key
        } else {
            derived = matter_crypto::derive_group_privacy_key(operational_group_key)
                .map_err(Error::Crypto)?;
            &derived
        };
        let clear = matter_crypto::aead::SessionAead::new(privacy_key)
            .ctr_apply(&privacy_nonce, &bytes[PRIVACY_REGION_START..header_len])?;
        let mut copy = Vec::with_capacity(bytes.len());
        copy.extend_from_slice(&bytes[..PRIVACY_REGION_START]);
        copy.extend_from_slice(&clear);
        copy.extend_from_slice(&bytes[header_len..]);
        deobfuscated = copy;
        &deobfuscated[..]
    } else {
        bytes
    };

    let (header, rest) = decode_header(wire)?;
    // AAD is the de-obfuscated header — the leading bytes up to `rest` (same
    // optimisation as `decode_secured`). The AAD keeps the P bit; only the
    // AEAD nonce masks it (build_nonce).
    let header_len = wire.len() - rest.len();
    let aad = &wire[..header_len];
    // Group messages carry the source node id in the header; `build_nonce`
    // uses it. The fallback (0) is never reached for a well-formed group
    // header (S=1).
    let nonce = build_nonce(&header, 0);
    let plaintext = matter_crypto::aead::SessionAead::new(operational_group_key)
        .decrypt(&nonce, aad, rest)
        .map_err(|_| Error::DecryptionFailed)?;
    Ok((header, plaintext))
}

/// Compose the AES-CCM nonce per Matter Core Spec §4.8.2:
/// `nonce = SecurityFlags(1) || MessageCounter(4 LE) || SourceNodeId(8 LE)`.
///
/// `nonce_source_node_id` is supplied by the caller because the nonce node
/// id is decoupled from the wire header: secured-session headers omit the
/// source node id (S=0), yet CASE sessions still mix the sender's
/// *operational* node id into the nonce (chip `CryptoContext::BuildNonce`).
/// PASE sessions and the historical header-coupled paths pass `0` or the
/// header's value respectively. When the header DOES carry a source node id
/// (group messages), the header value takes precedence — they must agree on
/// the wire anyway.
fn build_nonce(
    header: &SecuredMessageHeader,
    nonce_source_node_id: u64,
) -> [u8; matter_crypto::aead::AEAD_NONCE_LEN] {
    let mut nonce = [0u8; matter_crypto::aead::AEAD_NONCE_LEN];
    // The nonce carries the RAW security-flags byte, privacy bit included
    // (0x81 for chip group TX): chip `CryptoContext::BuildNonce` consumes
    // `PacketHeader::GetSecurityFlags()` = `mSecFlags.Raw()`. Pinned by the
    // full-frame chip KAT (`group_encode_matches_chip_privacy_vector`) —
    // note the `.nonce` field inside chip's own `TestSessionManagerDispatch`
    // vector struct claims a masked `0x01` byte, but it is unused by that
    // test and does NOT decrypt the fixture frames; only the raw byte does.
    nonce[0] = header.security_flags.bits();
    nonce[1..5].copy_from_slice(&header.message_counter.0.to_le_bytes());
    let node_id = match header.source_node_id {
        Some(NodeId(n)) => n,
        None => nonce_source_node_id,
    };
    nonce[5..13].copy_from_slice(&node_id.to_le_bytes());
    nonce
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::error::Error;

    /// Decode a hex string to bytes (test helper for the group KAT).
    fn hex(s: &str) -> Vec<u8> {
        assert!(
            s.len().is_multiple_of(2),
            "hex string must have even length"
        );
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Decode a 32-char hex string into a fixed 16-byte array.
    fn hex16(s: &str) -> [u8; 16] {
        let v = hex(s);
        let mut a = [0u8; 16];
        a.copy_from_slice(&v);
        a
    }

    /// Lowercase hex-encode bytes (for readable KAT assertion messages).
    fn hex_encode(b: &[u8]) -> String {
        use std::fmt::Write as _;
        b.iter()
            .fold(String::with_capacity(b.len() * 2), |mut s, x| {
                let _ = write!(s, "{x:02x}");
                s
            })
    }

    /// Spec §4.4.1 minimum header: version=0, S=0, DSIZ=0. Only the
    /// 8-byte fixed portion is present.
    #[test]
    fn minimal_header_roundtrip() {
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::empty(),
            session_id: SessionId(0x1234),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(0xAABB_CCDD),
            source_node_id: None,
            destination_node_id: None,
        };
        let bytes = encode_header(&header);
        assert_eq!(bytes.len(), 8, "fixed 8-byte header");
        // byte 0 = flags (0x00), bytes 1..3 = session_id LE (0x34 0x12),
        // byte 3 = security_flags (0x00), bytes 4..8 = counter LE.
        assert_eq!(bytes, vec![0x00, 0x34, 0x12, 0x00, 0xDD, 0xCC, 0xBB, 0xAA]);

        let (parsed, rest) = decode_header(&bytes).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_with_source_node_id() {
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::SOURCE_PRESENT,
            session_id: SessionId(0x0001),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(1),
            source_node_id: Some(NodeId(0x1122_3344_5566_7788)),
            destination_node_id: None,
        };
        let bytes = encode_header(&header);
        assert_eq!(bytes.len(), 16, "8 fixed + 8 source");
        let (parsed, rest) = decode_header(&bytes).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_with_unicast_destination() {
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::DEST_UNICAST,
            session_id: SessionId(0xFFFF),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(u32::MAX),
            source_node_id: None,
            destination_node_id: Some(DestNodeId::Node(NodeId(0xDEAD_BEEF_CAFE_BABE))),
        };
        let bytes = encode_header(&header);
        assert_eq!(bytes.len(), 16, "8 fixed + 8 dest");
        let (parsed, rest) = decode_header(&bytes).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_with_group_destination() {
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::DEST_GROUP,
            session_id: SessionId(7),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(42),
            source_node_id: None,
            destination_node_id: Some(DestNodeId::Group(0xABCD)),
        };
        let bytes = encode_header(&header);
        assert_eq!(bytes.len(), 10, "8 fixed + 2 group");
        let (parsed, rest) = decode_header(&bytes).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_with_source_and_destination() {
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::SOURCE_PRESENT | SecuredMessageFlags::DEST_UNICAST,
            session_id: SessionId(0x4242),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(0x1000_0000),
            source_node_id: Some(NodeId(0x1111_2222_3333_4444)),
            destination_node_id: Some(DestNodeId::Node(NodeId(0x5555_6666_7777_8888))),
        };
        let bytes = encode_header(&header);
        assert_eq!(bytes.len(), 24, "8 fixed + 8 source + 8 dest");
        let (parsed, rest) = decode_header(&bytes).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(rest, b"");
    }

    #[test]
    fn header_decode_keeps_payload_slice() {
        // Minimal header followed by 4 bytes of "encrypted payload".
        let mut bytes = vec![0x00, 0x34, 0x12, 0x00, 0xDD, 0xCC, 0xBB, 0xAA];
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let (_, rest) = decode_header(&bytes).unwrap();
        assert_eq!(rest, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn header_decode_truncated_fixed_portion() {
        let bytes = [0x00, 0x34, 0x12];
        let err = decode_header(&bytes).unwrap_err();
        assert!(matches!(err, Error::MalformedHeader(_)));
    }

    #[test]
    fn header_decode_truncated_source_node_id() {
        // Flags say S=1 but only 3 bytes of source node ID present.
        let mut bytes = vec![
            SecuredMessageFlags::SOURCE_PRESENT.bits(),
            0x01,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
        ];
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // 3 bytes only
        let err = decode_header(&bytes).unwrap_err();
        assert!(matches!(err, Error::MalformedHeader(_)));
    }

    #[test]
    fn header_decode_rejects_reserved_dsiz() {
        // Flags byte with DSIZ=0b11 in the low two bits is reserved.
        let bytes = [0b0000_0011, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        let err = decode_header(&bytes).unwrap_err();
        assert!(matches!(err, Error::MalformedHeader(_)));
    }

    #[test]
    fn header_decode_rejects_nonzero_version() {
        // Version field (bits 4..=7 of byte 0) must be 0 for the current
        // spec. A header advertising version=1 (bit 4 set) must be rejected.
        let bytes = [0b0001_0000, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        let err = decode_header(&bytes).unwrap_err();
        assert!(matches!(err, Error::MalformedHeader(_)));
    }

    #[test]
    fn header_decode_rejects_set_reserved_bit() {
        // Bit 3 of byte 0 is reserved and must be 0.
        let bytes = [0b0000_1000, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        let err = decode_header(&bytes).unwrap_err();
        assert!(matches!(err, Error::MalformedHeader(_)));
    }

    #[test]
    fn header_decode_accepts_all_legitimate_flag_bits() {
        // S + DSIZ=unicast set, version/reserved clear — must still decode.
        let bytes = [
            SecuredMessageFlags::SOURCE_PRESENT.bits() | SecuredMessageFlags::DEST_UNICAST.bits(),
            0x01,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            // 8-byte source node id + 8-byte dest node id.
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        assert!(decode_header(&bytes).is_ok());
    }

    /// Load-bearing invariant for the `decode_secured` AAD optimisation:
    /// the leading header bytes of the input (`&bytes[..header_len]`, where
    /// `header_len = bytes.len() - rest.len()`) are byte-for-byte identical to
    /// `encode_header(&decoded_header)`. If this ever drifts — e.g. a header
    /// field that doesn't round-trip exactly — `decode_secured` would feed the
    /// AES-CCM verifier the wrong AAD and silently fail ALL decryption. We pin
    /// it here across the maximal header shape (S=1, DSIZ=unicast) plus a
    /// trailing payload so `rest` is non-empty.
    #[test]
    fn aad_slice_equals_reencoded_header() {
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::SOURCE_PRESENT | SecuredMessageFlags::DEST_UNICAST,
            session_id: SessionId(0x4242),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(0x1234_5678),
            source_node_id: Some(NodeId(0x1111_2222_3333_4444)),
            destination_node_id: Some(DestNodeId::Node(NodeId(0x5555_6666_7777_8888))),
        };
        let mut bytes = encode_header(&header);
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // pretend payload+tag
        let (decoded, rest) = decode_header(&bytes).unwrap();
        let header_len = bytes.len() - rest.len();
        assert_eq!(
            &bytes[..header_len],
            encode_header(&decoded).as_slice(),
            "AAD slice must equal the re-encoded header byte-for-byte"
        );
    }

    /// Build a minimal secured header at `counter` for the secured-message
    /// tests below. S=0/DSIZ=0 (the common CASE/PASE unicast shape).
    fn secured_header(counter: u32) -> SecuredMessageHeader {
        SecuredMessageHeader {
            flags: SecuredMessageFlags::empty(),
            session_id: SessionId(0x0042),
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(counter),
            source_node_id: None,
            destination_node_id: None,
        }
    }

    /// Fixed, distinct key material for the secured-message tests. The exact
    /// bytes don't matter — only that encrypt/decrypt agree on them.
    fn test_session_keys() -> crate::session::SessionKeys {
        crate::session::SessionKeys {
            i2r_key: [0x11; 16],
            r2i_key: [0x22; 16],
            attestation_key: [0x33; 16],
        }
    }

    /// The cipher-taking internals MUST be byte-for-byte interchangeable
    /// with the public key-taking entry points: `encode_secured_with_cipher`
    /// emits exactly the frame `encode_secured` emits, and each decode path
    /// accepts the other's output. This is the referee for the per-session
    /// cipher cache — if the direction/key selection in the internal path
    /// ever drifts from the public one, real peers would silently fail to
    /// decrypt, so we pin the equivalence here rather than trusting that two
    /// copies of the key-choice `match` stay in step.
    #[test]
    fn with_cipher_paths_match_public_functions() {
        use crate::session::SessionRole;
        use matter_crypto::aead::SessionAead;

        let keys = test_session_keys();
        let header = secured_header(7);
        let payload = b"cipher parity payload";
        // Sender's nonce node id (non-zero, as on a CASE session).
        let nonce_node_id = 0x0102_0304_0506_0708u64;

        // Encode as the Initiator: the public path picks `i2r_key`, so the
        // hand-built cipher must be the i2r one for the outputs to match.
        let public = encode_secured(
            &header,
            payload,
            &keys,
            SessionRole::Initiator,
            nonce_node_id,
        )
        .unwrap();
        let cipher = SessionAead::new(&keys.i2r_key);
        let with_cipher =
            encode_secured_with_cipher(&header, payload, &cipher, nonce_node_id).unwrap();
        assert_eq!(
            public, with_cipher,
            "with-cipher encode must be byte-identical to the public encode"
        );

        // Decode direction: as the Responder we decrypt the peer's traffic
        // with `i2r_key` — the same cipher. Feed the internal decoder the
        // pre-parsed header, as `SessionManager::decode_inbound` does.
        let (parsed, rest) = decode_header(&public).unwrap();
        let header_len = public.len() - rest.len();
        let plaintext =
            decrypt_secured_with_header(&public, &parsed, header_len, &cipher, nonce_node_id)
                .unwrap();
        assert_eq!(plaintext, payload);

        // ...and the public decoder accepts the with-cipher frame.
        let mut public_window = ReplayWindow::new();
        let (public_header, public_plaintext) = decode_secured(
            &with_cipher,
            &keys,
            SessionRole::Responder,
            &mut public_window,
            nonce_node_id,
        )
        .unwrap();
        assert_eq!(public_header, parsed);
        assert_eq!(public_plaintext, payload);
    }

    /// A forged datagram must not advance the replay window when its tag
    /// fails to verify. Pre-fix, `decode_secured` recorded the counter
    /// BEFORE authenticating, so a forged large counter poisoned the window
    /// and rejected subsequent genuine (smaller-counter) traffic.
    #[test]
    fn forged_packet_does_not_poison_replay_window() {
        let keys = test_session_keys();
        let mut window = ReplayWindow::new();

        // Genuine message at counter=5 — encoded by the Initiator (sender),
        // decoded by us as the Responder (receiver). Seeds the window at 5.
        let genuine5 = encode_secured(
            &secured_header(5),
            b"hello",
            &keys,
            crate::session::SessionRole::Initiator,
            0,
        )
        .unwrap();
        let (h5, _) = decode_secured(
            &genuine5,
            &keys,
            crate::session::SessionRole::Responder,
            &mut window,
            0,
        )
        .unwrap();
        assert_eq!(h5.message_counter.0, 5);

        // Forged datagram: well-formed header with a far-future counter
        // (1000) but garbage ciphertext, so the AES-CCM tag cannot verify.
        let mut forged = encode_header(&secured_header(1000));
        forged.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00, 0x00, 0x00]);
        let err = decode_secured(
            &forged,
            &keys,
            crate::session::SessionRole::Responder,
            &mut window,
            0,
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::DecryptionFailed),
            "forged packet must fail authentication; got {err:?}",
        );

        // The window must still be seeded at 5, not 1000 — so a genuine
        // follow-up at counter=6 is accepted. Pre-fix this failed as
        // CounterTooOld (6 < 1000 - 32).
        let genuine6 = encode_secured(
            &secured_header(6),
            b"world",
            &keys,
            crate::session::SessionRole::Initiator,
            0,
        )
        .unwrap();
        let (h6, payload) = decode_secured(
            &genuine6,
            &keys,
            crate::session::SessionRole::Responder,
            &mut window,
            0,
        )
        .unwrap_or_else(|e| {
            panic!("genuine counter=6 must decode after a forged packet; got {e:?}")
        });
        assert_eq!(h6.message_counter.0, 6);
        assert_eq!(payload, b"world");
    }

    /// **Security-critical known-answer test.** A plain (`P = 0`) group
    /// message from the independent matter.js vector
    /// (`test-vectors/transport/group-message.json`, captured from
    /// `@matter/protocol` `GroupSession.encode` + Node `aes-128-ccm`) MUST
    /// decode: matter.js does not privacy-obfuscate group TX, and spec
    /// receivers accept `P = 0` frames. If the nonce, AAD, key, or header
    /// layout drifts by a single byte the tag check fails.
    ///
    /// (The encode direction moved to the chip privacy vector in
    /// [`group_encode_matches_chip_privacy_vector`] when group TX gained
    /// unconditional privacy for chip parity.)
    ///
    /// Inputs are read straight from the fixture:
    /// - `operational_group_key` = `4e6f436f6e74726f6c4d61747465724b`
    /// - `group_session_id` = 1, `group_id` = 0x1234, `message_counter` = 5
    /// - `source_node_id` = 0xDEADBEEFCAFEBABE
    /// - plaintext = `052102d10a0001003501290218` (a protocol header
    ///   `INITIATOR|RELIABLE`, opcode 0x21, exchange 0xd102, protocol 0x000a,
    ///   followed by the app payload tail `01003501290218`).
    #[test]
    fn group_decode_matches_matterjs_vector() {
        // ---- Fixture (verbatim from group-message.json) ----
        let key: [u8; 16] = hex16("4e6f436f6e74726f6c4d61747465724b");
        let wire =
            hex("0601000105000000bebafecaefbeadde34129506e3d4b7267e33e078a21650d79c70db886e5ddfc1398a6ee2212fb5");

        let (header, plaintext) = decode_group_secured(&wire, &key).unwrap();
        assert_eq!(header.session_id, SessionId(1));
        assert_eq!(header.message_counter, MessageCounter(5));
        assert_eq!(header.source_node_id, Some(NodeId(0xDEAD_BEEF_CAFE_BABE)));
        assert_eq!(header.destination_node_id, Some(DestNodeId::Group(0x1234)));
        assert!(!header.security_flags.contains(SecurityFlags::PRIVACY));
        assert_eq!(
            hex_encode(&plaintext),
            "052102d10a0001003501290218",
            "P=0 group plaintext must match the matter.js fixture"
        );
    }

    /// **Security-critical known-answer test.** The group-secured encode MUST
    /// byte-match the connectedhomeip "private group message" vector
    /// (`src/transport/tests/TestSessionManagerDispatch.cpp`), covering the
    /// full chip group TX format in one shot:
    ///
    /// - security flags `0x81` (`P | group`) in the header, the AAD AND the
    ///   AEAD nonce (chip `BuildNonce` takes the raw byte — the fixture
    ///   struct's `.nonce` field claims `\x01` but is unused by chip's test
    ///   and does not decrypt the frames; `\x81` does);
    /// - privacy key derived from the operational key (fixture `.privacyKey`);
    /// - privacy nonce = session id (BE) || MIC[5..16] (fixture
    ///   `.privacyNonce`);
    /// - CTR obfuscation of header bytes `[4..18]` (counter + source +
    ///   group id), ciphertext/MIC untouched.
    ///
    /// AES-CCM is deterministic for a fixed nonce, so the whole 40-byte wire
    /// message must be byte-identical to the fixture's `.privacy` frame.
    /// Vector stored at `test-vectors/transport/group-message-privacy-chip.json`.
    #[test]
    fn group_encode_matches_chip_privacy_vector() {
        use crate::protocol_header::{ExchangeFlags, ProtocolHeader, ProtocolId};

        // ---- Inputs (verbatim from the chip test vector) ----
        // encryptKey (operational group key; derives from epoch key
        // b0..bf + compressed fabric id 2906c908d115d362).
        let op_key: [u8; 16] = hex16("ca92d7a0942d1a511a0e26ad074f4c2f");
        let group_session_id: u16 = 0xdb7d;
        let group_id: u16 = 2;
        let counter: u32 = 0x1234_5679;
        let source_node_id: u64 = 0x0000_0000_0000_0001;

        // The fixture plaintext `01 64 ee 0e 20 7d` is a protocol header:
        // flags INITIATOR, opcode 0x64, exchange 0x0eee, protocol 0x7d20.
        let protocol_header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x64,
            exchange_id: 0x0eee,
            protocol_id: ProtocolId {
                vendor: 0,
                protocol: 0x7d20,
            },
            ack_counter: None,
        };
        let mut plaintext = Vec::new();
        crate::protocol_header::encode_protocol_header(&protocol_header, &mut plaintext);
        assert_eq!(
            hex_encode(&plaintext),
            "0164ee0e207d",
            "reconstructed plaintext must equal the fixture's decrypted payload"
        );

        // The privacy key chip lists for this vector is exactly what we
        // derive from the operational key (chains the two fixtures).
        assert_eq!(
            matter_crypto::derive_group_privacy_key(&op_key).unwrap(),
            hex16("bfe9da016a765365f2dd97a9f939e425"),
            "privacy key derivation must match the fixture's .privacyKey"
        );

        // ---- Encode ----
        let wire = encode_group_secured(
            &op_key,
            group_session_id,
            source_node_id,
            group_id,
            counter,
            &protocol_header,
            &[],
        )
        .unwrap();

        // ---- The full 40-byte privacy frame, byte-for-byte ----
        let expected = hex("067ddb81d926afce24c8a0981bdd44f4e7302b2f915a66c959629\
             0ebe4408217b3c0c921a2fca4e1");
        assert_eq!(
            hex_encode(&wire),
            hex_encode(&expected),
            "group wire message must byte-match chip's .privacy frame"
        );

        // ---- And it decodes back (P=1 RX path) ----
        let (header, decoded) = decode_group_secured(&wire, &op_key).unwrap();
        assert!(header.security_flags.contains(SecurityFlags::PRIVACY));
        assert!(header
            .security_flags
            .contains(SecurityFlags::SESSION_TYPE_GROUP));
        assert_eq!(header.session_id, SessionId(group_session_id));
        assert_eq!(header.message_counter, MessageCounter(counter));
        assert_eq!(header.source_node_id, Some(NodeId(source_node_id)));
        assert_eq!(
            header.destination_node_id,
            Some(DestNodeId::Group(group_id))
        );
        assert_eq!(decoded, plaintext);
    }

    /// The pre-derived-privacy-key variants must be a pure refactor of the
    /// derive-internally originals: identical wire bytes out, identical
    /// plaintext back. Pinned against BOTH known-answer vectors (chip's
    /// privacy frame for `P = 1`, matter.js's plain frame for `P = 0`), so a
    /// divergence shows up as a KAT failure rather than mere
    /// self-consistency.
    #[test]
    fn group_privacy_key_variant_matches_original() {
        use crate::protocol_header::{ExchangeFlags, ProtocolHeader, ProtocolId};

        // ---- chip privacy vector (P = 1) ----
        let op_key: [u8; 16] = hex16("ca92d7a0942d1a511a0e26ad074f4c2f");
        let privacy_key = matter_crypto::derive_group_privacy_key(&op_key).unwrap();
        assert_eq!(
            privacy_key,
            hex16("bfe9da016a765365f2dd97a9f939e425"),
            "the pre-derived key must be the fixture's .privacyKey"
        );
        let group_session_id: u16 = 0xdb7d;
        let group_id: u16 = 2;
        let counter: u32 = 0x1234_5679;
        let source_node_id: u64 = 1;
        let protocol_header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x64,
            exchange_id: 0x0eee,
            protocol_id: ProtocolId {
                vendor: 0,
                protocol: 0x7d20,
            },
            ack_counter: None,
        };

        let original = encode_group_secured(
            &op_key,
            group_session_id,
            source_node_id,
            group_id,
            counter,
            &protocol_header,
            &[],
        )
        .unwrap();
        let variant = encode_group_secured_with_privacy_key(
            &op_key,
            &privacy_key,
            group_session_id,
            source_node_id,
            group_id,
            counter,
            &protocol_header,
            &[],
        )
        .unwrap();
        assert_eq!(
            hex_encode(&variant),
            hex_encode(&original),
            "pre-derived-key encode must be byte-identical to the original"
        );
        assert_eq!(
            hex_encode(&variant),
            hex_encode(&hex(
                "067ddb81d926afce24c8a0981bdd44f4e7302b2f915a66c9596290ebe4408217b3c0c921a2fca4e1"
            )),
            "and still byte-match chip's .privacy frame"
        );

        // Decode both ways: same header, same plaintext.
        let (h_original, p_original) = decode_group_secured(&variant, &op_key).unwrap();
        let (h_variant, p_variant) =
            decode_group_secured_with_privacy_key(&variant, &op_key, &privacy_key).unwrap();
        assert_eq!(h_variant, h_original);
        assert_eq!(hex_encode(&p_variant), hex_encode(&p_original));
        assert_eq!(hex_encode(&p_variant), "0164ee0e207d");

        // A WRONG privacy key de-obfuscates the header to garbage, so the AAD
        // no longer matches and the tag check fails — the caller cannot smuggle
        // an unrelated key past the AEAD.
        assert!(
            decode_group_secured_with_privacy_key(&variant, &op_key, &[0xFF; 16]).is_err(),
            "a wrong privacy key must not decode a P=1 frame"
        );

        // ---- matter.js plain vector (P = 0) ----
        // The privacy key is unused on this path, so even a bogus one decodes
        // identically to the deriving original.
        let js_key: [u8; 16] = hex16("4e6f436f6e74726f6c4d61747465724b");
        let js_wire = hex(
            "0601000105000000bebafecaefbeadde34129506e3d4b7267e33e078a21650d79c70db886e5ddfc1398a6ee2212fb5",
        );
        let (h_js_original, p_js_original) = decode_group_secured(&js_wire, &js_key).unwrap();
        let (h_js_variant, p_js_variant) =
            decode_group_secured_with_privacy_key(&js_wire, &js_key, &[0xFF; 16]).unwrap();
        assert_eq!(h_js_variant, h_js_original);
        assert_eq!(hex_encode(&p_js_variant), hex_encode(&p_js_original));
        assert_eq!(
            hex_encode(&p_js_variant),
            "052102d10a0001003501290218",
            "P=0 group plaintext must match the matter.js fixture"
        );
    }

    /// The chip vector's intermediate `.encrypted` frame (P set in the
    /// header/AAD but header NOT yet obfuscated) pins the nonce rule in
    /// isolation: both the AAD and the AEAD nonce byte 0 carry the raw
    /// security flags `0x81`. We reproduce it by AEAD-encrypting exactly as
    /// `encode_group_secured` does and comparing before obfuscation.
    #[test]
    fn group_aead_nonce_carries_privacy_bit() {
        let op_key: [u8; 16] = hex16("ca92d7a0942d1a511a0e26ad074f4c2f");
        let header = SecuredMessageHeader {
            flags: SecuredMessageFlags::SOURCE_PRESENT | SecuredMessageFlags::DEST_GROUP,
            session_id: SessionId(0xdb7d),
            security_flags: SecurityFlags::SESSION_TYPE_GROUP | SecurityFlags::PRIVACY,
            message_counter: MessageCounter(0x1234_5679),
            source_node_id: Some(NodeId(1)),
            destination_node_id: Some(DestNodeId::Group(2)),
        };
        let aad = encode_header(&header);
        assert_eq!(
            hex_encode(&aad),
            "067ddb817956341201000000000000000200",
            "AAD/header carries P (0x81)"
        );
        let nonce = build_nonce(&header, 1);
        assert_eq!(
            hex_encode(&nonce),
            "81795634120100000000000000",
            "nonce byte 0 carries the raw security flags incl. P (0x81)"
        );
        let ct = matter_crypto::aead::encrypt(&op_key, &nonce, &aad, &hex("0164ee0e207d")).unwrap();
        let mut frame = aad;
        frame.extend_from_slice(&ct);
        let expected = hex("067ddb8179563412010000000000000002002b2f915a66c959629\
             0ebe4408217b3c0c921a2fca4e1");
        assert_eq!(
            hex_encode(&frame),
            hex_encode(&expected),
            "pre-privacy frame must byte-match chip's .encrypted fixture"
        );
    }

    /// Loopback: encode a group message then decode it with the SAME
    /// operational group key. Recover the payload, the group id, and the
    /// source node id from the decoded header.
    #[test]
    fn group_encode_decode_roundtrip() {
        use crate::protocol_header::{ExchangeFlags, ProtocolHeader, ProtocolId};

        let key: [u8; 16] = [0xA5; 16];
        let group_session_id: u16 = 0x0007;
        let group_id: u16 = 0xBEEF;
        let counter: u32 = 0x1234_5678;
        let source_node_id: u64 = 0x0011_2233_4455_6677;

        let protocol_header = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x06, // ReportData
            exchange_id: 0x4242,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter: None,
        };
        let app_payload = b"group payload bytes";

        let wire = encode_group_secured(
            &key,
            group_session_id,
            source_node_id,
            group_id,
            counter,
            &protocol_header,
            app_payload,
        )
        .unwrap();

        let (header, plaintext) = decode_group_secured(&wire, &key).unwrap();

        // Header fields recovered.
        assert_eq!(header.session_id, SessionId(group_session_id));
        assert_eq!(header.message_counter, MessageCounter(counter));
        assert_eq!(header.source_node_id, Some(NodeId(source_node_id)));
        assert_eq!(
            header.destination_node_id,
            Some(DestNodeId::Group(group_id))
        );
        assert!(header
            .security_flags
            .contains(SecurityFlags::SESSION_TYPE_GROUP));
        assert!(header.flags.contains(SecuredMessageFlags::DEST_GROUP));
        assert!(header.flags.contains(SecuredMessageFlags::SOURCE_PRESENT));

        // Plaintext = protocol header || app payload. Strip the header back
        // off and confirm the app payload survived the round-trip.
        let (decoded_ph, tail) =
            crate::protocol_header::decode_protocol_header(&plaintext).unwrap();
        assert_eq!(decoded_ph.opcode, 0x06);
        assert_eq!(decoded_ph.exchange_id, 0x4242);
        assert_eq!(decoded_ph.protocol_id, ProtocolId::INTERACTION_MODEL);
        assert_eq!(tail, app_payload);
    }

    /// A group message decoded with the WRONG group key must fail
    /// authentication (the tag will not verify).
    #[test]
    fn group_decode_wrong_key_fails() {
        use crate::protocol_header::{ExchangeFlags, ProtocolHeader, ProtocolId};

        let key: [u8; 16] = [0x11; 16];
        let wrong: [u8; 16] = [0x22; 16];
        let ph = ProtocolHeader {
            exchange_flags: ExchangeFlags::INITIATOR,
            opcode: 0x06,
            exchange_id: 1,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter: None,
        };
        let wire = encode_group_secured(&key, 1, 0xCAFE, 0x1234, 9, &ph, b"x").unwrap();
        let err = decode_group_secured(&wire, &wrong).unwrap_err();
        assert!(matches!(err, Error::DecryptionFailed));
    }

    mod replay_window {
        use super::super::*;

        #[test]
        fn would_reject_does_not_mutate() {
            // Seed the window at 100.
            let mut w = ReplayWindow::new();
            w.check_and_record(100).unwrap();

            // An in-window, not-yet-seen counter: would_reject says Ok and
            // must NOT record it — check_and_record afterwards still succeeds.
            assert!(w.would_reject(95).is_ok());
            assert!(
                w.check_and_record(95).is_ok(),
                "would_reject must not have recorded counter 95",
            );

            // Agreement on a replayed counter (100 was recorded as the seed).
            assert!(matches!(
                w.would_reject(100),
                Err(Error::ReplayedCounter { counter: 100 })
            ));
            assert!(matches!(
                w.check_and_record(100),
                Err(Error::ReplayedCounter { counter: 100 })
            ));

            // Agreement on a too-old counter (the window tracks 68..=99
            // behind the highest, 100).
            assert!(matches!(
                w.would_reject(67),
                Err(Error::CounterTooOld { counter: 67, .. })
            ));
            assert!(matches!(
                w.check_and_record(67),
                Err(Error::CounterTooOld { counter: 67, .. })
            ));

            // Empty-window case: would_reject accepts anything.
            let empty = ReplayWindow::new();
            assert!(empty.would_reject(7).is_ok());
        }

        #[test]
        fn first_counter_accepted() {
            let mut w = ReplayWindow::new();
            assert!(w.check_and_record(100).is_ok());
        }

        #[test]
        fn duplicate_rejected() {
            let mut w = ReplayWindow::new();
            w.check_and_record(100).unwrap();
            let err = w.check_and_record(100).unwrap_err();
            assert!(matches!(err, Error::ReplayedCounter { counter: 100 }));
        }

        #[test]
        fn strictly_increasing_accepted() {
            let mut w = ReplayWindow::new();
            for n in [10u32, 11, 12, 13, 100, 101] {
                w.check_and_record(n).unwrap();
            }
        }

        #[test]
        fn within_window_unseen_accepted() {
            // After seeing 100, counters 68..=99 (the 32-counter window)
            // that we have NOT yet seen must be accepted exactly once each.
            let mut w = ReplayWindow::new();
            w.check_and_record(100).unwrap();
            w.check_and_record(99).unwrap();
            w.check_and_record(98).unwrap();
            // Duplicates now rejected.
            assert!(w.check_and_record(99).is_err());
            assert!(w.check_and_record(98).is_err());
        }

        /// After seeing 100, the window tracks the 32 counters 68..=99
        /// behind it (spec §4.6.5.1, `MSG_COUNTER_WINDOW_SIZE` = 32; chip
        /// `PeerMessageCounter::ClassifyNonFutureCounter`, `offset <= 32`).
        /// An unseen 68, exactly 32 behind, is accepted once; 67 and below
        /// are too old.
        #[test]
        fn outside_window_rejected_as_too_old() {
            let mut w = ReplayWindow::new();
            w.check_and_record(100).unwrap();
            w.check_and_record(68).unwrap();
            assert!(matches!(
                w.check_and_record(68),
                Err(Error::ReplayedCounter { counter: 68 })
            ));
            let err = w.check_and_record(67).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::CounterTooOld {
                        counter: 67,
                        window_low: 68,
                        window_high: 100
                    }
                ),
                "expected CounterTooOld for 67; got {err:?}",
            );
        }

        /// A forward shift of exactly 32 keeps the old highest in the
        /// window, marked seen (chip `CommitWithPosition` shifts and sets
        /// bit `shift - 1`); a shift of 33 pushes it out.
        #[test]
        fn shift_of_exactly_32_keeps_the_old_highest_seen() {
            let mut w = ReplayWindow::new();
            w.check_and_record(1).unwrap();
            w.check_and_record(33).unwrap();
            assert!(matches!(
                w.check_and_record(1),
                Err(Error::ReplayedCounter { counter: 1 })
            ));

            let mut w = ReplayWindow::new();
            w.check_and_record(1).unwrap();
            w.check_and_record(34).unwrap();
            assert!(matches!(
                w.check_and_record(1),
                Err(Error::CounterTooOld { counter: 1, .. })
            ));
        }

        /// Counter 0 is marked seen until it leaves the window: a first
        /// counter of 32 keeps it 32 behind (a duplicate), 33 pushes it out
        /// (too old).
        #[test]
        fn counter_zero_at_the_window_edge() {
            let mut w = ReplayWindow::new();
            w.check_and_record(32).unwrap();
            assert!(matches!(
                w.check_and_record(0),
                Err(Error::ReplayedCounter { counter: 0 })
            ));

            let mut w = ReplayWindow::new();
            w.check_and_record(33).unwrap();
            assert!(matches!(
                w.check_and_record(0),
                Err(Error::CounterTooOld { counter: 0, .. })
            ));
        }

        #[test]
        fn forward_jump_slides_window() {
            // Going from 100 to 200 must accept 200 and forget everything
            // older than (200 - 32) = 168.
            let mut w = ReplayWindow::new();
            w.check_and_record(100).unwrap();
            w.check_and_record(200).unwrap();
            // 100 is now too old to deduplicate against.
            let err = w.check_and_record(100).unwrap_err();
            assert!(matches!(err, Error::CounterTooOld { .. }));
            // 200 is now a duplicate.
            let err = w.check_and_record(200).unwrap_err();
            assert!(matches!(err, Error::ReplayedCounter { counter: 200 }));
        }

        /// A peer's first counter 0 is rejected. Secure-session counters
        /// start at 1 or higher (spec §4.6.1.1), the spec syncs the peer's
        /// `max_message_counter` at 0 (§4.14.1.2, §4.14.2.3), and chip starts the
        /// reception state synced at 0 (`PeerMessageCounter::SetCounter(
        /// kInitialSyncValue)`), so 0 is its max counter: a duplicate.
        #[test]
        fn first_counter_zero_rejected() {
            let mut w = ReplayWindow::new();
            assert!(matches!(
                w.would_reject(0),
                Err(Error::ReplayedCounter { counter: 0 })
            ));
            assert!(matches!(
                w.check_and_record(0),
                Err(Error::ReplayedCounter { counter: 0 })
            ));
            // The rejection recorded nothing: 1 is still the first counter.
            w.check_and_record(1).unwrap();
        }

        /// Counter 0 stays rejected after the window moves, inside the
        /// window (chip marks the old max, 0, as seen when it shifts) and
        /// beyond it.
        #[test]
        fn counter_zero_rejected_after_any_start() {
            let mut w = ReplayWindow::new();
            w.check_and_record(5).unwrap();
            assert!(matches!(
                w.check_and_record(0),
                Err(Error::ReplayedCounter { counter: 0 })
            ));

            let mut w = ReplayWindow::new();
            w.check_and_record(31).unwrap();
            assert!(matches!(
                w.check_and_record(0),
                Err(Error::ReplayedCounter { counter: 0 })
            ));

            let mut w = ReplayWindow::new();
            w.check_and_record(1000).unwrap();
            assert!(matches!(
                w.check_and_record(0),
                Err(Error::CounterTooOld { counter: 0, .. })
            ));
        }

        /// The counters between 0 and a small first counter were never
        /// seen, so each is accepted once, as before (chip's window is
        /// clear when it syncs at 0).
        #[test]
        fn counters_below_a_small_first_counter_accepted() {
            let mut w = ReplayWindow::new();
            w.check_and_record(5).unwrap();
            for n in 1u32..5 {
                w.check_and_record(n).unwrap();
                assert!(w.check_and_record(n).is_err());
            }
        }

        #[test]
        fn first_counter_one_accepted() {
            let mut w = ReplayWindow::new();
            w.check_and_record(1).unwrap();
            assert!(w.check_and_record(1).is_err());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod replay_proptest {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Strictly-increasing counters are always accepted, from any
        /// non-zero start (0 is never a valid secure-session counter).
        #[test]
        fn monotonic_sequence_always_accepted(
            seed in 1u32..,
            len in 1usize..=100,
        ) {
            let mut window = ReplayWindow::new();
            let mut counter = seed;
            for _ in 0..len {
                prop_assert!(window.check_and_record(counter).is_ok());
                counter = counter.wrapping_add(1);
                if counter == 0 {
                    // Wrap-around isn't supported; we'd need re-keying.
                    break;
                }
            }
        }

        /// The window agrees, counter by counter, with a reference model of
        /// chip's `PeerMessageCounter` for a secure unicast session: synced
        /// at 0 (so 0 is seen), a counter is new iff it is ahead of the
        /// highest, or at most 32 behind it and not yet seen. A seen counter
        /// within 32 (the highest included) is a replay; anything further
        /// behind is too old. Steps stay mostly within ±40 of the highest,
        /// with occasional larger jumps.
        #[test]
        fn matches_chip_reference_model(
            steps in proptest::collection::vec(
                prop_oneof![8 => -40i64..=40, 1 => -200i64..=200],
                1..200,
            ),
        ) {
            let mut window = ReplayWindow::new();
            let mut seen = std::collections::BTreeSet::from([0u32]);
            let mut max = 0u32;
            for step in steps {
                let Ok(c) = u32::try_from((i64::from(max) + step).max(0)) else {
                    continue;
                };
                let expected = if c > max {
                    Ok(())
                } else if max - c <= 32 {
                    if seen.contains(&c) { Err("replay") } else { Ok(()) }
                } else {
                    Err("old")
                };
                let got = match window.check_and_record(c) {
                    Ok(()) => Ok(()),
                    Err(Error::ReplayedCounter { .. }) => Err("replay"),
                    Err(Error::CounterTooOld { .. }) => Err("old"),
                    Err(e) => panic!("unexpected error {e:?}"),
                };
                prop_assert_eq!(got, expected, "counter {} with max {}", c, max);
                if expected.is_ok() {
                    seen.insert(c);
                    max = max.max(c);
                }
            }
        }

        /// Any random first counter in the spec's start range [1, 2^28]
        /// is accepted, and 0 is rejected after it.
        #[test]
        fn random_first_counter_accepted(c in 1u32..=0x1000_0000) {
            let mut window = ReplayWindow::new();
            prop_assert!(window.check_and_record(c).is_ok());
            prop_assert!(window.check_and_record(0).is_err());
        }

        /// Whatever counter we record, recording it twice always errors.
        #[test]
        fn idempotent_replay_rejection(c in 1u32..) {
            let mut window = ReplayWindow::new();
            window.check_and_record(c).unwrap();
            prop_assert!(window.check_and_record(c).is_err());
        }
    }
}

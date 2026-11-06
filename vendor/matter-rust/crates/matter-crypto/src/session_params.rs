//! Matter `SessionParameters` — the optional capability structure a peer
//! carries inside session-establishment messages.
//!
//! It appears in PASE (`PBKDFParamRequest` / `PBKDFParamResponse`) and in CASE
//! (`Sigma1`, `Sigma2`, `Sigma2_Resume`). Every field is optional and the whole
//! structure is optional; an absent field means "assume the spec default", not
//! "the peer supports nothing".
//!
//! # Field map
//!
//! Pinned from chip `src/messaging/SessionParameters.h:50-59` (which cites
//! Matter Core Spec §4.12.8 "Parameters and Constants") and cross-checked
//! against matter.js `TlvSessionParameters`:
//!
//! ```text
//! tag  field                       type      units
//! ────────────────────────────────────────────────────
//!  1   SESSION_IDLE_INTERVAL       uint32    ms
//!  2   SESSION_ACTIVE_INTERVAL     uint32    ms
//!  3   SESSION_ACTIVE_THRESHOLD    uint16    ms
//!  4   dataModelRevision           uint16    —
//!  5   interactionModelRevision    uint16    —
//!  6   specificationVersion        uint32    —
//!  7   maxPathsPerInvoke           uint16    —
//! ```
//!
//! Tags 8 (`supportedTransports`) and 9 (`maxTcpMessageSize`) exist in
//! matter.js for the Matter 1.5 TCP transport. We do not implement TCP, so
//! they are **skipped, not rejected** — the same forward-compatibility rule
//! chip applies (`PairingSession.cpp`: *"Future proofing - Don't error out if
//! there are other tags"*).
//!
//! # Container tag numbers differ per message
//!
//! The structure's own context tag is **not** constant:
//!
//! | message         | context tag |
//! |-----------------|-------------|
//! | `Sigma1`        | 5           |
//! | `Sigma2`        | 5           |
//! | `Sigma2_Resume` | **4**       |
//!
//! Both [`decode`](SessionParameters::decode) and
//! [`encode`](SessionParameters::encode) take the tag explicitly so a caller
//! cannot silently use Sigma2's number on a `Sigma2_Resume`.
//!
//! # Why plain integers and not `Duration` / `MrpConfig`
//!
//! `matter-transport` depends on `matter-crypto`, so this crate cannot name
//! `MrpConfig` without creating a cargo cycle. The interval fields therefore
//! stay raw milliseconds, and `matter-transport` owns the conversion (see
//! `MrpConfig::merge_session_params`). This mirrors how `CaseSessionKeys`
//! already hands over pure key material for the transport to wrap.

use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};

use crate::error::{Error, Result};

/// A peer's (or our own) advertised session capabilities.
///
/// Every field is `None` when the peer did not send it. `None` means "assume
/// the spec default", which for the three MRP intervals is 500 ms / 300 ms /
/// 4000 ms — see `MrpConfig::for_peer` in `matter-transport`, which applies
/// exactly those.
///
/// `#[non_exhaustive]`: the structure has grown twice already (tags 6-7 in
/// Matter 1.3, tags 8-9 in 1.5), so additions must stay non-breaking. Build one
/// with [`new`](Self::new) and the chainable setters — a `#[non_exhaustive]`
/// struct cannot be built with literal syntax outside this crate, and
/// `Default::default()` followed by field assignment trips
/// `clippy::field_reassign_with_default` in the caller's own lint run.
///
/// # Example
///
/// ```
/// use matter_crypto::SessionParameters;
///
/// let params = SessionParameters::new()
///     .with_session_idle_interval_ms(500)
///     .with_max_paths_per_invoke(1);
///
/// // Sigma1 carries the structure at context tag 5.
/// let bytes = params.encode(5).expect("encode");
/// assert_eq!(SessionParameters::decode(&bytes).expect("decode"), params);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SessionParameters {
    /// `SESSION_IDLE_INTERVAL` (SII), milliseconds. Context tag 1.
    ///
    /// The peer's MRP retransmit base while it is idle. A sleepy device
    /// advertises a large value here to say "do not hammer me".
    pub session_idle_interval_ms: Option<u32>,
    /// `SESSION_ACTIVE_INTERVAL` (SAI), milliseconds. Context tag 2.
    ///
    /// The peer's MRP retransmit base while it is active.
    pub session_active_interval_ms: Option<u32>,
    /// `SESSION_ACTIVE_THRESHOLD` (SAT), milliseconds. Context tag 3.
    ///
    /// How long the peer stays "active" after receiving traffic.
    pub session_active_threshold_ms: Option<u16>,
    /// Data Model revision the peer is certified against. Context tag 4.
    pub data_model_revision: Option<u16>,
    /// Interaction Model revision the peer implements. Context tag 5.
    pub interaction_model_revision: Option<u16>,
    /// Specification version, e.g. `0x0104_0000` for Matter 1.4.0. Context tag 6.
    ///
    /// chip documents absence as "the peer is older than `0x0103_0000`".
    pub specification_version: Option<u32>,
    /// Maximum command paths the peer accepts in one `InvokeRequest`.
    /// Context tag 7. Absence means 1 (chip's hard default).
    pub max_paths_per_invoke: Option<u16>,
}

impl SessionParameters {
    /// An advertisement with no fields set — nothing is emitted until you add
    /// a field with one of the `with_*` setters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `SESSION_IDLE_INTERVAL` (SII), context tag 1, in milliseconds.
    #[must_use]
    pub fn with_session_idle_interval_ms(mut self, ms: u32) -> Self {
        self.session_idle_interval_ms = Some(ms);
        self
    }

    /// Set `SESSION_ACTIVE_INTERVAL` (SAI), context tag 2, in milliseconds.
    #[must_use]
    pub fn with_session_active_interval_ms(mut self, ms: u32) -> Self {
        self.session_active_interval_ms = Some(ms);
        self
    }

    /// Set `SESSION_ACTIVE_THRESHOLD` (SAT), context tag 3, in milliseconds.
    #[must_use]
    pub fn with_session_active_threshold_ms(mut self, ms: u16) -> Self {
        self.session_active_threshold_ms = Some(ms);
        self
    }

    /// Set the advertised Data Model revision, context tag 4.
    #[must_use]
    pub fn with_data_model_revision(mut self, revision: u16) -> Self {
        self.data_model_revision = Some(revision);
        self
    }

    /// Set the advertised Interaction Model revision, context tag 5.
    ///
    /// This must match the revision the caller actually emits at IM context
    /// tag `0xFF`; advertising one number and sending another is a
    /// self-contradiction a peer may act on.
    #[must_use]
    pub fn with_interaction_model_revision(mut self, revision: u16) -> Self {
        self.interaction_model_revision = Some(revision);
        self
    }

    /// Set the advertised specification version, context tag 6
    /// (e.g. `0x0104_0000` for Matter 1.4.0).
    ///
    /// Claim what the implementation actually supports. Over-claiming invites
    /// a peer to enable behaviour that is then unhandled; under-claiming only
    /// makes the peer down-shift, which is the safe direction.
    #[must_use]
    pub fn with_specification_version(mut self, version: u32) -> Self {
        self.specification_version = Some(version);
        self
    }

    /// Set the advertised maximum command paths per `InvokeRequest`,
    /// context tag 7.
    #[must_use]
    pub fn with_max_paths_per_invoke(mut self, paths: u16) -> Self {
        self.max_paths_per_invoke = Some(paths);
        self
    }

    /// True when no field is set, i.e. encoding this would emit an empty
    /// structure. Callers use it to decide whether to omit the optional
    /// element entirely rather than send `{}`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Decode the captured sub-element bytes of a `SessionParameters`
    /// structure.
    ///
    /// `bytes` must span the element exactly as it appears inside its parent:
    /// the context-tag control byte through the matching end-of-container byte
    /// (`0x18`) inclusive. That is precisely what the CASE and PASE decoders
    /// capture into their raw pass-through buffers.
    ///
    /// Decoding is **order-independent**. chip's decoder walks the tags in
    /// strictly ascending order and stops interpreting at the first gap; ours
    /// matches on each tag as it arrives, which accepts everything chip does
    /// and additionally survives a peer that reorders fields. Unknown tags and
    /// unknown container kinds are skipped.
    ///
    /// Out-of-range integers (a peer sending an SAT above `u16::MAX`, say) are
    /// **saturated to the field's spec width rather than rejected**. These
    /// fields are advisory timing hints; failing an entire handshake over one
    /// non-conformant advisory value would be a worse outcome than clamping,
    /// and `MrpConfig::for_peer` clamps the intervals again on the way in.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if `bytes` is not a single TLV structure
    ///   carrying a context tag.
    /// - [`Error::Codec`] on malformed TLV.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = TlvReader::new(bytes);
        match reader.next()? {
            Some(Element::ContainerStart {
                tag: Tag::Context(_),
                kind: ContainerKind::Structure,
            }) => {}
            _ => return Err(Error::InvalidParameter),
        }

        let mut out = Self::default();
        let mut depth = 0usize;
        loop {
            match reader.next()? {
                // A nested container (tag 8's bitmap in a future revision, or
                // anything we do not model): swallow it whole.
                Some(Element::ContainerStart { .. }) => depth += 1,
                Some(Element::ContainerEnd) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Some(Element::Scalar {
                    tag: Tag::Context(n),
                    value: Value::Uint(v),
                }) if depth == 0 => match n {
                    1 => out.session_idle_interval_ms = Some(saturate_u32(v)),
                    2 => out.session_active_interval_ms = Some(saturate_u32(v)),
                    3 => out.session_active_threshold_ms = Some(saturate_u16(v)),
                    4 => out.data_model_revision = Some(saturate_u16(v)),
                    5 => out.interaction_model_revision = Some(saturate_u16(v)),
                    6 => out.specification_version = Some(saturate_u32(v)),
                    7 => out.max_paths_per_invoke = Some(saturate_u16(v)),
                    // Tags 8/9 and anything later: not modelled, not an error.
                    _ => {}
                },
                // Any other scalar (wrong type for a known tag, or a tag shape
                // we do not model) is ignored for the same forward-compat
                // reason chip states outright.
                Some(_) => {}
                // Ran out of input before the end-of-container byte.
                None => return Err(Error::InvalidParameter),
            }
        }
        Ok(out)
    }

    /// Encode this structure as the sub-element bytes for context tag
    /// `context_tag` — the exact shape [`decode`](Self::decode) accepts, and
    /// the exact shape the CASE/PASE encoders splice into their parent
    /// structure.
    ///
    /// Only the fields that are `Some` are emitted. Field order is ascending
    /// by tag number, matching chip and matter.js, so a byte-parity fixture
    /// comparison is meaningful.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] if the TLV writer fails (only possible on an
    /// allocation failure in the underlying buffer).
    pub fn encode(&self, context_tag: u8) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        {
            let mut w = TlvWriter::new(&mut out);
            w.start_structure(Tag::Context(context_tag))?;
            if let Some(v) = self.session_idle_interval_ms {
                w.put_uint(Tag::Context(1), u64::from(v))?;
            }
            if let Some(v) = self.session_active_interval_ms {
                w.put_uint(Tag::Context(2), u64::from(v))?;
            }
            if let Some(v) = self.session_active_threshold_ms {
                w.put_uint(Tag::Context(3), u64::from(v))?;
            }
            if let Some(v) = self.data_model_revision {
                w.put_uint(Tag::Context(4), u64::from(v))?;
            }
            if let Some(v) = self.interaction_model_revision {
                w.put_uint(Tag::Context(5), u64::from(v))?;
            }
            if let Some(v) = self.specification_version {
                w.put_uint(Tag::Context(6), u64::from(v))?;
            }
            if let Some(v) = self.max_paths_per_invoke {
                w.put_uint(Tag::Context(7), u64::from(v))?;
            }
            w.end_container()?;
        }
        Ok(out)
    }
}

/// Clamp a wire `u64` into the field's `u32` spec width. See
/// [`SessionParameters::decode`] for why this saturates instead of erroring.
fn saturate_u32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Clamp a wire `u64` into the field's `u16` spec width. See
/// [`SessionParameters::decode`] for why this saturates instead of erroring.
fn saturate_u16(v: u64) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    /// Every field set, round-tripped at each of the two container tags the
    /// CASE messages actually use.
    #[test]
    fn all_fields_round_trip_at_both_container_tags() {
        let p = SessionParameters::new()
            .with_session_idle_interval_ms(3300)
            .with_session_active_interval_ms(1100)
            .with_session_active_threshold_ms(4000)
            .with_data_model_revision(19)
            .with_interaction_model_revision(11)
            .with_specification_version(0x0104_0000)
            .with_max_paths_per_invoke(1);

        for tag in [4u8, 5u8] {
            let bytes = p.encode(tag).unwrap();
            assert_eq!(SessionParameters::decode(&bytes).unwrap(), p, "tag {tag}");
        }
    }

    /// An absent field must stay `None` through a round trip — the whole point
    /// of the merge semantics in `MrpConfig::merge_session_params` is that
    /// "not sent" is distinguishable from "sent as the default value".
    #[test]
    fn absent_fields_stay_none() {
        let p = SessionParameters::new().with_session_idle_interval_ms(500);
        let bytes = p.encode(5).unwrap();
        let back = SessionParameters::decode(&bytes).unwrap();
        assert_eq!(back.session_idle_interval_ms, Some(500));
        assert_eq!(back.session_active_interval_ms, None);
        assert_eq!(back.session_active_threshold_ms, None);
        assert_eq!(back.max_paths_per_invoke, None);
    }

    #[test]
    fn empty_structure_decodes_to_all_none() {
        let p = SessionParameters::default();
        assert!(p.is_empty());
        let bytes = p.encode(5).unwrap();
        assert_eq!(SessionParameters::decode(&bytes).unwrap(), p);
    }

    /// Tags 8/9 (matter.js TCP fields) and any future tag must be skipped, not
    /// rejected — including when tag 8 arrives as a nested container.
    #[test]
    fn unknown_tags_are_skipped_not_rejected() {
        let mut raw = Vec::new();
        {
            let mut w = TlvWriter::new(&mut raw);
            w.start_structure(Tag::Context(5)).unwrap();
            w.put_uint(Tag::Context(1), 500).unwrap();
            // Tag 8 as a nested structure (matter.js encodes supportedTransports
            // as a bitmap; a future revision could nest something here).
            w.start_structure(Tag::Context(8)).unwrap();
            w.put_uint(Tag::Context(0), 1).unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(9), 64_000).unwrap();
            w.put_uint(Tag::Context(200), 7).unwrap();
            w.end_container().unwrap();
        }

        let p = SessionParameters::decode(&raw).unwrap();
        assert_eq!(p.session_idle_interval_ms, Some(500));
        assert_eq!(p.max_paths_per_invoke, None);
    }

    /// A nested container's members must not be mistaken for top-level fields.
    /// Tag 3 inside the tag-8 container would otherwise land in SAT.
    #[test]
    fn nested_container_members_do_not_leak_into_top_level_fields() {
        let mut raw = Vec::new();
        {
            let mut w = TlvWriter::new(&mut raw);
            w.start_structure(Tag::Context(5)).unwrap();
            w.start_structure(Tag::Context(8)).unwrap();
            w.put_uint(Tag::Context(3), 9999).unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap();
        }

        let p = SessionParameters::decode(&raw).unwrap();
        assert_eq!(
            p.session_active_threshold_ms, None,
            "a nested tag 3 must not be read as SAT"
        );
    }

    /// Field order must not matter: chip stops interpreting at the first
    /// out-of-order tag, we accept the lot.
    #[test]
    fn fields_decode_in_any_order() {
        let mut raw = Vec::new();
        {
            let mut w = TlvWriter::new(&mut raw);
            w.start_structure(Tag::Context(5)).unwrap();
            w.put_uint(Tag::Context(7), 3).unwrap();
            w.put_uint(Tag::Context(2), 300).unwrap();
            w.put_uint(Tag::Context(1), 500).unwrap();
            w.end_container().unwrap();
        }

        let p = SessionParameters::decode(&raw).unwrap();
        assert_eq!(p.session_idle_interval_ms, Some(500));
        assert_eq!(p.session_active_interval_ms, Some(300));
        assert_eq!(p.max_paths_per_invoke, Some(3));
    }

    /// Out-of-range values saturate rather than failing the whole decode.
    #[test]
    fn out_of_range_values_saturate() {
        let mut raw = Vec::new();
        {
            let mut w = TlvWriter::new(&mut raw);
            w.start_structure(Tag::Context(5)).unwrap();
            w.put_uint(Tag::Context(3), u64::from(u32::MAX)).unwrap();
            w.put_uint(Tag::Context(1), u64::MAX).unwrap();
            w.end_container().unwrap();
        }

        let p = SessionParameters::decode(&raw).unwrap();
        assert_eq!(p.session_active_threshold_ms, Some(u16::MAX));
        assert_eq!(p.session_idle_interval_ms, Some(u32::MAX));
    }

    #[test]
    fn non_structure_input_is_rejected() {
        // A bare scalar, not a structure.
        let mut raw = Vec::new();
        {
            let mut w = TlvWriter::new(&mut raw);
            w.put_uint(Tag::Context(5), 1).unwrap();
        }
        assert!(SessionParameters::decode(&raw).is_err());
        assert!(SessionParameters::decode(&[]).is_err());
    }

    /// A structure that never terminates must be an error, not a silent
    /// partial parse.
    #[test]
    fn truncated_structure_is_rejected() {
        let p = SessionParameters::new().with_session_idle_interval_ms(500);
        let bytes = p.encode(5).unwrap();
        // Drop the trailing end-of-container byte.
        assert!(SessionParameters::decode(&bytes[..bytes.len() - 1]).is_err());
    }
}

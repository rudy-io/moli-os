//! Matter TLV tag forms.
//!
//! All five tag forms from the Matter Core Specification §A.2 are
//! represented here. The wire format has separate 2-byte / 4-byte
//! sub-variants for `CommonProfile`/`ImplicitProfile` and 6-byte /
//! 8-byte sub-variants for `FullyQualified`; the public enum collapses
//! those under a single variant per form, and the writer picks the
//! minimum-width sub-variant from the value.

/// A Matter TLV tag.
///
/// The enum is marked `#[non_exhaustive]` so adding hypothetical future
/// variants is not a breaking change for downstream `match` expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Tag {
    /// No tag bytes follow the control octet.
    Anonymous,

    /// One tag byte follows, carrying the context-specific tag number.
    Context(u8),

    /// A common-profile tag number. The writer emits 2 bytes if the value
    /// fits in `u16`, otherwise 4 bytes.
    CommonProfile(u32),

    /// An implicit-profile tag number. The writer emits 2 bytes if the
    /// value fits in `u16`, otherwise 4 bytes.
    ImplicitProfile(u32),

    /// A fully-qualified tag. Vendor and profile are always 2 bytes each
    /// on the wire; the writer emits 2 bytes for `tag` if it fits in
    /// `u16`, otherwise 4 bytes.
    FullyQualified {
        /// 16-bit vendor identifier.
        vendor: u16,
        /// 16-bit profile identifier within the vendor.
        profile: u16,
        /// Tag number within the profile.
        tag: u32,
    },
}

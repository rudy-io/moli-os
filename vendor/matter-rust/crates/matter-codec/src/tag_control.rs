//! TLV tag-control bits (top 3 bits of the control octet).
//!
//! Defined in the Matter Core Specification §A.2. Crate-internal.

#![allow(dead_code)] // Some forms land in phase 2; keep them defined now.

/// Mask isolating the tag-control bits of a control octet.
pub(crate) const TAG_CONTROL_MASK: u8 = 0b1110_0000;

pub(crate) const ANONYMOUS: u8 = 0b000 << 5;
pub(crate) const CONTEXT: u8 = 0b001 << 5;
pub(crate) const COMMON_PROFILE_2: u8 = 0b010 << 5;
pub(crate) const COMMON_PROFILE_4: u8 = 0b011 << 5;
pub(crate) const IMPLICIT_PROFILE_2: u8 = 0b100 << 5;
pub(crate) const IMPLICIT_PROFILE_4: u8 = 0b101 << 5;
pub(crate) const FULLY_QUALIFIED_6: u8 = 0b110 << 5;
pub(crate) const FULLY_QUALIFIED_8: u8 = 0b111 << 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_is_zero() {
        assert_eq!(ANONYMOUS, 0);
    }

    #[test]
    fn context_is_0x20() {
        assert_eq!(CONTEXT, 0x20);
        assert_eq!(CONTEXT, 0b0010_0000);
    }

    #[test]
    fn mask_round_trips() {
        for high in [
            ANONYMOUS,
            CONTEXT,
            COMMON_PROFILE_2,
            COMMON_PROFILE_4,
            IMPLICIT_PROFILE_2,
            IMPLICIT_PROFILE_4,
            FULLY_QUALIFIED_6,
            FULLY_QUALIFIED_8,
        ] {
            let combined = high | 0x07; // 0x07 is in the element-type range
            assert_eq!(combined & TAG_CONTROL_MASK, high);
        }
    }
}

//! TLV element-type constants (low 5 bits of the control octet).
//!
//! Defined in the Matter Core Specification §A.2. Crate-internal.
//!
//! Naming convention: suffix `<N>` (e.g. `INT8`, `UINT64`, `FLOAT32`) is the
//! bit-width of the Rust primitive the element decodes to, NOT the
//! byte-width of the on-wire encoding. For length-prefixed types
//! (`UTF8_LEN8`, `BYTES_LEN16` …) the suffix is the bit-width of the length
//! field. This matches Rust's primitive naming (`u8`, `u16`, `u32`, `u64`)
//! and avoids the trap where `UINT_8` could be misread as "u8".

#![allow(dead_code)] // Some variants land in later phases; keep them defined now.

// Signed integers. Wire encoding is 1/2/4/8 bytes little-endian.
pub(crate) const INT8: u8 = 0x00;
pub(crate) const INT16: u8 = 0x01;
pub(crate) const INT32: u8 = 0x02;
pub(crate) const INT64: u8 = 0x03;

// Unsigned integers. Wire encoding is 1/2/4/8 bytes little-endian.
pub(crate) const UINT8: u8 = 0x04;
pub(crate) const UINT16: u8 = 0x05;
pub(crate) const UINT32: u8 = 0x06;
pub(crate) const UINT64: u8 = 0x07;

// Booleans encode the value in the type byte itself.
pub(crate) const BOOL_FALSE: u8 = 0x08;
pub(crate) const BOOL_TRUE: u8 = 0x09;

// IEEE 754 floats.
pub(crate) const FLOAT32: u8 = 0x0A;
pub(crate) const FLOAT64: u8 = 0x0B;

// UTF-8 strings. Suffix is the bit-width of the length field.
pub(crate) const UTF8_LEN8: u8 = 0x0C;
pub(crate) const UTF8_LEN16: u8 = 0x0D;
pub(crate) const UTF8_LEN32: u8 = 0x0E;
pub(crate) const UTF8_LEN64: u8 = 0x0F;

// Octet strings. Suffix is the bit-width of the length field.
pub(crate) const BYTES_LEN8: u8 = 0x10;
pub(crate) const BYTES_LEN16: u8 = 0x11;
pub(crate) const BYTES_LEN32: u8 = 0x12;
pub(crate) const BYTES_LEN64: u8 = 0x13;

// Special values and containers.
pub(crate) const NULL: u8 = 0x14;
pub(crate) const STRUCTURE: u8 = 0x15;
pub(crate) const ARRAY: u8 = 0x16;
pub(crate) const LIST: u8 = 0x17;
pub(crate) const END_OF_CONTAINER: u8 = 0x18;

/// Mask isolating the element-type bits of a control octet.
pub(crate) const ELEMENT_TYPE_MASK: u8 = 0b0001_1111;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_value() {
        assert_eq!(ELEMENT_TYPE_MASK, 0x1F);
    }

    #[test]
    fn element_type_codes_are_distinct() {
        let codes = [
            INT8,
            INT16,
            INT32,
            INT64,
            UINT8,
            UINT16,
            UINT32,
            UINT64,
            BOOL_FALSE,
            BOOL_TRUE,
            FLOAT32,
            FLOAT64,
            UTF8_LEN8,
            UTF8_LEN16,
            UTF8_LEN32,
            UTF8_LEN64,
            BYTES_LEN8,
            BYTES_LEN16,
            BYTES_LEN32,
            BYTES_LEN64,
            NULL,
            STRUCTURE,
            ARRAY,
            LIST,
            END_OF_CONTAINER,
        ];
        let mut seen = [false; 256];
        for c in codes {
            assert!(!seen[c as usize], "duplicate element type code 0x{c:02x}");
            seen[c as usize] = true;
            assert!(
                c & !ELEMENT_TYPE_MASK == 0,
                "code 0x{c:02x} bleeds outside element-type bits"
            );
        }
    }
}

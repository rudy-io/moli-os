//! Thread operational dataset validation and Extended PAN ID extraction.
//!
//! Implemented in M9-C2 (Thread commissioning). A Thread operational
//! dataset is **not** Matter TLV — it is Thread's own flat TLV format:
//! each element is `type(1 byte)`, `length(1 byte)`, `value(length
//! bytes)`, walked from offset 0 with no outer container. See the
//! Thread specification's Operational Dataset TLV encoding.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Maximum Thread operational dataset length in bytes, per the Thread
/// specification's Operational Dataset TLV encoding.
const MAX_DATASET_LEN: usize = 254;

/// Thread TLV type for the Extended PAN ID element.
const EXT_PAN_ID_TYPE: u8 = 0x02;

/// Length in bytes of the Extended PAN ID TLV value.
const EXT_PAN_ID_LEN: usize = 8;

/// Errors produced while validating a Thread operational dataset.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ThreadDatasetError {
    /// The dataset was empty.
    #[error("Thread operational dataset is empty")]
    Empty,

    /// The dataset exceeded the Thread spec's maximum length (254 bytes).
    /// Carries the actual (rejected) length.
    #[error("Thread operational dataset is too large: {0} bytes (max 254)")]
    TooLarge(usize),

    /// The bytes are not well-formed Thread TLVs — a TLV's declared
    /// `length` overran the remaining buffer.
    #[error("Thread operational dataset is malformed (truncated TLV)")]
    Malformed,

    /// No Extended PAN ID TLV (type 0x02, length 8) was present.
    #[error("Thread operational dataset has no Extended PAN ID TLV")]
    NoExtPanId,
}

/// A Thread operational dataset (Thread TLV bytes) used to provision a
/// device onto a Thread network. The caller obtains it from a border
/// router (e.g. `ot-ctl dataset active -x`, hex-decoded).
///
/// This is **not** Matter TLV. Thread's operational dataset uses its own
/// flat TLV encoding: each element is `type(1 byte)`, `length(1 byte)`,
/// `value(length bytes)`, walked from offset 0 with no outer container.
///
/// `Debug` is hand-written to redact `bytes` (renders only the length):
/// the dataset contains the Thread Network Key (TLV type `0x04`) and `PSKc`
/// — secrets that must never land in logs via a stray `{:?}`. Mirrors
/// `WiFiCredentials`' redacted `Debug` in `state_machine::commissioner`.
#[derive(Clone, PartialEq, Eq)]
pub struct ThreadDataset {
    bytes: Vec<u8>,
    ext_pan_id: [u8; 8],
}

impl core::fmt::Debug for ThreadDataset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ThreadDataset")
            .field("len", &self.bytes.len())
            .field("ext_pan_id", &format_args!("{:02x?}", self.ext_pan_id))
            .finish()
    }
}

impl ThreadDataset {
    /// Wrap and validate an operational dataset.
    ///
    /// Validates that the dataset is non-empty, within the Thread spec's
    /// 254-byte maximum, consists of well-formed TLVs (every TLV's
    /// declared length fits within the remaining buffer), and contains
    /// an Extended PAN ID TLV (type `0x02`, length 8).
    ///
    /// # Errors
    ///
    /// - [`ThreadDatasetError::Empty`] if `bytes` is empty.
    /// - [`ThreadDatasetError::TooLarge`] if `bytes` exceeds 254 bytes.
    /// - [`ThreadDatasetError::Malformed`] if the bytes are not
    ///   well-formed Thread TLVs (a TLV's length overruns the buffer).
    /// - [`ThreadDatasetError::NoExtPanId`] if no Extended PAN ID TLV
    ///   (type 2, len 8) is present.
    pub fn new(bytes: Vec<u8>) -> Result<Self, ThreadDatasetError> {
        if bytes.is_empty() {
            return Err(ThreadDatasetError::Empty);
        }
        if bytes.len() > MAX_DATASET_LEN {
            return Err(ThreadDatasetError::TooLarge(bytes.len()));
        }

        let mut ext_pan_id = None;
        let mut offset = 0usize;
        while offset < bytes.len() {
            // Every TLV needs at least a type byte and a length byte.
            let Some(&tlv_type) = bytes.get(offset) else {
                return Err(ThreadDatasetError::Malformed);
            };
            let Some(&tlv_len) = bytes.get(offset + 1) else {
                return Err(ThreadDatasetError::Malformed);
            };
            let value_start = offset + 2;
            let value_len = usize::from(tlv_len);
            let value_end = value_start
                .checked_add(value_len)
                .ok_or(ThreadDatasetError::Malformed)?;
            if value_end > bytes.len() {
                return Err(ThreadDatasetError::Malformed);
            }

            if tlv_type == EXT_PAN_ID_TYPE && value_len == EXT_PAN_ID_LEN {
                let mut id = [0u8; EXT_PAN_ID_LEN];
                id.copy_from_slice(&bytes[value_start..value_end]);
                ext_pan_id = Some(id);
            }

            offset = value_end;
        }

        match ext_pan_id {
            Some(ext_pan_id) => Ok(Self { bytes, ext_pan_id }),
            None => Err(ThreadDatasetError::NoExtPanId),
        }
    }

    /// Raw dataset bytes (the opaque octet-string for
    /// `AddOrUpdateThreadNetwork`).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Extended PAN ID (Thread dataset TLV type 2, 8 bytes) — the
    /// `ConnectNetwork` `network_id`.
    ///
    /// Captured once during [`ThreadDataset::new`], which only ever
    /// constructs a value after locating exactly this TLV.
    #[must_use]
    pub fn ext_pan_id(&self) -> [u8; 8] {
        self.ext_pan_id
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Reference dataset from the captured vector
    // (`test-vectors/thread/network_commissioning.json`).
    const DS: &str = "0e08000000000001000000030000184a0300001235060004001fffe002087896217f787f6ebe0708fdec3f34f3cd2020051071dccee3f164f15da92254e0b9c8a3a5030f4f70656e5468726561642d38396437010289d70410dc4b544c7a58671a2ce4f876f5d6dcd90c0402a0f7f8";

    #[test]
    fn parses_and_extracts_ext_pan_id() {
        let d = ThreadDataset::new(hex(DS)).unwrap();
        assert_eq!(
            d.ext_pan_id(),
            [0x78, 0x96, 0x21, 0x7f, 0x78, 0x7f, 0x6e, 0xbe]
        );
        assert_eq!(d.as_bytes(), hex(DS).as_slice());
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            ThreadDataset::new(vec![]),
            Err(ThreadDatasetError::Empty)
        ));
    }

    #[test]
    fn rejects_oversize() {
        assert!(matches!(
            ThreadDataset::new(vec![0u8; 300]),
            Err(ThreadDatasetError::TooLarge(300))
        ));
    }

    #[test]
    fn rejects_truncated_tlv() {
        // type 0x02 claims len 8 but only 2 value bytes follow.
        assert!(matches!(
            ThreadDataset::new(vec![0x02, 0x08, 0x01, 0x02]),
            Err(ThreadDatasetError::Malformed)
        ));
    }

    #[test]
    fn rejects_no_ext_pan_id() {
        // one well-formed TLV (type 3, len 0) but no ext-pan-id.
        assert!(matches!(
            ThreadDataset::new(vec![0x03, 0x00]),
            Err(ThreadDatasetError::NoExtPanId)
        ));
    }

    #[test]
    fn debug_redacts_network_key() {
        // DS (the captured vector) carries a type-0x04 (Network Key)
        // TLV with value dc4b544c7a58671a2ce4f876f5d6dcd9 — a recognizable
        // pattern that must never appear in `{:?}` output.
        let d = ThreadDataset::new(hex(DS)).unwrap();
        let rendered = format!("{d:?}");
        assert!(
            !rendered.contains("dc4b544c7a58671a2ce4f876f5d6dcd9"),
            "Debug must not contain the raw dataset bytes (network key): {rendered}",
        );
        assert!(rendered.contains("ThreadDataset"), "got {rendered}");
        assert!(
            rendered.contains("len"),
            "dataset length should appear: {rendered}"
        );
        // ext_pan_id (78:96:21:7f:78:7f:6e:be per `parses_and_extracts_ext_pan_id`)
        // is not secret and IS expected to appear, rendered by `{:02x?}`.
        assert!(
            rendered.contains("78, 96, 21, 7f, 78, 7f, 6e, be"),
            "ext_pan_id should appear in Debug output: {rendered}"
        );
    }
}

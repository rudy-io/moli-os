//! Matter ECDSA-P256 signature.
//!
//! Matter signatures are 64 bytes raw: `r(32) || s(32)`. This is the
//! format `ring`'s `ECDSA_P256_SHA256_FIXED` algorithm expects; no DER
//! wrapping is needed.

use core::fmt;

use crate::error::{Error, Result};

/// A 64-byte raw ECDSA signature: `r(32) || s(32)`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Signature([u8; 64]);

impl Signature {
    /// Construct from raw bytes.
    #[must_use]
    pub fn new(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// Construct from a byte slice. Rejects wrong-length input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongSignatureLength`] if `slice.len() != 64`.
    pub fn from_slice(slice: &[u8]) -> Result<Self> {
        let bytes: [u8; 64] = slice
            .try_into()
            .map_err(|_| Error::WrongSignatureLength(slice.len()))?;
        Ok(Self(bytes))
    }

    /// The raw 64-byte signature.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature(...)")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test-code carve-out.
mod tests {
    use super::*;

    #[test]
    fn from_slice_rejects_wrong_length() {
        let short = [0u8; 32];
        assert!(matches!(
            Signature::from_slice(&short),
            Err(Error::WrongSignatureLength(32))
        ));
    }

    #[test]
    fn from_slice_accepts_64_bytes() {
        let bytes = [0u8; 64];
        let sig = Signature::from_slice(&bytes).unwrap();
        assert_eq!(sig.as_bytes(), &bytes);
    }
}

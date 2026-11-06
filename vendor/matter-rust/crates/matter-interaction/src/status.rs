//! Interaction Model status codes — Matter Core Spec §8.10 (Status Codes).

#![forbid(unsafe_code)]

use crate::error::ImError;
use crate::{expect_message_struct, skip_container};
use matter_codec::{Element, Tag, TlvReader, Value};

/// Parse a bare `StatusResponseMessage` (`{ 0: Status (u8), 0xFF: IMRev }`) into
/// its status code, or `None` if the message is **not** a bare status response.
///
/// This disambiguates a message-level `StatusResponse` from a `WriteResponse`
/// (whose tag 0 is the `WriteResponses` array) and an `InvokeResponse` (whose tag
/// 0 is the `SuppressResponse` bool): only a `StatusResponse` carries a **scalar
/// uint** at context tag 0. The write/invoke verbs use this to detect a
/// `NEEDS_TIMED_INTERACTION (0xc6)` rejection, which the device returns as a
/// message-level `StatusResponse` rather than a per-path status.
///
/// # Errors
///
/// Returns [`ImError`] if `bytes` is not a valid IM message struct, or the
/// status value exceeds a single octet ([`ImError::InvalidStatusCode`]).
///
/// Both answers require a complete message: end of input before the
/// top-level end-of-container fails with [`ImError::Codec`] wrapping
/// [`matter_codec::Error::UnclosedContainer`], and bytes after it fail with
/// [`matter_codec::Error::TrailingData`] (chip's
/// `MessageParser::ExitContainer` rejects both).
pub fn parse_status_response(bytes: &[u8]) -> Result<Option<u8>, ImError> {
    let mut r = TlvReader::new(bytes);
    expect_message_struct(&mut r)?;
    loop {
        match r.next()? {
            // A bare StatusResponse carries Status as a scalar uint at ctx 0.
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(n),
            }) => {
                let code = u8::try_from(n).map_err(|_| ImError::InvalidStatusCode { code: n })?;
                r.finish()?;
                return Ok(Some(code));
            }
            None => return Err(ImError::Codec(matter_codec::Error::UnclosedContainer)),
            // Not a bare status response: end of message, or tag 0 is a bool
            // (InvokeResponse SuppressResponse) / a container (WriteResponse array).
            Some(
                Element::ContainerEnd
                | Element::Scalar {
                    tag: Tag::Context(0),
                    ..
                }
                | Element::ContainerStart {
                    tag: Tag::Context(0),
                    ..
                },
            ) => {
                r.finish()?;
                return Ok(None);
            }
            Some(Element::ContainerStart { .. }) => skip_container(&mut r)?,
            Some(_) => {}
        }
    }
}

/// An Interaction Model status, as carried by a `StatusIB`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImStatus {
    /// `SUCCESS` (0x00).
    Success,
    /// Any non-zero IM status code (e.g. 0x01 `FAILURE`, 0x86
    /// `UNSUPPORTED_ATTRIBUTE`, 0x88 `INVALID_ACTION`). The raw code is
    /// preserved so callers can log or branch on it.
    Failure(u8),
}

impl ImStatus {
    /// Map a raw IM status byte to [`ImStatus`].
    #[must_use]
    pub fn from_u8(code: u8) -> Self {
        if code == 0x00 {
            Self::Success
        } else {
            Self::Failure(code)
        }
    }

    /// The raw IM status byte: `0x00` for [`Success`](Self::Success), the
    /// preserved code for [`Failure`](Self::Failure). Inverse of [`Self::from_u8`].
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Success => 0x00,
            Self::Failure(code) => code,
        }
    }

    /// `true` iff this is [`ImStatus::Success`].
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    #[test]
    fn maps_success_and_failure_codes() {
        assert_eq!(ImStatus::from_u8(0x00), ImStatus::Success);
        assert!(matches!(ImStatus::from_u8(0x01), ImStatus::Failure(0x01)));
        assert!(matches!(ImStatus::from_u8(0x88), ImStatus::Failure(0x88)));
    }

    #[test]
    fn is_success_only_for_zero() {
        assert!(ImStatus::from_u8(0x00).is_success());
        assert!(!ImStatus::from_u8(0x01).is_success());
    }

    #[test]
    fn parse_status_response_reads_bare_status() {
        let bytes = crate::build_status_response(0xc6);
        assert_eq!(parse_status_response(&bytes).unwrap(), Some(0xc6));
        let ok = crate::build_status_response(0x00);
        assert_eq!(parse_status_response(&ok).unwrap(), Some(0x00));
    }

    #[test]
    fn parse_status_response_none_for_write_response() {
        // A WriteRequest's tag 0 is a bool (SuppressResponse); a WriteResponse's
        // tag 0 is the WriteResponses array. Neither is a bare status response.
        let write = crate::build_write_request(&[]);
        assert_eq!(parse_status_response(&write).unwrap(), None);
    }

    #[test]
    fn parse_status_response_none_for_invoke_request() {
        // InvokeRequest tag 0 is a bool (SuppressResponse) ⇒ not a status response.
        let inv = crate::build_invoke_request(
            crate::CommandPath {
                endpoint: 1,
                cluster: 0x06,
                command: 0x02,
            },
            &[0x15, 0x18],
        );
        assert_eq!(parse_status_response(&inv).unwrap(), None);
    }
}

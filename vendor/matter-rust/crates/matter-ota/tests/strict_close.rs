//! The Provider's command-fields decoders require one complete struct.
//!
//! `QueryImage`, `ApplyUpdateRequest` and `NotifyUpdateApplied` decoding used
//! to treat end of input as the struct's close and ignore bytes after it.
//! `parse_invoke_request` hands these decoders a blob it re-encoded from a
//! closed container, so on the IM path the blob is already well-formed. These
//! functions are public, though, and take arbitrary bytes, so they now make
//! the same check chip's `MessageParser::ExitContainer` makes on the message
//! itself: every strict prefix and any trailing bytes are rejected. The
//! documented errors are checked: `OtaError::Tlv(TrailingData)` for trailing
//! bytes, and `OtaError::Tlv(UnclosedContainer)` for the struct one byte short.

#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use matter_clusters::gen::ota_software_update_provider as prov;
use matter_codec::Error as CodecError;
use matter_ota::{
    handle_apply_update_request, handle_query_image, parse_notify_update_applied, OtaError,
};
use prov::DownloadProtocolEnum;

const SUFFIXES: &[&[u8]] = &[&[0x00], &[0x04, 0x00], &[0x15, 0x18], &[0x18], &[0xFF]];

fn assert_strict(name: &str, full: &[u8], parse: impl Fn(&[u8]) -> Result<(), OtaError>) {
    if let Err(e) = parse(full) {
        panic!("{name}: the full struct must decode, got {e:?}");
    }
    for suffix in SUFFIXES {
        let mut tailed = full.to_vec();
        tailed.extend_from_slice(suffix);
        let got = parse(&tailed);
        assert!(
            matches!(got, Err(OtaError::Tlv(CodecError::TrailingData))),
            "{name}: trailing bytes {suffix:02x?} gave {got:?}, want TrailingData"
        );
    }
    let got = parse(&full[..full.len() - 1]);
    assert!(
        matches!(got, Err(OtaError::Tlv(CodecError::UnclosedContainer))),
        "{name}: missing final end-of-container gave {got:?}, want UnclosedContainer"
    );
    for n in 0..full.len() {
        assert!(
            parse(&full[..n]).is_err(),
            "{name}: prefix of {n} / {} bytes accepted (truncated struct)",
            full.len()
        );
    }
}

#[test]
fn query_image_decoding_is_strict() {
    // The frozen vector from byte_parity.rs: VendorID, ProductID,
    // SoftwareVersion, ProtocolsSupported = [BdxSynchronous]. The array is
    // the nested container whose close used to be implicit at end of input.
    let fields = prov::encode_query_image(
        0xFFF1,
        0x8000,
        1,
        &vec![DownloadProtocolEnum::BdxSynchronous],
        None,
        None,
        None,
        None,
    );
    assert_eq!(hex::encode(&fields), "152500f1ff25010080240201360304001818");
    assert_strict("QueryImage", &fields, |b| {
        handle_query_image(b, None).map(drop)
    });
}

#[test]
fn apply_update_request_decoding_is_strict() {
    let fields = prov::encode_apply_update_request(&vec![0xDE, 0xAD, 0xBE, 0xEF], 2);
    assert_strict("ApplyUpdateRequest", &fields, |b| {
        handle_apply_update_request(b).map(drop)
    });
}

#[test]
fn notify_update_applied_decoding_is_strict() {
    let fields = prov::encode_notify_update_applied(&vec![0xDE, 0xAD, 0xBE, 0xEF], 2);
    assert_strict("NotifyUpdateApplied", &fields, |b| {
        parse_notify_update_applied(b).map(drop)
    });
}

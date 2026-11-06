//! Pure `OtaSoftwareUpdateProvider` (0x0029) command handlers.
//!
//! Each handler takes the **anonymous-tagged command-fields TLV** of an inbound
//! request (as produced by [`matter_interaction::parse_invoke_request`], whose
//! `InvokedCommand::fields_tlv` is already re-anonymised) and returns the
//! anonymous-tagged command-fields TLV of the response — ready to wrap with
//! [`matter_interaction::build_invoke_response_command`]. They contain **no I/O**:
//! the networked provider server (a later F phase) owns the socket, the CASE
//! session, and the BDX transfer; these functions are just the decision logic.
//!
//! ## Why the server codecs are hand-rolled here
//!
//! The `matter-clusters` emitter generated only the **client** direction for
//! 0x0029 — [`encode_query_image`](prov::encode_query_image) (request) and
//! [`QueryImageResponse::decode`](prov::QueryImageResponse) /
//! [`ApplyUpdateResponse::decode`](prov::ApplyUpdateResponse) (responses). A
//! Provider needs the **inverse**: decode the request, encode the response. Those
//! are hand-rolled below over [`matter_codec`]; the generated client codecs serve
//! as the unit-test oracles.

#![forbid(unsafe_code)]

use matter_clusters::gen::ota_software_update_provider as prov;
use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter, Value};
use prov::{ApplyUpdateActionEnum, DownloadProtocolEnum, StatusEnum};

/// Errors surfaced while decoding an OTA request or shaping its response.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OtaError {
    /// The underlying TLV codec rejected the request bytes.
    #[error("TLV codec error: {0}")]
    Tlv(#[from] matter_codec::Error),

    /// An Interaction Model framing operation failed (reserved for the
    /// provider-server wiring in later F phases).
    #[error("interaction model error: {0}")]
    Im(#[from] matter_interaction::ImError),

    /// A generated cluster codec rejected the bytes (reserved for callers that
    /// decode responses with the `matter-clusters` codecs).
    #[error("cluster codec error: {0}")]
    Cluster(#[from] matter_clusters::error::ClusterError),

    /// A required command field was absent from the request.
    #[error("missing required OTA field: {0}")]
    MissingField(&'static str),

    /// The request's command-fields blob was not the expected anonymous struct.
    #[error("unexpected TLV type for OTA field: {0}")]
    UnexpectedType(&'static str),

    /// An integer field did not fit its declared Rust width.
    #[error("value out of range for OTA field: {0}")]
    InvalidLength(&'static str),
}

/// A firmware image the Provider is willing to serve to a Requestor.
///
/// Built by the controller from the `.ota` it intends to push; handed to
/// [`handle_query_image`] to shape an `UpdateAvailable` response. The
/// `image_uri` is the BDX locator the Requestor will open the transfer against —
/// `bdx://<provider-node-id-hex>/<filename>` (the BDX transfer itself lands in a
/// later F phase).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageOffer {
    /// The `SoftwareVersion` of the offered image (must exceed the Requestor's
    /// current version for it to update).
    pub software_version: u32,
    /// Human-readable version string (`SoftwareVersionString`), e.g. `"2.0"`.
    pub software_version_string: String,
    /// The BDX locator the Requestor downloads from (`ImageURI`):
    /// `bdx://<node-hex>/<filename>`.
    pub image_uri: String,
    /// Opaque `UpdateToken` correlating this offer with the later
    /// `ApplyUpdateRequest`/`NotifyUpdateApplied`.
    pub update_token: Vec<u8>,
}

/// Decoded fields of an inbound `QueryImage` request that the Provider acts on.
/// Optional request fields (`HardwareVersion`, `Location`, `RequestorCanConsent`,
/// `MetadataForProvider`) are accepted on the wire but not needed to decide the
/// response, so they are skipped during decode.
struct QueryImageRequest {
    #[allow(dead_code)]
    // decoded for completeness; vendor/product matching is a later-phase concern.
    vendor_id: u16,
    #[allow(dead_code)]
    product_id: u16,
    #[allow(dead_code)]
    software_version: u32,
    protocols_supported: Vec<DownloadProtocolEnum>,
}

/// Decide the `QueryImageResponse` for an inbound `QueryImage`.
///
/// `offer = None` ⇒ `Status = NotAvailable`. With an `offer`, the response is
/// `UpdateAvailable` **iff** the Requestor advertised `BdxSynchronous` in its
/// `ProtocolsSupported` (the only download protocol this Provider serves);
/// otherwise `Status = DownloadProtocolNotSupported`. On `UpdateAvailable` the
/// response carries the offer's `ImageURI`, `SoftwareVersion`,
/// `SoftwareVersionString`, and `UpdateToken`.
///
/// Returns the anonymous-tagged `QueryImageResponse` command-fields TLV.
///
/// # Errors
///
/// Returns [`OtaError`] if `request_fields_tlv` is not a well-formed `QueryImage`
/// command-fields struct (not an anonymous struct, a field of the wrong type, or
/// a required field absent). The struct must be complete: end of input before
/// its end-of-container is [`OtaError::Tlv`] wrapping
/// [`matter_codec::Error::UnclosedContainer`], and bytes after it are
/// [`matter_codec::Error::TrailingData`].
pub fn handle_query_image(
    request_fields_tlv: &[u8],
    offer: Option<&ImageOffer>,
) -> Result<Vec<u8>, OtaError> {
    let req = decode_query_image(request_fields_tlv)?;
    let bdx_supported = req
        .protocols_supported
        .iter()
        .any(|p| matches!(p, DownloadProtocolEnum::BdxSynchronous));

    Ok(match offer {
        None => encode_query_image_response_status(StatusEnum::NotAvailable),
        Some(_) if !bdx_supported => {
            encode_query_image_response_status(StatusEnum::DownloadProtocolNotSupported)
        }
        Some(offer) => encode_query_image_response_available(offer),
    })
}

/// Authorise an inbound `ApplyUpdateRequest`, returning an `ApplyUpdateResponse`
/// with `Action = Proceed` and `DelayedActionTime = 0` (this Provider has no
/// user-consent or staggered-rollout policy yet; both are deferred).
///
/// The request's `UpdateToken`/`NewVersion` are decoded to validate the request
/// is well-formed, then discarded.
///
/// Returns the anonymous-tagged `ApplyUpdateResponse` command-fields TLV.
///
/// # Errors
///
/// Returns [`OtaError`] if `request_fields_tlv` is not a well-formed
/// `ApplyUpdateRequest` command-fields struct. As for [`handle_query_image`],
/// a truncated struct or trailing bytes are rejected.
pub fn handle_apply_update_request(request_fields_tlv: &[u8]) -> Result<Vec<u8>, OtaError> {
    let (_token, _new_version) =
        decode_token_and_version(request_fields_tlv, "ApplyUpdateRequest")?;
    Ok(encode_apply_update_response(
        ApplyUpdateActionEnum::Proceed,
        0,
    ))
}

/// Decode an inbound `NotifyUpdateApplied` request to its `(UpdateToken,
/// SoftwareVersion)`. This command carries **no response payload**; the caller
/// replies with a bare `Success` status (via
/// [`matter_interaction::build_invoke_response_status`]).
///
/// # Errors
///
/// Returns [`OtaError`] if `request_fields_tlv` is not a well-formed
/// `NotifyUpdateApplied` command-fields struct. As for [`handle_query_image`],
/// a truncated struct or trailing bytes are rejected.
pub fn parse_notify_update_applied(request_fields_tlv: &[u8]) -> Result<(Vec<u8>, u32), OtaError> {
    decode_token_and_version(request_fields_tlv, "NotifyUpdateApplied")
}

// --- hand-rolled server-direction codecs over matter-codec -----------------

/// Consume the leading anonymous structure of a command-fields blob, leaving the
/// reader positioned on the first member.
fn expect_anon_struct(r: &mut TlvReader<'_>, ctx: &'static str) -> Result<(), OtaError> {
    match r.next()? {
        Some(Element::ContainerStart {
            kind: ContainerKind::Structure,
            ..
        }) => Ok(()),
        _ => Err(OtaError::UnexpectedType(ctx)),
    }
}

/// Decode a `QueryImage` request's command-fields struct.
fn decode_query_image(fields_tlv: &[u8]) -> Result<QueryImageRequest, OtaError> {
    let mut r = TlvReader::new(fields_tlv);
    expect_anon_struct(&mut r, "QueryImage")?;

    let mut vendor_id = None;
    let mut product_id = None;
    let mut software_version = None;
    let mut protocols_supported = Vec::new();

    loop {
        match r.next()? {
            None => return Err(OtaError::Tlv(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(v),
            }) => {
                vendor_id = Some(
                    u16::try_from(v).map_err(|_| OtaError::InvalidLength("QueryImage.VendorID"))?,
                );
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(v),
            }) => {
                product_id = Some(
                    u16::try_from(v)
                        .map_err(|_| OtaError::InvalidLength("QueryImage.ProductID"))?,
                );
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(v),
            }) => {
                software_version = Some(
                    u32::try_from(v)
                        .map_err(|_| OtaError::InvalidLength("QueryImage.SoftwareVersion"))?,
                );
            }
            Some(Element::ContainerStart {
                tag: Tag::Context(3),
                kind: ContainerKind::Array,
            }) => {
                decode_protocols_supported(&mut r, &mut protocols_supported)?;
            }
            // Optional ctx4..7 (HardwareVersion/Location/RequestorCanConsent/
            // MetadataForProvider) and any future fields: not needed to decide.
            Some(Element::ContainerStart { .. }) => r.skip_container()?,
            Some(_) => {}
        }
    }
    r.finish()?;

    Ok(QueryImageRequest {
        vendor_id: vendor_id.ok_or(OtaError::MissingField("QueryImage.VendorID"))?,
        product_id: product_id.ok_or(OtaError::MissingField("QueryImage.ProductID"))?,
        software_version: software_version
            .ok_or(OtaError::MissingField("QueryImage.SoftwareVersion"))?,
        protocols_supported,
    })
}

/// Read the `ProtocolsSupported` array body (reader positioned after its start)
/// as `DownloadProtocolEnum` discriminants, until the array's end. End of input
/// first is `UnclosedContainer`.
fn decode_protocols_supported(
    r: &mut TlvReader<'_>,
    out: &mut Vec<DownloadProtocolEnum>,
) -> Result<(), OtaError> {
    loop {
        match r.next()? {
            None => return Err(OtaError::Tlv(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => return Ok(()),
            Some(Element::Scalar {
                value: Value::Uint(v),
                ..
            }) => {
                let raw = u8::try_from(v)
                    .map_err(|_| OtaError::InvalidLength("QueryImage.ProtocolsSupported"))?;
                out.push(DownloadProtocolEnum::from_raw(raw));
            }
            Some(Element::ContainerStart { .. }) => r.skip_container()?,
            Some(_) => {}
        }
    }
}

/// Decode a token+version request struct (`ApplyUpdateRequest` and
/// `NotifyUpdateApplied` share this shape: ctx0 `UpdateToken` bytes, ctx1 version
/// u32). `ctx` names the command for error messages.
fn decode_token_and_version(
    fields_tlv: &[u8],
    ctx: &'static str,
) -> Result<(Vec<u8>, u32), OtaError> {
    let mut r = TlvReader::new(fields_tlv);
    expect_anon_struct(&mut r, ctx)?;

    let mut token = None;
    let mut version = None;

    loop {
        match r.next()? {
            None => return Err(OtaError::Tlv(matter_codec::Error::UnclosedContainer)),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Bytes(v),
            }) => token = Some(v),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(v),
            }) => {
                version = Some(u32::try_from(v).map_err(|_| OtaError::InvalidLength("Version"))?);
            }
            Some(Element::ContainerStart { .. }) => r.skip_container()?,
            Some(_) => {}
        }
    }
    r.finish()?;

    Ok((
        token.ok_or(OtaError::MissingField("UpdateToken"))?,
        version.ok_or(OtaError::MissingField("Version"))?,
    ))
}

/// Encode a status-only `QueryImageResponse` (anon struct, ctx0 `Status`).
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
fn encode_query_image_response_status(status: StatusEnum) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(status.to_raw()))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode an `UpdateAvailable` `QueryImageResponse` carrying the offered image
/// (ctx0 `Status=UpdateAvailable`, ctx2 `ImageURI`, ctx3 `SoftwareVersion`,
/// ctx4 `SoftwareVersionString`, ctx5 `UpdateToken`).
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
fn encode_query_image_response_available(offer: &ImageOffer) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(
        Tag::Context(0),
        u64::from(StatusEnum::UpdateAvailable.to_raw()),
    )
    .expect("infallible: vec writer");
    w.put_utf8(Tag::Context(2), &offer.image_uri)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(3), u64::from(offer.software_version))
        .expect("infallible: vec writer");
    w.put_utf8(Tag::Context(4), &offer.software_version_string)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(5), &offer.update_token)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode an `ApplyUpdateResponse` (ctx0 `Action`, ctx1 `DelayedActionTime`).
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
fn encode_apply_update_response(
    action: ApplyUpdateActionEnum,
    delayed_action_time: u32,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(action.to_raw()))
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(1), u64::from(delayed_action_time))
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    fn sample_offer() -> ImageOffer {
        ImageOffer {
            software_version: 2,
            software_version_string: "2.0".into(),
            image_uri: "bdx://0000000000000001/fw.ota".into(),
            update_token: vec![1, 2, 3, 4],
        }
    }

    /// Build a `QueryImage` request with the generated client encoder (the oracle).
    fn query_image(protocols: &[DownloadProtocolEnum]) -> Vec<u8> {
        prov::encode_query_image(
            0xFFF1,
            0x8000,
            1,
            &protocols.to_vec(),
            None,
            None,
            None,
            None,
        )
    }

    #[test]
    fn query_image_with_offer_and_bdx_yields_update_available() {
        let req = query_image(&[DownloadProtocolEnum::BdxSynchronous]);
        let offer = sample_offer();
        let resp = handle_query_image(&req, Some(&offer)).expect("handle");

        let decoded = prov::QueryImageResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.status, StatusEnum::UpdateAvailable);
        assert_eq!(
            decoded.image_uri.as_deref(),
            Some("bdx://0000000000000001/fw.ota")
        );
        assert_eq!(decoded.software_version, Some(2));
        assert_eq!(decoded.software_version_string.as_deref(), Some("2.0"));
        assert_eq!(decoded.update_token.as_deref(), Some(&[1u8, 2, 3, 4][..]));
    }

    #[test]
    fn query_image_without_offer_yields_not_available() {
        let req = query_image(&[DownloadProtocolEnum::BdxSynchronous]);
        let resp = handle_query_image(&req, None).expect("handle");

        let decoded = prov::QueryImageResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.status, StatusEnum::NotAvailable);
        assert_eq!(decoded.image_uri, None);
        assert_eq!(decoded.software_version, None);
    }

    #[test]
    fn query_image_without_bdx_yields_protocol_not_supported() {
        // Requestor offers only HTTPS — which this Provider does not serve.
        let req = query_image(&[DownloadProtocolEnum::Https]);
        let offer = sample_offer();
        let resp = handle_query_image(&req, Some(&offer)).expect("handle");

        let decoded = prov::QueryImageResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.status, StatusEnum::DownloadProtocolNotSupported);
        assert_eq!(decoded.image_uri, None);
    }

    #[test]
    fn apply_update_request_yields_proceed() {
        let req = prov::encode_apply_update_request(&vec![9, 9, 9], 2);
        let resp = handle_apply_update_request(&req).expect("handle");

        let decoded = prov::ApplyUpdateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.action, ApplyUpdateActionEnum::Proceed);
        assert_eq!(decoded.delayed_action_time, 0);
    }

    #[test]
    fn notify_update_applied_parses_token_and_version() {
        let req = prov::encode_notify_update_applied(&vec![7, 7], 3);
        let (token, version) = parse_notify_update_applied(&req).expect("parse");
        assert_eq!(token, vec![7, 7]);
        assert_eq!(version, 3);
    }

    #[test]
    fn query_image_missing_required_field_errors() {
        // An anonymous struct with only VendorID — missing ProductID/SoftwareVersion.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0xFFF1).unwrap();
        w.end_container().unwrap();
        assert!(matches!(
            handle_query_image(&buf, None),
            Err(OtaError::MissingField(_))
        ));
    }
}

//! Matter-specific X.509 DN attribute extraction.
//!
//! Matter Core Spec §6.5.6 reserves OIDs under the CSA arc
//! `1.3.6.1.4.1.37244` for vendor and product identifiers in
//! Distinguished Names. M6.2.1 implements OID matching and value
//! extraction for the two attestation-relevant cases:
//!
//! - [`VendorId`][] OID: `1.3.6.1.4.1.37244.2.1`
//! - [`ProductId`][] OID: `1.3.6.1.4.1.37244.2.2`
//!
//! Values are encoded as 4-character UPPERCASE hex strings of the u16
//! identifier (Matter §6.5.6.1). e.g. VID `0xFFF1` appears as the
//! UTF-8 string `"FFF1"`.
//!
//! Matter §6.2.2.2 also permits `Mvid:`/`Mpid:` substrings in the
//! commonName when neither Matter-specific OID occurs in the DN.

/// A Matter vendor identifier. 16-bit, allocated by the CSA.
///
/// Wraps a `u16` for type-distinct API surface. `Display` formatting
/// emits a 4-character zero-padded UPPERCASE hex string — matches
/// Matter §6.5.6.1's encoding of [`VendorId`] inside cert DNs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VendorId(u16);

impl VendorId {
    /// Wrap a raw `u16` as a `VendorId`.
    ///
    /// `VendorId` and [`ProductId`] are deliberately distinct types
    /// to prevent accidental interchange at API boundaries. The
    /// following must fail to compile:
    ///
    /// ```compile_fail
    /// use matter_commissioning::attestation::{ProductId, VendorId};
    /// fn takes_vid(_: VendorId) {}
    /// takes_vid(ProductId::new(0));
    /// ```
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Return the underlying `u16` value.
    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for VendorId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:04X}", self.0)
    }
}

/// A Matter product identifier. 16-bit, allocated by the vendor.
///
/// Same shape as [`VendorId`] — see its docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProductId(u16);

impl ProductId {
    /// Wrap a raw `u16` as a `ProductId`.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Return the underlying `u16` value.
    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for ProductId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:04X}", self.0)
    }
}

use x509_parser::der_parser::oid::Oid;
use x509_parser::x509::X509Name;

// We use the `oid!` macro from `asn1-rs` (re-exported through
// `x509-parser::der_parser`). It builds an `Oid<'static>` at compile
// time from a dotted-notation literal — no runtime allocation, no
// `unwrap`, no const-fn-availability concerns. Invoked via its
// fully-qualified path to avoid module-vs-macro name collisions with
// the sibling `der_parser::oid` module.

/// Matter [`VendorId`] DN attribute OID: `1.3.6.1.4.1.37244.2.1`.
///
/// Per Matter Core Spec §6.5.6.1, the attribute value is a UTF-8
/// string of 4 UPPERCASE hex characters encoding the u16 VID.
#[rustfmt::skip]
pub(crate) const MATTER_VID_OID: Oid<'static> =
    x509_parser::der_parser::oid!(1.3.6.1.4.1.37244.2.1);

/// Matter [`ProductId`] DN attribute OID: `1.3.6.1.4.1.37244.2.2`.
///
/// Same encoding rules as [`MATTER_VID_OID`].
#[rustfmt::skip]
pub(crate) const MATTER_PID_OID: Oid<'static> =
    x509_parser::der_parser::oid!(1.3.6.1.4.1.37244.2.2);

/// Extract the Matter [`VendorId`] from an X.509 subject DN, if present.
///
/// Returns:
/// - `Ok(Some(vid))` — VID attribute or permitted commonName fallback parsed.
/// - `Ok(None)` — VID absent from the applicable encoding.
/// - `Err(_)` — VID present but its value is malformed
///   (not 4 hex chars, not UTF-8, or hex doesn't fit a u16, or not
///   UPPERCASE per Matter §6.5.6.1).
pub(crate) fn extract_vid(name: &X509Name<'_>) -> Result<Option<VendorId>, MatterDnError> {
    extract_matter_u16(name, &MATTER_VID_OID, "Mvid:").map(|opt| opt.map(VendorId::new))
}

/// Extract the Matter [`ProductId`] from an X.509 subject DN, if present.
///
/// Same semantics as [`extract_vid`].
pub(crate) fn extract_pid(name: &X509Name<'_>) -> Result<Option<ProductId>, MatterDnError> {
    extract_matter_u16(name, &MATTER_PID_OID, "Mpid:").map(|opt| opt.map(ProductId::new))
}

fn extract_matter_u16(
    name: &X509Name<'_>,
    oid: &Oid<'_>,
    prefix: &str,
) -> Result<Option<u16>, MatterDnError> {
    // Presence of EITHER dedicated OID disables fallback for the entire DN;
    // never combine a dedicated VID with a commonName PID (or vice versa).
    if !name
        .iter_attributes()
        .any(|a| a.attr_type() == &MATTER_VID_OID || a.attr_type() == &MATTER_PID_OID)
    {
        let mut malformed_prefix = false;
        for attr in name.iter_common_name() {
            let raw = attr.as_str().map_err(|_| MatterDnError::NonUtf8Value)?;
            match extract_common_name_u16(raw, prefix) {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => {}
                Err(MatterDnError::MalformedFallback) => malformed_prefix = true,
                Err(err) => return Err(err),
            }
        }
        return if malformed_prefix {
            Err(MatterDnError::MalformedFallback)
        } else {
            Ok(None)
        };
    }

    let mut attrs = name.iter_attributes().filter(|a| a.attr_type() == oid);
    let Some(attr) = attrs.next() else {
        return Ok(None);
    };
    if attrs.next().is_some() {
        return Err(MatterDnError::DuplicateAttribute);
    }
    let raw = attr.as_str().map_err(|_| MatterDnError::NonUtf8Value)?;
    parse_matter_u16(raw).map(Some)
}

fn parse_matter_u16(raw: &str) -> Result<u16, MatterDnError> {
    if raw.len() != 4 {
        return Err(MatterDnError::WrongLength { actual: raw.len() });
    }
    let value = u16::from_str_radix(raw, 16).map_err(|_| MatterDnError::NotHex)?;
    // Reject lowercase by re-formatting and comparing — Matter §6.5.6.1
    // is explicit about UPPERCASE.
    let canonical = format!("{value:04X}");
    if raw != canonical {
        return Err(MatterDnError::NotUppercase);
    }
    Ok(value)
}

fn extract_common_name_u16(raw: &str, prefix: &str) -> Result<Option<u16>, MatterDnError> {
    let mut found_prefix = false;
    // Matter §6.2.2.2: use the leftmost correctly encoded match, skipping
    // malformed earlier occurrences. No separator is required after 4 digits.
    // `get` also safely rejects a four-byte slice ending inside UTF-8.
    for (offset, _) in raw.match_indices(prefix) {
        found_prefix = true;
        if let Some(hex) = raw.get(offset + prefix.len()..offset + prefix.len() + 4) {
            if let Ok(value) = parse_matter_u16(hex) {
                return Ok(Some(value));
            }
        }
    }
    if found_prefix {
        Err(MatterDnError::MalformedFallback)
    } else {
        Ok(None)
    }
}

/// Internal error type for Matter DN extraction failures. Wrapped into
/// [`crate::attestation::error::AttestationError::Parse`] by callers.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MatterDnError {
    /// More than one dedicated attribute of the same type is ambiguous.
    #[error("duplicate Matter VID/PID DN attribute")]
    DuplicateAttribute,
    /// A fallback prefix occurs, but no correctly encoded value follows it.
    #[error("Matter commonName VID/PID prefix has no valid uppercase 4-digit hex value")]
    MalformedFallback,
    /// The Matter VID/PID attribute value is not valid UTF-8.
    #[error("Matter VID/PID DN attribute is not valid UTF-8")]
    NonUtf8Value,
    /// The attribute value is the wrong length (must be 4 chars).
    #[error("Matter VID/PID DN attribute is {actual} chars, expected 4")]
    WrongLength {
        /// The actual length we observed.
        actual: usize,
    },
    /// The attribute value contains non-hex characters.
    #[error("Matter VID/PID DN attribute is not hex")]
    NotHex,
    /// The attribute value contains lowercase hex digits (Matter §6.5.6.1 requires UPPERCASE).
    #[error("Matter VID/PID DN attribute must be UPPERCASE hex")]
    NotUppercase,
}

#[cfg(test)]
mod tests {
    use super::{ProductId, VendorId};

    use der::Encode as _;
    use std::str::FromStr as _;
    use x509_cert::name::Name;

    // Synthetic DN fixtures from the encoding rules in Matter §6.2.2.2.
    // Parsing a Name directly avoids generating unrelated keys/certificates.
    #[test]
    #[allow(clippy::expect_used)] // Test fixtures are valid DER Names.
    fn common_name_fallback_obeys_matter_encoding_and_precedence() {
        for (dn, expected) in [
            ("CN=Test Mvid:FFF1 Mpid:8000", (Some(0xFFF1), Some(0x8000))),
            ("CN=Mpid:00B1Mvid:002A", (Some(0x002A), Some(0x00B1))),
            // The next character can also be hex; separators are irrelevant.
            (
                "CN=Mvid:FFF1ACME Mpid:00B19899",
                (Some(0xFFF1), Some(0x00B1)),
            ),
            // Skip malformed occurrences, then use the first valid occurrence.
            (
                "CN=Mpid:Mvid:FFF1 Mpid:12cd Mpid:FE67 Mpid:8000",
                (Some(0xFFF1), Some(0xFE67)),
            ),
            ("CN=PAI Mvid:FFF1", (Some(0xFFF1), None)),
            ("CN=Unscoped PAA", (None, None)),
            ("OU=Mvid:FFF1 Mpid:8000", (None, None)),
            // RFC 4514 lists RDNs in reverse encoded order. Scan every CN,
            // preserving the first valid encoded occurrence for each ID.
            (
                "CN=Mvid:FFF1 Mpid:8000,CN=Device",
                (Some(0xFFF1), Some(0x8000)),
            ),
            ("CN=Mpid:8000,CN=Mvid:FFF1", (Some(0xFFF1), Some(0x8000))),
            (
                "CN=Mvid:FFF2 Mpid:8001,CN=Mvid:FFF1 Mpid:8000",
                (Some(0xFFF1), Some(0x8000)),
            ),
            (
                "CN=Mvid:FFF1 Mpid:8000,CN=Mvid:bad",
                (Some(0xFFF1), Some(0x8000)),
            ),
            (
                "CN=Mvid:bad,CN=Mvid:FFF1 Mpid:8000",
                (Some(0xFFF1), Some(0x8000)),
            ),
            // A single dedicated OID disables fallback for BOTH identifiers.
            (
                "1.3.6.1.4.1.37244.2.1=FFF2,CN=Mvid:FFF1 Mpid:8000",
                (Some(0xFFF2), None),
            ),
            (
                "1.3.6.1.4.1.37244.2.2=8001,CN=Mvid:FFF1 Mpid:8000",
                (None, Some(0x8001)),
            ),
            (
                "1.3.6.1.4.1.37244.2.1=FFF2,CN=Mvid:invalid Mpid:invalid",
                (Some(0xFFF2), None),
            ),
        ] {
            let der = Name::from_str(dn)
                .expect("fixture Name")
                .to_der()
                .expect("Name DER");
            let (_, name) = x509_parser::x509::X509Name::from_der(&der).expect("parse Name");
            assert_eq!(
                super::extract_vid(&name).expect(dn).map(VendorId::as_u16),
                expected.0,
                "{dn}"
            );
            assert_eq!(
                super::extract_pid(&name).expect(dn).map(ProductId::as_u16),
                expected.1,
                "{dn}"
            );
        }
    }

    #[test]
    #[allow(clippy::expect_used)] // Test fixtures are valid DER Names, with invalid IDs.
    fn malformed_ids_do_not_become_absent_or_fall_back() {
        for dn in [
            "CN=Mvid:",
            "CN=Mvid:FFF",
            "CN=Mvid:fff1",
            "CN=Mvid:GGGG",
            "CN=Mvid:\\+123",
            "CN=Mvid:ééé",
            "CN=Device,CN=Mvid:bad",
            "1.3.6.1.4.1.37244.2.1=fff1,CN=Mvid:FFF1",
            "1.3.6.1.4.1.37244.2.1=FFF1,1.3.6.1.4.1.37244.2.1=FFF2",
        ] {
            let der = Name::from_str(dn)
                .expect("fixture Name")
                .to_der()
                .expect("Name DER");
            let (_, name) = x509_parser::x509::X509Name::from_der(&der).expect("parse Name");
            assert!(super::extract_vid(&name).is_err(), "{dn}");
        }
        for dn in ["CN=Mvid:FFF1 Mpid:", "CN=Mvid:FFF1 Mpid:800g"] {
            let der = Name::from_str(dn)
                .expect("fixture Name")
                .to_der()
                .expect("Name DER");
            let (_, name) = x509_parser::x509::X509Name::from_der(&der).expect("parse Name");
            assert!(super::extract_pid(&name).is_err(), "{dn}");
        }
    }

    #[test]
    fn vendor_id_round_trips_through_u16() {
        let v = VendorId::new(0xFFF1);
        assert_eq!(v.as_u16(), 0xFFF1);
    }

    #[test]
    fn vendor_id_display_is_4char_uppercase_hex() {
        assert_eq!(VendorId::new(0x0001).to_string(), "0001");
        assert_eq!(VendorId::new(0xFFF1).to_string(), "FFF1");
        assert_eq!(VendorId::new(0x00AB).to_string(), "00AB");
    }

    #[test]
    fn product_id_round_trips_through_u16() {
        let p = ProductId::new(0x8000);
        assert_eq!(p.as_u16(), 0x8000);
    }

    #[test]
    fn product_id_display_is_4char_uppercase_hex() {
        assert_eq!(ProductId::new(0x0001).to_string(), "0001");
        assert_eq!(ProductId::new(0x8000).to_string(), "8000");
    }

    use x509_parser::prelude::{FromDer, X509Certificate};

    const DAC_DER: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-DAC-FFF1-8000-0004-Cert.der"
    );

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_vid_finds_dac_vid() {
        let (_, cert) = X509Certificate::from_der(DAC_DER).expect("happy-path DAC parses");
        let vid = super::extract_vid(cert.subject())
            .expect("VID well-formed")
            .expect("VID present");
        assert_eq!(vid, super::VendorId::new(0xFFF1));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn extract_pid_finds_dac_pid() {
        let (_, cert) = X509Certificate::from_der(DAC_DER).expect("happy-path DAC parses");
        let pid = super::extract_pid(cert.subject())
            .expect("PID well-formed")
            .expect("PID present");
        assert_eq!(pid, super::ProductId::new(0x8000));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn matter_vid_oid_constant_matches_spec_arc() {
        // Round-trip the OID's components to confirm the const
        // assembles the arc we intended (Matter §6.5.6.1).
        let parts: Vec<u64> = super::MATTER_VID_OID.iter().expect("iterable").collect();
        assert_eq!(parts, vec![1, 3, 6, 1, 4, 1, 37244, 2, 1]);
    }
}

//! Matter distinguished-name (DN) handling.
//!
//! A Matter DN is a TLV list of context-tagged attributes. Each
//! attribute's context tag identifies the attribute kind (per spec
//! §6.5.6 Table 71); the attribute's value type follows from the
//! tag (most are UTF-8 strings; Matter-specific identifiers are
//! unsigned integers).

use matter_codec::{Element, Tag, TlvReader, TlvWriter, Value};

use crate::error::{Error, Result};
use crate::tlv_tags as tags;

/// A single distinguished-name attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DnAttribute {
    // --- Standard X.509 attributes (UTF-8 string-valued) ---
    /// X.509 `CommonName` (CN) attribute — UTF-8 string.
    CommonName(String),
    /// X.509 `Surname` (SN) attribute — UTF-8 string.
    Surname(String),
    /// X.509 `SerialNumber` attribute — UTF-8 string.
    SerialNumber(String),
    /// X.509 `CountryName` (C) attribute — UTF-8 string (2-character ISO 3166).
    CountryName(String),
    /// X.509 `LocalityName` (L) attribute — UTF-8 string.
    LocalityName(String),
    /// X.509 `StateOrProvinceName` (ST) attribute — UTF-8 string.
    StateOrProvinceName(String),
    /// X.509 `OrganizationName` (O) attribute — UTF-8 string.
    OrganizationName(String),
    /// X.509 `OrganizationalUnitName` (OU) attribute — UTF-8 string.
    OrganizationalUnitName(String),
    /// X.509 `Title` attribute — UTF-8 string.
    Title(String),
    /// X.509 `Name` attribute — UTF-8 string.
    Name(String),
    /// X.509 `GivenName` attribute — UTF-8 string.
    GivenName(String),
    /// X.509 `Initials` attribute — UTF-8 string.
    Initials(String),
    /// X.509 `GenerationQualifier` attribute — UTF-8 string.
    GenerationQualifier(String),
    /// X.509 `DnQualifier` attribute — UTF-8 string.
    DnQualifier(String),
    /// X.509 `Pseudonym` attribute — UTF-8 string.
    Pseudonym(String),
    /// X.509 `DomainComponent` (DC) attribute — UTF-8 string.
    DomainComponent(String),

    // --- Matter-specific attributes ---
    /// Matter Node Identifier (operational identity), spec §6.5.6 tag 17.
    NodeId(u64),
    /// Matter Intermediate CA Identifier, spec §6.5.6 tag 19.
    IcacId(u64),
    /// Matter Root CA Identifier, spec §6.5.6 tag 20.
    RcacId(u64),
    /// Matter Fabric Identifier, spec §6.5.6 tag 21.
    FabricId(u64),
    /// Matter CASE Authenticated Tag (NOC-CAT), spec §6.5.6 tag 22.
    CaseAuthenticatedTag(u32),

    /// Matter Vendor Identifier (attestation certificates), CSA OID
    /// `1.3.6.1.4.1.37244.2.1`.
    ///
    /// Used in DAC/PAI/PAA X.509 attestation certificate DNs (Matter
    /// §6.5.6.1), where the value is a 4-character UPPERCASE-hex
    /// `PrintableString` (e.g. `0xFFF1` → `"FFF1"`). This is an X.509-only
    /// attribute: it has no Matter operational-TLV cert encoding (those
    /// certs use the node/fabric/icac/rcac/case-tag identifiers above).
    VendorId(u16),

    /// Matter Product Identifier (attestation certificates), CSA OID
    /// `1.3.6.1.4.1.37244.2.2`.
    ///
    /// Used in DAC/PAI X.509 attestation certificate DNs (Matter
    /// §6.5.6.1), where the value is a 4-character UPPERCASE-hex
    /// `PrintableString` (e.g. `0x8001` → `"8001"`). Like
    /// [`DnAttribute::VendorId`], this is an X.509-only attribute with no
    /// Matter operational-TLV cert encoding.
    ProductId(u16),

    /// Forward-compatibility fallback for spec-defined attributes
    /// the typed variants above don't enumerate yet (e.g., tag 18
    /// matter-firmware-signing-id, tag 23 matter-vvs-id, tags 24-26).
    Other {
        /// Context tag number from the wire.
        tag: u8,
        /// Value decoded from the wire with its TLV type preserved.
        value: DnAttributeValue,
    },
}

/// The wire-typed value carried inside a [`DnAttribute::Other`].
///
/// `#[non_exhaustive]`: TLV admits value types beyond the three surfaced here
/// (e.g. signed integers, booleans); marking this lets a future variant be
/// added without a semver break. Downstream `match`es must include a `_` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DnAttributeValue {
    /// A UTF-8 string value.
    Utf8(String),
    /// An unsigned integer value.
    Uint(u64),
    /// A raw byte-string value.
    Bytes(Vec<u8>),
}

/// An ordered list of DN attributes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DistinguishedName(Vec<DnAttribute>);

impl DistinguishedName {
    /// Construct from a list of attributes (preserves order).
    #[must_use]
    pub fn new(attrs: Vec<DnAttribute>) -> Self {
        Self(attrs)
    }

    /// Iterate the attributes in their wire order.
    pub fn iter(&self) -> core::slice::Iter<'_, DnAttribute> {
        self.0.iter()
    }

    /// First Matter Node Identifier attribute, if present.
    #[must_use]
    pub fn node_id(&self) -> Option<u64> {
        self.0.iter().find_map(|a| match a {
            DnAttribute::NodeId(v) => Some(*v),
            _ => None,
        })
    }

    /// First Matter Fabric Identifier attribute, if present.
    #[must_use]
    pub fn fabric_id(&self) -> Option<u64> {
        self.0.iter().find_map(|a| match a {
            DnAttribute::FabricId(v) => Some(*v),
            _ => None,
        })
    }

    /// First Matter Root CA Identifier attribute, if present.
    #[must_use]
    pub fn rcac_id(&self) -> Option<u64> {
        self.0.iter().find_map(|a| match a {
            DnAttribute::RcacId(v) => Some(*v),
            _ => None,
        })
    }

    /// First Matter Intermediate CA Identifier attribute, if present.
    #[must_use]
    pub fn icac_id(&self) -> Option<u64> {
        self.0.iter().find_map(|a| match a {
            DnAttribute::IcacId(v) => Some(*v),
            _ => None,
        })
    }

    /// First Common Name attribute, if present.
    #[must_use]
    pub fn common_name(&self) -> Option<&str> {
        self.0.iter().find_map(|a| match a {
            DnAttribute::CommonName(v) => Some(v.as_str()),
            _ => None,
        })
    }

    /// Read a DN from a TLV reader positioned BEFORE the DN's
    /// `Element::ContainerStart` event. Consumes the list and its
    /// closing `ContainerEnd`.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLV stream is malformed or contains
    /// invalid DN attributes.
    // Used only in tests — suppress dead_code lint.
    #[allow(dead_code)]
    pub(crate) fn read(reader: &mut TlvReader<'_>) -> Result<Self> {
        match reader.next()? {
            Some(Element::ContainerStart {
                kind: matter_codec::ContainerKind::List,
                ..
            }) => {}
            _ => return Err(Error::WrongFieldType(0)),
        }
        Self::read_from_open_list(reader)
    }

    /// Read a DN from a reader where the `ContainerStart` has already
    /// been consumed. Used by `MatterCertificate::from_tlv` which has
    /// dispatched on the issuer / subject context tag via its own
    /// `next()` call.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLV stream is malformed or contains
    /// invalid DN attributes.
    pub(crate) fn read_from_open_list(reader: &mut TlvReader<'_>) -> Result<Self> {
        let mut attrs = Vec::new();
        loop {
            match reader.next()? {
                None => return Err(matter_codec::Error::UnclosedContainer.into()),
                Some(Element::ContainerEnd) => break,
                Some(Element::Scalar { tag, value }) => {
                    let Tag::Context(tag_num) = tag else {
                        return Err(Error::InvalidDnAttribute(0));
                    };
                    attrs.push(decode_attribute(tag_num, value)?);
                }
                Some(Element::ContainerStart { .. }) => {
                    return Err(Error::WrongFieldType(0));
                }
                // Element is #[non_exhaustive]; future variants are
                // wire-format violations inside a DN list.
                Some(_) => return Err(Error::WrongFieldType(0)),
            }
        }
        Ok(Self(attrs))
    }

    /// Write the DN as a TLV list under `outer_tag`.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] from the underlying writer.
    pub(crate) fn write(&self, writer: &mut TlvWriter<'_>, outer_tag: Tag) -> Result<()> {
        writer.start_list(outer_tag)?;
        for attr in &self.0 {
            encode_attribute(writer, attr)?;
        }
        writer.end_container()?;
        Ok(())
    }
}

impl<'a> IntoIterator for &'a DistinguishedName {
    type Item = &'a DnAttribute;
    type IntoIter = core::slice::Iter<'a, DnAttribute>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Decode a single DN attribute given its context tag and TLV value.
/// `pub(crate)` so `MatterCertificate::from_tlv` can use it for the
/// already-consumed-ContainerStart fast path.
pub(crate) fn decode_attribute(tag: u8, value: Value) -> Result<DnAttribute> {
    use DnAttribute as A;
    match (tag, value) {
        (tags::DN_COMMON_NAME, Value::Utf8(s)) => Ok(A::CommonName(s)),
        (tags::DN_SURNAME, Value::Utf8(s)) => Ok(A::Surname(s)),
        (tags::DN_SERIAL_NUMBER, Value::Utf8(s)) => Ok(A::SerialNumber(s)),
        (tags::DN_COUNTRY_NAME, Value::Utf8(s)) => Ok(A::CountryName(s)),
        (tags::DN_LOCALITY_NAME, Value::Utf8(s)) => Ok(A::LocalityName(s)),
        (tags::DN_STATE_OR_PROVINCE, Value::Utf8(s)) => Ok(A::StateOrProvinceName(s)),
        (tags::DN_ORGANIZATION_NAME, Value::Utf8(s)) => Ok(A::OrganizationName(s)),
        (tags::DN_ORG_UNIT_NAME, Value::Utf8(s)) => Ok(A::OrganizationalUnitName(s)),
        (tags::DN_TITLE, Value::Utf8(s)) => Ok(A::Title(s)),
        (tags::DN_NAME, Value::Utf8(s)) => Ok(A::Name(s)),
        (tags::DN_GIVEN_NAME, Value::Utf8(s)) => Ok(A::GivenName(s)),
        (tags::DN_INITIALS, Value::Utf8(s)) => Ok(A::Initials(s)),
        (tags::DN_GENERATION_QUALIFIER, Value::Utf8(s)) => Ok(A::GenerationQualifier(s)),
        (tags::DN_DN_QUALIFIER, Value::Utf8(s)) => Ok(A::DnQualifier(s)),
        (tags::DN_PSEUDONYM, Value::Utf8(s)) => Ok(A::Pseudonym(s)),
        (tags::DN_DOMAIN_COMPONENT, Value::Utf8(s)) => Ok(A::DomainComponent(s)),
        (tags::DN_MATTER_NODE_ID, Value::Uint(v)) => Ok(A::NodeId(v)),
        (tags::DN_MATTER_ICAC_ID, Value::Uint(v)) => Ok(A::IcacId(v)),
        (tags::DN_MATTER_RCAC_ID, Value::Uint(v)) => Ok(A::RcacId(v)),
        (tags::DN_MATTER_FABRIC_ID, Value::Uint(v)) => Ok(A::FabricId(v)),
        (tags::DN_MATTER_NOC_CAT, Value::Uint(v)) => {
            let v32 = u32::try_from(v).map_err(|_| Error::FieldValueOutOfRange { tag })?;
            Ok(A::CaseAuthenticatedTag(v32))
        }

        // Other variants are spec-defined-but-not-typed (e.g., tag 18,
        // tag 23 matter-vvs-id, tags 24-26). These tags have no explicit
        // typed variant above, so any TLV scalar value type is preserved
        // as-is.
        // is_untyped_dn_tag guards against routing a wrong-typed value
        // for a known tag (e.g., Uint for DN_COMMON_NAME) through here.
        (n, Value::Utf8(s)) if is_untyped_dn_tag(n) => Ok(A::Other {
            tag: n,
            value: DnAttributeValue::Utf8(s),
        }),
        (n, Value::Uint(v)) if is_untyped_dn_tag(n) => Ok(A::Other {
            tag: n,
            value: DnAttributeValue::Uint(v),
        }),
        (n, Value::Bytes(b)) if is_untyped_dn_tag(n) => Ok(A::Other {
            tag: n,
            value: DnAttributeValue::Bytes(b),
        }),

        // Truly unknown tag — wire-format violation.
        (n, _) if !is_dn_tag(n) => Err(Error::InvalidDnAttribute(n)),

        // Known tag with wrong-typed value (e.g., Uint for DN_COMMON_NAME).
        (n, _) => Err(Error::InvalidDnAttributeType(n)),
    }
}

fn encode_attribute(writer: &mut TlvWriter<'_>, attr: &DnAttribute) -> Result<()> {
    use DnAttribute as A;
    match attr {
        A::CommonName(s) => writer.put_utf8(Tag::Context(tags::DN_COMMON_NAME), s)?,
        A::Surname(s) => writer.put_utf8(Tag::Context(tags::DN_SURNAME), s)?,
        A::SerialNumber(s) => writer.put_utf8(Tag::Context(tags::DN_SERIAL_NUMBER), s)?,
        A::CountryName(s) => writer.put_utf8(Tag::Context(tags::DN_COUNTRY_NAME), s)?,
        A::LocalityName(s) => writer.put_utf8(Tag::Context(tags::DN_LOCALITY_NAME), s)?,
        A::StateOrProvinceName(s) => {
            writer.put_utf8(Tag::Context(tags::DN_STATE_OR_PROVINCE), s)?;
        }
        A::OrganizationName(s) => {
            writer.put_utf8(Tag::Context(tags::DN_ORGANIZATION_NAME), s)?;
        }
        A::OrganizationalUnitName(s) => {
            writer.put_utf8(Tag::Context(tags::DN_ORG_UNIT_NAME), s)?;
        }
        A::Title(s) => writer.put_utf8(Tag::Context(tags::DN_TITLE), s)?,
        A::Name(s) => writer.put_utf8(Tag::Context(tags::DN_NAME), s)?,
        A::GivenName(s) => writer.put_utf8(Tag::Context(tags::DN_GIVEN_NAME), s)?,
        A::Initials(s) => writer.put_utf8(Tag::Context(tags::DN_INITIALS), s)?,
        A::GenerationQualifier(s) => {
            writer.put_utf8(Tag::Context(tags::DN_GENERATION_QUALIFIER), s)?;
        }
        A::DnQualifier(s) => writer.put_utf8(Tag::Context(tags::DN_DN_QUALIFIER), s)?,
        A::Pseudonym(s) => writer.put_utf8(Tag::Context(tags::DN_PSEUDONYM), s)?,
        A::DomainComponent(s) => {
            writer.put_utf8(Tag::Context(tags::DN_DOMAIN_COMPONENT), s)?;
        }
        A::NodeId(v) => writer.put_uint(Tag::Context(tags::DN_MATTER_NODE_ID), *v)?,
        A::IcacId(v) => writer.put_uint(Tag::Context(tags::DN_MATTER_ICAC_ID), *v)?,
        A::RcacId(v) => writer.put_uint(Tag::Context(tags::DN_MATTER_RCAC_ID), *v)?,
        A::FabricId(v) => writer.put_uint(Tag::Context(tags::DN_MATTER_FABRIC_ID), *v)?,
        A::CaseAuthenticatedTag(v) => {
            writer.put_uint(Tag::Context(tags::DN_MATTER_NOC_CAT), u64::from(*v))?;
        }
        // VID/PID are X.509-attestation-only DN attributes (DAC/PAI/PAA
        // subject DNs). They have no Matter operational-TLV cert encoding,
        // so refuse rather than invent a context tag.
        A::VendorId(_) => return Err(Error::DnAttributeNotTlvEncodable("VendorId")),
        A::ProductId(_) => return Err(Error::DnAttributeNotTlvEncodable("ProductId")),
        A::Other { tag, value } => match value {
            DnAttributeValue::Utf8(s) => writer.put_utf8(Tag::Context(*tag), s)?,
            DnAttributeValue::Uint(v) => writer.put_uint(Tag::Context(*tag), *v)?,
            DnAttributeValue::Bytes(b) => writer.put_bytes(Tag::Context(*tag), b)?,
        },
    }
    Ok(())
}

/// Whether `tag` is in the spec-defined range for DN attributes
/// (1–26 inclusive). Higher tags are rejected as `InvalidDnAttribute`
/// because the spec does not define them today.
const fn is_dn_tag(tag: u8) -> bool {
    matches!(tag, 1..=26)
}

/// Whether `tag` is a spec-defined DN tag that does NOT have an
/// explicit typed variant in [`DnAttribute`]. These are tags for which
/// we accept any TLV scalar value type and preserve it as
/// [`DnAttribute::Other`].
///
/// Currently: tag 18 (matter-firmware-signing-id), tag 23 (matter-vvs-id,
/// the VID Verification Signer id) and tags 24-26 (assigned by neither chip
/// nor matter.js).
const fn is_untyped_dn_tag(tag: u8) -> bool {
    matches!(tag, 18 | 23..=26)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    fn write_dn(dn: &DistinguishedName) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        dn.write(&mut w, Tag::Anonymous).unwrap();
        buf
    }

    fn read_dn(bytes: &[u8]) -> DistinguishedName {
        let mut r = TlvReader::new(bytes);
        DistinguishedName::read(&mut r).unwrap()
    }

    #[test]
    fn round_trip_common_name() {
        let dn = DistinguishedName::new(vec![DnAttribute::CommonName("CN".into())]);
        let bytes = write_dn(&dn);
        assert_eq!(read_dn(&bytes), dn);
    }

    #[test]
    fn round_trip_matter_node_id() {
        let dn = DistinguishedName::new(vec![DnAttribute::NodeId(0xDEAD_BEEF_CAFE_BABE)]);
        let bytes = write_dn(&dn);
        assert_eq!(read_dn(&bytes), dn);
    }

    #[test]
    fn round_trip_multiple_attributes_preserves_order() {
        let dn = DistinguishedName::new(vec![
            DnAttribute::FabricId(1),
            DnAttribute::NodeId(2),
            DnAttribute::CommonName("device".into()),
        ]);
        let bytes = write_dn(&dn);
        let parsed = read_dn(&bytes);
        assert_eq!(parsed, dn);
        assert!(matches!(
            parsed.iter().next(),
            Some(DnAttribute::FabricId(1))
        ));
    }

    #[test]
    fn round_trip_other_attribute_for_tag_18() {
        let dn = DistinguishedName::new(vec![DnAttribute::Other {
            tag: 18,
            value: DnAttributeValue::Uint(42),
        }]);
        let bytes = write_dn(&dn);
        assert_eq!(read_dn(&bytes), dn);
    }

    #[test]
    fn read_rejects_unknown_dn_tag() {
        let mut buf = Vec::new();
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_list(Tag::Anonymous).unwrap();
            w.put_utf8(Tag::Context(100), "bogus").unwrap();
            w.end_container().unwrap();
        }
        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            DistinguishedName::read(&mut r),
            Err(Error::InvalidDnAttribute(100))
        ));
    }

    #[test]
    fn read_rejects_wrong_type_for_known_tag() {
        let mut buf = Vec::new();
        {
            let mut w = TlvWriter::new(&mut buf);
            w.start_list(Tag::Anonymous).unwrap();
            w.put_uint(Tag::Context(tags::DN_COMMON_NAME), 42).unwrap();
            w.end_container().unwrap();
        }
        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            DistinguishedName::read(&mut r),
            Err(Error::InvalidDnAttributeType(_))
        ));
    }

    #[test]
    fn typed_accessors_return_expected_values() {
        let dn = DistinguishedName::new(vec![
            DnAttribute::FabricId(7),
            DnAttribute::NodeId(42),
            DnAttribute::CommonName("device-007".into()),
        ]);
        assert_eq!(dn.node_id(), Some(42));
        assert_eq!(dn.fabric_id(), Some(7));
        assert_eq!(dn.common_name(), Some("device-007"));
        assert_eq!(dn.rcac_id(), None);
    }
}

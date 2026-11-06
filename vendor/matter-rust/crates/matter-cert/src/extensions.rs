//! Matter certificate extensions.
//!
//! The spec (§6.5.4) defines exactly five extensions: basic-constraints,
//! key-usage, extended-key-usage, subject-key-identifier, authority-
//! key-identifier. Each appears at most once in the extensions list;
//! absent extensions deserialise to `None`. Wire-format unknown
//! extension tags are rejected as `Error::WrongFieldType` because the
//! spec does not permit additions today.

use bitflags::bitflags;
use matter_codec::{Element, Tag, TlvReader, TlvWriter, Value};

use crate::error::{Error, Result};
use crate::tlv_tags as tags;

/// Decoded certificate extensions.
///
/// `#[non_exhaustive]`: the spec (§6.5.4) defines exactly five extensions
/// today, but marking this prevents a future spec-driven addition from being
/// a semver-breaking change for downstream crates. Outside `matter-cert`,
/// construct via [`Extensions::builder`] (or [`Extensions::default`]) rather
/// than a struct literal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Extensions {
    /// Basic constraints extension, if present.
    pub basic_constraints: Option<BasicConstraints>,
    /// Key usage flags, if present.
    pub key_usage: Option<KeyUsage>,
    /// Extended key usage OID list, if present.
    pub extended_key_usage: Option<Vec<u32>>,
    /// Subject key identifier (20-byte SHA-1 of public key), if present.
    pub subject_key_identifier: Option<KeyIdentifier>,
    /// Authority key identifier (20-byte SHA-1 of issuer public key), if present.
    pub authority_key_identifier: Option<KeyIdentifier>,
}

/// Builder for [`Extensions`] (the supported construction path for downstream
/// crates, since [`Extensions`] is `#[non_exhaustive]`).
///
/// Each setter takes the already-wrapped `Option`, mirroring the struct
/// fields one-to-one; unset fields remain `None`.
#[derive(Debug, Clone, Default)]
pub struct ExtensionsBuilder(Extensions);

impl ExtensionsBuilder {
    /// Set the basic-constraints extension.
    #[must_use]
    pub fn basic_constraints(mut self, v: Option<BasicConstraints>) -> Self {
        self.0.basic_constraints = v;
        self
    }

    /// Set the key-usage extension.
    #[must_use]
    pub fn key_usage(mut self, v: Option<KeyUsage>) -> Self {
        self.0.key_usage = v;
        self
    }

    /// Set the extended-key-usage OID list.
    #[must_use]
    pub fn extended_key_usage(mut self, v: Option<Vec<u32>>) -> Self {
        self.0.extended_key_usage = v;
        self
    }

    /// Set the subject-key-identifier extension.
    #[must_use]
    pub fn subject_key_identifier(mut self, v: Option<KeyIdentifier>) -> Self {
        self.0.subject_key_identifier = v;
        self
    }

    /// Set the authority-key-identifier extension.
    #[must_use]
    pub fn authority_key_identifier(mut self, v: Option<KeyIdentifier>) -> Self {
        self.0.authority_key_identifier = v;
        self
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> Extensions {
        self.0
    }
}

/// Basic constraints extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BasicConstraints {
    /// Whether this certificate's subject may sign other certificates.
    pub is_ca: bool,
    /// Maximum number of intermediate certificates that may follow.
    /// `None` means unbounded (or no constraint set).
    pub path_len_constraint: Option<u8>,
}

impl BasicConstraints {
    /// Construct a [`BasicConstraints`] extension.
    ///
    /// Provided because the struct is `#[non_exhaustive]`: callers in other
    /// crates cannot use a struct literal, so this constructor is the stable
    /// way to build one. Any future spec-driven field will gain a default
    /// here without breaking existing callers.
    #[must_use]
    pub const fn new(is_ca: bool, path_len_constraint: Option<u8>) -> Self {
        Self {
            is_ca,
            path_len_constraint,
        }
    }
}

bitflags! {
    /// Matter spec §6.5.4 key-usage bits. Identical layout to X.509
    /// KeyUsage but only the bits the spec defines are surfaced.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct KeyUsage: u16 {
        /// Subject's public key is used for verifying digital signatures.
        const DIGITAL_SIGNATURE  = 0x0001;
        /// Subject's public key is used only for verifying signatures on
        /// certificates and revocation information.
        const CONTENT_COMMITMENT = 0x0002;
        /// Subject's public key is used for enciphering private or secret keys.
        const KEY_ENCIPHERMENT   = 0x0004;
        /// Subject's public key is used for directly enciphering raw user data.
        const DATA_ENCIPHERMENT  = 0x0008;
        /// Subject's public key is used for key agreement.
        const KEY_AGREEMENT      = 0x0010;
        /// Subject's public key is used for verifying signatures on public key
        /// certificates.
        const KEY_CERT_SIGN      = 0x0020;
        /// Subject's public key is used for verifying signatures on certificate
        /// revocation lists.
        const CRL_SIGN           = 0x0040;
        /// When used with `KEY_AGREEMENT`, the subject's public key may only be
        /// used for enciphering data during key agreement.
        const ENCIPHER_ONLY      = 0x0080;
        /// When used with `KEY_AGREEMENT`, the subject's public key may only be
        /// used for deciphering data during key agreement.
        const DECIPHER_ONLY      = 0x0100;
    }
}

/// 20-byte key identifier (Subject Key Identifier or Authority Key
/// Identifier per spec §6.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyIdentifier(pub [u8; 20]);

impl KeyIdentifier {
    /// Construct from a byte slice; rejects wrong-length input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongKeyIdentifierLength`] if `slice` is not exactly
    /// 20 bytes.
    pub fn from_slice(slice: &[u8]) -> Result<Self> {
        let bytes: [u8; 20] = slice
            .try_into()
            .map_err(|_| Error::WrongKeyIdentifierLength(slice.len()))?;
        Ok(Self(bytes))
    }
}

impl Extensions {
    /// Start building an [`Extensions`] value.
    ///
    /// Because [`Extensions`] is `#[non_exhaustive]`, downstream crates cannot
    /// build it with a struct literal; this builder is the supported path.
    /// Unset fields default to `None`.
    #[must_use]
    pub fn builder() -> ExtensionsBuilder {
        ExtensionsBuilder(Self::default())
    }

    /// Read the extensions list (positioned BEFORE its `ContainerStart`).
    ///
    /// # Errors
    ///
    /// Returns an error if the TLV is malformed, a required type is wrong,
    /// a field is duplicated, or an unknown extension tag appears.
    // Used only in tests — suppress dead_code lint.
    #[allow(dead_code)]
    pub(crate) fn read(reader: &mut TlvReader<'_>) -> Result<Self> {
        match reader.next()? {
            Some(Element::ContainerStart {
                kind: matter_codec::ContainerKind::List,
                ..
            }) => {}
            _ => return Err(Error::WrongFieldType(tags::CERT_EXTENSIONS)),
        }
        Self::read_from_open_list(reader)
    }

    /// Read extensions where the `ContainerStart` is already consumed.
    /// Used by `MatterCertificate::from_tlv` which has dispatched on
    /// the extensions context tag via its own `next()` call.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLV is malformed, a required type is wrong,
    /// a field is duplicated, or an unknown extension tag appears.
    pub(crate) fn read_from_open_list(reader: &mut TlvReader<'_>) -> Result<Self> {
        let mut out = Self::default();
        loop {
            match reader.next()? {
                None => return Err(matter_codec::Error::UnclosedContainer.into()),
                Some(Element::ContainerEnd) => break,
                Some(Element::ContainerStart {
                    tag: Tag::Context(t),
                    kind: matter_codec::ContainerKind::Structure,
                }) if t == tags::EXT_BASIC_CONSTRAINTS => {
                    if out.basic_constraints.is_some() {
                        return Err(Error::DuplicateField(t));
                    }
                    out.basic_constraints = Some(BasicConstraints::read_body(reader)?);
                }
                Some(Element::Scalar {
                    tag: Tag::Context(t),
                    value: Value::Uint(bits),
                }) if t == tags::EXT_KEY_USAGE => {
                    if out.key_usage.is_some() {
                        return Err(Error::DuplicateField(t));
                    }
                    let bits16 =
                        u16::try_from(bits).map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                    out.key_usage = Some(KeyUsage::from_bits_truncate(bits16));
                }
                Some(Element::ContainerStart {
                    tag: Tag::Context(t),
                    kind: matter_codec::ContainerKind::Array,
                }) if t == tags::EXT_EXTENDED_KEY_USAGE => {
                    if out.extended_key_usage.is_some() {
                        return Err(Error::DuplicateField(t));
                    }
                    out.extended_key_usage = Some(read_uint_array(reader)?);
                }
                Some(Element::Scalar {
                    tag: Tag::Context(t),
                    value: Value::Bytes(b),
                }) if t == tags::EXT_SUBJECT_KEY_IDENTIFIER => {
                    if out.subject_key_identifier.is_some() {
                        return Err(Error::DuplicateField(t));
                    }
                    out.subject_key_identifier = Some(KeyIdentifier::from_slice(&b)?);
                }
                Some(Element::Scalar {
                    tag: Tag::Context(t),
                    value: Value::Bytes(b),
                }) if t == tags::EXT_AUTHORITY_KEY_IDENTIFIER => {
                    if out.authority_key_identifier.is_some() {
                        return Err(Error::DuplicateField(t));
                    }
                    out.authority_key_identifier = Some(KeyIdentifier::from_slice(&b)?);
                }
                Some(elem) => {
                    let tag_num = element_tag_number(&elem);
                    return Err(Error::WrongFieldType(tag_num));
                }
            }
        }
        Ok(out)
    }

    /// Write the extensions list under `outer_tag`.
    ///
    /// # Errors
    ///
    /// Propagates any [`matter_codec::Error`] from the underlying writer.
    pub(crate) fn write(&self, writer: &mut TlvWriter<'_>, outer_tag: Tag) -> Result<()> {
        writer.start_list(outer_tag)?;
        if let Some(bc) = self.basic_constraints {
            bc.write_body(writer, Tag::Context(tags::EXT_BASIC_CONSTRAINTS))?;
        }
        if let Some(ku) = self.key_usage {
            writer.put_uint(Tag::Context(tags::EXT_KEY_USAGE), u64::from(ku.bits()))?;
        }
        if let Some(eku) = &self.extended_key_usage {
            writer.start_array(Tag::Context(tags::EXT_EXTENDED_KEY_USAGE))?;
            for oid in eku {
                writer.put_uint(Tag::Anonymous, u64::from(*oid))?;
            }
            writer.end_container()?;
        }
        if let Some(ski) = &self.subject_key_identifier {
            writer.put_bytes(Tag::Context(tags::EXT_SUBJECT_KEY_IDENTIFIER), &ski.0)?;
        }
        if let Some(aki) = &self.authority_key_identifier {
            writer.put_bytes(Tag::Context(tags::EXT_AUTHORITY_KEY_IDENTIFIER), &aki.0)?;
        }
        writer.end_container()?;
        Ok(())
    }
}

impl BasicConstraints {
    fn read_body(reader: &mut TlvReader<'_>) -> Result<Self> {
        let mut is_ca = false;
        let mut path_len = None;
        loop {
            match reader.next()? {
                None => return Err(matter_codec::Error::UnclosedContainer.into()),
                Some(Element::ContainerEnd) => break,
                Some(Element::Scalar {
                    tag: Tag::Context(t),
                    value: Value::Bool(b),
                }) if t == tags::BC_IS_CA => {
                    is_ca = b;
                }
                Some(Element::Scalar {
                    tag: Tag::Context(t),
                    value: Value::Uint(v),
                }) if t == tags::BC_PATH_LEN_CONSTRAINT => {
                    let v8 = u8::try_from(v).map_err(|_| Error::FieldValueOutOfRange { tag: t })?;
                    path_len = Some(v8);
                }
                Some(_) => {
                    return Err(Error::WrongFieldType(tags::EXT_BASIC_CONSTRAINTS));
                }
            }
        }
        Ok(Self {
            is_ca,
            path_len_constraint: path_len,
        })
    }

    // `BasicConstraints` is `Copy` (bool + Option<u8>); take by value.
    fn write_body(self, writer: &mut TlvWriter<'_>, outer_tag: Tag) -> Result<()> {
        writer.start_structure(outer_tag)?;
        writer.put_bool(Tag::Context(tags::BC_IS_CA), self.is_ca)?;
        if let Some(plc) = self.path_len_constraint {
            writer.put_uint(Tag::Context(tags::BC_PATH_LEN_CONSTRAINT), u64::from(plc))?;
        }
        writer.end_container()?;
        Ok(())
    }
}

// Used by Extensions::read_from_open_list.
fn read_uint_array(reader: &mut TlvReader<'_>) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    loop {
        match reader.next()? {
            None => return Err(matter_codec::Error::UnclosedContainer.into()),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                value: Value::Uint(v),
                ..
            }) => {
                let v32 = u32::try_from(v).map_err(|_| Error::FieldValueOutOfRange {
                    tag: tags::EXT_EXTENDED_KEY_USAGE,
                })?;
                out.push(v32);
            }
            Some(_) => return Err(Error::WrongFieldType(tags::EXT_EXTENDED_KEY_USAGE)),
        }
    }
    Ok(out)
}

// Used by Extensions::read_from_open_list.
fn element_tag_number(elem: &Element) -> u8 {
    match elem {
        Element::Scalar { tag, .. } | Element::ContainerStart { tag, .. } => match tag {
            Tag::Context(n) => *n,
            _ => 0,
        },
        // ContainerEnd and any future non_exhaustive variants: return 0.
        _ => 0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test-code carve-out.
mod tests {
    use super::*;

    fn round_trip(ext: &Extensions) {
        let mut buf = Vec::new();
        {
            let mut w = TlvWriter::new(&mut buf);
            ext.write(&mut w, Tag::Anonymous).unwrap();
        }
        let mut r = TlvReader::new(&buf);
        let parsed = Extensions::read(&mut r).unwrap();
        assert_eq!(parsed, *ext);
    }

    #[test]
    fn round_trip_empty() {
        round_trip(&Extensions::default());
    }

    #[test]
    fn round_trip_basic_constraints_only() {
        round_trip(&Extensions {
            basic_constraints: Some(BasicConstraints {
                is_ca: true,
                path_len_constraint: Some(3),
            }),
            ..Default::default()
        });
    }

    #[test]
    fn round_trip_key_usage_only() {
        round_trip(&Extensions {
            key_usage: Some(KeyUsage::DIGITAL_SIGNATURE | KeyUsage::KEY_CERT_SIGN),
            ..Default::default()
        });
    }

    #[test]
    fn round_trip_extended_key_usage_only() {
        round_trip(&Extensions {
            extended_key_usage: Some(vec![1, 2, 3]),
            ..Default::default()
        });
    }

    #[test]
    fn round_trip_key_identifiers() {
        round_trip(&Extensions {
            subject_key_identifier: Some(KeyIdentifier([0xAB; 20])),
            authority_key_identifier: Some(KeyIdentifier([0xCD; 20])),
            ..Default::default()
        });
    }

    #[test]
    fn round_trip_all_extensions() {
        round_trip(&Extensions {
            basic_constraints: Some(BasicConstraints {
                is_ca: true,
                path_len_constraint: None,
            }),
            key_usage: Some(KeyUsage::DIGITAL_SIGNATURE),
            extended_key_usage: Some(vec![42]),
            subject_key_identifier: Some(KeyIdentifier([0x11; 20])),
            authority_key_identifier: Some(KeyIdentifier([0x22; 20])),
        });
    }
}

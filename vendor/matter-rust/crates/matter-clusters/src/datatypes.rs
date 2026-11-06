//! Hand-written Matter *global* datatypes referenced by generated cluster
//! code but defined outside any single cluster.

use crate::error::ClusterError;
use crate::types::Nullable;
use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};

/// A semantic tag (`semtag` global, Matter spec §7.19.2) — e.g. the entries
/// of `Descriptor.TagList`.
///
/// Fields per the spec: `MfgCode` (0, nullable vendor-id), `NamespaceID`
/// (1, enum8), `Tag` (2, enum8), `Label` (3, optional nullable string).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SemanticTagStruct {
    /// Manufacturer code (`null` for standard namespaces).
    pub mfg_code: Nullable<u16>,
    /// Namespace identifier.
    pub namespace_id: u8,
    /// Tag within the namespace.
    pub tag: u8,
    /// Optional human-readable label.
    pub label: Option<Nullable<String>>,
}

impl SemanticTagStruct {
    /// Decode the fields of an already-opened anonymous structure (reader
    /// positioned after the struct start; consumes to its matching end).
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] on a malformed structure or missing required
    /// field.
    pub fn decode_from(r: &mut TlvReader<'_>) -> Result<Self, ClusterError> {
        let mut mfg_code: Option<Nullable<u16>> = None;
        let mut namespace_id: Option<u8> = None;
        let mut tag: Option<u8> = None;
        let mut label: Option<Nullable<String>> = None;
        loop {
            match r.next()? {
                Some(Element::ContainerEnd) => break,
                Some(Element::Scalar {
                    tag: Tag::Context(0),
                    value: Value::Null,
                }) => {
                    mfg_code = Some(Nullable::Null);
                }
                Some(Element::Scalar {
                    tag: Tag::Context(0),
                    value: Value::Uint(v),
                }) => {
                    mfg_code =
                        Some(Nullable::Value(u16::try_from(v).map_err(|_| {
                            ClusterError::InvalidLength("SemanticTag.MfgCode")
                        })?));
                }
                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Uint(v),
                }) => {
                    namespace_id = Some(
                        u8::try_from(v)
                            .map_err(|_| ClusterError::InvalidLength("SemanticTag.NamespaceID"))?,
                    );
                }
                Some(Element::Scalar {
                    tag: Tag::Context(2),
                    value: Value::Uint(v),
                }) => {
                    tag = Some(
                        u8::try_from(v)
                            .map_err(|_| ClusterError::InvalidLength("SemanticTag.Tag"))?,
                    );
                }
                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Null,
                }) => {
                    label = Some(Nullable::Null);
                }
                Some(Element::Scalar {
                    tag: Tag::Context(3),
                    value: Value::Utf8(s),
                }) => {
                    label = Some(Nullable::Value(s));
                }
                None => return Err(ClusterError::Tlv(matter_codec::Error::UnclosedContainer)),
                Some(Element::ContainerStart { .. }) => r.skip_container()?,
                Some(_) => {}
            }
        }
        Ok(Self {
            mfg_code: mfg_code.ok_or(ClusterError::MissingField("SemanticTag.MfgCode"))?,
            namespace_id: namespace_id
                .ok_or(ClusterError::MissingField("SemanticTag.NamespaceID"))?,
            tag: tag.ok_or(ClusterError::MissingField("SemanticTag.Tag"))?,
            label,
        })
    }

    /// Decode from a standalone anonymous TLV structure.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] if the bytes are not an anonymous structure or
    /// a field is malformed.
    pub fn decode(tlv: &[u8]) -> Result<Self, ClusterError> {
        let mut r = TlvReader::new(tlv);
        match r.next()? {
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            }) => {}
            _ => {
                return Err(ClusterError::UnexpectedType {
                    context: "SemanticTagStruct",
                })
            }
        }
        Self::decode_from(&mut r)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use matter_codec::TlvWriter;

    #[test]
    fn decodes_a_minimal_tag() {
        // { MfgCode(0)=null, NamespaceID(1)=7, Tag(2)=3 }
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_null(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(1), 7).unwrap();
        w.put_uint(Tag::Context(2), 3).unwrap();
        w.end_container().unwrap();
        let t = SemanticTagStruct::decode(&buf).unwrap();
        assert_eq!(
            t,
            SemanticTagStruct {
                mfg_code: Nullable::Null,
                namespace_id: 7,
                tag: 3,
                label: None
            }
        );
    }
}

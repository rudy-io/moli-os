//! Concrete IM paths: `CommandPathIB` and `AttributePathIB` — Matter
//! Appendix A.6.

#![forbid(unsafe_code)]

use crate::error::ImError;
use matter_codec::{Element, Tag, TlvReader, Value};

/// A concrete command path: `(endpoint, cluster, command)`.
///
/// Encoded as a `CommandPathIB` TLV **list** (Matter Appendix A.6):
/// context tag 0 = endpoint, 1 = cluster, 2 = command.
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandPath {
    /// Matter endpoint (always 0 for commissioning).
    pub endpoint: u16,
    /// Cluster ID.
    pub cluster: u32,
    /// Command ID.
    pub command: u32,
}

/// A concrete attribute path: `(endpoint, cluster, attribute)`.
///
/// Encoded as an `AttributePathIB` TLV **list** (Matter Appendix A.6):
/// context tag 2 = endpoint, 3 = cluster, 4 = attribute. Commissioning
/// reads only concrete attributes, so no wildcard/list-index fields are
/// emitted.
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttributePath {
    /// Matter endpoint.
    pub endpoint: u16,
    /// Cluster ID.
    pub cluster: u32,
    /// Attribute ID.
    pub attribute: u32,
}

/// A read-request attribute path with optional (wildcard) components. A `None`
/// field is **omitted** from the encoded `AttributePathIB`, which the Matter IM
/// interprets as a wildcard (Appendix A.6): omit `attribute` → all attributes of
/// the cluster; omit `endpoint` → all endpoints; etc. Responses are always keyed
/// by a concrete [`AttributePath`].
///
/// `#[non_exhaustive]`: a read/subscribe path may gain optional spec components
/// (e.g. a data-version filter); marking it keeps such additions non-breaking.
/// Build via [`ReadPath::concrete`] / [`ReadPath::cluster`] / [`ReadPath::all`]
/// / [`ReadPath::new`].
#[derive(Copy, Clone, Debug, Eq, Default, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct ReadPath {
    /// Endpoint, or `None` for all endpoints.
    pub endpoint: Option<u16>,
    /// Cluster, or `None` for all clusters.
    pub cluster: Option<u32>,
    /// Attribute, or `None` for all attributes.
    pub attribute: Option<u32>,
}

impl ReadPath {
    /// A read path from raw optional components (a `None` component is a
    /// wildcard). Prefer [`Self::concrete`] / [`Self::cluster`] / [`Self::all`]
    /// for the common shapes.
    #[must_use]
    pub const fn new(endpoint: Option<u16>, cluster: Option<u32>, attribute: Option<u32>) -> Self {
        Self {
            endpoint,
            cluster,
            attribute,
        }
    }

    /// A concrete `(endpoint, cluster, attribute)` path (no wildcards).
    #[must_use]
    pub const fn concrete(endpoint: u16, cluster: u32, attribute: u32) -> Self {
        Self {
            endpoint: Some(endpoint),
            cluster: Some(cluster),
            attribute: Some(attribute),
        }
    }

    /// All attributes of `cluster` on `endpoint`.
    #[must_use]
    pub const fn cluster(endpoint: u16, cluster: u32) -> Self {
        Self {
            endpoint: Some(endpoint),
            cluster: Some(cluster),
            attribute: None,
        }
    }

    /// Every attribute on every endpoint/cluster (full wildcard).
    #[must_use]
    pub const fn all() -> Self {
        Self {
            endpoint: None,
            cluster: None,
            attribute: None,
        }
    }
}

impl From<AttributePath> for ReadPath {
    fn from(p: AttributePath) -> Self {
        Self {
            endpoint: Some(p.endpoint),
            cluster: Some(p.cluster),
            attribute: Some(p.attribute),
        }
    }
}

/// Consume an `AttributePathIB` list body (reader positioned just after the
/// list's `ContainerStart`) into an [`AttributePath`], without materialising
/// the members. The `bool` reports a `ListIndex` (context tag 5) equal to
/// `null`, which in a `ReportData` signals a list **append** (Matter
/// §10.6.4). Out-of-range values surface as [`ImError::UnexpectedValue`].
pub(crate) fn attribute_path_from_reader(
    r: &mut TlvReader<'_>,
) -> Result<(AttributePath, bool), ImError> {
    let mut endpoint = None;
    let mut cluster = None;
    let mut attribute = None;
    let mut append = false;
    loop {
        match r.next()? {
            None => {
                return Err(ImError::Codec(matter_codec::Error::UnclosedContainer));
            }
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(n),
            }) => {
                endpoint =
                    Some(u16::try_from(n).map_err(|_| {
                        ImError::UnexpectedValue("AttributePath.endpoint exceeds u16")
                    })?);
            }
            Some(Element::Scalar {
                tag: Tag::Context(3),
                value: Value::Uint(n),
            }) => {
                cluster =
                    Some(u32::try_from(n).map_err(|_| {
                        ImError::UnexpectedValue("AttributePath.cluster exceeds u32")
                    })?);
            }
            Some(Element::Scalar {
                tag: Tag::Context(4),
                value: Value::Uint(n),
            }) => {
                attribute = Some(u32::try_from(n).map_err(|_| {
                    ImError::UnexpectedValue("AttributePath.attribute exceeds u32")
                })?);
            }
            Some(Element::Scalar {
                tag: Tag::Context(5),
                value: Value::Null,
            }) => append = true,
            Some(Element::ContainerStart { .. }) => crate::skip_container(r)?,
            Some(_) => {}
        }
    }
    Ok((
        AttributePath {
            endpoint: endpoint.ok_or(ImError::MissingField("AttributePath.endpoint"))?,
            cluster: cluster.ok_or(ImError::MissingField("AttributePath.cluster"))?,
            attribute: attribute.ok_or(ImError::MissingField("AttributePath.attribute"))?,
        },
        append,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    // `Element` / `TlvReader` / `Tag` arrive via `use super::*` (the module's
    // own matter_codec import); only the writer is extra here.
    use matter_codec::TlvWriter;

    /// Drive `attribute_path_from_reader` over a writer-built `AttributePathIB`.
    fn parse(build: impl FnOnce(&mut TlvWriter<'_>)) -> Result<(AttributePath, bool), ImError> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_list(Tag::Anonymous).unwrap();
        build(&mut w);
        w.end_container().unwrap();
        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        attribute_path_from_reader(&mut r)
    }

    #[test]
    fn streaming_path_parse_matches_member_semantics() {
        // Normal path + ListIndex null append marker.
        let (p, append) = parse(|w| {
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.put_uint(Tag::Context(3), 0x0006).unwrap();
            w.put_uint(Tag::Context(4), 0xFFFC).unwrap();
            w.put_null(Tag::Context(5)).unwrap();
        })
        .unwrap();
        assert_eq!((p.endpoint, p.cluster, p.attribute), (1, 0x0006, 0xFFFC));
        assert!(append);

        // Duplicate tag: last wins (parity with the member-vec iteration).
        let (p, _) = parse(|w| {
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.put_uint(Tag::Context(2), 2).unwrap();
            w.put_uint(Tag::Context(3), 6).unwrap();
            w.put_uint(Tag::Context(4), 0).unwrap();
        })
        .unwrap();
        assert_eq!(p.endpoint, 2);

        // Unknown nested container inside the path list is skipped.
        let (p, append) = parse(|w| {
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.start_structure(Tag::Context(9)).unwrap();
            w.put_uint(Tag::Context(0), 7).unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(3), 6).unwrap();
            w.put_uint(Tag::Context(4), 0).unwrap();
        })
        .unwrap();
        assert_eq!(p.cluster, 6);
        assert!(!append);
    }

    #[test]
    fn streaming_path_parse_range_and_missing_errors() {
        // endpoint exceeding u16 → UnexpectedValue.
        assert!(matches!(
            parse(|w| {
                w.put_uint(Tag::Context(2), 0x0001_0000).unwrap();
                w.put_uint(Tag::Context(3), 6).unwrap();
                w.put_uint(Tag::Context(4), 0).unwrap();
            }),
            Err(ImError::UnexpectedValue(_))
        ));
        // missing attribute → MissingField.
        assert!(matches!(
            parse(|w| {
                w.put_uint(Tag::Context(2), 0).unwrap();
                w.put_uint(Tag::Context(3), 6).unwrap();
            }),
            Err(ImError::MissingField("AttributePath.attribute"))
        ));
    }

    #[test]
    fn truncated_path_body_errors_unclosed_container() {
        // Build a full valid path, then chop the trailing end-of-container.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_list(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 6).unwrap();
        w.put_uint(Tag::Context(4), 0).unwrap();
        w.end_container().unwrap();
        buf.pop(); // remove the list's 0x18
        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        assert!(matches!(
            attribute_path_from_reader(&mut r),
            Err(ImError::Codec(matter_codec::Error::UnclosedContainer))
        ));
    }

    #[test]
    fn non_null_list_index_leaves_append_false() {
        let (path, append) = parse(|w| {
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.put_uint(Tag::Context(3), 6).unwrap();
            w.put_uint(Tag::Context(4), 0).unwrap();
            w.put_uint(Tag::Context(5), 3).unwrap(); // concrete index, not null
        })
        .unwrap();
        assert_eq!((path.endpoint, path.cluster, path.attribute), (1, 6, 0));
        assert!(!append, "only ListIndex=null signals append");
    }

    #[test]
    fn wrong_typed_member_is_ignored() {
        // A wrong-typed duplicate AFTER the valid member must not clobber it.
        let (path, append) = parse(|w| {
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.put_uint(Tag::Context(3), 6).unwrap();
            w.put_uint(Tag::Context(4), 0).unwrap();
            w.put_utf8(Tag::Context(2), "nope").unwrap(); // ignored
        })
        .unwrap();
        assert_eq!((path.endpoint, path.cluster, path.attribute), (1, 6, 0));
        assert!(!append);
    }
}

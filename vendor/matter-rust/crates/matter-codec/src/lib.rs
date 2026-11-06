//! Matter TLV (Tag-Length-Value) encoding and decoding.
//!
//! # Scope
//!
//! The whole of Matter Core Specification §A.2: all scalar element types,
//! UTF-8 and octet strings, every tag form (anonymous, context, common
//! profile, implicit profile, fully-qualified), and containers (structure,
//! array, list) with a 32-level depth limit. [`TlvWriter`] always picks the
//! narrowest legal encoding for a tag or a length.
//!
//! [`TlvReader`] can be driven three ways: element-at-a-time with
//! [`next`](TlvReader::next), zero-copy with
//! [`next_ref`](TlvReader::next_ref) (yielding [`ElementRef`] /
//! [`ValueRef`], which borrow strings and octet strings straight out of the
//! input), or as a whole tree with [`read_value`](TlvReader::read_value).
//! Containers you do not care about can be skipped outright — see
//! [`skip_container`](TlvReader::skip_container), or
//! [`skip_container_span`](TlvReader::skip_container_span) when you want the
//! raw bytes back to forward verbatim.
//!
//! Verified by spec test vectors, a `proptest` round-trip property, and a
//! `cargo-fuzz` target.
//!
//! # Usage
//!
//! ```
//! use matter_codec::{Tag, TlvWriter};
//! # fn main() -> Result<(), matter_codec::Error> {
//! let mut bytes = Vec::new();
//! let mut writer = TlvWriter::new(&mut bytes);
//! writer.put_bool(Tag::Anonymous, true)?;
//! assert_eq!(bytes, [0x09]);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod element_type;
mod tag_control;

pub mod error;
pub mod reader;
pub mod tag;
pub mod value;
pub mod writer;

pub use error::{Error, Result};
pub use reader::{ContainerKind, Element, ElementRef, ElementSpan, TlvReader, MAX_DEPTH};
pub use tag::Tag;
pub use value::{Value, ValueRef};
pub use writer::TlvWriter;

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

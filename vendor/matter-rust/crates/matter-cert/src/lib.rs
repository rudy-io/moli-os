//! Matter protocol certificate format — parsing and serialisation.
//!
//! Implements Matter Core Specification §6.5: a TLV-encoded variant of
//! X.509 used for both attestation chains (DAC → PAI → PAA) and
//! operational chains (NOC → ICAC → RCAC).
//!
//! # Scope
//!
//! - **Parse and serialise** — [`MatterCertificate`] over the Matter TLV
//!   form, byte-exact on round-trip. Distinguished names including the
//!   Matter-specific OIDs ([`name`]) and the extension set
//!   ([`extensions`]: basic constraints, key usage, extended key usage,
//!   subject and authority key identifiers).
//! - **Public keys and signatures** — P-256 key extraction
//!   ([`public_key`]) and the raw `r || s` Matter [`signature`] form.
//! - **X.509 DER conversion** — real Matter signatures are made over the
//!   X.509 DER `TBSCertificate`, not over the TLV form, so
//!   [`MatterCertificate::verify_signed_by`] reconstructs it. Byte parity
//!   against matter.js's `asUnsignedDer()` is the correctness gate.
//! - **Chain validation** — [`CertificateChain::validate`] against
//!   [`TrustedRoots`], checking time bounds, the CA bit above the leaf,
//!   DN linkage, the path-length constraint, and each signature.
//! - **Issuance** — [`Builder`] constructs an [`UnsignedCertificate`], and
//!   [`operational`] adds role-aware constructors that bake in the
//!   extension and DN profile the spec mandates for RCAC, ICAC, and NOC.
//!   Signing is a separate step, so it can happen in an HSM, an OS
//!   keychain, or an offline ceremony rather than in this process.
//!
//! Cryptographic verification is delegated to `ring`. This crate
//! never implements the underlying maths.

#![forbid(unsafe_code)]

mod tlv_tags;
mod x509;

pub mod builder;
pub mod certificate;
pub mod chain;
pub mod error;
pub mod extensions;
pub mod name;
pub mod operational;
pub mod public_key;
pub mod signature;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod time;

pub use builder::{Builder, UnsignedCertificate};
pub use certificate::MatterCertificate;
pub use chain::{CertificateChain, TrustAnchor, TrustedRoots};
pub use error::{Error, Result};
pub use extensions::{BasicConstraints, Extensions, ExtensionsBuilder, KeyIdentifier, KeyUsage};
pub use name::{DistinguishedName, DnAttribute, DnAttributeValue};
pub use public_key::PublicKey;
pub use signature::Signature;
pub use time::MatterTime;

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

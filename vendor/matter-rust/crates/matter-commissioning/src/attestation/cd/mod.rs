//! Certification Declaration (CD) verification (M6.4.3).
//!
//! Matter Core Spec §6.3.1: the Certification Declaration is a CMS /
//! PKCS#7 `SignedData` blob signed by a CSA-controlled signing root,
//! declaring the device's vendor ID, product ID list, format
//! version, security level, security information, version number, and
//! certification type.
//!
//! The commissioner verifies the CD's signature against a trusted
//! root and cross-checks the declared VID + PID against the verified
//! DAC subject before accepting the device's claimed identity.
//! Without this step, a genuine DAC for product X could fraudulently
//! claim to commission product Y.
//!
//! # Parts
//!
//! - [`CdSigningRoots`]: the trust store of CD signing-root keys, and
//!   [`verify_certification_declaration`]: the verification flow with full
//!   CMS / signature / VID / PID checks. Failures are reported as the
//!   `CertificationDeclaration*` variants of
//!   [`crate::attestation::AttestationError`].
//! - `parse_pem_public_key`: the PEM `SubjectPublicKeyInfo` parser that
//!   loads the bundled CSA-test root and [`CdSigningRoots::from_pem`]'s keys.
//! - `parse_inner_cd_tlv`: the inner CD TLV parser — exposes `vendor_id` and
//!   `product_ids` for the VID / PID cross-check.

#![forbid(unsafe_code)]

mod verifier;

pub use verifier::{
    verify_certification_declaration, verify_certification_declaration_with_paa, CdSigningRoots,
};

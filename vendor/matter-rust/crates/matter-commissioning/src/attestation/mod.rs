//! Matter device attestation verification.
//!
//! This module implements the commissioner-side checks of Matter Core
//! Spec §6.2 — verifying that a device's Device Attestation Certificate
//! (DAC) chains to a trusted Product Attestation Authority (PAA) root,
//! and that the device holds the DAC private key for the current
//! commissioning session.
//!
//! # Phase status
//!
//! - **M6.2.1:** typed [`Dac`], [`Pai`], [`Paa`] wrappers around
//!   X.509 DER; [`PaaTrustStore`] with bundled CSA test roots;
//!   [`VendorId`] and [`ProductId`] newtypes. Parsing only.
//! - **M6.2.2:** [`verify_chain`] — `rustls-webpki` 0.103 path
//!   validation with `KeyUsage::client_auth()`, plus a Matter
//!   VID/PID equality overlay. Six new [`AttestationError`] variants:
//!   [`AttestationError::InvalidChain`],
//!   [`AttestationError::TimeBoundsViolation`],
//!   [`AttestationError::BasicConstraintsViolation`],
//!   [`AttestationError::UntrustedRoot`],
//!   [`AttestationError::VidMismatch`],
//!   [`AttestationError::PaiVidNotAuthorized`].
//!   Coverage: happy-path test against the bundled CSA chain, 7
//!   directed mapping tests on `webpki::Error` -> typed variant, an
//!   8-row negative-fixture integration matrix, and a libfuzzer
//!   target on [`Dac::from_der`].
//! - **M6.2.3 (current; M6.2 feature-complete):**
//!   [`verify_attestation_response`] — pure ECDSA P-256/SHA-256
//!   verification via `ring` over `attestation_elements ||
//!   attestation_challenge`. One new [`AttestationError`] variant:
//!   [`AttestationError::BadResponseSignature`] (deliberately
//!   coarse — one outcome for any failure, to prevent the error
//!   channel from leaking which secret an attacker got close to).
//!   Coverage: directed mutation tests (signature, challenge,
//!   elements, key, malformed-key — all reject), proptest layer
//!   (sign-then-verify round-trip with random P-256 keypairs +
//!   single-bit-flip rejections on signature/challenge/elements),
//!   and matter.js byte-parity test against
//!   `test-vectors/attestation/response/happy-path.json` (Rust and
//!   matter.js must produce identical accept/reject verdicts on a
//!   happy-path tuple and four single-byte mutations).
//!
//! # What's deferred past M6.2
//!
//! - **Certification Declaration (CD) parsing/verification.**
//!   `attestation_elements` is treated as opaque bytes by
//!   [`verify_attestation_response`]. The CSA-signed CD inside it
//!   has its own trust chain and is verified in M6.4.x. **Hard gate:
//!   CD verification MUST land before M6.6 commissions a real
//!   device** — without it, a genuine DAC for product X could
//!   fraudulently claim to commission product Y. Tracked in
//!   `TODO-1.0.md`.
//! - **`AttestationRequest` / `CertificateChainRequest` cluster
//!   message framing.** M6.4.
//! - **DCL trust-root distribution.** Post-1.0.
//! - **Real-device commissioning.** M6.6.
//!
//! # Trust scope
//!
//! [`PaaTrustStore`]'s `with_example_device_roots()` constructor embeds
//! **test** roots only. Production callers must build their own
//! store via `PaaTrustStore::empty()` + `PaaTrustStore::add()`.

#![forbid(unsafe_code)]

pub mod cd;
pub mod chain;
pub mod error;
pub mod extensions;
/// X.509 attestation-certificate profile enforcement (Matter §6.2.2),
/// mirroring chip's `VerifyAttestationCertificateFormat`. Crate-internal:
/// the commissioner runs it automatically during attestation.
pub(crate) mod profile;
pub mod response;
pub mod trust_store;
pub mod x509;

pub use cd::{
    verify_certification_declaration, verify_certification_declaration_with_paa, CdSigningRoots,
};
pub use chain::{verify_chain, ChainVerification};
pub use error::AttestationError;
pub use extensions::{ProductId, VendorId};
pub use response::{
    extract_attestation_elements_fields, verify_attestation_response, verify_dac_signed_elements,
    AttestationElementsFields, AttestationResponse,
};
pub use trust_store::PaaTrustStore;
pub use x509::{Dac, Paa, Pai};

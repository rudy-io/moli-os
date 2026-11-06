//! Test-only helpers for `matter-crypto`.
//!
//! Available only when the `test-support` Cargo feature is enabled.
//! Production callers must NOT enable this feature — these helpers
//! exist solely to drive the M3.3 matter.js byte-parity verification
//! tests, which need deterministic scalar values matching captured
//! matter.js handshakes.
//!
//! # Design note: why no `FixedBytesRng`
//!
//! `ring::rand::SecureRandom` is a sealed trait — external crates cannot
//! implement it. Rather than wrapping an RNG, these helpers call dedicated
//! `pub(crate)` constructors on `PaseProver`/`PaseVerifier` that accept
//! the pre-decoded scalar bytes directly. This is simpler and more explicit:
//! the caller supplies exactly the values the state machine needs, not a
//! byte stream that the constructor then re-parses.
//!
//! # Scalar validity
//!
//! Every scalar passed to these helpers must be a valid non-zero P-256 scalar
//! (big-endian, < curve order). Passing the zero scalar or a value ≥ the
//! curve order returns `Err(Error::InvalidScalar)`.

use matter_cert::{MatterTime, TrustedRoots};

use crate::case::initiator::CaseInitiator;
use crate::case::responder::CaseResponder;
use crate::case::{CaseCredentials, ResumptionRecord};
use crate::error::Result;
use crate::pase::{PasePbkdfParams, PaseProver, PaseVerifier};

// =============================================================================
// Public helpers
// =============================================================================

/// Construct a [`PaseProver`] (known-params path) with a fixed `x` scalar.
///
/// Skips PBKDF param negotiation. The prover's first outbound message (after
/// calling [`PaseProver::start`]) will be Pake1, not `PBKDFParamRequest`.
///
/// Used to match matter.js's captured handshake values in byte-parity tests.
/// Production code must NOT enable the `test-support` feature.
///
/// # Errors
///
/// - [`crate::Error::PbkdfIterationsTooLow`] if `params.iterations < 1000`.
/// - [`crate::Error::PbkdfSaltLengthInvalid`] if `params.salt.len()` ∉ \[16, 32\].
/// - [`crate::Error::InvalidScalar`] if `x_scalar` is zero or not a valid
///   P-256 scalar (i.e., ≥ curve order).
pub fn prover_with_scalar(
    pin: u32,
    params: PasePbkdfParams,
    x_scalar: [u8; 32],
) -> Result<PaseProver> {
    PaseProver::new_with_known_params_with_scalar(pin, params, x_scalar)
}

/// Construct a [`PaseProver`] (negotiation path) with a fixed `x` scalar
/// AND a fixed `initiator_random`.
///
/// The prover's first outbound message (after calling [`PaseProver::start`])
/// will be `PBKDFParamRequest`, carrying the fixed `initiator_random`.
///
/// Used to match matter.js's captured handshake values in byte-parity tests.
///
/// # Errors
///
/// - [`crate::Error::InvalidScalar`] if `x_scalar` is zero or not a valid
///   P-256 scalar (i.e., ≥ curve order).
pub fn prover_with_scalar_and_random(
    pin: u32,
    x_scalar: [u8; 32],
    initiator_random: [u8; 32],
) -> Result<PaseProver> {
    PaseProver::new_with_negotiation_with_scalar(pin, x_scalar, initiator_random)
}

/// Construct a [`PaseVerifier`] from pre-computed verification values, with
/// a fixed `y` scalar.
///
/// In production a device stores `w0` and `L` at provisioning time (not the
/// PIN). This constructor mirrors [`PaseVerifier::new`] but injects a fixed
/// `y` scalar for deterministic testing. Uses an all-zero `responder_random`
/// and `responder_session_id = 0`; for full byte-parity use
/// [`verifier_with_scalar_and_random`] instead.
///
/// # Parameters
///
/// - `w0`: 32-byte big-endian P-256 scalar derived from the PIN.
/// - `l`: 65-byte uncompressed P-256 point `L = w1·P`.
/// - `params`: PBKDF2 parameters used when `w0`/`L` were derived.
/// - `y_scalar`: the fixed 32-byte scalar to use instead of a random one.
///
/// # Errors
///
/// - [`crate::Error::PbkdfIterationsTooLow`] if `params.iterations < 1000`.
/// - [`crate::Error::PbkdfSaltLengthInvalid`] if `params.salt.len()` ∉ \[16, 32\].
/// - [`crate::Error::InvalidScalar`] if `w0` or `y_scalar` is zero or not a
///   valid P-256 scalar.
pub fn verifier_with_scalar(
    w0: [u8; 32],
    l: [u8; 65],
    params: PasePbkdfParams,
    y_scalar: [u8; 32],
) -> Result<PaseVerifier> {
    PaseVerifier::new_with_scalar(w0, l, params, y_scalar)
}

/// Construct a [`PaseVerifier`] from pre-computed verification values, with
/// a fixed `y` scalar, `responder_random`, and `responder_session_id`.
///
/// Used by the M3.3 byte-parity test harness to reproduce `PBKDFParamResponse`
/// bytes exactly as captured from a matter.js run with a deterministic RNG.
///
/// # Parameters
///
/// - `w0`: 32-byte big-endian P-256 scalar derived from the PIN.
/// - `l`: 65-byte uncompressed P-256 point `L = w1·P`.
/// - `params`: PBKDF2 parameters used when `w0`/`L` were derived.
/// - `y_scalar`: the fixed 32-byte scalar to use instead of a random one.
/// - `responder_random`: the fixed 32-byte responder nonce to embed in the response.
/// - `responder_session_id`: the session ID to embed in the `PBKDFParamResponse`.
///
/// # Errors
///
/// - [`crate::Error::PbkdfIterationsTooLow`] if `params.iterations < 1000`.
/// - [`crate::Error::PbkdfSaltLengthInvalid`] if `params.salt.len()` ∉ \[16, 32\].
/// - [`crate::Error::InvalidScalar`] if `w0` or `y_scalar` is zero or not a
///   valid P-256 scalar.
pub fn verifier_with_scalar_and_random(
    w0: [u8; 32],
    l: [u8; 65],
    params: PasePbkdfParams,
    y_scalar: [u8; 32],
    responder_random: [u8; 32],
    responder_session_id: u16,
) -> Result<PaseVerifier> {
    PaseVerifier::new_with_scalar_and_random(
        w0,
        l,
        params,
        y_scalar,
        responder_random,
        responder_session_id,
    )
}

/// Construct a [`PaseProver`] (negotiation path) with a fixed `x` scalar,
/// `initiator_random`, and `initiator_session_id`.
///
/// The prover's first outbound message (after calling [`PaseProver::start`])
/// will be `PBKDFParamRequest`, carrying the fixed `initiator_random` and
/// `initiator_session_id`. Used in M3.3 byte-parity tests to reproduce the
/// exact `PBKDFParamRequest` bytes captured from matter.js.
///
/// # Errors
///
/// - [`crate::Error::InvalidScalar`] if `x_scalar` is zero or not a valid
///   P-256 scalar (i.e., ≥ curve order).
pub fn prover_with_scalar_random_and_session_id(
    pin: u32,
    x_scalar: [u8; 32],
    initiator_random: [u8; 32],
    initiator_session_id: u16,
) -> Result<PaseProver> {
    PaseProver::new_with_negotiation_with_scalar_and_session_id(
        pin,
        x_scalar,
        initiator_random,
        initiator_session_id,
    )
}

/// Construct a [`PaseVerifier`] from a PIN with a fixed `y` scalar.
///
/// Derives `w0` and `L` from the PIN using PBKDF2, then constructs the
/// verifier with the given fixed scalar. Useful when the test PIN is known
/// but the raw `w0`/`L` bytes are not.
///
/// # Errors
///
/// - [`crate::Error::PbkdfIterationsTooLow`] / [`crate::Error::PbkdfSaltLengthInvalid`]
///   if `params` are out of spec.
/// - [`crate::Error::PinDerivationFailed`] if PBKDF2 fails.
/// - [`crate::Error::InvalidScalar`] if `y_scalar` is zero or not a valid
///   P-256 scalar.
pub fn verifier_with_scalar_from_pin(
    pin: u32,
    params: PasePbkdfParams,
    y_scalar: [u8; 32],
) -> Result<PaseVerifier> {
    PaseVerifier::new_from_pin_with_scalar(pin, params, y_scalar)
}

// =============================================================================
// CASE helpers (M4.3)
// =============================================================================

/// Construct a [`CaseInitiator`] with a fixed ephemeral private key and
/// initiator random.
///
/// Bypasses the OS RNG entirely — the ephemeral keypair is derived from
/// `eph_private_key` and the 32-byte nonce is supplied directly.
/// Used by the matter.js byte-parity tests to replay a captured handshake
/// with deterministic inputs.
///
/// `now` is the injected validation clock for the peer certificate chain (see
/// [`CaseInitiator::new`]).
///
/// # Errors
///
/// - [`crate::Error::EphemeralKeyGenerationFailed`] if `eph_private_key` is
///   zero, >= the P-256 curve order, or otherwise not a valid scalar.
pub fn case_initiator_with_eph_key(
    credentials: CaseCredentials,
    trusted_roots: TrustedRoots,
    peer_node_id: u64,
    peer_fabric_id: u64,
    eph_private_key: [u8; 32],
    initiator_random: [u8; 32],
    now: MatterTime,
) -> Result<CaseInitiator> {
    CaseInitiator::new_with_eph_and_random(
        credentials,
        trusted_roots,
        peer_node_id,
        peer_fabric_id,
        eph_private_key,
        initiator_random,
        now,
    )
}

/// Resumption-path variant of [`case_initiator_with_eph_key`].
///
/// The Sigma1 message produced by [`CaseInitiator::start`] will include
/// `resumption_id` (tag 6) and `initiator_resume_mic` (tag 7) derived from
/// `record`. All random inputs are still bypassed.
///
/// `now` is the injected validation clock for the peer certificate chain (see
/// [`CaseInitiator::new`]).
///
/// # Errors
///
/// - [`crate::Error::EphemeralKeyGenerationFailed`] if `eph_private_key` is
///   zero, >= the P-256 curve order, or otherwise not a valid scalar.
#[allow(clippy::too_many_arguments)]
pub fn case_initiator_with_resumption_eph_key(
    credentials: CaseCredentials,
    trusted_roots: TrustedRoots,
    peer_node_id: u64,
    peer_fabric_id: u64,
    record: ResumptionRecord,
    eph_private_key: [u8; 32],
    initiator_random: [u8; 32],
    now: MatterTime,
) -> Result<CaseInitiator> {
    CaseInitiator::new_with_resumption_eph_and_random(
        credentials,
        trusted_roots,
        peer_node_id,
        peer_fabric_id,
        record,
        eph_private_key,
        initiator_random,
        now,
    )
}

/// Construct a [`CaseResponder`] with a fixed ephemeral private key and
/// responder random.
///
/// Bypasses the OS RNG entirely — the ephemeral keypair is derived from
/// `eph_private_key` and the 32-byte nonce is supplied directly.
/// Used by the matter.js byte-parity tests to replay a captured handshake
/// with deterministic inputs.
///
/// `now` is the injected validation clock for the initiator certificate chain
/// (see [`CaseResponder::new`]).
///
/// # Errors
///
/// - [`crate::Error::EphemeralKeyGenerationFailed`] if `eph_private_key` is
///   zero, >= the P-256 curve order, or otherwise not a valid scalar.
pub fn case_responder_with_eph_key(
    credentials: CaseCredentials,
    trusted_roots: TrustedRoots,
    eph_private_key: [u8; 32],
    responder_random: [u8; 32],
    now: MatterTime,
) -> Result<CaseResponder> {
    CaseResponder::new_with_eph_and_random(
        credentials,
        trusted_roots,
        eph_private_key,
        responder_random,
        now,
    )
}

/// Construct a [`CaseResponder`] like [`case_responder_with_eph_key`] but
/// additionally fixing the fresh resumption id that
/// [`CaseResponder::accept_resumption`] would otherwise sample from the OS
/// CSPRNG, so the emitted `Sigma2_Resume` is deterministic.
///
/// Used by the resumption-accepted matter.js byte-parity test to replay a
/// captured `Sigma1 → Sigma2_Resume` exchange with fully deterministic inputs.
///
/// # Errors
///
/// - [`crate::Error::EphemeralKeyGenerationFailed`] if `eph_private_key` is
///   zero, >= the P-256 curve order, or otherwise not a valid scalar.
pub fn case_responder_with_eph_key_and_resumption_id(
    credentials: CaseCredentials,
    trusted_roots: TrustedRoots,
    eph_private_key: [u8; 32],
    responder_random: [u8; 32],
    new_resumption_id: [u8; 16],
    now: MatterTime,
) -> Result<CaseResponder> {
    let mut responder = CaseResponder::new_with_eph_and_random(
        credentials,
        trusted_roots,
        eph_private_key,
        responder_random,
        now,
    )?;
    responder.set_new_resumption_id_override(new_resumption_id);
    Ok(responder)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pase::PaseMessageKind;

    // ─── Helpers ──────────────────────────────────────────────────────────

    /// A valid P-256 scalar: the value 7 in big-endian (non-zero, < curve order).
    fn valid_scalar() -> [u8; 32] {
        let mut s = [0u8; 32];
        s[31] = 0x07;
        s
    }

    fn test_params() -> PasePbkdfParams {
        PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0x42u8; 16],
        }
    }

    // ─── prover_with_scalar ───────────────────────────────────────────────

    #[test]
    fn prover_with_scalar_constructs_known_params() {
        // After construction with known-params, the prover is in
        // AwaitingStartKnownParams. `expected_inbound()` returns None
        // because the machine is waiting to emit (via start()), not to receive.
        let prover = prover_with_scalar(20_202_021, test_params(), valid_scalar()).unwrap();
        assert!(
            prover.expected_inbound().is_none(),
            "after construction, prover has not yet called start()"
        );
    }

    #[test]
    fn prover_with_scalar_rejects_zero_scalar() {
        let result = prover_with_scalar(20_202_021, test_params(), [0u8; 32]);
        assert!(result.is_err(), "zero scalar must be rejected");
    }

    // ─── prover_with_scalar_and_random ────────────────────────────────────

    #[test]
    fn prover_with_scalar_and_random_constructs_negotiation() {
        let prover = prover_with_scalar_and_random(
            20_202_021,
            valid_scalar(),
            [0xABu8; 32], // fixed initiator_random
        )
        .unwrap();
        // AwaitingStartNegotiation: expected_inbound() is None before start().
        assert!(
            prover.expected_inbound().is_none(),
            "after construction, negotiation prover has not yet called start()"
        );
    }

    #[test]
    fn prover_with_scalar_and_random_rejects_zero_scalar() {
        let result = prover_with_scalar_and_random(20_202_021, [0u8; 32], [0xABu8; 32]);
        assert!(result.is_err(), "zero scalar must be rejected");
    }

    // ─── verifier_with_scalar_from_pin ────────────────────────────────────

    #[test]
    fn verifier_with_scalar_from_pin_constructs() {
        // Exercises both pin derivation and scalar injection.
        let verifier =
            verifier_with_scalar_from_pin(20_202_021, test_params(), valid_scalar()).unwrap();
        // After construction the verifier is in AwaitingFirstMessage.
        assert_eq!(
            verifier.expected_inbound(),
            Some(PaseMessageKind::PbkdfParamRequest),
            "verifier should be awaiting the first inbound message"
        );
    }

    #[test]
    fn verifier_with_scalar_from_pin_rejects_zero_scalar() {
        let result = verifier_with_scalar_from_pin(20_202_021, test_params(), [0u8; 32]);
        assert!(result.is_err(), "zero scalar must be rejected");
    }

    // ─── verifier_with_scalar ─────────────────────────────────────────────

    #[test]
    fn verifier_with_scalar_rejects_zero_w0() {
        // All-zero w0 is not a valid P-256 scalar (zero = identity).
        let result = verifier_with_scalar(
            [0u8; 32],    // zero w0 — invalid
            [0x04u8; 65], // l doesn't matter; w0 validation fires first
            test_params(),
            valid_scalar(),
        );
        assert!(result.is_err(), "zero w0 must be rejected");
    }

    #[test]
    fn verifier_with_scalar_rejects_zero_y() {
        let result = verifier_with_scalar(
            valid_scalar(), // valid w0
            [0x04u8; 65],
            test_params(),
            [0u8; 32], // zero y — invalid
        );
        assert!(result.is_err(), "zero y scalar must be rejected");
    }
}

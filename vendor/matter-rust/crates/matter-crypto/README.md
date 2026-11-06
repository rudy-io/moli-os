# matter-crypto

Matter protocol session establishment and the key derivations around it
— PASE (Password Authenticated Session Establishment) via SPAKE2+, CASE
(Certificate Authenticated Session Establishment) via SIGMA-I,
operational identity derivations, the ICD check-in codec, and the
AES-CCM AEAD the secured-message layer uses. Part of the
[matter-rust](https://github.com/phunapps/matter-rust) workspace.

## Scope

### PASE — spec §3.10

- Sans-IO state machines (`PaseProver`, `PaseVerifier`) — drive bytes
  through method-per-message-type APIs; caller owns the transport.
- SPAKE2+ math over P-256 with Matter's M and N constants.
- PBKDF2 setup-PIN derivation; HKDF session-key derivation.
- Constant-time confirmation tag comparison via `subtle`.
- Byte-for-byte verified against matter.js for three handshake
  scenarios (negotiation, known-params, max-iterations).

### CASE — spec §4.13

- Sans-IO `CaseInitiator` / `CaseResponder` state machines.
- SIGMA-I math: ephemeral P-256 ECDH, mutual ECDSA signatures,
  AES-CCM-128 encrypted blobs.
- NOC chain validation via `matter-cert::CertificateChain::validate`.
- Pluggable signing via the `CaseSigner` trait — wire your own
  HSM/TPM/secure-element by implementing one method.
- Session resumption: Sigma1 + Sigma2_Resume fast path. The caller
  drives record lookup via the `Sigma1Outcome` enum (sans-IO purity).
- Byte-for-byte verified against matter.js for three scenarios:
  new session, resumption accepted, and resumption declined.

### Operational identity — spec §4.3

- Compressed Fabric Identifier and operational IPK derivation.
- Group session key, group privacy key, and the group multicast IPv6
  address.

### ICD check-in — spec §4.18.2

- The Check-In message codec: the payload an intermittently-connected
  device sends a registered client when it briefly wakes.

### AEAD

- AES-128-CCM-128 helpers, used by CASE here and by
  `matter-transport`'s secured-message framing. `SessionAead` keeps the
  expanded AES key across calls; prefer it over the free functions on
  any path that encrypts or decrypts more than once per key.

## Status

**0.3.1**, published on crates.io. PASE and CASE feature-complete, and
validated against real silicon through the higher-level crates.
Stability: a `0.x` crate, so a **minor** bump may break API.

```toml
[dependencies]
matter-crypto = "0.5"
```

## Minimal example

```rust
use matter_crypto::{PasePbkdfParams, PaseProver, PaseVerifier};

fn main() -> matter_crypto::Result<()> {
    let pin = 20202021_u32;
    let params = PasePbkdfParams {
        iterations: 1_000,
        salt: vec![0x42u8; 16],
    };

    // Each side picks its own local session id, exchanged during the handshake.
    let mut prover = PaseProver::new_with_negotiation(pin, /* initiator_session_id */ 1)?;
    let mut verifier = PaseVerifier::new_from_pin(pin, params, /* responder_session_id */ 2)?;

    // Drive the 5-message handshake. Both peers are in-process here; in
    // production the caller pipes each `Vec<u8>` across the network.
    let m = prover.start()?;
    verifier.handle_pbkdf_request(&m)?;
    let m = verifier.next_message()?;
    prover.handle_pbkdf_response(&m)?;
    let m = prover.next_message()?;
    verifier.handle_pake1(&m)?;
    let m = verifier.next_message()?;
    prover.handle_pake2(&m)?;
    let m = prover.next_message()?;
    verifier.handle_pake3(&m)?;

    let prover_keys = prover.finish()?;
    let verifier_keys = verifier.finish()?;
    assert_eq!(prover_keys.ke, verifier_keys.ke);
    Ok(())
}
```

## Minimal example — CASE

```rust,no_run
use matter_cert::{MatterCertificate, MatterTime, TrustedRoots};
use matter_crypto::{
    CaseCredentials, CaseInitiator, CaseResponder, RingSigner, Sigma1Outcome,
};

/// Build one side's operational identity.
///
/// `noc` is a `MatterCertificate` from matter-cert (issued by this fabric's
/// CA chain), `signer` holds the NOC private key, `ipk` is the fabric's
/// 16-byte Identity Protection Key, and `rcac_public_key` is the fabric root
/// CA's SEC1-uncompressed public key. Commissioning (matter-commissioning)
/// produces all four.
fn credentials(
    noc: MatterCertificate,
    signer: RingSigner,
    ipk: [u8; 16],
    rcac_public_key: [u8; 65],
    fabric_id: u64,
    node_id: u64,
) -> CaseCredentials {
    CaseCredentials {
        noc,
        icac: None,
        signer: Box::new(signer),
        fabric_id,
        node_id,
        ipk,
        rcac_public_key,
    }
}

/// Drive the 3-message Sigma1/2/3 handshake. Both peers are in-process here;
/// in a real deployment the caller pipes each message across the network.
fn handshake(
    initiator_creds: CaseCredentials,
    responder_creds: CaseCredentials,
    trusted_roots: TrustedRoots,
    responder_node_id: u64,
    fabric_id: u64,
    now: MatterTime,
) -> matter_crypto::Result<()> {
    let mut initiator = CaseInitiator::new(
        initiator_creds,
        trusted_roots.clone(),
        responder_node_id,
        fabric_id,
        /* initiator_session_id */ 1,
        now,
    )?;
    let mut responder = CaseResponder::new(
        responder_creds,
        trusted_roots,
        /* responder_session_id */ 2,
        now,
    )?;

    let sigma1 = initiator.start()?;
    let outcome = responder.handle_sigma1(&sigma1)?;
    assert!(matches!(outcome, Sigma1Outcome::NewSession));

    let sigma2 = responder.next_message()?;
    initiator.handle_sigma2(&sigma2)?;

    let sigma3 = initiator.next_message()?;
    responder.handle_sigma3(&sigma3)?;

    let init_out = initiator.finish()?;
    let resp_out = responder.finish()?;
    // Both sides derive the same session keys.
    assert_eq!(init_out.keys.i2r_key, resp_out.keys.i2r_key);
    Ok(())
}
```

## Cryptographic primitives

This crate never implements crypto primitives. Underlying math:
- [`ring`](https://github.com/briansmith/ring) — SHA-256, HMAC, HKDF,
  PBKDF2, ECDSA-verify.
- [`p256`](https://crates.io/crates/p256) — P-256 scalar/point
  arithmetic for SPAKE2+ (ring deliberately doesn't expose these).
- [`subtle`](https://crates.io/crates/subtle) — constant-time
  comparison for PASE confirmation tags.
- [`aes`](https://crates.io/crates/aes) +
  [`ccm`](https://crates.io/crates/ccm) — AES-CCM-128 for CASE
  encrypted blobs (ring 0.17 does not expose AES-CCM).

## Cross-verification

PASE messages produced by our `PaseProver` and `PaseVerifier` are
byte-identical to matter.js's output for the same inputs. CI runs
this verification on every PR against three captured handshake
scenarios.

CASE messages are byte-identical to matter.js's output for the same
inputs, on all three captured scenarios in `test-vectors/case/`:
new session (Sigma1/2/3), resumption accepted (Sigma1 →
Sigma2_Resume), and resumption declined (Sigma1 → full Sigma2/3).

## License

Apache 2.0. See [LICENSE](../../LICENSE).

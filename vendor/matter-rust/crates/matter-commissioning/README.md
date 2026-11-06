# matter-commissioning

Matter commissioning: setup payloads, the commissioning state machine, device
attestation, NOC issuance, and network commissioning.

Part of [`matter-rust`](https://github.com/phunapps/matter-rust).

> Status: **0.10.2**. The commissioning flow here has
> been driven against real Matter hardware — over IP and over BLE, onto Wi-Fi
> and onto Thread — not only against tests.
>
> What the crate gives you:
> - **Setup payloads** — QR and manual pairing codes, decode and encode.
> - **Device attestation** — typed `Dac` / `Pai` / `Paa` wrappers, chain
>   validation against a `PaaTrustStore`, `AttestationResponse` signature
>   verification, and CSA Certification Declaration (CMS) verification.
> - **NOC issuance** — `FabricRecord`, CSR verification, RCAC/NOC minting,
>   and the `OperationalCredentials` command codecs.
> - **The commissioning state machine** — a sans-IO cursor over the whole
>   flow, `SecurePairing` through `Action::Done(CommissionedFabric)`,
>   including the network-commissioning subgraph (Wi-Fi, Thread, or a device
>   already on its operational network) and the PASE→CASE handoff.
> - **An async driver**, behind the off-by-default `driver` feature: the
>   Tokio IO layer that runs that state machine for real (PASE, mDNS, CASE,
>   Invoke/Read round-trips). See [below](#optional-driver-feature).
>
> Stability: this is a `0.x` crate, so a **minor** bump may break API.
> Encodings are byte-checked against matter.js where fixtures exist.
>
> If you want a complete controller — commissioning plus reading, writing,
> invoking and subscribing — use
> [`matter-controller`](https://crates.io/crates/matter-controller), which is
> built on this crate. Reach for `matter-commissioning` directly when you want
> the commissioning pieces on their own, or want to drive the state machine
> from your own IO layer.

## Example: parse a QR code

```rust
use matter_commissioning::setup::parse_qr;

let payload = parse_qr("MT:Y.K90AFN00KA0648G00")?;
assert_eq!(payload.vendor_id, Some(0xFFF1));
assert_eq!(payload.passcode.as_u32(), 20_202_021);
# Ok::<(), matter_commissioning::SetupError>(())
```

(That QR string is the spec's example payload, kept as a fixture at
`test-vectors/commissioning/setup/qr-spec-example.json`. Substitute the code
printed on your own device.)

## Example: parse a manual pairing code

```rust
use matter_commissioning::setup::parse_manual_code;

let payload = parse_manual_code("11693312331")?;
assert_eq!(payload.discriminator.short(), 0x5);
# Ok::<(), matter_commissioning::SetupError>(())
```

## Example: parse a DAC and reach for a trusted root

```rust,no_run
use matter_commissioning::{Dac, PaaTrustStore, VendorId};

# fn run(dac_der: &[u8]) -> Result<(), matter_commissioning::AttestationError> {
let dac = Dac::from_der(dac_der)?;
assert_eq!(dac.subject_vid(), VendorId::new(0xFFF1));

let trust_store = PaaTrustStore::with_example_device_roots();
assert!(trust_store.len() > 0);
# Ok(())
# }
```

Parsing a DAC does not validate it. Chain validation against the trust store is
the next example.

## Example: validate an attestation chain

```rust,no_run
use matter_cert::time::MatterTime;
use matter_commissioning::{verify_chain, Dac, Pai, PaaTrustStore};

# fn run(dac_der: &[u8], pai_der: &[u8])
#   -> Result<(), matter_commissioning::AttestationError> {
let dac = Dac::from_der(dac_der)?;
let pai = Pai::from_der(pai_der)?;
let store = PaaTrustStore::with_example_device_roots();
let now = MatterTime::from_unix_secs(1_704_067_200);

let chain = verify_chain(&dac, &pai, &store, now)?;
println!("DAC verified for VID={} PID={}", chain.vendor_id, chain.product_id);
# Ok(())
# }
```

Production callers build their own `PaaTrustStore` from CSA-published
production roots — `PaaTrustStore::empty()` plus `add()` per root, or
`matter-controller`'s `AttestationTrust::from_dirs`, which loads PAA and CD
roots from two directories. The bundled `with_example_device_roots()` carries
the CSA **test** roots: fine for examples and integration tests, and it will
reject an arbitrary certified product.

## Example: verify an attestation response

```rust,no_run
use matter_commissioning::{
    verify_attestation_response, AttestationResponse,
};

# fn run(
#     attestation_elements: Vec<u8>,
#     signature: [u8; 64],
#     dac_public_key: &[u8],
#     attestation_challenge: &[u8; 16],
# ) -> Result<(), matter_commissioning::AttestationError> {
let response = AttestationResponse {
    attestation_elements,
    signature,
};
verify_attestation_response(&response, attestation_challenge, dac_public_key)?;
# Ok(())
# }
```

The `dac_public_key` is exactly what `Dac::public_key()` returns
(raw SEC1 uncompressed P-256, 65 bytes). The `attestation_challenge`
is the 16-byte session value at `[32..48]` of the PASE/CASE session
key blob (exposed as `CaseSessionKeys::attestation_challenge` or
`PaseSessionKeys::attestation_key`). Any verification failure folds
into the single coarse `AttestationError::BadResponseSignature`.

## Example: configure the state machine and drive it

`Commissioner` is sans-IO: it emits an `Action` describing what to send, and you
feed the device's reply back with `on_response`. The loop below is cut down to
the two action shapes the early stages produce — the
[full loop](#example-full-commissioning-driver-loop-reaching-actiondone) further
down handles every variant.

```rust,no_run
use std::sync::Arc;

use matter_cert::time::MatterTime;
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::noc::{FabricRecord, NocRng, SystemNocRng};
use matter_commissioning::{
    Action, Commissioner, CommissionerConfig, NetworkCredentials, PaaTrustStore, SetupPayload,
};
use matter_crypto::{RingSigner, Signer};

# fn run(
#     pase_attestation_challenge: [u8; 16],
#     setup: SetupPayload,
# ) -> Result<(), Box<dyn std::error::Error>> {
let (signer, _pkcs8) = RingSigner::generate()?;
let signer: Arc<dyn Signer> = Arc::new(signer);
let rng_for_fabric = SystemNocRng;
let fabric = FabricRecord::new_root_only(
    /* fabric_id */ 0x0000_0000_0000_0001,
    signer,
    MatterTime::from_unix_secs(1_704_067_200),
    MatterTime::from_unix_secs(1_735_689_600),
    /* rcac_id */ 0xDEAD_BEEF_CAFE_F00D,
    &rng_for_fabric,
)?;

let paa = PaaTrustStore::with_example_device_roots();
let cd_signing_roots = CdSigningRoots::with_example_device_roots();
let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
let cfg = CommissionerConfig {
    pase_attestation_challenge,
    fabric: &fabric,
    setup_payload: &setup,
    paa_trust_store: &paa,
    cd_signing_roots: &cd_signing_roots,
    commissioner_node_id: 0x1,
    assigned_node_id: 0x2,
    ipk_epoch_key: [0x42_u8; 16],
    case_admin_subject: 0x1,
    admin_vendor_id: 0xFFF1,
    now: MatterTime::from_unix_secs(1_704_067_200),
    rng,
    // This device is already on its operational network; see the Wi-Fi /
    // Thread section below for the provisioning variants.
    network: NetworkCredentials::AlreadyOnNetwork,
};
let mut sm = Commissioner::new(cfg)?;
loop {
    match sm.poll()? {
        Action::ReadAttribute { expect, .. } | Action::Invoke { expect, .. } => {
            // The caller (or the `driver` feature) frames the request
            // into an Invoke/Read envelope, routes via matter-transport
            // over the PASE session, and feeds the response back:
            let response_bytes: &[u8] = unimplemented!("driver supplies the bytes");
            sm.on_response(expect, response_bytes)?;
        }
        Action::Abort { send_disarm_failsafe, reason } => {
            eprintln!("commissioning aborted at {:?}: {reason}", sm.stage());
            if send_disarm_failsafe {
                // ... send DisarmFailsafe (ArmFailSafe with expiry=0) over PASE ...
            }
            break;
        }
        Action::Done(_) => break,
        other => unreachable!("this cut-down example doesn't handle {other:?}"),
    }
}
# Ok(())
# }
```

Those first stages are `SecurePairing` → `ReadCommissioningInfo` →
`ArmFailsafe` → `ConfigRegulatory`. The cursor then continues through
attestation, CSR and NOC issuance, network commissioning, and the CASE
handoff, as the following sections show.

## Example: attestation flow through CD verification

The same driver loop works unchanged — after `ConfigRegulatory` the state
machine emits three more `Action::Invoke` calls (PAI cert, DAC cert,
AttestationRequest) and one off-wire `AttestationVerification` step, which
includes the CSA-signed Certification Declaration check:

```rust,no_run
use matter_commissioning::{Commissioner, Expectation};

# fn run(
#     sm: &mut Commissioner,
#     pai_response_tlv: &[u8],
#     dac_response_tlv: &[u8],
#     attestation_response_tlv: &[u8],
# ) -> Result<(), Box<dyn std::error::Error>> {
// After ConfigRegulatory, cursor reaches SendPaiCertRequest.

// Stage 4: PAI cert request.
let _ = sm.poll()?;
sm.on_response(Expectation::PaiCertChainResponse, pai_response_tlv)?;

// Stage 5: DAC cert request.
let _ = sm.poll()?;
sm.on_response(Expectation::DacCertChainResponse, dac_response_tlv)?;

// Stage 6: AttestationRequest with fresh 32-byte random nonce.
let _ = sm.poll()?;
sm.on_response(Expectation::AttestationResponse, attestation_response_tlv)?;

// Stage 7: AttestationVerification (off-wire). Runs the whole
// verifier chain — chain validation, attestation signature, nonce
// echo, then CD verification — and advances past attestation on
// success. On failure, `poll()` returns a typed `CommissioningError`
// and the cursor transitions to `Failed`.
let _ = sm.poll()?;
# Ok(())
# }
```

From there the cursor walks into the CSR and NOC issuance stages
(`SendOpCertSigningRequest` → `ValidateCsr` → `GenerateNocChain` →
`SendTrustedRootCert` → `SendNoc`).

## Example: verify a Certification Declaration standalone

`verify_certification_declaration` can be called directly without
involving the state machine — useful for offline analysis of captured
CD blobs:

```rust,no_run
use matter_commissioning::{
    verify_certification_declaration, AttestationError, CdSigningRoots,
    ProductId, VendorId,
};

# fn run(cd_bytes: &[u8]) -> Result<(), AttestationError> {
let trust = CdSigningRoots::with_example_device_roots();
verify_certification_declaration(
    cd_bytes,
    VendorId::new(0xFFF1),
    ProductId::new(0x8001),
    &trust,
)?;
# Ok(())
# }
```

Production callers replace `with_example_device_roots()` with
`CdSigningRoots::from_pem(&[my_root_pem])` loading the CSA-published
signing root(s) supplied by deployment.

The verifier performs five checks in order:
1. Parse the CMS/PKCS#7 SignedData via the `cms` crate.
2. Validate the CMS envelope shape (single signer, attached content,
   `ecdsa-with-SHA256`).
3. Verify the ECDSA-P256/SHA-256 signature against each trusted root;
   accept on first match.
4. Decode the inner Matter-TLV CD body to extract `vendor_id` +
   `product_id_array`.
5. Cross-check the declared VID/PID against the `expected_vid` /
   `expected_pid` arguments.

Any failure surfaces as a specific
`AttestationError::CertificationDeclaration*` variant.

## Example: full commissioning driver loop reaching `Action::Done`

The complete cursor walks from `SecurePairing` through
`Action::Done(CommissionedFabric)`. The caller frames Invoke envelopes +
routes via `matter-transport`, then performs mDNS find-operational + the
SIGMA handshake when the state machine signals `Action::EstablishCase`.
The `driver` feature ships exactly such a caller, so you only need to write
this loop yourself if you are supplying your own IO:

```rust,no_run
use matter_commissioning::{
    Action, CommissionedFabric, Commissioner, CommissioningError,
};

# fn run(mut sm: Commissioner) -> Result<CommissionedFabric, CommissioningError> {
loop {
    match sm.poll()? {
        Action::Invoke { expect, .. } | Action::ReadAttribute { expect, .. } => {
            // Caller frames the request into Invoke/Read envelope and
            // routes via matter-transport. The session is PASE for all
            // pre-NOC stages and CASE after EstablishCase succeeds.
            let response_bytes: &[u8] = unimplemented!("driver supplies the bytes");
            sm.on_response(expect, response_bytes)?;
        }
        Action::EstablishCase { fabric_id, peer_node_id } => {
            // Driver work: mDNS find-operational for the operational
            // service name keyed off (compressed_fabric_id, peer_node_id),
            // then run the SIGMA-I handshake from matter-crypto.
            // Pretend success here:
            let _ = (fabric_id, peer_node_id);
            sm.on_case_established()?;

            // On failure instead:
            //   sm.on_response(Expectation::CaseFailed, &[])?;
        }
        Action::EvictCase { .. } => {
            // Reserved for multi-fabric eviction; never emitted by
            // the current new-fabric flow.
        }
        Action::Done(commissioned_fabric) => {
            return Ok(commissioned_fabric);
        }
        Action::Abort { send_disarm_failsafe, reason } => {
            eprintln!("commissioning aborted: {reason}");
            if send_disarm_failsafe {
                // ... send ArmFailSafe(expiry=0) over PASE ...
            }
            return Err(CommissioningError::CaseEstablishmentFailed); // pick a representative error
        }
        // `Action` is `#[non_exhaustive]`: a future minor release can add a
        // variant this loop has never seen. Return an error rather than
        // panicking — the driver stays in control and can still disarm the
        // failsafe on the device before giving up.
        _ => return Err(CommissioningError::InvalidConfig("unhandled action")),
    }
}
# }
```

The returned `CommissionedFabric` carries the long-lived fabric record
(RCAC + IPK + fabric ID), the peer's operational node ID, the device's
NOC public key, and the terminal stage cursor (always
`Stage::Cleanup`).

## Wi-Fi commissioning configuration

```rust,no_run
use std::sync::Arc;

use matter_cert::time::MatterTime;
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::noc::{FabricRecord, NocRng};
use matter_commissioning::{
    Commissioner, CommissionerConfig, NetworkCredentials, PaaTrustStore, SetupPayload,
    WiFiCredentials,
};

# fn run(
#     pase_attestation_challenge: [u8; 16],
#     fabric: FabricRecord,
#     setup: SetupPayload,
#     paa: PaaTrustStore,
#     cd_roots: CdSigningRoots,
#     rng: Arc<dyn NocRng>,
# ) -> Result<(), Box<dyn std::error::Error>> {
// `CommissionerConfig` borrows the fabric, payload and trust stores — they
// must outlive the `Commissioner`.
let config = CommissionerConfig {
    pase_attestation_challenge,
    fabric: &fabric,
    setup_payload: &setup,
    paa_trust_store: &paa,
    cd_signing_roots: &cd_roots,
    commissioner_node_id: 0x1,
    assigned_node_id: 0x2,
    ipk_epoch_key: [0x42_u8; 16],
    case_admin_subject: 0x1,
    admin_vendor_id: 0xFFF1,
    now: MatterTime::from_unix_secs(1_704_067_200),
    rng,
    network: NetworkCredentials::WiFi(WiFiCredentials {
        ssid: b"matter".to_vec(),
        credentials: b"hunter22".to_vec(),
    }),
};
let mut sm = Commissioner::new(config)?;
# let _ = &mut sm;
# Ok(())
# }
```

For Ethernet-only devices (or devices already on their operational
network), set `network: NetworkCredentials::AlreadyOnNetwork` — the state
machine detects the network shape at `Stage::ReadNetworkCommissioningInfo`
and skips the Wi-Fi sub-cursor.

Thread commissioning is supported: set
`network: NetworkCredentials::Thread(dataset)` with a
[`ThreadDataset`](src/thread_dataset.rs) built from an operational dataset
(e.g. `ot-ctl dataset active -x`, hex-decoded). If the supplied credential
type doesn't match what the device actually offers — e.g. `Thread`
credentials against a device whose `NetworkCommissioning::FeatureMap` lacks
the Thread bit — commissioning fails fast with
`CommissioningError::NetworkFeatureUnsupported { needed }`, naming the
network type the device is missing.

## Optional `driver` feature

Everything above is sans-IO: the state machine says what to send and consumes
what comes back, but never touches a socket. The `driver` feature adds the
Tokio IO layer that closes the loop:

```toml
matter-commissioning = { version = "0.10", features = ["driver"] }
```

`driver::commission` takes a `DriverConfig` — the same `CommissionerConfig` as
above, the passcode, and the controller's persistent commissioner operational
identity (its NOC plus PKCS#8 key, which the caller owns and stores) — along
with an `AsyncDatagram` transport and an mDNS `Discovery`. It then runs the
whole thing: resolve the commissionable device, PASE (SPAKE2+),
the poll loop with each action framed as an Invoke or Read over the right
session, mDNS find-operational, the CASE handshake, and `CommissioningComplete`.

`driver::commission_ble` is the same flow over a BLE/BTP `AsyncDatagram` — MRP
suppressed, since BTP is already reliable and ordered (spec §4.12) — with a
separate UDP transport for the operational phase. It does **not** contain a
Bluetooth stack: scanning and the GATT/BTP connection happen above this crate.
`matter-ble` provides them and `matter-controller` wires the two together.

The transport seam is the `AsyncDatagram` trait, so the driver is not tied to
one socket implementation; `InMemoryDatagram` is what the in-process end-to-end
tests commission over.

There is a runnable operator binary for the IP path:

```bash
cargo run -p matter-commissioning --example commission_ip --features driver -- --help
```

## Optional `tracing` feature

Enable per-method spans for observability:

```toml
matter-commissioning = { version = "...", features = ["tracing"] }
```

Span field names (`stage`, `expectation`) align best-effort with
matter.js's log-event format so operators can grep across both
implementations.

With `driver` on as well, `tracing` also emits a debug event per message
sent or received. These record direction, ids, protocol and opcode (some
also a length), but no message payload bytes. The only bytes they dump are
public or encrypted: the attestation certificates, the RCAC being installed,
and the CASE Sigma2 messages (whose secret parts are ciphertext).

The separate `wiretrace` feature (which implies `tracing`) is **for
debugging only, never production**: it makes those events record the full
decrypted plaintext of every commissioning exchange, including the IPK, the
Wi-Fi passphrase and the Thread dataset, so that `cargo xtask trace-diff`
can compare a run against matter.js.

## Byte parity

Every fixture in `test-vectors/commissioning/setup/` is captured from
matter.js by `cargo xtask capture-setup`. The integration test in
`tests/setup_byte_parity.rs` asserts that `encode_qr` / `encode_manual_code`
produce byte-identical output and that `parse_qr` / `parse_manual_code`
recover the same `SetupPayload`.

For attestation-response verification, `test-vectors/attestation/response/`
is captured by `cargo xtask capture-attestation`. The integration test
in `tests/attestation_response_byte_parity.rs` asserts that Rust and
matter.js's `NodeJsStyleCrypto.verifyEcdsa` produce the same
accept/reject verdict for a happy-path tuple plus four single-byte
mutations. (Byte-parity is on verdicts, not raw bytes — ECDSA's `k`
is randomized per signing call, so the captured signature varies
across script runs while the test assertions remain stable.)

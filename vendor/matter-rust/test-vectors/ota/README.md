# test-vectors/ota — OTA Software Update Provider (Milestone 9-F)

Wire vectors for the `OtaSoftwareUpdateProvider` (0x0029) command path: the
Provider's `QueryImage` → `QueryImageResponse`, `ApplyUpdateRequest` →
`ApplyUpdateResponse`, and `NotifyUpdateApplied` exchange.

## Status: roundtrip floor (F1); the live oracle is F4's end-to-end test

This directory holds only this README: there are **no chip-captured binary
vectors**, and that is deliberate. The F1 phase proves the *mechanism* and freezes a **roundtrip-floor**
vector in code — see
[`crates/matter-ota/tests/byte_parity.rs`](../../crates/matter-ota/tests/byte_parity.rs).
Those frozen command-fields hex strings are emitted by our own client+server
codecs (the generated `encode_query_image` and the hand-rolled Provider
handlers) and verified field-by-field through the full Interaction-Model
envelope. They guard against accidental wire-layout drift; they are **not** an
independent oracle.

### Why not chip-captured vectors at F1

The **live `ota-requestor-app` is the PRIMARY wire oracle**, run by F4's
end-to-end test
([`crates/integration-tests/tests/ota_flow.rs`](../../crates/integration-tests/tests/ota_flow.rs),
launched by `just integration-ota`). A real requestor rejects
a malformed `QueryImageResponse` or BDX block, so a successful
announce → query → BDX download → apply is itself strong wire evidence.

Capturing the *decrypted* `QueryImage`/`QueryImageResponse` bytes from chip's
`ota-provider-app` ↔ `ota-requestor-app` exchange is harder than past matter.js
captures (the payloads are CASE-encrypted) and needs both reference apps plus a
decrypted-payload extraction path. No connectedhomeip checkout is present in the
F1 build environment, so this is impractical here, and F1 deliberately **falls
back to the roundtrip floor**.

### Upgrading to chip bytes

The chip capture was never made. To replace the floor, capture the decoded
command-fields TLV the chip apps log at high verbosity while the live test runs,
and drop them in as `query_image.json` /
`query_image_response.json` (hex string + a provenance note), then upgrade
`byte_parity.rs` to assert against the chip bytes instead of the self-generated
floor.

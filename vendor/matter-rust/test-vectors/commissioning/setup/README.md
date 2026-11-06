# test-vectors/commissioning/setup

Byte-parity fixtures for `matter-commissioning::setup`, captured from
matter.js by `cargo xtask capture-setup`.

## On-disk format

Each JSON file is one fixture:

```json
{
  "intent": "human-readable description",
  "input": { /* matter-rust SetupPayload shape */ },
  "expected": { "qr": "MT:..." } | { "manual": "..." }
}
```

The `input` shape mirrors matter-rust's `SetupPayload`:

- `version`: u8 (always `0` today)
- `vendor_id`: u16 | null
- `product_id`: u16 | null
- `commissioning_flow`: `"Standard"` | `"UserIntent"` | `"Custom"`
- `discovery_capabilities`: array of `"SoftAp"` | `"Ble"` | `"OnNetwork"`
- `discriminator`: u16 in `0..=0xFFF`
- `passcode`: u32 in `1..=99999998` (Core Spec §5.1.7.1 range), never a spec-disallowed value

## Capture procedure

1. `cd xtask/scripts/capture-setup`
2. `npm install` (one-time)
3. `cd - && cargo xtask capture-setup`

The script overwrites every `*.json` in this directory. Run it whenever
the fixture inputs change or the matter.js dependency is bumped.

## Why these vectors

Each scenario in `xtask/scripts/capture-setup/index.js` covers an edge of
the payload space:

- `qr-spec-example` — the Matter Core Spec §5.1.3.1 worked example.
- `qr-minimal` — mid-range fields, single discovery transport.
- `qr-all-discovery` — every discovery bit set.
- `qr-user-intent` — UserIntent flow.
- `qr-high-vid-pid` — VID/PID near the 16-bit ceiling.
- `qr-edge-discriminator-0` / `qr-edge-discriminator-fff` — discriminator extremes.
- `qr-edge-passcode-min` / `qr-edge-passcode-max` — passcode extremes.
- `manual-11-minimal` / `manual-11-mid` — 11-digit form, two values.
- `manual-21-with-vidpid` / `manual-21-edges` — 21-digit form, two values.

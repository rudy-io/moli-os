# matter-ble

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../LICENSE)

Matter **BLE commissioning transport** — the BTP (Bluetooth Transport Protocol)
engine, plus an optional BLE central role for commissioning a factory-fresh
device over Bluetooth.

Part of [`matter-rust`](https://github.com/phunapps/matter-rust), a Rust Matter
controller. Most users want [`matter-controller`](../matter-controller/), which
drives this crate for you via `MatterController::commission_ble`.

## What's here

Two layers, split so the protocol is testable without a radio:

- **The BTP engine (always compiled, sans-IO).** Commissionable-advertisement
  parsing, the handshake request/response codec, and `BtpSession` — RX
  reassembly, TX segmentation, window and ack-timeout accounting, sequence
  wraparound. No I/O, no async, no Bluetooth stack: you feed it bytes and it
  tells you what to send. Dependencies are `bitflags` and `thiserror`.
- **The BLE central role (feature `central`).** A [btleplug](https://docs.rs/btleplug)
  central that scans for a device by discriminator, connects, opens the Matter
  GATT service (C1 write / C2 indication), runs the handshake, and drives the
  session from an async pump task — exposing a `BtpChannel` that sends and
  receives whole Matter messages.

The engine is verified byte-for-byte against `connectedhomeip`'s
`TestBleLayer`/`TestBtpEngine` vectors.

```toml
[dependencies]
matter-ble = "0.3"                                   # BTP engine only
matter-ble = { version = "0.3", features = ["central"] }  # + BLE central role
```

## Platform support

**Live commissioning works on Linux/BlueZ. It does not work on macOS
(CoreBluetooth)** — scanning is fine, but the first GATT operation fails, before
PASE. The cause is root-caused and confirmed on hardware: CoreBluetooth rejects
the CHIPoBLE operations with `CBError.uuidNotAllowed`, which also explains the
long-standing macOS `chip-tool` BLE GATT-write failures on the same rig — it is
not specific to this crate. The central bounds every wait, so macOS fails within
~15-30 s with a clear error rather than hanging. Full writeup and options are in
[`TODO-1.0.md`](../../TODO-1.0.md). Drive live BLE commissioning from Linux.

`central` pulls platform Bluetooth stacks (libdbus on Linux, CoreBluetooth on
macOS), which is why it is opt-in. On macOS, constructing a `BleCentral` triggers
the one-time Bluetooth permission prompt (TCC) — do it from a user-initiated
flow, never at library init.

## Status

Validated against real hardware: an ESP32-C6 commissioned over BLE onto both
Wi-Fi and Thread. See [`docs/tested-devices.md`](../../docs/tested-devices.md).

Pre-1.0 — the API may change. Breaking changes bump the minor version while 0.x.

## License

[Apache 2.0](../../LICENSE).

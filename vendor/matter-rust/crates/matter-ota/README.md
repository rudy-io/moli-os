# matter-ota

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../LICENSE)

Matter **OTA Software Update Provider** — the controller-side half of Matter's
firmware-update flow (Core Spec §11.20, cluster `OtaSoftwareUpdateProvider`
`0x0029`).

Part of [`matter-rust`](https://github.com/phunapps/matter-rust), a Rust Matter
controller. Most users want [`matter-controller`](../matter-controller/), which
wires this crate into a working provider via `MatterController::serve_ota`.

## What's here

A controller is the actor that *pushes* firmware to the devices it manages, so
this crate implements the **Provider** role — answering a device's `QueryImage`,
authorising its `ApplyUpdateRequest`, and acknowledging `NotifyUpdateApplied`.
The device-side Requestor (`0x002A`) is not implemented; that belongs to a device
stack such as [`rs-matter`](https://github.com/project-chip/rs-matter).

This crate is **pure command-handler logic**: decoded request TLV in, response
TLV out. No sockets, no CASE sessions, no image transfer. That separation keeps
the protocol testable without a network:

- the **image bytes** travel over BDX — see [`matter-bdx`](../matter-bdx/);
- the **server** that owns the socket, advertises over mDNS, accepts CASE, and
  routes IM vs BDX by protocol ID lives in
  [`matter-controller`](../matter-controller/).

```toml
[dependencies]
matter-ota = "0.6"
```

## Status

Validated in-process end to end: a requestor drives `QueryImage` → BDX transfer →
`ApplyUpdateRequest` → `NotifyUpdateApplied`, and reassembles the image
byte-exactly. Byte-grounded against `connectedhomeip`. For a live run against
chip's `ota-requestor-app`, see
[`docs/runbooks/m9-f4-ota-end-to-end.md`](../../docs/runbooks/m9-f4-ota-end-to-end.md).

Pre-1.0 — the API may change. Breaking changes bump the minor version while 0.x.

## License

[Apache 2.0](../../LICENSE).

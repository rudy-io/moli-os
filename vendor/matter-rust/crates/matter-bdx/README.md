# matter-bdx

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../LICENSE)

Matter **BDX** (Bulk Data Exchange, Core Spec §11.21) — the protocol Matter uses
to move large payloads, most visibly OTA firmware images, over an exchange.

Part of [`matter-rust`](https://github.com/phunapps/matter-rust), a Rust Matter
controller.

## What's here

Message codecs for the nine BDX message types the transfer needs — `SendInit`
(0x01), `SendAccept` (0x02), `ReceiveInit` (0x04), `ReceiveAccept` (0x05),
`BlockQuery` (0x10), `Block` (0x11), `BlockEOF` (0x12), `BlockAck` (0x13),
`BlockAckEOF` (0x14) — and `BlockSender`, a **receiver-driven, sans-IO** sender
state machine. Abort reasons surface as `BdxStatusCode`; the `StatusReport`
message that carries one on the wire is a Secure Channel message, encoded by the
caller.

Sans-IO by design: `BlockSender` takes an incoming BDX message and returns the
message to send, with no sockets or async involved. The caller owns the
transport, which is what lets the transfer be tested exhaustively without a
network. Dependencies are `bitflags` and `thiserror` — nothing else, not even the
other `matter-*` crates.

Receiver-driven means the receiver asks for each block (`BlockQuery`) and the
sender answers; the sender never pushes. EOF is decided by the rule chip uses:
the final block is the one where `offset + len == image.len()`.

Byte-grounded against `connectedhomeip`'s `BdxMessages`, `BdxTransferSession`,
and `BdxOtaSender`.

```toml
[dependencies]
matter-bdx = "0.3"
```

## Scope

The sender role only — enough for a controller to serve an OTA image. There is no
receiver/downloader here (a device stack's job), and no session, exchange, or
socket handling: [`matter-controller`](../matter-controller/) drives this over a
CASE session for [`matter-ota`](../matter-ota/).

Within the sender role, `BlockSender` implements the receiver-driven happy path
plus abort. Transfer resumption, acting on `BlockQueryWithSkip` (the message
decodes, but the sender does not honour a skip), and sender-drive mode are not
implemented. BDX `Metadata` is an opaque byte passthrough — empty for OTA — so
no TLV dependency is needed.

## Status

Validated in-process: a requestor drove a full OTA transfer and reassembled a
2500-byte image byte-exactly.

Pre-1.0 — the API may change. Breaking changes bump the minor version while 0.x.

## License

[Apache 2.0](../../LICENSE).

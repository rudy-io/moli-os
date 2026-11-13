# matter-codec

Matter TLV (Tag-Length-Value) encoder and decoder.

Part of the [`matter-rust`](https://github.com/phunapps/matter-rust) workspace.

[![Crates.io](https://img.shields.io/crates/v/matter-codec.svg)](https://crates.io/crates/matter-codec)
[![Docs.rs](https://docs.rs/matter-codec/badge.svg)](https://docs.rs/matter-codec)

## What this crate does

Encodes and decodes Matter TLV per the Matter Core Specification §A.2:

- All 11 element types: `bool`, signed/unsigned integers (1/2/4/8 byte),
  IEEE 754 floats (4/8 byte), UTF-8 strings, octet strings, `null`,
  structures, arrays, lists.
- All 5 tag forms: anonymous, context-specific, common profile, implicit
  profile, fully qualified. Writer picks the minimum-width sub-form (2 vs
  4 byte tag numbers; 6 vs 8 byte fully-qualified).
- All 4 length-field widths for strings and octet strings (1/2/4/8 byte
  little-endian). Writer picks the minimum.
- Recursive `read_value` / `write_value` for containers, with a 32-level
  nesting limit enforced by the reader.

Reading is available element-at-a-time, zero-copy (`ElementRef` /
`ValueRef` borrow strings and octet strings straight out of the input),
or as a whole tree. Containers you do not care about can be skipped
outright with `skip_container`, or with `skip_container_span` when you
want the raw bytes back to forward verbatim.

```toml
[dependencies]
matter-codec = "0.3"
```

## What this crate does not do

- Cluster definitions. See
  [`matter-clusters`](https://crates.io/crates/matter-clusters).
- Certificates. See [`matter-cert`](https://crates.io/crates/matter-cert).
- Interaction Model messages. See
  [`matter-interaction`](https://crates.io/crates/matter-interaction).
- Anything network. See
  [`matter-transport`](https://crates.io/crates/matter-transport).
- Cryptographic primitives. See `ring` / `aws-lc-rs`.

## Usage

```rust
use matter_codec::{Tag, TlvReader, TlvWriter, Value};

let mut bytes = Vec::new();
let mut writer = TlvWriter::new(&mut bytes);
writer.put_uint(Tag::Context(0), 42)?;
assert_eq!(bytes, [0x24, 0x00, 0x2A]);

let mut reader = TlvReader::new(&bytes);
let (tag, value) = reader.read_value()?;
assert_eq!(tag, Tag::Context(0));
assert_eq!(value, Value::Uint(42));
# Ok::<(), matter_codec::Error>(())
```

`read_value` builds an owned tree. To avoid that allocation, call
`TlvReader::next()` (returning `Element::Scalar`,
`Element::ContainerStart`, or `Element::ContainerEnd`, with nesting
depth tracked internally) or `TlvReader::next_ref()` for the borrowing
`ElementRef` / `ValueRef` equivalents.

## Correctness posture

`matter-codec` is verified by:

- **Spec test vectors** transcribed from Matter Core Spec §A.2 (in the
  `test-vectors/tlv/` directory of the workspace), cross-checked against
  matter.js's TLV codec at capture time.
- **An integration test** that loads every captured vector and asserts
  byte-for-byte encode equality and structural decode round-trip.
- **`proptest` round-trip property** over arbitrary `(Tag, Value)` trees
  bounded to depth 4 — `decode(encode(v)) == v`.
- **A `cargo-fuzz` target** (`fuzz_decode`) that runs `TlvReader::read_value`
  on adversarial bytes. The weekly CI workflow runs it for 5 minutes every
  Monday. The decoder never panics; malformed input surfaces as `Error`.

If `matter-codec`'s output differs from matter.js for the same input, we
treat it as a bug in `matter-codec` and investigate.

## Cryptographic posture

`matter-codec` performs no cryptography. It is pure data encoding.

## MSRV

Rust 1.89. The workspace MSRV was raised to 1.89 on 2026-09-08 so
AES-CCM could move onto the current RustCrypto generation (`ccm` 0.6
requires `aes` 0.9, which requires rustc 1.89). It had been raised from
1.75 to 1.88 on 2026-05-24 to land patched `time >= 0.3.47`
(RUSTSEC-2026-0009) pulled in transitively by `x509-parser` / `asn1-rs`
in the `matter-commissioning` crate. See the workspace `CHANGELOG.md`.

## License

Apache 2.0. See `LICENSE` at the workspace root.

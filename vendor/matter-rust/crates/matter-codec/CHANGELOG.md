# Changelog

All notable changes to `matter-codec` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- MSRV raised from Rust 1.75 to Rust 1.88 (workspace-level bump).
  See workspace `CHANGELOG.md` for rationale.

## [0.1.0] - 2026-05-17

First publishable release.

### Added

- `Tag` enum with all 5 spec-defined tag forms (Anonymous,
  Context(u8), CommonProfile(u32), ImplicitProfile(u32),
  FullyQualified { vendor, profile, tag }).
- `Value` enum with all 11 element types (Bool, Uint, Int, Float,
  Double, Utf8, Bytes, Null, Structure, Array, List).
- `TlvWriter` with `put_*` primitives for every scalar plus
  `start_structure` / `start_array` / `start_list` / `end_container`
  plus a recursive `write_value` convenience. Minimal-width encoding
  for integers, tags, and length fields.
- `TlvReader` with `next()` streaming primitive returning
  `Element::Scalar` / `ContainerStart` / `ContainerEnd`, plus a
  recursive `read_value` tree-builder. Container nesting limited
  to `MAX_DEPTH = 32` levels per spec recommendation.
- `Error` enum with `thiserror::Error` derive covering every wire
  failure mode (UnexpectedEof, InvalidTagControl, InvalidElementType,
  InvalidUtf8, IntegerOutOfRange, BufferTooSmall, LengthOverflow,
  UnexpectedEndOfContainer, UnclosedContainer, ContainerTooDeep).

### Test coverage

- 49 writer unit tests covering every put_* method at every width
  boundary, every tag form, and the recursive write_value dispatch.
- 48 reader unit tests covering every element type, every tag form,
  three streaming error paths, and the recursive read_value tree
  builder with three additional container-specific error paths.
- 5 internal unit tests for the private `element_type` and
  `tag_control` modules (2 + 3 respectively).
- 1 integration test loading 24 spec-derived TLV vectors and
  asserting byte-for-byte encode + structural decode round-trip.
- 2 proptest round-trip properties (scalar + full Value with
  containers up to depth 4) at 256 cases each.
- A `cargo-fuzz` target (`fuzz_decode`) that runs `read_value` on
  arbitrary bytes; weekly 5-minute CI run.

### Notes

- MSRV is Rust 1.75.
- No cryptographic primitives are implemented in this crate.

[0.1.0]: https://github.com/phunapps/matter-rust/releases/tag/matter-codec-v0.1.0

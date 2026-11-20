# test-vectors/bdx — Bulk Data Exchange (Milestone 9-F2)

Byte-parity vectors for the BDX message codecs (Matter Core §11.21).

## Provenance: hand-derived from chip's `BdxMessages.cpp`

The frozen hex in `crates/matter-bdx/src/messages.rs` tests is **hand-derived
from the wire layout in connectedhomeip `src/protocols/bdx/BdxMessages.cpp`**
(the authoritative spec implementation) — not captured from a live exchange.
BDX bodies are simple little-endian structures, so the layout is reproduced
field-by-field and annotated in each test.

These are a **regression guard** against wire-layout drift. The **live
`ota-requestor-app` is the PRIMARY wire oracle** and validates the BDX transfer
end-to-end in phase F4 (it pulls and reassembles our blocks — a successful
download is itself strong wire evidence). If a captured vector becomes available
(decrypted BDX payloads from a chip provider↔requestor run), drop it here as JSON
and upgrade the codec tests to assert against it.

# test-vectors

Binary fixtures captured from `matter.js` (and, where applicable, the Matter
Core Specification's own test vectors) used to validate `matter-rust` outputs
byte-for-byte.

Layout:

```
tlv/      Milestone 0–1 — TLV encoding test cases (see test-vectors/tlv/README.md)
certs/    Milestone 2 — Matter certificate samples
pase/     Milestone 3 — PASE / SPAKE2+ exchanges
case/     Milestone 4 — CASE / SIGMA exchanges
```

For TLV specifically, see [test-vectors/tlv/README.md](tlv/README.md).

Each subdirectory has its own `README.md` (added when the corresponding
milestone starts) describing the capture procedure and the on-disk format.

Vectors are version-controlled. They are stable inputs to our tests; changing
one without a documented reason is a red flag in code review.

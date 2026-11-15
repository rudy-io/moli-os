# Matter certificate test vectors

Captured Matter operational certificates used by `matter-cert` to verify
parsing and round-trip serialisation byte-for-byte.

Source: `@matter/protocol` 0.16.11 — `CertificateAuthority` generates fresh
RCAC, ICAC, and NOC certificates via `NodeJsStyleCrypto`, then serialises them
as Matter TLV with `asSignedTlv()` (first byte `0x15`, anonymous structure).

## File layout

```
manifest.toml       index of every captured certificate
rcac-no-icac.bin    RCAC for a root-only fabric (no intermediate CA)
icac.bin            ICAC (intermediate CA) from a three-tier fabric chain
noc.bin             NOC (node operational cert) for node 1 in fabric 1
```

## Regenerating

```
cargo xtask capture-cert
```

Prerequisite: `npm install` inside `xtask/scripts/capture-cert/`. The
committed `package-lock.json` pins the matter.js version so two contributors
regenerating produce structurally identical certificates (though the key bytes
will differ because keys are generated fresh each run).

**Important:** regenerating replaces the `.bin` files. If the integration test
(`crates/matter-cert/tests/certificates.rs`) already exists it will continue
to pass because it tests round-trip on whatever bytes are committed, not
against a fixed expected byte sequence.

## How the Rust harness uses these

`crates/matter-cert/tests/certificates.rs` (added in M2.1) loads
`manifest.toml`, parses each `.bin` via `MatterCertificate::from_tlv`,
asserts any optional expected-field annotations in the manifest, then
re-serialises via `to_tlv` and asserts byte-for-byte equality with the
original `.bin`.

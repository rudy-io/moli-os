# Happy-path attestation chain fixtures

DAC + PAI test certificates vendored from
[project-chip/connectedhomeip](https://github.com/project-chip/connectedhomeip)
at commit `43aa98c2d30ee547c6b587b9de7bbb794f175ece` (tag `v1.4.0.0`),
from `credentials/test/attestation/`.

> Note: at `v1.4.0.0` the `Chip-Test-*` fixtures live under
> `credentials/test/attestation/`, not
> `credentials/development/attestation/`.

| File | Role | Subject VID | Subject PID |
|---|---|---|---|
| `Chip-Test-DAC-FFF1-8000-0004-Cert.der` | DAC | `0xFFF1` | `0x8000` |
| `Chip-Test-PAI-FFF1-8000-Cert.der` | PAI | `0xFFF1` | `0x8000` |

Both chain to `crates/matter-commissioning/src/attestation/csa_test_roots/Chip-Test-PAA-FFF1-Cert.der`.

## Scope (M6.2.1)

Consumed by the parser tests added in M6.2.1 (X.509 DER ingestion of
DAC and PAI, including extraction of the Matter VID/PID DN
attributes). The integration test file and exact API names are
defined during M6.2.1 implementation.

Chain validation against the PAA root happens in M6.2.2.
`AttestationResponse` signature verification happens in M6.2.3.

## License

Apache License 2.0.

## Re-vendoring

To update against a newer upstream tag:

1. Clone the upstream repo at the target tag (shallow):
   ```bash
   git clone --depth 1 --branch <tag> \
     https://github.com/project-chip/connectedhomeip.git /tmp/chip-attestation-vendor
   ```
2. Copy `Chip-Test-DAC-FFF1-8000-0004-Cert.der` and
   `Chip-Test-PAI-FFF1-8000-Cert.der` from
   `/tmp/chip-attestation-vendor/credentials/test/attestation/` into
   this directory. (Note: the path was
   `credentials/development/attestation/` in older releases.)
3. Record the new upstream commit SHA and tag near the top of this
   README — keep it in sync with
   `crates/matter-commissioning/src/attestation/csa_test_roots/README.md`,
   which must reference the same commit.
4. Run `cargo test -p matter-commissioning` to confirm the parser
   tests still pass with the new DER.

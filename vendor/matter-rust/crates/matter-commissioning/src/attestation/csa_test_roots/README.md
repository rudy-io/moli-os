# CSA test PAA roots

These DER-encoded Product Attestation Authority (PAA) certificates are
vendored from
[project-chip/connectedhomeip](https://github.com/project-chip/connectedhomeip)
at commit `43aa98c2d30ee547c6b587b9de7bbb794f175ece` (tag `v1.4.0.0`),
from the directory `credentials/test/attestation/`.

> Note: some references name
> `credentials/development/attestation/`. At `v1.4.0.0` the
> `Chip-Test-*` fixtures live under `credentials/test/attestation/`
> instead (byte-identical copies of the two PAAs also exist under
> `credentials/development/paa-root-certs/`). Filenames are unchanged.

| File | Subject VID | Purpose |
|---|---|---|
| `Chip-Test-PAA-FFF1-Cert.der` | `0xFFF1` | VID-scoped PAA used by the test attestation chain (DAC/PAI for VID `0xFFF1`, PID `0x8000`). |
| `Chip-Test-PAA-NoVID-Cert.der` | (none) | Non-VID-scoped PAA. Exercises the `Paa::subject_vid() -> Option<VendorId>` path. |

## Why these files live inside the crate

`PaaTrustStore::with_csa_test_roots()` embeds them at compile time via
`include_bytes!`. cargo only packages files under the crate root in the
published tarball, so keeping these inside the crate guarantees
`with_csa_test_roots()` keeps working if/when `matter-commissioning` is
ever published to crates.io. (Publishing is currently deferred.)

## License

Apache License 2.0 — same as both upstream connectedhomeip and this
crate. No additional NOTICE attribution is required by the upstream
LICENSE file.

## Re-vendoring

To update against a newer upstream tag:

1. Clone the upstream repo at the target tag (shallow):
   ```bash
   git clone --depth 1 --branch <tag> \
     https://github.com/project-chip/connectedhomeip.git /tmp/chip-attestation-vendor
   ```
2. Copy `Chip-Test-PAA-FFF1-Cert.der` and `Chip-Test-PAA-NoVID-Cert.der`
   from `/tmp/chip-attestation-vendor/credentials/test/attestation/` into
   this directory, overwriting the existing files. (Note: the path was
   `credentials/development/attestation/` in older releases. If the
   files aren't where this README expects them, search the upstream
   tree with `find /tmp/chip-attestation-vendor -name 'Chip-Test-PAA-FFF1-Cert.der'`.)
3. Record the new upstream commit SHA and tag near the top of this
   README.
4. Run `cargo test -p matter-commissioning attestation` to confirm the
   new DER still parses through the trust-store loader.
5. If parsing fails, the upstream cert profile may have changed
   meaningfully — escalate.

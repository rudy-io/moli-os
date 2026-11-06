# Vendored Matter library

Upstream: https://github.com/phunapps/matter-rust
License: Apache-2.0; see LICENSE (preserved verbatim).
Base: `c59b61fdd7cf0b9cfcd3b1257cb87f336e59a48d`.
Corrected source commit: `d80b1a3fd2204119ddf47406f4bb59916ba7576b`.
The correction history was obtained from https://github.com/Roddygithub/matter-rust;
this is provenance only, not a build dependency.

Corrections on top of the base:

- `9c8c323c221505fac1d7c322d5e0468c9410abaf` bound invoke sends by caller lifetime and roll back unsaved fabrics
- `130d07aa447da0df825f1e26562d0da39612e19f` fix(controller): abort timed exchanges after guarded send errors
- `429350c0171d62fbf911b7600231ba0480d4b875` fix(controller): purge retransmits when pending action guards end
- `d80b1a3fd2204119ddf47406f4bb59916ba7576b` Support Matter attestation VID/PID commonName fallback

## Export

Started with `git archive d80b1a3fd2204119ddf47406f4bb59916ba7576b | tar -x`.
Retained all eleven library crates, integration-tests, crate READMEs, examples,
benches, tests, all test-vectors, root README, LICENSE, Cargo.toml and Cargo.lock.
The initial library source and fixtures were exported verbatim; Moli-local adaptations are listed below. Optional BLE, OTA/BDX and unstable
provider functionality remain available. No .git, target or untracked data was exported. The upstream rustfmt configuration is retained.

Pruned only these root entries (not needed to build or run the retained tests):

- `.cargo`
- `.config`
- `.github`
- `.gitignore`
- `CHANGELOG.md`
- `CONTRIBUTING.md`
- `TODO-1.0.md`
- `deny.toml`
- `docs`
- `examples`
- `justfile`
- `scripts`
- `xtask` except `xtask/src/integration.rs`, embedded verbatim by an integration-tests unit test

Removed `xtask` from workspace members; otherwise preserved workspace metadata,
dependency definitions and lints. Cargo refreshes the retained lockfile to remove
the pruned tooling dependency closure. Moli excludes this nested workspace and
uses direct path dependencies into it.

## Validation

Run from this directory: `cargo test --workspace --locked`.
BLE compilation additionally requires the Linux D-Bus development library:
`cargo check -p matter-controller --all-features --locked`.
Hardware-dependent integration tests keep their upstream ignored/opt-in behavior.
The fixtures include public synthetic/test credentials; they are required for
cryptographic and protocol regression tests and must not be replaced by real data.
The ext_pan_id printed in thread_dataset.rs is a public synthetic test vector,
not a MAC address; any leak-check exception must match only that exact fixture
occurrence and must never bypass the private denylist or exempt this directory.

## Moli-local adaptations after the export

- Controller `Node::write_before` and `invoke_timed_before` carry absolute deadlines
  through existing guarded action replies; expired timed work is rejected before CASE setup.
- `matter-interaction::parse_invoke_response_checked` validates a single response,
  explicit status paths and duplicate members for the guarded invoke APIs. Legacy
  parser APIs retain their behavior; new regressions cover malformed acknowledgements.
- Controller and interaction-model tests cover these additional APIs and validation.

These adaptations are maintained inside Moli, not fetched from a personal fork.

- Guarded attribute writes additionally use a strict write-response parser, rejecting duplicate or mismatched acknowledgements.

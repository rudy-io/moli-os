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
The initial library source and fixtures were exported verbatim; Moli-local
adaptations are listed below. No .git, target or untracked data was exported. The
upstream rustfmt configuration is retained.

## Trimmed to what Moli builds and tests

Only the crates in Moli's dependency graph are kept: `matter-codec`, `matter-cert`,
`matter-crypto`, `matter-transport`, `matter-commissioning`, `matter-interaction`,
`matter-clusters` and `matter-controller`. Removed from the export:

- `crates/matter-ble`, `crates/matter-ota`, `crates/matter-bdx` and
  `crates/integration-tests`, with their `[workspace] members` entries;
- every `fuzz/`, `benches/` and `examples/` directory, i.e. the fuzzing corpora
  under `matter-codec` and `matter-commissioning`;
- the `ota` and `ble` features of `matter-controller` and their optional path
  dependencies, and the test-only `matter-ble` dev-dependency of
  `matter-commissioning` together with the two BLE transport tests it served
  (`commission_ble_loopback.rs`, `pase_over_btp.rs`). Default features of the
  nested workspace are now empty, which matches how Moli depends on it
  (`default-features = false`);
- the `xtask` workspace member, already pruned before this trim.

`test-vectors/` is kept: the integration tests of the retained crates read it
(certificate chains, cluster byte parity, TLV vectors), even though Moli's own
check script runs only those crates' library targets.

Upstream `README.md`, `LICENSE` and the upstream rustfmt configuration are kept
verbatim. Nothing else was rewritten: the retained sources are the exported ones
plus the corrections listed above.

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

Run from this directory: `cargo test --workspace --locked`. There is no BLE
target left to build, so no system D-Bus library is required.
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
The three corrections below that only make sense on top of them are also kept as
a three-commit series in [`MOLI-PATCHES/`](MOLI-PATCHES/README.md), ready to be
sent upstream in order.

- Guarded attribute writes additionally use a strict write-response parser, rejecting duplicate or mismatched acknowledgements.
- Deadline-bounded writes and invokes distinguish "never queued" from "queued,
  answer lost": the latter is `Error::OutcomeUnknown`, so a caller can report an
  unknown lock/cover state instead of a failure. A write list naming the same
  attribute twice is refused before any bytes are sent. A steady-state report
  that sets `SuppressResponse` together with `MoreChunkedMessages` is rejected
  (`InvalidAction`) and its exchange closed, instead of leaving the device
  waiting on an exchange nothing would ever answer. Moli surfaces the unknown
  outcome through `pilotes.matter.outcome_unknown`.
- Subscription priming and steady-state reports honor parsed `SuppressResponse`:
  Success remains reliable when requested; suppressed final steady-state reports
  flush their standalone MRP ACK and close, while priming/chunks retain their
  follow-up exchange semantics. A focused actor regression covers delivery,
  standalone ACKs, handshake completion and repeated-report exchange reclamation.
  Trace diagnostics contain only max interval, suppression and attribute count.
- The host idle-report failure was traced to firewall drops at the ephemeral
  UDP listener, not established as a liveness-grace defect. The five-second
  grace is unchanged; no timeout increase or periodic polling was introduced.

- Controller builder `listen_port(u16)` selects the operational UDP bind port in
  both default and custom discovery paths. The default and `MatterController::open`
  retain port `0` (ephemeral); commissioning keeps its separate transient socket.
  This supports administrator-managed scoped inbound firewall rules for idle
  subscription reports without polling/keepalives or SDK firewall changes.
  Focused builder tests check the default and the actual configured socket port
  through the custom-discovery spawn seam, without mDNS or hardware.

# Matter-rust patch status

Checked 2026-11-15 against upstream `phunapps/matter-rust` main at
`c59b61fdd7cf0b9cfcd3b1257cb87f336e59a48d`.

The three commits below fix real issues, but **are not directly applicable to
that upstream revision**. They were prepared on top of the four controller
fixes recorded in `../MOLI-PROVENANCE.md` (`9c8c323`, `130d07a`, `429350c`,
`d80b1a3`) and Moli's local API adaptations (absolute-deadline methods and
strict response parsers). A dry-run of `git am` against the upstream checkout
failed on patch 1 in `actor.rs` and `node.rs`; therefore no upstream PR was
opened. Rebase the prerequisites first, then regenerate and test each focused
patch against the resulting upstream base.

```
0001-outcome-unknown.patch           fix(controller): report an unknown outcome when the deadline fires after queueing, including an Operational reply delivered after expiry
0002-duplicate-writes.patch          fix(controller): refuse duplicate write paths before sending
0003-suppressed-chunked-report.patch fix(controller): reject a suppressed response that promises more chunks
```

The saved series can be applied only to a `matter-rust` tree that already
carries those adaptations:

```sh
git am 0001-outcome-unknown.patch 0002-duplicate-writes.patch 0003-suppressed-chunked-report.patch
```

## What was checked locally

- `git am` of the three patches on the vendored tree from before these fixes:
  clean, three commits created.
- The resulting tree is identical, file by file, to `crates/` here after the
  fixes were applied in Moli. Only `MOLI-PROVENANCE.md`, a Moli file, is not
  part of the series.
- Each step compiles alone: `cargo check -p matter-controller
  --no-default-features` passes without a warning after commit 1 and after
  commit 2, so the series is bisectable.
- The regressions added are the ones already in the tree:
  `deadline_node_apis_abandon_queued_work` (updated for the new variant),
  `a_duplicate_write_path_is_refused_before_sending`,
  `a_queued_request_never_answered_reports_an_unknown_outcome`, and
  `a_suppressed_chunked_report_is_rejected_instead_of_left_open`.

## Upstream follow-up

The next upstream work is to agree on and land the prerequisite deadline-aware
controller APIs and strict response parsing (or replace the patches with
upstream-native equivalents). Then rebase this series, run the focused
controller/interaction tests on upstream main, and submit only patches that
apply cleanly and stand alone. The refreshed patch 1 must also classify a reply
channel that closes after queue acceptance and malformed responses (including
timed invokes) as `OutcomeUnknown`; Moli's current vendored tree has regressions
for these cases. Do not send this snapshot as-is.

## Scope

No breaking API change: `Error` is `#[non_exhaustive]`, `OutcomeUnknown` is one
more variant, and existing method signatures are unchanged. The only behaviour
that changes is the ambiguous one: a deadline that elapses after the request was
queued no longer presents itself as a plain failure.

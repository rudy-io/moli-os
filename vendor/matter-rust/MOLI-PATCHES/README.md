# Three commits for upstream

These three patches are the local corrections made on top of the four review
fixes already in `MOLI-PROVENANCE.md` (`9c8c323`, `130d07a`, `429350c`,
`d80b1a3`) and of Moli's local adaptations (absolute deadlines in
`write_before`/`invoke_before`/`invoke_timed_before`, strict response parsers).
They are provided as separate commits so they can be sent upstream one by one.

```
0001-outcome-unknown.patch           fix(controller): report an unknown outcome when the deadline fires after queueing
0002-duplicate-writes.patch          fix(controller): refuse duplicate write paths before sending
0003-suppressed-chunked-report.patch fix(controller): reject a suppressed response that promises more chunks
```

Apply in this order, from the root of a `matter-rust` tree that already carries
those adaptations:

```sh
git am 0001-outcome-unknown.patch 0002-duplicate-writes.patch 0003-suppressed-chunked-report.patch
```

## What was checked

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

## Scope

No breaking API change: `Error` is `#[non_exhaustive]`, `OutcomeUnknown` is one
more variant, and existing method signatures are unchanged. The only behaviour
that changes is the ambiguous one: a deadline that elapses after the request was
queued no longer presents itself as a plain failure.

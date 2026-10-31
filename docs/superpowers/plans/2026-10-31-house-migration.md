# House migration implementation plan

> Execute with `subagent-driven-development`, `test-driven-development` and
> `verification-before-completion`. Each task is a separate reviewable branch.

**Goal:** modular, tested migration integrations with inexpensive upstream merges.
**Design:** `docs/superpowers/specs/2026-10-31-house-migration.md`.
**Base:** upstream `c627b87c3f4ca74f124260f4894f3311a11c030e`.

## Task 1 — Fork discipline and upstream updates (controller)

Own `docs/fork/`, `scripts/fork-*`, their tests and optional fork-only workflow.

1. Preserve `main`; create `fork/integration` and independent feature branches.
2. Run and record baseline container checks and frontend checks.
3. Test a helper on disposable Git repositories: clean merge, conflict, dirty
   tree, missing upstream, repeated invocation. Never push or reset implicitly.
4. Implement the smallest helper that preserves history and leaves conflicts
   reviewable. Document updating, releases and rollback.
5. Run the full required checks, review and commit.

## Task 2 — Native Airzone (isolated implementer)

Own `crates/moli-airzone/`, `integrations/airzone/` and
`locales/{fr,en}/airzone.json`. Small registry/dependency changes belong to the
task branch and are reconciled by the controller during integration.

1. Read project guidance and primary protocol sources. Document provenance.
2. Write failing behaviour tests with synthetic responses: zone identity,
   capabilities, system modes, on/off, setpoint, fan levels and malformed data.
3. Implement the parser and command encoder; reuse runtime/network contracts.
4. Test HTTP verbs, paths and bodies against a loopback server, write failure,
   deadline/no replay and recovery without contacting a physical controller.
5. Add catalogue, translated messages, onboarding and examples.
6. Run full checks, inspect the diff, commit and submit a report for review.

## Task 3 — Native local Overkiz (isolated implementer)

Own `crates/moli-overkiz/`, `integrations/overkiz/` and
`locales/{fr,en}/overkiz.json`, plus minimal registration on its branch.

1. Confirm primary local API and device-definition semantics.
2. Write failing tests for device URL identity, io/RTS positions, advertised
   commands, malformed definitions and conservative handling of gates/windows.
3. Implement pinned HTTPS, vault token, state polling/events as needed and
   exact command encoding. Do not replay writes or infer execution success.
4. Exercise transport and authentication errors without real credentials.
5. Add catalogue, translations, onboarding and capability limitations.
6. Run full checks, commit and submit a report for review.

## Task 4 — Temporary Home Assistant bridge (isolated implementer)

Own `crates/moli-homeassistant/`, `integrations/homeassistant/` and
`locales/{fr,en}/homeassistant.json`, plus minimal registration on its branch.

1. Test explicit entity selection, unique stable aliases, immutable safety
   classification and an empty per-point write allowlist by default.
2. Test typed mappings for supported climate/light/switch/cover/vacuum/media/
   sensor/binary_sensor state and capability combinations, contact inversion,
   missing/unavailable data and refusal of stale commands.
3. Implement only supported primitive service calls. Reject opaque scenes,
   scripts, automation, group commands and unsafe openers rather than hiding
   downstream targets from Moli's guard.
4. Test bounded poll/write transport, no retries, absent/revoked vault token,
   auth failures, malformed attributes and aliases surviving entity renames.
   Verify no POST for denied/unknown effects and no generic or broad targets.
5. Document local/cloud dependency and per-entity native cutover.
6. Run full checks, commit and submit a report for review.

## Task 5 — Capability-driven controls (controller or isolated implementer)

Own climate/cover/media Svelte controls, small pure helpers, read-only device
classification and Node tests. Update
only the necessary frontend translations and CI test command.

1. Write failing tests for device numeric ranges/steps, read-only control,
   supported modes, missing state, availability and cover command capabilities.
2. Extract pure helpers and connect the existing UI to actual point contracts.
3. Verify existing drivers and new drivers use the same contract.
4. Build, run Node and i18n checks, inspect local rendered controls if feasible.
5. Run the full project checks and commit.

## Task 6 — Integration and independent review (controller + reviewer)

1. Review each task's contract and implementation independently; fix concrete
   findings before cherry-picking. Keep shared registration changes small.
2. Integrate commits into `fork/integration`, resolving only genuine overlaps.
3. Run full container checks and frontend checks on the final exact revision.
4. Exercise `check-config` and catalogue with synthetic configurations, plus
   local simulators; do not send commands to the home.
5. Independent whole-branch review: credentials, safety routing, stale state,
   timeouts, native identities, integration seams and test quality.
6. Push clean feature/integration branches to the fork. Record results and
   remaining device-validation work; keep production deployment separate.

## Coordination

Independent implementers use separate worktrees, so concurrent work never
shares a working tree. They are not alone in the repository and must not revert
another task. Shared registry edits are kept minimal and merged centrally.
Local task briefs and progress stay under the ignored `.superpowers/sdd/`.
The controller owns final integration, checks and review. No private household
configuration, real identifiers or credentials enter the public fork.

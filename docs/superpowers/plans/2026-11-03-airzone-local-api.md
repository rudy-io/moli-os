# Airzone Local API Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task. Independent bounded components may be delegated under the existing user authorization.

**Goal:** Cover the documented Airzone Local API with safe runtime publication, addressed commands and an auditable test matrix.

**Architecture:** A shared field registry supplies typed points and encoders. The driver owns identity, inventory, scope and lifecycle; the client owns bounded transport and exact operations. Existing Moli controls render these points.

**Tech Stack:** Rust, Tokio, serde_json, existing moli-net/runtime/core; Svelte.

**Spec:** ../specs/2026-11-03-airzone-local-api-design.md

## Global Constraints

- No real-house calls, secrets or deployment; no production dependency additions.
- Existing zone IDs and guards remain; read_only defaults true.
- Explicit host, bounded responses, no arbitrary paths, no write retry.
- FR/EN strings; explicit units; absent/unknown capabilities never authorize writes.
- Sole user commit identity; full scripts/check.sh before each commit.

## Review Focus

- Conflicting case aliases or unknown modes must not enable a different command.
- Optional endpoint failure must disable its own writes without breaking healthy zones.
- Changed unit or membership after admission must reject the command.
- Double-setpoint-only and ERV-only devices must remain readable without fake thermostat fields.
- Read-only must survive stale metadata, all origins and direct transport use.

## Task 1 — Typed operations and read-only transport

Files: crates/moli-airzone/src/api.rs and its tests. Parent owns Config and runtime wiring.

Interface: retain exchange and acknowledge compatibility; add typed read requests for all read operations and GET alternatives, including bounded HTML. Config gains pub read_only: bool. Writes validate addressed envelopes and exact operation allowlist.

- [x] Add failing loopback tests for routes/methods/queries, read-only zero PUT, IAQ/system/integration acknowledgements and typed failures.
- [x] Observe failures, implement operations, run crate tests.

## Task 2 — Parameter registry

Files: crates/moli-airzone/src/fields.rs, fields_tests.rs, locales/{fr,en}/airzone_fields.json and integrations/airzone/coverage.json.

Interface: Scope enum Zone/System/Dhw/Iaq/Webserver/Version/Integration; points(scope, raw, read_only) -> Vec<(PointSpec, Value)>; encode(scope, raw, key, value) -> Result<(String, Json)>; normalize(raw) -> Result<Json>. Registry does not own device identities or networking.

- [x] Add failing tests for documented fields, aliases, bounded arrays/text, units, absent capabilities, setpoint/stage/angle/fan/IAQ relationships and invalid writes.
- [x] Implement shared registry and translated labels, retaining existing canonical zone point names.
- [x] Compare coverage with official schema and documented example-only fields; record errata.

## Task 3 — Runtime and resource scope

Files: lib.rs, model.rs, resources.rs and lifecycle tests.

- [x] Fail tests for default read-only, optional sections, DHW/IAQ/system entities, discovery and missing-member guards.
- [x] Integrate registry with existing zone identities; support double setpoints and non-thermostat resources.
- [x] Read optional metadata and sections, isolate failures, route exact single commands with fresh capabilities and matching units/members.
- [x] Verify command refusal, cancellation, recovery and no optimistic state.

## Task 4 — User access and documentation

Files: relevant generic web point/climate controls; integration README/onboarding; CLI only if needed for read-only diagnostic operations.

- [x] Test text commands and read-only/offline display using synthetic data.
- [x] Reuse existing views; expose diagnostics without rendering remote HTML or offering arbitrary write access.
- [x] Document every operation, compatibility caveat, setup and read-only default.

## Task 5 — Validation and publication

- [x] Run full scripts/check.sh, web tests/build/i18n and relevant model checks.
- [x] Independent review of protocol completeness, guard scope and read-only behavior; fix findings with regressions.
- [ ] Check staged files for private data and unrelated changes; commit coherent changes as user.
- [ ] Publish branch and PR with exact coverage and test/hardware limitations; verify remote head/check state.

Local validation on 2026-11-03: 1,159 Rust tests (90 Airzone), 86 web tests, build, 3,351 translation keys with zero hardcoded strings, 66 solar checks, and 107 model tests run (one skipped). Independent review findings have regression coverage. The visual evidence is a synthetic component bench, not a deployed dashboard. Publication is tracked by the pull request rather than asserted before the commit exists.

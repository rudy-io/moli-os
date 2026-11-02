# Somfy Cloud Observation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Execute task-by-task, with behavioral RED/GREEN and review gates.

**Goal:** Retrieve Somfy scenarios and programming through an optional, strictly read-only cloud observer, with private owner-only viewing and independent authentication.
**Architecture:** One overkiz driver with independently supervised local/cloud sources; fixed HTTP read endpoints plus OAuth, one token writer, encrypted bounded catalogue, owner-only cached API and a small System panel.
**Tech Stack:** Existing Rust/Tokio/serde/moli-net/runtime and Svelte5; avoid new runtime dependencies.
**Spec:** docs/superpowers/specs/2026-11-01-somfy-cloud-observation-design.md

## Global Constraints

Real house strictly read-only; no HA credential/session extraction, commands, activation or imported automations. Independent Somfy login; no server read-only OAuth scope is implied. Keep cloud changes on feature/tahoma-cloud-readonly, baseline d81b2be, without changing public native PR99. Run scripts/check.sh before each commit. Public fixtures fully synthetic. Existing pilot remains until a tested isolated trial is ready.

## Review Focus

- A site with several gateways is a whole setup inventory, never falsely gateway-filtered; auth candidates use BOB sub-site externalOID.
- A refresh response lost or persisted late cannot cause old-token replay, even after restart/reconnect.
- One successful source cannot revive stale devices from the other, remove them, or hide its warning.
- Unknown/failed/oversized programme data is never an empty inventory or silently truncated; wall-time/astronomical schedules are not falsely evaluated.
- Full action parameters and notification destinations never reach generic device/SSE/MCP/logs; owner-only cached reads expose no auth data.

### Task 1: bounded catalogue and protocol models

Files: create crates/moli-overkiz/src/inventory.rs and inventory/{model,parse,tests}.rs as useful; minimal pub mod inventory in lib.rs; synthetic fixtures integrations/overkiz/fixtures/cloud-*.json, manifest citations and CLOUD-FIXTURES.md provenance. No transport/runtime changes.
Interfaces: public inventory::Region (emea/apac/snaba), Scope (setup site_oid, verified anchor gateway_id, region), CollectionKind (Scenarios, CalendarDays, CalendarRules, Triggers, Schedule, DelayedSchedule); CollectionStatus and failure qualification; Catalog version1 with instance, connection_generation, scope, per-collection status/last-attempt/last-observation/bounded raw object definitions and interpretation warnings, optional timezone and target labels. Shared constants INVENTORY_KEY=cloud_inventory_v1, MAX_CATALOG_BYTES=1048576, MAX_COLLECTION_BYTES=524288, MAX_DEFINITION_BYTES=65536, MAX_DEPTH=24, MAX_NODES=32768, MAX_OBJECTS=2048, MAX_STRING_BYTES=8192, MAX_LABEL_BYTES=256, MAX_ID_BYTES=256. Public bounded parse/encode/decode/apply APIs named and documented in task report for consumers; raw body/definitions have redacted Debug. No tokens in this DTO.
- [x] Write failing tests for each official synthetic collection and linking refs, unknown wrappers preserved, empty200 vs malformed object/scalar, duplicate recognized IDs, limits at/over boundary, deep conditions, Unicode, typed params/order, failed update preserving prior data/time, cross-scope/generation refusal, native wall-time vs UTC-ms kept distinct and DAWN/DUSK unevaluated.
- [x] Implement pure validation and per-collection update; unknown bounded objects retained with partial interpretation, structural/list/size violations reject incoming collection. Target metadata retains URL/label only, location timezone only. No command creation or interpretation. Catalogue identities scope exact setup; anchor is not a gateway filter.
- [x] Tests prove encoded/decoded catalog meets same limits and Debug omits definition markers. Focused crate tests then fullcheck before commit.

### Task 2: constrained cloud HTTP and rotating OAuth

Files: create crates/moli-overkiz/src/cloud/{mod,client,session,protocol,tests}.rs as useful, add mod cloud; FR/EN overkiz keys used by errors. No controller/UI changes yet.
Interfaces: cloud::Config optional enabling marker/poll_secs(default1800,range300..86400), public bounded PendingRequest/RequestEnvelope and ConnectionState DTOs for API (Login, SelectSite, Refresh only; redacted Debug), hard-coded secret keys for session/pending/state. Session internal versioned generation/scope/phase/expiry/tokens, never serialized via API. Closed endpoint enum and production transport to fixed HTTPS hosts; injectable crate-private Transport for scripted tests. Session persistence via crate-private async SessionSink trait with save() acknowledgement, later channel adapter. CloudClient login/discover/select/refresh/read cycle consumes these interfaces. Document exact signatures in report.
- [x] RED tests assert method+host+path allowlist; GET exec/schedule permitted, every action/config/logout path absent; redirects not followed; form encoding Unicode,+,&,%,spaces; deadlines/body limits/error redaction.
- [x] Implement independent password grant -> Ginaite exchange -> BOB discovery pages20/max200 -> explicit candidate/setup selection -> scoped refresh. Source constants pinned to pyoverkiz; unknown country requires explicit allowed region, known region cannot be overridden. Whole setup preserved for multi-gateway choices. No password persisted after input consumption; no first-site selection.
- [x] RED/GREEN state-machine tests: checkpoint before refresh; serialized rotations; new refresh saved before reads; absent refresh retains previous only when response is otherwise valid; persistence failure retries new state, never old token; partial valid token response retains replacement refresh; lost response and restarting Refreshing require login;401 at most one refresh/re-read;403/404/429 semantics and Retry-After; no body/token leak in errors/Debug. Each collection independent; bounded cycle90s, requests10s, no fanout or automatic actuator replay.
- [x] Fullcheck and commit. Task3 owns actor wiring, so pure client tests must establish public safety contract now.

### Task 3: source-independent driver, atomic onboarding queue and snapshot persistence

Files: moli-overkiz/src/lib.rs, new cloud/worker.rs/runtime_tests.rs; narrow changes moli-runtime/src/{secrets,hub,driver}.rs plus meaningful vault tests. Existing local client/model stay unchanged unless required adapter; no duplicate transport/policy.
Interfaces: atomic SecretStore::insert_if_absent and take, scoped Hub/DriverCtx wrappers; one encrypted pending slot returns busy rather than overwrites. Controller polls only when cloud enabled, consumes request atomically, owns session/catalog/state writes and generation. Cloud worker uses bounded typed events + oneshot persistence ack and tracked cancellation; no cloned unrestricted Hub. Config.host becomes Option<String>, at least one source required; cloud-only skips all native credential/pin calls. Local actor/controller factoring may be chosen by implementer provided invariants/tests hold.
- [x] RED tests for atomic competing queue/take, missing vault, stale generations, cancellation while committing and lock/cache consistency. Secret transaction ACK must follow file sync/rename/parent-directory sync on Unix; offload blocking IO and retain serialization until completion after caller cancellation. Existing labels::write_atomic has no fsync and cannot by itself satisfy the session checkpoint contract. Keep correction scoped to secrets; avoid global IO redesign. Expose no HTTP generic secret getter. Document existing single-process data-dir assumption.
- [x] RED/GREEN real Hub tests: cloud-only no local calls; disabled cloud zero tasks/requests; delayed/failed cloud leaves native polls/preflight responsive; missing/parked local leaves cloud active; source-owned removal both ways; cold restart held-offline not revived by other source; warning aggregation/recovery; stop drops workers and no closed-channel spin.
- [x] Publish only read-only summary/count/freshness device; no full definitions; block every origin and stale writable summary metadata before native preflight. Preserve native45tests and guards. Snapshot and status encrypted; persist once per cycle, show stale on storage failure; bound catalogue and checkpoint before accepting state.
- [x] Queue requests: Login changes connection generation and invalidates old-scope catalogue exposure; SelectSite validates saved authenticated candidates; Refresh coalesces while cycle runs. Ignore/abort stale worker events after reconnect. Read-only cloud cannot write even if native read_only=false. No account access in tests.
- [x] Fullcheck and commit; report exact controller/API seams for Task4.

### Task 4: owner-only catalogue and connection UI

Files: new moli-api/src/overkiz.rs, module/routes/Cargo dependency; HTTP tests; web/src/maison/ui/SomfyCloud.svelte plus focused pure helpers/tests and Systeme.svelte integration; FR/EN locales.
Interfaces: GET /api/integrations/overkiz-cloud lists configured cloud instances/status (owner only); GET /api/integrations/overkiz-cloud/{instance}/catalog returns validated cached Catalog plus live freshness, only matching connection/scope; POST /api/integrations/overkiz-cloud/{instance}/requests atomically queues bounded Login/SelectSite/Refresh, returns202requestid or409busy. Handler uses people_api::is_owner, validates kind/cloud config, fixed keys; no Somfy request inside HTTP handlers. Compare authoritative read_session_binding with state/catalogue identity, permitting qualified same-identity stale history. Additional cached owner-only definitions/{collection}/{index}?revision=... GET supplies Rust-serialized JSON text without JS integer rounding; use SHA-256 of persisted catalogue bytes as revision (existing ring), return409 on mismatch, share all auth/bounds/binding logic.
- [x] RED/GREEN HTTP tests for unauthenticated LAN/guest/non-owner denial, correct owner/PIN fallback only without owners, auth before secret read, invalid instance, fixed-key isolation, no credentials/status leakage, malformed/oversized cached DTO rejection, old-generation catalog unavailable, cached GET zero upstream calls, queue conflict without password loss.
- [x] UI reuses System/Atelier style and current PIN mechanism; already-authenticated non-owner sees owner requirement, not repeated PIN. Loginpassword masked/cleared aftersubmit, explicit setup/gateway choice (all site gateways included), statuses and per-collection dates, view definitions on demand, escaped text, no localStorage, optional JSON download, manual refresh. No execute/edit/import/activate control. Poll onboarding only while pending with teardown/cancellation. Preserve opaque JSON fields without pretending unknown types are understood. All FR/EN.
- [x] Build and existing+focused web tests, i18n, fullcheck, simulated browser visual check with synthetic data only; commit.

### Task 5: documentation, whole-feature review and isolated trial

Files: integration README/onboarding/manifest/CLOUD.md and coverage updates, sample config, tests/fixtures provenance docs. Root owns delivery docs after implementation contracts settle.
- [x] Document genuine API evidence and Classic/modern qualification limits, session scope/read-only enforcement, all bounds, isolated configuration, user-only account connection steps. No cloud source snapshot/token/personal data in repo.
- [x] Independent whole-branch review, one fix wave and scoped re-review; full local test evidence at 9183b395.
- [x] Compare release/performance of a representative synthetic catalogue against baseline. Two runs and budget deviations are recorded in docs/PERFORMANCE.md; image size and unexplained relative memory variation remain open. Disabled-cloud task/timer invariants are tested; no new external runtime package.
- [ ] Fresh remote CI for the published branch (tracked by PR checks).
- [ ] Commit/push cloud feature; create draft PR on user's fork against feature/tahoma-unified to keep the cloud diff separate. Do not publish it into nativePR99 or message contributors without instruction.
- [x] Prepare isolated local read-only trial from tested build with persistent encrypted storage, keep current real-house pilot/HA intact. If no safe independent Somfy credential/session available, ask user to authenticate in prepared local UI (never in chat) as final connection step; continue all independent checks while waiting.
- [ ] Authenticated inventory retrieval if user completes login: six collections separately verified, report actual counts/status/scope, no effects. No claim of100% TaHoma parity or real-account validation before evidence.

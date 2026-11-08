# TaHoma saved-scenario execution

Implementation follows the user's explicit request to launch saved TaHoma scenarios. It is a separate dependent change: the cloud observer remains read-only by default.

## Provider contract

The primary Somfy Classic module `overkiz/api/action` in the official [bootstrap](https://classic.tahomalink.com/enduser-mobile-web/steer-html5-client/tahoma/bootstrap.js) defines `executeById` as POST `exec/${actionGroupOID}`, with an `execId` string acknowledgement. A fresh unauthenticated GET on **2026-11-04 at 15:37 UTC** returned 371,868 bytes, identical to the archived primary source. Its SHA-256 is `f8e4b8a50631a51ac22f2c38c4f9e4735f11dcc3f507f835290830b3a0c273ac`. This is an interoperability contract observed in Somfy’s own application, not a published, guaranteed API. It may change. This is distinct from POST `/exec/apply`, whose body contains an ad-hoc action group. No generic action payload, local scenario endpoint, scheduling change or cancellation is added.

Only fixed regional HTTPS hosts and the saved-scenario endpoint are allowed. An OID must be a bounded safe path identifier from the selected current catalogue. The accepted response acknowledges submission, not successful physical completion. POST is never retried, including after 401, timeout, transport error or an invalid response.

## Consent and command contract

The absence of encrypted consent means execution is disabled. An owner explicitly authorizes each scenario with a fresh Moli PIN. The global Overkiz `read_only` setting remains authoritative: activation is refused while it is true, and revocation remains possible. Enabling a scenario never changes global read-only or local device capabilities.

Consent binds the instance, connection generation, selected scope, scenario OID and exact raw definition. Changing the account, scope or scenario definition invalidates it. Catalogue revisions are SHA-256 hashes of the persisted catalogue bytes. Runtime command tokens additionally bind the whole catalogue revision; replay after a poll, scope switch or reconnection is refused.

The owner catalogue response adds `execution: { read_only, scenarios: [{ id, permitted, executable, point, value, reason }] }`. Reasons include `global_read_only`, `permission_required`, `stale`, `offline`, `not_supported`. The frontend sends the provided point and opaque enum value using the existing Hub command route. It never reconstructs actions or manufacturer parameters.

The owner-only `POST /api/integrations/overkiz-cloud/{instance}/scenario-permission` accepts `{ generation, revision, scenario_id, enabled, pin }`, verifies fresh PIN and scope, then queues an encrypted request. The generic observation request endpoint cannot submit permission changes. The existing single Controller persists consent; it does not introduce another session or token writer.

A scenario is a virtual Hub device with a generic public name, one enum `run` point and `Semantic::Control`. Raw definitions and private scenario names stay on owner-only surfaces. The machine-control guard requires human approval for agents and unauthenticated household callers, independent of room mapping. Guests are excluded from these devices at the shared room filter, even after room labelling and even with a valid human PIN session. This closes commands, snapshot, device reads and event visibility consistently. Every launch uses Hub's deadline and journal. No unknown physical target is assumed safe based on missing room information.

The actor serializes execution with token rotation and observation. Before one POST it verifies the selected gateway scope and fetches the current scenario definitions. A missing or changed definition rejects the command. Somfy exposes no conditional revision for this POST: its GET and execution are not atomic. An external modification in that interval can change the actions executed by the provider. Moli rechecks immediately before submission but cannot guarantee that provider-side race is absent. The command's abandonment/deadline is checked immediately before sending. Pending configuration requests cannot be overtaken by new commands. On restart no queued physical command is persisted or replayed.

## Verification

All automated execution evidence uses synthetic transport and synthetic vault fixtures. Tests cover default denial, scoped consent and revocation, global read-only, stale revisions and changed definitions, private data redaction, fixed route bounds, machine-control guard and journal, token/session persistence, cancellation before POST, and no POST retry. No household scenario is executed during development or validation.

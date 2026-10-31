# Modular house migration

Status: implementation design following the requested fork and migration audit.
Upstream starting point: `c627b87c3f4ca74f124260f4894f3311a11c030e`.

## Outcome

Add local Airzone and Somfy Overkiz integrations and an explicit, temporary Home
Assistant bridge. Keep upstream behaviour and the command hub as the single
write path. Enable a staged migration on an ARM64 Linux host without changing
the running home during development. A bridge is still a dependency on Home
Assistant; passing simulator tests does not establish hardware compatibility.

## Extension boundaries

- One native driver crate and one catalogue package per integration. Reuse
  `moli-net`, `moli-core` and `moli-runtime`; do not copy their supervision,
  validation, TLS, history or command-routing implementations.
- Keep registration changes in the binary small. Preserve the typed points
  contract so the same controls work in the dashboard and assistant.
- Use stable protocol identifiers; names and addresses are presentation and
  transport details. Explicit bridge aliases survive entity renames.
- Publish actual capabilities. Unknown capabilities remain read-only or absent.
  Do not invent positions, operating modes, temperatures or acknowledgements.
- Device snapshots drive state. A successful write means accepted, not executed.
  Writes have a bounded deadline and are never automatically retried.
- Secrets stay in the encrypted store; configuration and fixtures contain only
  documented placeholders. Local HTTPS keeps certificate pinning.
- User-facing strings are translated in French and English.

## Native Airzone

Use the local Webserver API: POST for reads, PUT for writes. Model the controller,
systems and zones from protocol responses. Expose temperatures, setpoint,
on/off and supported modes/fan levels only where justified by the API. Master
and zone controls must not invent independent system mode control. Validate
reported bounds and membership before encoding a command. Polling failure
marks data unavailable through normal supervision. The cloud-only Aidoo path
is outside this local driver and can remain behind the bridge.

## Local Somfy / Overkiz

Use developer-mode local HTTPS and a vault-held bearer token. Build capabilities
from the gateway's device definitions and states. Translate positions only when
reported; RTS equipment without feedback has no fabricated position. Match
supported open, close, stop and position commands explicitly. Do not treat an
execution identifier as confirmation that a motor moved. Openers with security
implications must not acquire an unguarded generic writable path. Unsupported
device classes remain absent/read-only, with a documented reason.
Known gates and windows remain read-only by default. An explicit opt-in pins
both the native URL and expected class; every enabled actuation point uses
`Semantic::Control` to require human authorization. A cycle-only motor gets a
distinct impulse command, never fabricated open/close semantics.

## Temporary Home Assistant bridge

Opt-in explicit entities with stable Moli aliases, polling and a narrow service
allowlist. Default to read-only; enable individual points per entity. Require a
reviewed immutable safety classification; unknown effects and unsafe openers
stay read-only. Refuse writes without a fresh, available capability snapshot.
Map
climate, light, switch, cover, vacuum, media and sensor capabilities using HA
state metadata, including contact inversion for binary sensors. Reject unknown
services and malformed values. Unavailable or
removed entities become unavailable. No arbitrary HA service API and no opaque
scene, script, automation or group execution: these can hide protected targets
from Moli's guard. HA cloud integrations remain cloud integrations behind this
bridge. Retire bridge entries individually after native integration validation.
Unknown HA templates, aliases and automation side effects cannot be inferred by
Moli: do not enable writes until their actual effects have been reviewed. HA
scenes remain in HA during the pilot or become reviewed Moli primitive steps.

## Dashboard

Climate, cover and media controls use point capabilities, access flags, availability,
numeric ranges and increments. Keep supported existing point keys. Do not show
unsupported modes or send position commands to read-only position sensors.
Extract small pure capability helpers with behavioural tests; keep Svelte
presentation changes focused. Preserve translations and accessibility. Keep
read-only and partial-capability devices classified consistently. Initial vacuum
controls can use Atelier; a dedicated family vacuum UI is a separate scope.

## Fork maintenance

`main` mirrors upstream. Product changes live on independent feature branches
and `fork/integration`. Track upstream updates with a reviewable merge, never
rewrite published integration history or automatically merge a failing update.
Use a tested helper and documented steps to fetch upstream, create an update
branch, run checks and review conflicts. Changes in the fork should also be
reviewable as small independent patches. Do not open upstream PRs automatically.

## Acceptance and limits

Protocol fixtures and loopback servers test normal responses, invalid data,
timeouts, unsupported commands, deadlines and unavailable equipment. Full
`scripts/check.sh` runs in the dev image before every commit. Frontend changes
also pass build, translation checks and relevant Node tests. The final branch
receives an independent review and simulated end-to-end checks. Record the
tested upstream revision and exact commands. Hardware, iPhone remote access,
manufacturer schedules and power-loss recovery require a separate controlled
pilot; no claim of complete household replacement before that evidence exists.

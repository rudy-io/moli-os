# TaHoma coverage and convergence

This branch combines the local-driver convergence from [PR25](https://github.com/rudy-io/moli-os/pull/25)
and the merged [PR99](https://github.com/rudy-io/moli-os/pull/99) with an optional read-only cloud observer.
It does not provide complete TaHoma application parity.
The local comparison was refreshed against PR25 at `131e41ed54cdabb4146a07cbd4062636a236efe3`.

| Function | Implemented in this driver | Limit or next step |
| --- | --- | --- |
| Local device snapshots, availability | Yes, bounded HTTPS and pinned certificate | TOFU first-contact limitation explained in README |
| 18 ordinary cover classes | Yes, according to declared valid commands | Other families require qualified mappings |
| Closure / Deployment position | Yes, state axis first, one conversion for reads/writes | Both reported axes keep Deployment convention; ambiguous absent axes refuse numeric writes |
| Open, stop, close | Only declared bindings with valid arity | No invented command from a label |
| Low-speed open, close and target | One qualified `setClosureAndLinearSpeed` action per request | Ordinary io covers only; no inferred or measured speed, no Position-command fallback; normal stop unchanged |
| RTS | Qualified movement and My variants | No measured position or feedback invented |
| My/favourite recall | Published enum and encoder use the same binding | Only qualified arities; native acknowledgement is not completion |
| Memorized favourite position | Genuine state read; one-parameter numeric setter | Invalid/absent values unknown; zero-parameter save-current meaning unqualified |
| Gates/windows | Observation, exact identity/class opt-in for guarded writes | Every write keeps Control semantics; no change to core approval policy |
| Global read-only option | All write permissions removed and execution refused before actuation | Explicit true required; false compatibility default |
| Stateless known openers | Visible with opening_state unknown | Presence is not physical feedback |
| Local pin/token failure | Local source parked and its devices offline; cloud source independent | Human verification required, no blind certificate replacement |
| Partial snapshots | Missing devices offline immediately, removed after three successful absent snapshots | Reappearance resets the counter; failed reads do not count |
| Position semantics | Current opening uses Position; reviewed opener writes keep Control | Favourite configuration never substitutes for current opening |
| Readback after commands | Snapshot observation only, no optimistic state | execId means receipt, not movement |
| Events | Not implemented | Official API supports listeners; current snapshots every five seconds |
| Tilt, combined axes, arbitrary speed | Not mapped | Need distinct qualified bindings; two sequential commands may interrupt each other |
| Lighting, sockets, sensors, climate, security | Not complete mappings | Local API varies by device; some data/functions require cloud or another platform |
| Saved TaHoma scenarios | Cloud reads bounded action definitions; never executes/imports them | Somfy excludes local Developer Mode scenario execution; modern cloud availability still needs account qualification |
| Grouped immediate actions | No public batch point | Any future batch must validate each action and preserve opener guard |
| Agenda / SMART / holidays | Cloud reads calendar days/rules, triggers and scheduled/delayed records | Opaque definitions, no recurrence evaluation, synchronization or guaranteed future horizon |
| App notifications and administration | Not implemented | Moli rules/notifications and Somfy administration are separate concerns |
| Optional cloud complement | Independent session, fixed read endpoints, bounded encrypted catalogue and owner-only viewing | Opt-in; no cloud physical devices or command path; real account compatibility is separately qualified |

## Evidence and qualification

The local implementation has behavioral RED/GREEN tests, real Moli Hub and simulated TLS
coverage. Every origin is refused in read-only mode; stale writable metadata also fails
before preflight. Fixtures are synthetic. A historical normalized inventory was replayed
through the actual pure production model with all networking disabled. Its placeholder
identities cannot route a live command or establish a writable-opener allowlist.

Low-speed regression tests exercise the exact one-command TLS payload, Closure conversion
under both observed position families, normal movement compatibility, optional-argument
metadata, duplicate/malformed definitions, excluded protocols/classes, fresh-definition
revalidation, read-only enforcement (including stale writable publication), protected-room
approval, cancellation/deadline and refusal without replay. The synthetic low-speed fixture
contains no real installation data. No physical low-speed movement was tested.
See [qualified shapes and pinned primary sources](README.md) for this bounded mapping.

No native authenticated Moli hardware commands were tested. This work does not establish
physical operation, 100% house coverage, or complete replacement of Home Assistant.
A scene label and identifier are not its action definition; an equivalent Moli scene needs
all targets, commands, parameters, order and delays, followed by explicit review.

The cloud observer has synthetic parser, protocol and rotating-session tests. Its
Classic route evidence is described in [CLOUD.md](CLOUD.md); neither that evidence
nor fabricated responses prove that a modern account exposes all six collections.
Failures retain last-good data with its observation time. Unknown definitions are
preserved without evaluation; cached visibility is not a claim of live activation.

## Sources and cloud boundary

- [Official local OpenAPI](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/blob/3eae7a51ba55eee28649319942239aab7b139eb7/docs/openapi.yaml): snapshots, events, immediate actions and execution tracking.
- [Somfy confirms local scenes remain out of scope, September 2026](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/issues/232#issuecomment-5700209880). An undocumented local endpoint may return an execId without executing a saved scenario.
- [HA local device limitations](https://www.home-assistant.io/integrations/overkiz/#device-support-via-the-local-api).
- [Dedicated RTS My parameter tests](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/test_rts_injection.py#L153) and [real public RTS definition](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/fixtures/setup/setup_tahoma_oceania.json#L198).
- [HA memorized-position setting](https://github.com/home-assistant/core/blob/0e52c3f4401c385d365d1675538a4757ec71d05f/homeassistant/components/overkiz/number.py#L77).
- [Current Somfy cloud authentication implementation](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/pyoverkiz/auth/strategies.py): OAuth and multi-site tokens; not a generic login cookie.

The cloud extension uses its own session: sharing Home Assistant's rotating refresh
token could invalidate that session. It scopes observations to an explicitly selected
setup, verifies an anchor gateway and leaves native device identity unchanged. It
publishes a read-only summary rather than duplicate physical devices. It cannot retry
a local action through cloud, and opaque scenario definitions are never executable
points. The cloud contribution is kept separate from the native convergence PR.

The proposed common driver retains the `overkiz` kind and SHA-256 identities for
compatibility with existing fork installations. Raw-URL IDs collide after the core's
slash-to-underscore normalization and can exceed its point-length limit. Agreement
with PR25's author on naming and identity remains pending; no migration is automatic.
Any future PR25 import needs an explicit mapping from original URLs for labels,
rooms and automation references. `Semantic::Position` is now used without removing
Control semantics from sensitive writes. UI changes belong in a separate PR.

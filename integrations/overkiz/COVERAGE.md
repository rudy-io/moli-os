# TaHoma coverage and convergence

This is a local-driver convergence candidate for [PR25](https://github.com/rudy-io/moli-os/pull/25)
and [PR99](https://github.com/rudy-io/moli-os/pull/99), not complete TaHoma application parity.
The comparison used PR25 at `eb05dfbdb906df3a14d26baef77f3990e386c8e3`.

| Function | Implemented in this driver | Limit or next step |
| --- | --- | --- |
| Local device snapshots, availability | Yes, bounded HTTPS and pinned certificate | TOFU first-contact limitation explained in README |
| 18 ordinary cover classes | Yes, according to declared valid commands | Other families require qualified mappings |
| Closure / Deployment position | Yes, state axis first, one conversion for reads/writes | Both reported axes keep Deployment convention; ambiguous absent axes refuse numeric writes |
| Open, stop, close | Only declared bindings with valid arity | No invented command from a label |
| RTS | Qualified movement and My variants | No measured position or feedback invented |
| My/favourite recall | Published enum and encoder use the same binding | Only qualified arities; native acknowledgement is not completion |
| Memorized favourite position | Genuine state read; one-parameter numeric setter | Invalid/absent values unknown; zero-parameter save-current meaning unqualified |
| Gates/windows | Observation, exact identity/class opt-in for guarded writes | Every write keeps Control semantics; no change to core approval policy |
| Global read-only option | All write permissions removed and execution refused before actuation | Explicit true required; false compatibility default |
| Stateless known openers | Visible with opening_state unknown | Presence is not physical feedback |
| Pin/token failure | Parked and devices offline | Human verification required, no blind certificate replacement |
| Readback after commands | Snapshot observation only, no optimistic state | execId means receipt, not movement |
| Events | Not implemented | Official API supports listeners; current snapshots every five seconds |
| Tilt, combined axes, speed | Not mapped | Need distinct qualified bindings; two sequential commands may interrupt each other |
| Lighting, sockets, sensors, climate, security | Not complete mappings | Local API varies by device; some data/functions require cloud or another platform |
| Saved TaHoma scenarios | Not executed by this local driver | Somfy explicitly excludes local Developer Mode scenario execution |
| Grouped immediate actions | No public batch point | Any future batch must validate each action and preserve opener guard |
| Agenda / SMART / holidays | Not imported or synchronized | No complete contract confirmed in the inspected local spec or cloud client |
| App notifications and administration | Not implemented | Moli rules/notifications and Somfy administration are separate concerns |
| Optional cloud complement | Design/research only, disabled/absent in this driver | Separate Somfy session, reviewed OAuth rotation, read-only first |

## Evidence and qualification

The local implementation has behavioral RED/GREEN tests, real Moli Hub and simulated TLS
coverage. Every origin is refused in read-only mode; stale writable metadata also fails
before preflight. Fixtures are synthetic. A historical normalized inventory was replayed
through the actual pure production model with all networking disabled. Its placeholder
identities cannot route a live command or establish a writable-opener allowlist.

No native authenticated Moli hardware commands were tested. This work does not establish
physical operation, 100% house coverage, or complete replacement of Home Assistant.
A scene label and identifier are not its action definition; an equivalent Moli scene needs
all targets, commands, parameters, order and delays, followed by explicit review.

## Sources and cloud boundary

- [Official local OpenAPI](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/blob/3eae7a51ba55eee28649319942239aab7b139eb7/docs/openapi.yaml): snapshots, events, immediate actions and execution tracking.
- [Somfy confirms local scenes remain out of scope, September 2026](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/issues/232#issuecomment-5700209880). An undocumented local endpoint may return an execId without executing a saved scenario.
- [HA local device limitations](https://www.home-assistant.io/integrations/overkiz/#device-support-via-the-local-api).
- [Dedicated RTS My parameter tests](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/test_rts_injection.py#L153) and [real public RTS definition](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/fixtures/setup/setup_tahoma_oceania.json#L198).
- [HA memorized-position setting](https://github.com/home-assistant/core/blob/0e52c3f4401c385d365d1675538a4757ec71d05f/homeassistant/components/overkiz/number.py#L77).
- [Current Somfy cloud authentication implementation](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/pyoverkiz/auth/strategies.py): OAuth and multi-site tokens; not a generic login cookie.

The optional cloud extension needs its own session: sharing Home Assistant's rotating
refresh token could invalidate that session. Local/cloud reconciliation must use exact
native identity and explicit site scope, keep the local source preferred, and never retry
an ambiguous local action through cloud. Opaque cloud scenarios must not bypass opener
approvals. These are requirements for the separate extension, not features already shipped.

The canonical driver name, compatibility for raw-URL IDs from PR25, and Semantic::Position
adoption after PR30 remain coordinated upstream. UI changes belong in a separate PR.

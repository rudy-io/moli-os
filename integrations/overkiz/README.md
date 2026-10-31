# Overkiz local developer mode

Native local HTTPS integration, token from the encrypted vault, certificate pinned
before credentials leave. No cloud login or Home Assistant dependency. Network-free
construction validates configuration. Disabled instances are not started by Moli.

`RollerShutter` devices can expose `control` (open/close/stop, according to
advertised arity) and `percent_control` (percentage open, io only). A measured io closure
in 0–100 maps to 100−closure. Special positions, targets and malformed readings
become unknown. RTS has no inferred position; one-argument RTS movement commands
receive gateway duration 0. `setPosition` is deliberately unsupported.

Windows and gates default to read-only. An explicit `writable_openers` entry
reviewing the **exact native device URL** and expected `ui_class` (`Gate` or
`Window` only) enables only its individually advertised open/close/stop and io
`setClosure` capabilities. An advertised zero-argument `cycle` is a distinct
`control = "cycle"` impulse: it never becomes open or close. Other cycle arities
are unsupported. Reviewed openers do not use up/down aliases. Every writable
point of a reviewed opener, including its percentage, has `Semantic::Control`.
API/MCP/CLI/assistant and household dashboard origins require human approval for
all these writes, including close/stop. Authenticated Ui, System and approved
Automation retain the existing runtime bypass; this is not a physical interlock.
A renamed device keeps its policy; a changed class fails closed, including a
change to RollerShutter. Unknown classes remain read-only and cannot be opted in.
No fake lock semantics or global execution cancellation. Model hints are exactly
`Overkiz Gate` / `Overkiz Window`, including read-only and cycle-only variants.

Identity is the lowercase SHA-256 digest of the complete exact native URL, so
long URLs do not exceed the hub's point limit. The exact URL is retained for each
command; names/hosts are independent. Duplicate native URLs or resulting identities
in a snapshot fail closed. No released identifier format was migrated.

The driver polls the local device snapshot every five seconds and rereads it before
each write. It revalidates availability, exact reviewed class, capabilities and point semantics, never retries `exec/apply`,
and bounds the entire write flow to the earlier of its hub deadline and 4.5 seconds.
An execution ID means accepted, not motor movement. Only snapshots publish state.
Authentication or certificate changes require human action; transient errors use
normal supervisor recovery. No per-device tasks or event listener.

Not verified on physical hardware. Gateway/firmware support, RTS stop behaviour,
and reset/re-pair identity continuity require a controlled pilot. Snapshot polling
has higher traffic than event listeners and state updates can lag five seconds.

Protocol sources consulted at pinned revisions (no source code copied):
- [Somfy OpenAPI](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/blob/3eae7a51ba55eee28649319942239aab7b139eb7/docs/openapi.yaml)
- [Somfy setup](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/blob/3eae7a51ba55eee28649319942239aab7b139eb7/README.md)
- [PyOverkiz RTS arity tests](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/test_rts_injection.py)
- [PyOverkiz local device definitions](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/fixtures/setup/setup_local_tahoma.json)

# TaHoma / Overkiz local developer mode

Native local HTTPS integration, token from the encrypted vault, certificate pinned
before credentials leave. No cloud login or Home Assistant dependency. Network-free
construction validates configuration. Disabled instances are not started by Moli.

Certificate pinning uses **trust on first use (TOFU)** when neither the
configuration nor the encrypted vault contains `cert_sha256`. That first TLS
handshake accepts the certificate presented by the configured gateway; it does
not independently prove the gateway's identity. Its SHA-256 fingerprint is
stored in the vault before the bearer token is sent. Use a trusted LAN for this
first contact, or set an independently verified `cert_sha256` in the options.
A configured fingerprint takes precedence over the cached one. Later certificate
changes stop the driver and require verification before replacing the pin; they
are never accepted automatically. This follows the shared `moli-net`/Hue TOFU
model, not public-CA certificate verification.

The common cover model incorporates the families and favourite work discussed in
[PR25](https://github.com/rudy-io/moli-os/pull/25), alongside the transport and opener
protections in [PR99](https://github.com/rudy-io/moli-os/pull/99). It remains one driver,
with the existing `overkiz` kind and stable identities. Canonical naming and any migration
from PR25's raw-URL IDs must be agreed explicitly; neither branch silently replaces them.

Eighteen ordinary shutter, blind, screen, curtain, awning and pergola classes can expose
only their validated command bindings. The same binding publishes each control and encodes
its native payload, so a favourite cannot be encoded but absent from the public enum.
`control` may offer open/close/stop/favourite according to the native definition and arity.
Root and nested `uiClass` must agree when both are supplied; labels never grant capabilities.

Position is always percentage open. An explicitly reported DeploymentState uses its value;
otherwise ClosureState is inverted (100 minus closure). Reported state axes take precedence
over extra setters: a shutter may advertise both setters but report ClosureState only.
If both state axes are reported, Deployment retains PR25's convention. With no state axis,
exactly one valid setter identifies the conversion; both or neither leave numeric position
and memorisation unsupported. No silent clamping or guessed axis. States 108 and 124 stay
unknown, and the stored favourite never substitutes for a measured current position.

`control = "favourite"` recalls validated zero-argument `goToMemorized1Position` or `my`.
The explicitly qualified RTS `my` with one parameter receives duration `[0]`, as verified in
PyOverkiz's dedicated fixture and tests. Other one-parameter recall variants are refused.
`favourite_position` reads the real memorized configuration and writes only through an
exact one-parameter `setMemorized1Position`, using the established family conversion.
The unqualified zero-parameter variant is not exposed. A reported RTS memorized configuration
is readable when its axis is known; RTS still has no invented measured position.

Set **`read_only = true`** for observation. All published write permissions are removed,
and both the model and runtime refuse every write path, including stale writable metadata,
before actuation. Readings and physical capability inference are retained. The default
remains false for existing configurations; onboarding explicitly enables observation.
Known covers, gates and windows remain visible with a read-only unknown `opening_state`
when no state is reported, including stateless RTS gates. Unknown classes never gain writes.

Windows and gates default to read-only. An explicit `writable_openers` entry
reviewing the **exact native device URL** and expected `ui_class` (`Gate` or
`Window` only) enables its individually advertised, validated bindings: open/close/stop,
qualified io percentage control through `setClosure` or `setDeployment` according to
the established family conversion, and qualified favourite recall and memorisation.
An advertised zero-argument `cycle` is a distinct
`control = "cycle"` impulse: it never becomes open or close. Other cycle arities
are unsupported. Reviewed openers do not use up/down aliases. Every writable
point of a reviewed opener, including percentage and favourite writes, has `Semantic::Control`.
API/MCP/CLI/assistant and household dashboard origins require human approval for
all these writes, including close/stop and favourites. Authenticated Ui, System and approved
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

Read-only validation on 2026-10-31: a separate gateway probe verified the TLS chain
and hostname against the official Overkiz CA, and a redacted real device inventory
from a Home Assistant local-API diagnostic was replayed through this production
model with networking disabled. Position, open/closed state and availability
conversion were checked; no feedback was invented for stateless RTS gates.
This did not exercise authenticated native Moli transport or issue any physical
command. Full model/firmware qualification, RTS stop behaviour and reset/re-pair
identity continuity still require a controlled pilot. Snapshot polling has higher
traffic than event listeners and state updates can lag five seconds.

Protocol sources consulted at pinned revisions (cover-family/favourite design also follows PR25):
- [Somfy OpenAPI](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/blob/3eae7a51ba55eee28649319942239aab7b139eb7/docs/openapi.yaml)
- [Somfy setup](https://github.com/Somfy-Developer/Somfy-TaHoma-Developer-Mode/blob/3eae7a51ba55eee28649319942239aab7b139eb7/README.md)
- [PyOverkiz RTS arity tests](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/test_rts_injection.py)
- [PyOverkiz local device definitions](https://github.com/iMicknl/python-overkiz-api/blob/5f31858570e14be4f58e6cc58b7a448000556449/tests/fixtures/setup/setup_local_tahoma.json)


This change does not implement the cloud complement or saved TaHoma scenarios. The official
local API excludes those scenarios; even an execution ID from an undocumented local scene
endpoint does not establish support. See [coverage and remaining work](COVERAGE.md).
The UI/card and `Semantic::Position` integration remain separate upstream work following
[PR30](https://github.com/rudy-io/moli-os/pull/30). No core guard is changed here.

# Home Assistant temporary migration bridge

A dormant native driver, enabled only by an explicit `[[driver]]` entry. It keeps
selected entities available while each device moves to its native Moli driver.
It is still dependent on Home Assistant, including any cloud used by HA.
Simulator tests do not establish compatibility with hardware.

> **Experimental and transitional.** Reading has run on a real home; commands are
> so far covered by simulators only. Start read-only, then enable one ordinary
> point at a time.

**Use a dedicated, non-admin Home Assistant user for Moli.** A long-lived access
token carries every right of the user who created it: the driver only calls the
services listed below, but the token itself would let whoever steals it act on the
whole HA home. Create a user for Moli (not an administrator), sign in as it, and
create the token from that user's profile.

## Boundary

- List every entity with its immutable `native_id` alias, current `entity_id`,
  domain, reviewed room and safety (`unknown`, `ordinary`, `machine`). Identity is
  `instance:native_id`; changing an HA name or locator must preserve that alias.
  The published native name retains the alias, full HA `entity_id` and optional
  `friendly_name`, so a neutral alias or UI label cannot hide a gate from the Hub
  guard. Malformed or oversized friendly names are rejected, never truncated.
- `writable = []` by default. Enable exact point keys individually after reviewing
  their hardware, room and **all** HA-side automation/template/custom side effects.
  `unknown` cannot be writable. The driver cannot discover hidden external effects.
- All writes enter through Moli's Hub, guard, journal and command queue. Machine
  points, vacuum actions and cover movement use `Semantic::Control`, requiring
  human approval from household/agent origins under the existing guard policy.
  This never pretends that power/open means `Lock`.
- Covers require `ordinary` safety and an HA class in awning/blind/curtain/shade/
  shutter for writes. Missing/unknown class, gates, garage doors, doors and windows
  remain read-only, even after a friendly rename. Observed group membership also
  removes write rights. No scenes, scripts, automations, locks, opaque groups,
  templates, arbitrary services, URLs, broad or multiple HA targets.
- HA metadata only narrows configured rights. Configuration is fixed for a driver
  run. Changing it requires an operator review and restart.

## Points and exact service adapters

| Domain | Readings / optional writable points | HA service |
|---|---|---|
| sensor | finite numeric reading with reported unit/class, or text `state` | none |
| binary_sensor | contact / occupancy / water_leak / smoke / battery_low / value | none |
| climate | temperature; target_temperature, mode, on | set_temperature, set_hvac_mode |
| light | on, brightness (0–100 %), color (#rrggbb), color_temp (K) | turn_on, turn_off; exact brightness / rgb_color / color_temp_kelvin attributes |
| switch | on | turn_on, turn_off |
| cover | state; control and percent_control (0 closed, 100 open) | open_cover, close_cover, stop_cover, set_cover_position |
| vacuum | state, battery; control | start, pause, stop, return_to_base |
| media_player | state, title, artist; playing, volume (0–100 %), mute | media_play, media_pause, volume_set, volume_mute |

Only feature-advertised operations are writable. Missing or invalid readings on declared readable points are explicit `null`,
replacing previous samples; write-only controls never get a fabricated state. Binary HA door/window/opening `on` means open; Moli `contact`
is **true when closed**. Unknown binary classes have `Other` meaning. Text state
never infers OnOff. Climate temperatures need an explicit °C/°F unit from the entity
or, when absent, HA’s global `unit_system.temperature` from `/api/config`; °F is converted
both ways. An invalid explicit entity unit is never hidden by the global fallback.
Writable single setpoints need explicit valid min/max/step
and the TARGET_TEMPERATURE feature; heat_cool range operation is excluded.
Known modes use the existing Moli keys chaud/froid/déshumidification/ventilation/
auto. `on` needs an explicit supported `turn_on_mode` and advertised off mode.
Light colour requires an advertised RGB-compatible mode and strict #rrggbb.
Kelvin colour temperature requires advertised colour_temp support and valid
explicit Kelvin bounds (integer increments). Invalid readings become explicit `null` on declared readable points.
Light effects, media URLs/sources/groups and vacuum segments are excluded. Vacuum controls initially use Atelier.

`model = "Home Assistant <domain>"` is a UI hint, never safety evidence. Frontends
must respect each point's `access`, kind and availability independently.

## Transport and freshness

Configure exactly one host, port and an explicit `http` or `pinned_https` choice.
No full URLs or path templates. HTTP is plaintext and must be chosen deliberately.
While using HTTP, the integration publishes a non-blocking warning that its
long-lived token travels unencrypted and recommends pinned HTTPS. The warning
is visible in Atelier and System, REST snapshots and SSE. It does not interrupt polling.
It is cleared when the driver stops or restarts; pinned HTTPS does not publish it.
HTTPS requires an operator-verified 64-hex SHA-256 certificate pin before any
credential is sent; no insecure TLS or credentialed TOFU. No redirects are
followed. Credentials live only in the instance-scoped encrypted vault. In
addition to `token`, the operator provides `token_endpoint`, the exact endpoint
binding described in onboarding. Changing host/port/transport/pin parks the driver
until the token is explicitly rebound; no credential automatically follows edits.

Only `/api/states/<exact_entity_id>` is polled for entity state. Instances with
climate entities also read `/api/config` once per sweep, using the same bounded,
endpoint-bound client. Only the global temperature unit is retained; other core
configuration is discarded. Missing/invalid metadata or a failed read disables
the fallback until a later successful sweep. A changed or lost global unit
invalidates cached climate command snapshots until their next state poll.
Commands select one hardcoded
`/api/services/<domain>/<service>` and a JSON body with that exact entity only.
They never POST synthetic state. A successful response means dispatch accepted;
state is confirmed by a subsequent poll, without optimistic value changes.

At most 128 entities, 5–300 s polling, 64 KiB accepted responses, 256-byte state
strings, 256-byte friendly names, 64 attribute keys and 16-element capability lists. The reused pinned
HTTPS transport additionally caps allocation at 4 MiB before the bridge checks
its 64 KiB parse limit; plain HTTP enforces 64 KiB while streaming.
One entity is polled at a time, interleaved with commands. No unattended sweeps
starve the command queue. Errors make the selected entity offline and invalidate
its command snapshot; current-run freshness expires. Auth rejection parks the
whole driver. Repeated transport errors trigger normal supervisor backoff.
Commands have one bounded total budget below the Hub deadline and are never
retried, including ambiguous failures. Expired/abandoned orders never dispatch.
Shutdown cancels in-flight work. Disabled driver: no task or polling.

## Native cutover

Validate one native integration on a neutral target under human supervision,
then disable its bridge entry and related HA/Moli duplicate automations before
assigning the native device's labels/flows. Never keep two control owners. HA
scenes stay in HA during the pilot or become separately reviewed finite Moli
primitive-step drafts; the bridge does not execute them.

## Protocol provenance

No HA code is copied. Service payloads and feature mappings are derived from
primary sources consulted 2026-10-31:

- [Official REST API](https://contributors.home-assistant.io/core/internals/rest-api/)
- [HA climate constants](https://github.com/home-assistant/core/blob/dev/homeassistant/components/climate/const.py)
- [HA cover constants](https://github.com/home-assistant/core/blob/dev/homeassistant/components/cover/const.py)
- [HA light constants](https://github.com/home-assistant/core/blob/dev/homeassistant/components/light/const.py)
- [HA vacuum constants](https://github.com/home-assistant/core/blob/dev/homeassistant/components/vacuum/const.py)
- [HA media player constants](https://github.com/home-assistant/core/blob/dev/homeassistant/components/media_player/const.py)
- [HA binary sensor constants](https://github.com/home-assistant/core/blob/dev/homeassistant/components/binary_sensor/const.py)

These links describe upstream schemas, not proof of a particular installation's
capabilities or absence of hidden effects. Synthetic fixtures and loopback tests
cover this driver's contract without contacting a real home.

## Validation status

2026-10-31: the bridge was also observed against a real Home Assistant installation
with every configured point read-only, including climate readings using HA's
global temperature unit. No service commands were sent to that installation.
Hardware models and firmware versions were not recorded for this observation;
it does not qualify physical write behaviour or the native Airzone/Overkiz drivers.
Their hardware validation remains outstanding.

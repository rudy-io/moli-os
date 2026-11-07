# Airzone Local API

Native local integration of the documented Airzone Local API 1.78. One instance
per controller, no Home Assistant or cloud dependency. The runtime field registry
in [coverage.json](coverage.json) models 118 parameter schemas, 31 explicitly
writable parameters and 17 energy leaves. Descriptors also cover known
example/prose fields; these do not acquire write permission without a write
contract. Coverage is conditional on what a controller actually reports.

The [coverage audit](coverage.md) and [operation matrix](coverage-matrix.json)
separately track the 12 Local API operations, unsupported variants, UI evidence,
hardware validation and the documented Open/Web/MQTT/WebSocket extensions.
Registry completeness alone does not establish 100% API or end-to-end coverage.

[Continuous synchronization](synchronization.md) describes receipt freshness,
command confirmation and deterministic regression evidence. An optional
[Cloud Web API reader](../../crates/moli-airzone/src/cloud/README.md) is available
in Installation for a normal Airzone account. It is separate, read-only and does
not provide the Open API schedule operations.

## Configuration and migration

```toml
[[driver]]
id = "airzone"
kind = "airzone"
[driver.options]
host = "controller.example"
port = 3000
poll_secs = 30
read_only = true
# write = "extended"    # optional new commands; read_only = true still blocks all writes
mac = "aa:bb:cc:dd:ee:ff" # optional identity pin
# dhw_units = 0          # only if DHW omits units: 0 Celsius, 1 Fahrenheit
# port = 3443           # HTTPS: replace port above and set a verified fingerprint
# tls_fingerprint = "<64 hexadecimal SHA-256 characters>"
```

**TOML instances retain their historical command scope**: an omitted `read_only`
defaults to `false`, preserving existing automations. An omitted `write` defaults
to `"legacy"`: only `on`, `target_temperature`, `mode` and `fan_speed` remain
writable when advertised. They map to the Local API `on`, `setpoint`, `mode` and
`speed` parameters on an addressed zone, including historical master-zone controls.
Reading additional fields never enables additional commands.

Set `write = "extended"` explicitly to enable the newly covered commands:
whole-system modes, hot water, renaming, installer stages/Q-Adapt, grille/slat,
ventilation/IAQ, integration label and the combined setpoint + turn-on action.
Current capabilities, scope and guards still apply. `read_only = true` overrides
both write modes at publication, execution and transport boundaries.

Set `read_only = true` for an observation-only trial. Gateways added, edited or reactivated from the dashboard explicitly
use `read_only = true`. In Installation, an owner can then authorize control
with a fresh server-verified Moli PIN; returning to read-only uses the existing
human session. Explicit TOML `enabled = false` and `read_only = true` always take
precedence, and the card explains these locks. The per-gateway choice is stored
in registry v3 and leaves hand-written TOML untouched. If persistence fails,
revocation still blocks commands in memory; the card explains that it may be
lost after restart and offers an explicit retry. A durable save is required
before the UI confirms persistence or permits control again.
This button does not select `write = "extended"`; newly managed gateways retain
the historical command scope. A pre-existing explicit TOML opt-in is preserved.
Enabling verifies the pinned identity; revocation also works offline or while
paused. Changing mode sends no physical command. Authorized commands still use
Moli's existing guards; revoking does not stop the heating already running.
Older v1/v2 entries declaring writable mode remain invalid. The restriction applies to published capabilities, the executor and transport,
including integration configuration. It is not merely a disabled UI button.
No startup or diagnostic operation writes `/integration`.
An empty `driver` label is a valid unset value: the integration endpoint remains
available, and Moli does not register itself automatically to fill that label.

Hosts are explicit IPv4 addresses or DNS names, not URLs. Configuration is
validated before networking. Polling is 5–3600 seconds. HTTPS requires an explicit
certificate fingerprint and uses the shared pinned transport; no certificate
verification bypass or automatic downgrade. Pin replacement is deliberate.
The MAC is configured or learned through `/webserver` and persisted by the
runtime known-address store; changing an IP/name does not change device IDs.
Without persistent storage, configure the MAC explicitly.

## Published resources

| Resource | Stable native identity | Data and commands when advertised |
|---|---|---|
| Zone | `<MAC>_s<N>_z<M>` | Existing on/temperature/target keys, double setpoints, sleep, air/floor stages, thermostat information, battery/radio, demands, air quality, windows, angles and alerts |
| Shared settings | `<MAC>_s<N>_z<M>_control` | Modes, fan, slats, Q-Adapt and ERV controls, with direct affected-zone members |
| System | `<MAC>_s<N>_system` | Firmware/type/manufacturer, scenes, ERV, meter and 17 individual energy values |
| Hot water | `<MAC>_dhw` | Temperature, setpoint/bounds, on/off and powerful mode |
| Air-quality sensor | `<MAC>_s<N>_iaq<M>` | IAQ index/score, CO2, particles, TVOC, pressure, ventilation mode and name |
| Gateway | `<MAC>_gateway` | Webserver/API versions, connection information, optional-section status and integration driver label |

All published points are available through normal Moli REST/SSE/MCP, history
and Atelier. Text writes use an explicit Save action. Renaming the manufacturer's
`name` point is distinct from changing a local Moli label. Advanced settings
remain generic typed points; this does not introduce a separate Airzone app.
Unknown units are displayed without an invented degree symbol. Mixed or unknown
units are not averaged as room temperatures.

A present reading is not necessarily a writable capability. Modes, speeds,
stages and modern grille angles use the current advertised choices. Setpoints
use current bounds, units and step. Integer, name-alphabet/11-character and
threshold ordering restrictions are enforced. `acs_setpoint` remains read-only
when its unit is unknown; `dhw_units` is an explicit operator choice in that case,
never inferred from the first zone and never overriding a reported unit.

`humidity = 0` is an ambiguous manufacturer sentinel: retain it as diagnostic
`humidity_reported`, without adding a false ambient humidity measurement.
`power` (documented ambiguously as kW/h) and `consumption_ue` are retained without
inventing billable energy. `energy_data` leaves preserve their documented kW/kWh.
Deprecated grille code 4 has no documented meaning and is not offered as a write.
The metadata `driver` has a 120-character write bound (readings are retained up to
240 characters) and is not a firmware update.

## Operations and diagnostics

| Official operation | Integration use |
|---|---|
| POST /hvac | Zone inventory, system metadata (127 broadcast with per-system fallback), DHW system 0 without zoneID |
| GET /hvac | Explicit read-only diagnostic alternative, lowercase query keys |
| PUT /hvac | One current, validated parameter on one zone/system/DHW resource |
| POST /iaq | Dedicated sensor inventory when `num_airqsensor` is advertised |
| GET /iaq | Explicit read-only diagnostic alternative |
| PUT /iaq | Validated sensor name or supported ventilation mode |
| POST /webserver | Identity verification and gateway information |
| POST /version | API version and diagnostic |
| POST /integration | Current integration label/version |
| PUT /integration | Explicit guarded text-point command only |
| POST /demo | Explicit synthetic diagnostic; never discovery or live capabilities |
| GET / | Controller thermostat-page availability/size; no HTML execution or embedding |

Paths except `/` are relative to `/api/v1/`. Moli deliberately sends one addressed
parameter per hub command, except the targeted `target_temperature_on` action
which sends and validates the documented addressed `on + setpoint` pair.
Other manufacturer multi-parameter and broadcast write
payloads are not exposed as a raw proxy; each documented writable parameter is
covered individually. The special DHW `{systemID:0}` is an addressed resource,
not a zone broadcast.

Read-only operator commands use an existing configured instance:

```sh
moli-os airzone-inspect --instance airzone version
moli-os airzone-inspect --instance airzone webserver
moli-os airzone-inspect --instance airzone hvac --system 0 --zone 0
moli-os airzone-inspect --instance airzone hvac --system 127 --get
moli-os airzone-inspect --instance airzone hvac --system 0
moli-os airzone-inspect --instance airzone iaq --system 1 --sensor 0 --get
moli-os airzone-inspect --instance airzone integration
moli-os airzone-inspect --instance airzone demo
moli-os airzone-inspect --instance airzone thermostat
```

These commands cannot write, even with a writable instance configuration. The
host comes from that instance, paths/methods are closed, and a configured MAC is
checked. Diagnostic output removes unknown keys and names/MAC/integration labels,
keeps known alarm codes, marks demo as synthetic and never emits the thermostat
HTML. Measurements and system topology still describe an installation: inspect
an export before sharing it publicly.

## Lifecycle, scope and bounds

Existing zone identities and room guards are preserved. A reported master is not
assumed to be zone 1. Unambiguous master domains have direct zone members;
independently advertised shared settings without a mode master use the entire
system as their conservative scope. Unknown or missing members never silently
reduce a shared command's scope. Global DHW/integration commands require a
complete system inventory, including previously published systems. Partial IAQ
inventories remain read-only.

Optional invalid humidity/demand readings are omitted. Malformed essential zone
measurements, units or master capability data retain the earlier conservative
policy: isolate the zone and disable its system's shared controls, remembering
its identity. Invalid/duplicate identities and inconsistent nested groups reject
the envelope. Flat multi-system arrays and nested system arrays are accepted.
Permanently removing a known zone still requires a deliberate inventory reset;
this change does not add automatic destructive cleanup or physical pairing.

Commands re-read identity, the zone inventory and only the target resource's
necessary capabilities. They compare admitted units and guard membership before
one PUT within the remaining deadline (at most 4.5 seconds). HTTP 200 parameter
errors, wrong acknowledgements, cancellation and expiry fail without a retry.
Acceptance is separate from the later state read. Queued commands interrupt slow
read-only polls, including optional diagnostics; confirmation resumes afterwards.
A failed optional section becomes unavailable without taking healthy zones down.

Responses are bounded by the shared transport (4 MiB). Known fields are projected
before retention: 240-character texts, bounded lists and errors, 16 KiB serialized
JSON per record and 2 MiB aggregate optional-resource snapshot. These are data
bounds, not a measurement of process RSS. Oversized sections are rejected
atomically rather than truncating their guard inventory. HTML acceptance is
64 KiB; pinned HTTPS first applies the shared 4 MiB response bound.

## Provenance, verification and limits

The contract inventory pins the official OpenAPI 1.78 source SHA-256 and records
its known errata (case aliases, inconsistent enums/shapes/units). Tests cover
publication, the 31 encoders, exact HTTP/TLS requests, acknowledgements, partial
failures, bounds, expiry, read-only and guarded membership using synthetic data.
No copied implementation or new production dependency is required; TLS fixtures
use dev dependencies already present elsewhere in the workspace.

References:
- [Official Local API](https://developers.airzonecloud.com/docs/local-api/)
- [OpenAPI 1.78](https://developers.airzonecloud.com/downloads/local-api/1.78/lapi_openapi.yaml)
- [Integration manual](https://doc.airzone.es/producto/Gama_AZ6/Airzone/Comunes/Manuales/MI_AZ6_WSCLAPI_A4_MUL.pdf)
- [aioairzone reference](https://github.com/Noltari/aioairzone/tree/b683595ffe4660b5a01a7b0acceeb0931444f306) is historical protocol context; no implementation copied.

This is documented Local API coverage, not Airzone Cloud, physical thermostat
pairing, network commissioning, firmware installation or manufacturer calendars:
those operations do not exist in this local specification. Runtime capabilities
and firmware compatibility still require qualification on each target controller.
No physical command or deployment was performed for this extension.

Read-only hardware validation on 2026-10-31 exercised the unmodified Hyper
transport and snapshot parser against a Webserver reporting API type `ws_az`,
API 1.80, Webserver firmware 4.21 and system firmware 5.34. The exact commercial
model was not independently established. Repeated reads succeeded and preserved
the reported temperatures, setpoints, on/off states and modes. No command
executor was started; private captures and identifiers are not included here.

This validates only the observed read path on that firmware. Some Airzone
firmware requires headers and body in one TCP segment, a quirk handled by
aioairzone's transport; compatibility with other firmware is not established.
Actuation, acknowledgements, recovery and long-running operation remain to be
qualified. Validate read-only on the intended device first, then a
human-supervised neutral-zone command with original state restored. If the
reported setpoint does not fit the advertised increment, resolve that
inconsistency before a command test: this driver preserves the reading but
refuses an off-grid write. Manufacturer schedules remain active and can change
state independently.

### Rollback compatibility

Older driver binaries reject unknown TOML options. Before rolling back, restore
a configuration compatible with that version: `read_only`, `write`,
`tls_fingerprint` and `dhw_units` are not accepted by the original driver. Removing `read_only` does not
make an older driver read-only; keep the instance disabled if observation-only
operation is required. Restore a matching managed-gateway registry/backup too.
Keep the current configuration, registry and encrypted store backed up first.

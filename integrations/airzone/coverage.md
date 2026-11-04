# Airzone API coverage and evidence

Checked against the official developer portal on **2026-11-04**. This is an
inventory of documented interfaces, implementation paths and gaps. It is not a
claim of 100% Airzone coverage or physical validation of every device family.

## Reading the matrix

- [coverage-matrix.json](coverage-matrix.json) has 79 operation/RPC rows and 145
  native field rows. Each row separates implementation, Rust/model/UI evidence,
  test references and hardware validation.
- [api-contract.json](api-contract.json) records every published operation,
  request/response contract and named schema in the five interfaces below.
  Common schemas share a content-addressed catalog. REST `$ref` values resolve
  through the corresponding interface's schema-name map; AsyncAPI schemas use
  complete property paths, including alternatives. These are protocol facts,
  without copied explanatory prose, sample account identifiers or credentials.
- [coverage.json](coverage.json) is the **runtime field registry**, not an
  end-to-end coverage report. Its 145 entries include 118 named parameter
  schemas, flattened energy fields and 10 fields documented only in examples.
- A field present in documentation is not necessarily available on a gateway.
  Missing, null, unsupported, failed and stale values must remain distinct.
  A test reference means the test exists; the task's actual test run is separate.
- Every request and response field inherits its operation status unless a more
  specific field mapping is recorded. Recording a schema never counts as implementation.

The native field matrix includes all 118 named Local API parameters. Nine more
Local API schemas describe envelopes/addressing rather than independent points.
The other interfaces remain visible in the denominator: they are not excluded
merely because the current driver is called `airzone` or uses the Local API.

## Official interfaces and prerequisites

| Interface | Published version | Operations | Named schemas | Current integration |
| --- | --- | ---: | ---: | --- |
| [Local API](https://developers.airzonecloud.com/docs/local-api/) | 1.78 | 12 REST | 127 | Native Rust; write variants and app workflows have explicit gaps |
| [Open API](https://developers.airzonecloud.com/docs/open-api/) | 1.2 | 24 REST | 607 | Not implemented; OAuth application registration requires Airzone approval |
| [Web API](https://developers.airzonecloud.com/docs/web-api/) | 1.0 | 13 REST | 607 | Optional read-only account reader; field/variant coverage remains partial |
| [MQTT](https://developers.airzonecloud.com/docs/mqtt/) | 1.0.0 | 3 channel operations + 23 RPC variants | 119 | Not implemented; gateway/broker provisioning must be established |
| [WebSocket](https://developers.airzonecloud.com/docs/websocket-api/) | 1.0.0 | 4 channel operations | 128 | Not implemented; companion to Web API with JWT authentication |

The [Local API version index](https://developers.airzonecloud.com/api/v1/lapi/versions)
ends at **1.78** on the audit date. A gateway reporting a newer API version does
not establish a published specification for that version.

Local API requires a compatible gateway with the service enabled. HTTP uses
port 3000; HTTPS uses 3443 and a device certificate that must be explicitly
verified/pinned. The published service does not authenticate clients. Moli's
PIN, per-gateway pilot mode and capability/guard checks remain necessary for
commands. No Local API pagination, event subscription or numeric rate limit is
documented; its synchronization is polling-based.

Open API is the documented third-party OAuth interface. Application registration
is explicitly manual with Airzone, and includes client credentials, callback
registration and user consent. Its schedule operations, installation/group
commands and AZNS subscription management are not supplied by Local API.

Web API uses user login and JWT refresh. Its documented login does **not** have
the same stated manual application-registration requirement as Open API. It
must not be labelled inaccessible merely because OAuth registration is missing.
Both REST cloud interfaces paginate installations (at most 10 per page).

The optional Web API reader now supplies nine transport paths for login, refresh,
installation inventory, installation details, device status/configuration, gateway
status, location and profile. The application exposes these through a separate
read-only panel. Coverage remains **partial**: server-side name/MAC filters,
`pendingInstallations`, some optional account/group/device fields and complete typed
modeling of the 607 schemas remain outstanding. The three physical write operations
and provider logout are absent. WebSocket events are documented but not connected.
Schedules are not exposed by the published Web API, despite shared schedule schemas.

MQTT documents online events, status events and typed RPC namespaces for
gateway, zone, system, DHW, energy clamp and Aidoo resources. It does not define
the broker endpoint, TLS/authentication provisioning or sufficient firmware
eligibility information. These are **unknown prerequisites**, not evidence that
the user's equipment lacks MQTT. QoS 1 can deliver duplicates; retained online
status alone is not proof of a current successful device snapshot.

WebSocket has separate user and installation subscriptions. Initial device state
ends with `DEVICE_STATE_END`; later changes arrive through device/webserver
events. The server periodically asks for the current JWT; the correlated reply
must arrive within five seconds. Missing/expired authentication closes the
connection with code 4001. Reconnect, token renewal, snapshot completion and
incremental merge need tests before these events can drive Moli state.

## Native Local API gaps

| Operation/variant | Existing evidence | Remaining work |
| --- | --- | --- |
| `POST /hvac`, system/zone/DHW and broadcast reads | Typed transport, model/resource fixtures, all named fields and unchanged-read heartbeats | Validate each hardware family; synthetic fixtures do not establish physical availability |
| `GET /hvac`, `GET /iaq` | Typed transport and explicit diagnostic CLI | Manual Installation diagnostics expose these alternatives with bounded selectors |
| `POST /iaq` | Optional inventory after system advertises sensors; per-sensor field mapping | Physical IAQ availability/behavior not established by synthetic tests |
| Addressed single-field `PUT /hvac`, `PUT /iaq`, `PUT /integration` | Typed encoders, capabilities, guard and ACK tests | Physical confirmation remains separate from API acceptance |
| Multiple fields in one `PUT /hvac` | Explicit official `Change_setpoint_and_turn_on` example | The targeted Allumer à action validates on + setpoint and their complete ACK; arbitrary multi-field writes remain absent |
| Broadcast HVAC/IAQ writes | PUT prose reuses POST addressing; no complete broadcast ACK example | Full affected scope, permissions and response semantics need a typed contract; never add an unchecked raw proxy |
| Multiple fields in `PUT /iaq` | Generic write prose/schema, no explicit compound example | Clarify/validate contract and target capabilities; currently rejected |
| `POST /webserver`, `/version`, `/integration` | Typed gateway metadata and tests | Keep last successful observation separate from last attempt/error |
| `POST /demo` | Explicit synthetic diagnostic; not discovery or real device state | Available in manual Moli diagnostics; explicitly synthetic, never imported as live state |
| `GET /` | Bounded HTML availability diagnostic, certificate-pin tests | Available in manual Moli diagnostics; manufacturer HTML is never executed inside Moli |

Broadcast write semantics are less explicit than broadcast reads. The matrix
records this ambiguity instead of declaring an invented ACK contract supported.
An addressed compound write is explicitly documented for HVAC and can be
implemented without first introducing broadcast writes.

Battery and radio coverage are only reported for compatible radio thermostats
(documented firmware prerequisites are preserved in the contract). A missing
battery percentage must not become zero. See [thermostat metadata](thermostat-metadata.md)
for field semantics and a read-only diagnosis procedure. IAQ naming is unavailable on Aidoo
Pro; ventilation control additionally depends on active `iaq_vmc`. Hardware
families, firmware limits, units, ranges and advertised enum choices constrain
each write even when its field has a compiled encoder.

Physical thermostat pairing/removal and firmware installation do not appear as
Local API operations. This does not prove their absence from every manufacturer
interface. No undocumented commissioning endpoint is invented here.

## Current implementation counts

Of the **79 operation/RPC rows**, **19 transport paths** exist. At application level,
**10 operations are complete**, **11 are partial** and **58 are not implemented**.
The ten complete operations are local reads/diagnostics and the integration-label
write; the two other local write endpoints remain partial because compound/general
broadcast variants are missing. The nine Web paths remain partial for the field
and variant gaps above. These are implementation counts, not physical validation.

## Validation and maintenance

Run from the repository root:

```sh
python3 integrations/airzone/validate_coverage.py
python3 integrations/airzone/validate_coverage.py --test
```

These offline checks detect missing operations/RPC variants, missing native
fields, unresolved schema references, altered contract fingerprints, nonexistent
test evidence, and a false complete status over unimplemented variants. The
negative tests deliberately remove an operation/field or falsify evidence.
They neither access a gateway nor replace Rust, UI, integration or physical
tests. The full repository check remains required before publishing changes.

Source bytes were downloaded directly from these official URLs; fingerprints
are stored per interface in `api-contract.json`:

- Local: `https://developers.airzonecloud.com/downloads/local-api/1.78/lapi_openapi.yaml`
- Open: `https://developers.airzonecloud.com/downloads/open-api-cloud.openapi.yml`
- Web: `https://developers.airzonecloud.com/downloads/webapi.openapi.yml`
- MQTT and WebSocket: the public documentation pages above, using their embedded
  AsyncAPI JSON. The WebSocket page's separate YAML download returned HTTP 403
  during this audit; the readable embedded contract was used instead.

For an upstream documentation update, first archive the new official source
bytes and compare their SHA-256, operations, variants, schemas and constraints.
Update the factual contract and matrix together, then adjust the independent
denominator constants after reviewing actual additions/removals. Do not update a
status merely because a new registry field or test name exists. Attach the test
run and, when applicable, hardware evidence without including private addresses,
MACs, names, tokens or manufacturer account data.

# Airzone Local API

One native `airzone` driver instance per local Webserver, HTTP port 3000 by
 default. No Home Assistant or cloud dependency. This does not implement Aidoo
Cloud, DHW, energy, IAQ, double setpoints, stage configuration or schedules.
No installation request modifies `/integration` or the controller configuration.

```toml
[[driver]]
id = "airzone"
kind = "airzone"
[driver.options]
host = "controller.example"
port = 3000
poll_secs = 30
mac = "aa:bb:cc:dd:ee:ff" # optional; otherwise learned from /webserver
```

`Airzone::new` validates configuration without networking. Hosts are explicit
IPv4 addresses or DNS hostnames (no URLs, paths, embedded port, discovery or
IPv6 in this initial version). Poll interval: 5–3600 seconds. Controller MAC
is configured or learned and retained across driver restarts; it never derives
from the host or zone name. Learned identity is persisted by the runtime's
known-address store. Changing an identity requires deliberate reconfiguration.
Without persistent runtime storage, configure `mac` to pin it across process
restarts. No authentication is specified by the local API: isolate its network.

Zones use stable `<controller MAC>_s<systemID>_z<zoneID>` identities. Names are
bounded labels; they never automatically become Moli rooms. Each zone exposes
`on`, `temperature`, `target_temperature`, read-only `mode`, optional humidity,
reported demands and bounded diagnostics. Numeric bounds, unit and increment
come from the current response; readings are not rounded to the increment.
Writes enforce reported bounds and increment (relative to `minTemp`, tolerance
1e-6). Celsius and Fahrenheit are published explicitly without conversion.

A separate `_control` device publishes only advertised modes and `fan_speed`
(string enum of supported wire speeds, `0` is auto). A zone is a master only
when it advertises `modes`, regardless of its number. Only a resolved master
domain offers common controls. Its `members` enumerate directly affected zones
so Moli's room guard checks all of them. Multiple masters require explicit,
consistent `master_zoneID` on every zone, including the masters. Ambiguous
common control is omitted. Zone on/off and setpoint never inherit broad scope.
No writes use system or zone broadcast IDs.

Each write performs identity and full capability reads before the one PUT.
It refuses changed shared membership (the guard used the previous membership),
unknown/missing zones, invalid capabilities and cancelled/expired commands.
Preflight and PUT share a total budget of min(remaining command deadline,
4500 ms); requests are sequential and never retried. A parameter-level error
on HTTP 200 still fails. Both object and single-element list acknowledgements
are accepted after checking IDs and returned values. Acceptance never invents
achieved state: a separate full read publishes confirmation. Failed reads mark
all devices offline through the normal driver supervisor. Missing zones become
offline without resetting their last measurements.

## Provenance and limits

Independent Rust implementation; no upstream code copied. References:

- [Airzone Local API documentation](https://developers.airzonecloud.com/docs/local-api/).
- [Official OpenAPI 1.78](https://developers.airzonecloud.com/downloads/local-api/1.78/lapi_openapi.yaml):
  `/webserver`, `/hvac`, modes, speeds and parameter errors.
- [Official Local API manual](https://doc.airzone.es/producto/Gama_AZ6/Airzone/Comunes/Manuales/MI_AZ6_WSCLAPI_A4_MUL.pdf),
  printed pages 20–33 (POST reads, addressed PUT writes).
- [aioairzone v1.0.5 reference revision](https://github.com/Noltari/aioairzone/tree/b683595ffe4660b5a01a7b0acceeb0931444f306):
  `localapi.py`, `system.py`, `zone.py`, `const.py`, `http.py`; Apache-2.0,
  used to understand envelope shapes and master scope.

The fixture is entirely synthetic, with multiple systems, non-contiguous IDs,
a master numbered 8, fractional measurements and Fahrenheit. Tests use pure
parsing and loopback simulators. No real controller has been contacted or
commanded by this implementation. In particular, Hyper transport compatibility
is unverified: some Airzone firmware requires headers and body in one TCP
segment, a quirk handled by aioairzone's transport. Simulator success does not
establish support on that firmware. Validate read-only on the intended device
first, then a human-supervised neutral-zone command with original state restored.
Manufacturer schedules remain active and can change state independently.

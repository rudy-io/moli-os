# Matter natif expérimental / Experimental native Matter (IP)

[Guide de revue et matrice de validation / Maintainer handoff](REVIEW.md).

**Matter IP testé sur pont Hue / Hue IP validation: [hardware report](HARDWARE.md).** Native Rust controller; no Matter Server
or Python service. The standard build has no BlueZ/BLE dependency and supports devices
already on an IP network, including Thread devices reachable through an existing border
router. Bluetooth commissioning is not part of this contribution: the vendored stack is
trimmed to what Moli builds and tests, so a Thread device is onboarded with its own app or
another controller first and then driven over IP; see [thread.md](thread.md).
The default release image exceeds the project's 25 MB budget. An optional compact
build is measured below that budget, at the cost of symbolized local backtraces; see
`docs/components/exploitation.md` and `docs/PERFORMANCE.md`.

```toml
[[driver]]
id = "matter"
kind = "matter"
[driver.options]
paa_roots = "/run/matter-trust/paa"
cd_roots = "/run/matter-trust/cd"
# udp_port = 5541 # Optional fixed listener; scope the host firewall accordingly.
```

Requires Rust 1.89 or newer (durable cross-process vault locking). The complete
default-feature application passes `cargo check --locked -p moli-os` on Rust 1.89.0;
this minimum-version check does not validate BLE hardware.
The corrected Matter library is included inside Moli at `vendor/matter-rust`.
Controller/codec and their sibling crates resolve through local paths. Building
Moli requires no personal fork or sibling checkout. Apache-2.0 license, upstream
origin, export boundaries and local adaptations are recorded in
[`MOLI-PROVENANCE.md`](../../vendor/matter-rust/MOLI-PROVENANCE.md).

Both directories must contain production attestation roots accepted by
`matter-controller::AttestationTrust::from_dirs`. Supply these explicitly and mount them
read-only in containers. Example/test roots are never selected automatically.
IPv6 and mDNS multicast must reach the device (normally host networking).

Open a commissioning window from the device's existing controller (multi-admin/share
Matter device). Feed the resulting manual or QR pairing code through stdin using
`moli-os secrets set matter pairing_code`, then restart Moli. Never put the code in TOML,
shell arguments, logs, tickets or API requests. Initial devices must already be on-network;
use the manufacturer's controller for initial Wi-Fi/Thread setup. The pairing code is
read once per driver run and consumed **before** network commissioning. A supervisor
restart therefore cannot silently repeat a partially completed pairing. On failure,
inspect the device's fabric membership before explicitly supplying a new code.

The encrypted vault stores `controller_snapshot` (private fabric keys, hex encoded,
maximum decoded size 1 MiB). Back up the vault and master key together. Never copy this
snapshot between simultaneously running controllers. Without a writable vault the driver
fails closed. An empty fabric reports a clear waiting status and exposes no commands.

Descriptor DeviceTypeList (attribute 0) and ServerList (attribute 1) are read together.
Endpoint 0 and aggregator containers are excluded; child IDs use node ID plus endpoint.
Bridged NodeLabel is used as a display name when present and bounded; renaming
refreshes discovery without changing identity. Missing labels keep the generated name.
Rediscovery preserves surviving devices and compatible samples; only disappeared
endpoints are removed. Upsert prunes obsolete or incompatible point readings. Supported functions include OnOff, dimming/color, covers, thermostats, guarded locks
and sensor measurements. The exact capability gates and limitations are in
[COMPATIBILITY.md](COMPATIBILITY.md). BooleanState is contact only for device
type 0x0015 (true = closed), leak only for 0x0043 (true = water detected). Unknown or
contradictory contact/leak types are ignored. Known invalid/null sensor samples publish
Null to clear stale readings. Bridged endpoints (type 0x0013 or server 0x0039) remain
offline until Reachable (0x0039/0x0011) reports true as well as node connectivity;
reconnect invalidates that priming. Writes use the same endpoint availability gate.
Descriptor arrays over 128 servers or 32 types are rejected.

Primary references: [device types](https://github.com/project-chip/connectedhomeip/blob/master/src/app/zap-templates/zcl/data-model/chip/matter-devices.xml),
[contact polarity implementation](https://github.com/project-chip/connectedhomeip/blob/master/examples/contact-sensor-app/nxp/k32w0/main/AppTask.cpp),
[water leak example](https://project-chip.github.io/connectedhomeip-doc/examples/water-leak-detector-app/linux/README.html),
[Reachable definition](https://github.com/project-chip/connectedhomeip/blob/master/data_model/1.6/clusters/BridgedDeviceBasicInformationCluster.xml).
Fixtures are synthetic protocol descriptors, not captures or hardware validation.

There are at most 64 node workers, 128 endpoints per node and 128 queued updates.
Subscriptions mark devices offline during reconnect, retry failed discovery/subscription
after ten seconds, and rebuild after lag. Commands require a known online capability, validated input and a live caller deadline.
Invokes, explicitly timed lock operations and attribute writes carry that deadline
through queueing, reconnect and retransmissions; acknowledgements must be unambiguous
and match the expected path.
No optimistic state update is published. Restart after adding a node to enumerate it.
No new HTTP endpoint is introduced.

FR : configurer explicitement les racines d'attestation de production, ouvrir le partage
Matter depuis le contrôleur existant, saisir le code via l'entrée standard de `secrets set`,
puis redémarrer. Le coffre chiffré conserve le tissu Matter ; un essai réel sur pont Hue est documenté dans `HARDWARE.md` ; les radios Thread/BLE restent non validées. Cette contribution ne contient aucun
chemin Bluetooth : le commissionnement se fait avec l'application du fabricant ou un autre
contrôleur, puis Moli pilote l'appareil en IP à travers le routeur de bordure existant.

### Bridges and bounded subscriptions

Subscription filters use endpoint wildcards and are deduplicated per
cluster/attribute. Reports retain concrete endpoint paths; dispatch accepts only
discovered endpoints and command planning still checks their own capabilities and
policies. This avoids repeating identical filters for every bridged endpoint.
A synthetic 100-endpoint regression checks this filter-count bound; it is not a
100-device hardware capacity claim. Distinct cluster/attribute count and the SDK's
bounded report buffering still limit very large or heterogeneous bridges.

The later Hue failure was localized with phase diagnostics: Descriptor and
capability reads succeeded, but 212 repeated subscription paths caused
`Transport/PayloadTooLarge`. Compacting them to 28 filters restored subscription
and live OnOff, with independent read-only observation and state restoration.
It was an application request-size defect, not evidence of bridge non-conformance
or a network-only failure. See [hardware evidence](HARDWARE.md).

### Host firewall and UDP listening port

`udp_port` is optional and defaults to `0` (an ephemeral port, preserving existing
behavior). Set a free fixed port, for example `udp_port = 5541`, when the host
firewall needs an explicit inbound rule. Allow UDP to that port only from the
intended Matter devices or trusted device network; IPv4/IPv6 and mDNS discovery
need rules appropriate to the deployment. Moli never changes the host firewall.

An established outgoing UDP flow can expire before the publisher's next report:
initial reads and commands may succeed while the first idle subscription report
and its MRP retries are blocked. Diagnose this using the firewall logs and the
negotiated reporting interval rather than lengthening timeouts or polling battery
devices. A fixed listener and a narrow inbound rule preserve unsolicited reports.

# Matter natif expérimental / Experimental native Matter (IP)

**Matter IP testé sur pont Hue / Hue IP validation: [hardware report](HARDWARE.md).** Native Rust controller; no Matter Server
or Python service. The standard build has no BlueZ/BLE dependency and supports devices
already on an IP network, including Thread devices reachable through an existing border
router. Optional `matter-ble` builds can provision a Thread device over Bluetooth using
an existing border router's operational dataset; see [thread.md](thread.md).
The release-image size currently exceeds the project's 25 MB budget; measurements
and the unresolved comparison are recorded in `docs/PERFORMANCE.md`.

```toml
[[driver]]
id = "matter"
kind = "matter"
[driver.options]
paa_roots = "/run/matter-trust/paa"
cd_roots = "/run/matter-trust/cd"
```

Requires Rust 1.89 or newer (durable cross-process vault locking).
The controller and codec are pinned to commit
`d80b1a3fd2204119ddf47406f4bb59916ba7576b` of the contribution's matter-rust fork.
It adds the absolute-deadline/cancellation API and durability fixes required here;
registry `matter-controller` 0.18.3 alone does not provide that API. This is an
experimental dependency choice to review before merging, not an upstream release claim.
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
Rediscovery removes previous node devices before recreating the descriptor snapshot,
clearing obsolete points and samples. Supported points: OnOff (read/write), temperature,
humidity, occupancy, contact and leak (read-only). BooleanState is contact only for device
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
after ten seconds, and rebuild after lag. Commands require a known online OnOff endpoint,
a boolean and a live caller deadline; `invoke_before` checks the deadline before transport.
No optimistic state update is published. Restart after adding a node to enumerate it.
No new HTTP endpoint is introduced.

FR : configurer explicitement les racines d'attestation de production, ouvrir le partage
Matter depuis le contrôleur existant, saisir le code via l'entrée standard de `secrets set`,
puis redémarrer. Le coffre chiffré conserve le tissu Matter ; un essai réel sur pont Hue est documenté dans `HARDWARE.md` ; les radios Thread/BLE
restent non validées. La compilation standard ne contient pas Bluetooth ; le mode optionnel
`matter-ble` peut transmettre un dataset existant, sans créer ni administrer le routeur Thread.

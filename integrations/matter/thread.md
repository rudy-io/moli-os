# Thread commissioning (optional) / Appairage Thread optionnel

The Matter driver speaks IP to a Thread device through a border router. It does
not implement, replace, configure or flash an OpenThread Border Router (OTBR).
Provide a working border router/radio and IPv6 route first. Follow the
[OpenThread deployment guide](https://openthread.io/guides/border-router).
On Proxmox, radio passthrough, multicast and IPv6 routing must be checked outside
Moli. No host network settings are modified by this integration.

The default scratch image remains IP-only. For initial onboarding without a
manufacturer's controller, build a Linux native binary with Bluetooth support:

```sh
cargo build --release -p moli-os --features matter-ble
```

This opt-in feature uses btleplug/BlueZ and needs the system D-Bus development
library plus pkg-config at build time, and access to BlueZ/system D-Bus and a
Bluetooth adapter at runtime. It is not the standard scratch deployment and it
has not been validated on physical hardware by this contribution.

Set `ble_thread = true` under `[driver.options]` for the instance to commission.
Store the **complete active operational dataset** exported by your own OTBR in
the existing encrypted vault as `thread_dataset`. Transfer it directly through
stdin to `moli-os secrets set <instance> thread_dataset`; do not print it, put it
in TOML, shell arguments, a repository, a ticket or a chat. It contains the Thread
network key and PSKc. The driver bounds its hexadecimal form to 508 characters,
decodes it without logging values, and validates the Thread TLV structure before
starting Bluetooth. An incomplete dataset may still be refused by the device.

Place the new device in its own pairing mode. Supply its manual/QR setup code
through stdin to `moli-os secrets set <instance> pairing_code`, then restart Moli.
The driver scans by discriminator, establishes BLE/BTP/PASE, verifies attestation,
installs the fabric, provisions the supplied Thread dataset and establishes CASE
over IP after the device joins. The snapshot is saved through the encrypted vault.
The setup code is consumed before network activity, so an uncertain failure cannot
be retried automatically on every supervised restart. Inspect device fabric
membership before explicitly supplying another pairing code. The dataset remains
in the encrypted vault for subsequent devices; replace it if the network changes.

Without `matter-ble`, requesting Bluetooth commissioning reports a translated
error and sends no Bluetooth request. Once commissioned, ongoing reads, commands
and subscriptions use the common IP path; BLE is not the ongoing control channel.

FR : compiler explicitement avec `matter-ble`, fournir un adaptateur Bluetooth et
un routeur Thread opérationnel, puis ranger le dataset et le code d'appairage dans
le coffre via l'entrée standard. Aucun réseau Thread n'est créé automatiquement.
Les tests logiciels valident le décodage et la compilation ; portée radio, sommeil,
autonomie et interopérabilité IKEA restent à tester avec du matériel réel.

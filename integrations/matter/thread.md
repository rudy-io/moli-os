# Thread commissioning (optional) / Appairage Thread optionnel

The Matter driver speaks IP to a Thread device through a border router. It does
not implement, replace, configure or flash an OpenThread Border Router (OTBR).
Provide a working border router/radio and IPv6 route first. Follow the
[OpenThread deployment guide](https://openthread.io/guides/border-router).
On Proxmox, radio passthrough, multicast and IPv6 routing must be checked outside
Moli. No host network settings are modified by this integration.

The contribution ships no Bluetooth path: the vendored stack keeps only the crates
Moli builds and tests, and the `matter-ble` feature is not part of it. A Thread device
therefore has to be commissioned once with its manufacturer's app or another
controller; Moli then reaches it over IP through the border router above.

`ble_thread = true` under `[driver.options]` is refused with an explicit message in this build.
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

Requesting Bluetooth commissioning reports a translated error and sends no radio
request. Once commissioned elsewhere, ongoing reads, commands and subscriptions use
the common IP path; Bluetooth is not the ongoing control channel.

FR : cette contribution ne compile aucun chemin Bluetooth. L'appareil Thread se
commissionne une fois avec l'application du fabricant ou un autre contrôleur, puis
Moli le joint en IP via le routeur de bordure. Aucun réseau Thread n'est créé
automatiquement, et portée radio, sommeil, autonomie et interopérabilité IKEA
restent à valider avec du matériel réel.

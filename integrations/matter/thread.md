# Thread commissioning (optional) / Appairage Thread optionnel

The Matter driver speaks IP to a Thread device through a border router. It does
not implement, replace, configure or flash an OpenThread Border Router (OTBR).
Provide a working border router/radio and IPv6 route first. Follow the
[OpenThread deployment guide](https://openthread.io/guides/border-router).
On Proxmox, radio passthrough, multicast and IPv6 routing must be checked outside
Moli. No host network settings are modified by this integration.

The contribution ships no Bluetooth path: the vendored stack keeps only the crates
Moli builds and tests, and the `matter-ble` feature is not part of it. It also has
no Thread commissioning of its own: there is no `thread_dataset` secret and no
`ble_thread` option, so nothing asks you for a Thread network key. A Thread device
therefore has to be commissioned once with its manufacturer's app or another
controller; Moli then reaches it over IP through the border router above.

Open a commissioning window from the device's existing controller (multi-admin or
share), then supply its manual/QR setup code through stdin to
`moli-os secrets set <instance> pairing_code`, then restart Moli. The driver
verifies attestation, installs its fabric and establishes CASE over the existing
IPv6 route. The snapshot is saved through the encrypted vault.
The setup code is consumed before network activity, so an uncertain failure cannot
be retried automatically on every supervised restart. Inspect device fabric
membership before explicitly supplying another pairing code.

Once commissioned, ongoing reads, commands and subscriptions use the common IP
path; Bluetooth is not the ongoing control channel.

FR : cette contribution ne compile aucun chemin Bluetooth et n'appaire aucun
réseau Thread. L'appareil Thread se commissionne une fois avec l'application du
fabricant ou un autre contrôleur, puis Moli le joint en IP via le routeur de
bordure. Aucun réseau Thread n'est créé automatiquement, et portée radio, sommeil,
autonomie et interopérabilité IKEA restent à valider avec du matériel réel.

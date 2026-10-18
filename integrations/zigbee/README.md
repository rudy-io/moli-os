# Zigbee natif

- Pilote sans Zigbee2MQTT ni broker : Moli parle EZSP (ASH sur série ou TCP) à un coordinateur
  Silicon Labs, `crates/moli-zigbee`.
- Dormant : rien ne tourne sans `[[driver]] kind = "zigbee"` dans `moli.toml`.
- Pour l'instant la crate pose seulement sa configuration et son branchement ; les couches
  ASH, EZSP puis le réseau arrivent ensuite.
- Pas de fixture pour le moment.

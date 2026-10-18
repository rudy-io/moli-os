# Onboarding : Zigbee natif

1. Demander où est branchée la clé coordinateur (`/dev/ttyUSB0`, ou `tcp://<hôte>:6638` pour un
   adaptateur réseau) et son firmware (EZSP 13 ou plus).
2. Déclarer `kind = "zigbee"` avec `port` (et `baud`, `flow`, `channel` si besoin). La clé du
   réseau est rangée par le pilote dans le coffre, jamais dans un fichier.
3. `moli-os check-config`, redémarrer. Le pilote est encore provisoire : il attend son réseau.

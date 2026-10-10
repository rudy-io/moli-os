# Onboarding : appareil ESPHome (satellite vocal)

1. Demander l'adresse de l'appareil (réservation DHCP conseillée) et s'il est déjà relié à Home
   Assistant.
2. La clé API (32 octets en base64) : celle de la configuration ESPHome de l'appareil, ou celle
   que Home Assistant a posée (`noise_psk` de l'entrée ESPHome). Toujours par un tube, jamais
   affichée : `… | moli-os secrets set <instance> api_key`.
3. Déclarer `kind = "esphome"` avec `host`, et `mac` (lue dans l'application ESPHome ou sur la
   box) : avec elle, un appareil qui a changé d'adresse est retrouvé sur le réseau local (au plus
   une recherche toutes les 10 minutes, journal « esphome device found at a new address » avec la
   nouvelle adresse à reporter dans `host`). `moli-os check-config`, redémarrer.
4. Tester : l'appareil apparaît avec son nom ; `volume` a la valeur du boîtier ; changer le volume
   depuis Moli change celui du boîtier.
5. La voix (tours de parole, conversation) demande que Moli soit le seul « cerveau » vocal : retirer
   l'appareil de l'intégration ESPHome de Home Assistant à ce moment-là, pas avant.

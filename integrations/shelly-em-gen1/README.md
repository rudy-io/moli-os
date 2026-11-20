# Shelly EM, première génération

- Ancienne API HTTP locale des Shelly Gen1 : `GET /status` toutes les 10 s, sans websocket.
  Les Shelly Gen2 et suivants (Pro 3EM, Pro EM…) ont le pilote natif `shelly`.
- Réutilisé : la documentation publique de l'API Gen1 (`/shelly`, `/status`), aucun code copié.
- `fixtures/status.json` : capturée sur un vrai Shelly EM (`SHEM`, firmware
  `20230913-114150/v1.14.0-gcb84623`), novembre 2026 ; MAC, réseau et numéro de série remplacés.
- Vérifié en lecture sur ce compteur, les deux pinces branchées dans `[energy]` (arrivée et
  production solaire) : mêmes kWh que l'intégration Shelly de Home Assistant.
- Lecture seule. Ni le relais, ni `/reboot` : l'appareil est fragile (ESP8266, il peut se figer
  après de longues durées de marche) et un redémarrage reste une décision humaine.
- Reste : l'authentification HTTP de base (appareil protégé par mot de passe) ; le Shelly 3EM
  (trois pinces, mêmes champs) n'a jamais été vu.

# IzyPower (micro-onduleurs, via le cloud)

Pilote natif `crates/moli-izypower`, **lecture seule**.

- **Pourquoi le cloud** : les micro-onduleurs IzyPower (puces Wi-Fi Espressif) n'ouvrent aucun
  port sur le réseau de la maison (sondé : 80, 443, 502, 1883, 6607, 8080, 8899…) ; ils ne
  parlent qu'au cloud.
- **Transport** : `https://application.izypowercloud.fr/photo_voltaic/api`, **HTTPS seulement**
  (l'application et l'intégration HACS utilisent HTTP : le mot de passe passerait en clair).
  `POST /login` → jeton JWT (`x-tts-access-token`, `app-platform: izy`), gardé jusqu'à son
  expiration, nouvelle identification sur un 401. Puis toutes les 3 minutes :
  `/powerStations/page`, `/v3/powerStations/info/<id>`, `/device/page?powerId=<id>`,
  `/report/layoutPower/<id>` (courbe du jour, un point toutes les 3 minutes, puissance par
  entrée PV et par micro-onduleur).
- **Appareils** : la centrale (`station_<id>` : puissance, production jour / mois / année,
  **production totale** au sens `energy`, puissance installée) et un appareil par
  micro-onduleur (`<numéro de série>` : puissance, puissance par entrée, production du jour,
  signal Wi-Fi, puissance nominale, en ligne).
- **Heures** : `lastUpdate` est à l'heure du serveur (UTC+8) ; `lastUpdateStr` porte son
  décalage et sert en premier. Une centrale sans nouvelle depuis 20 minutes (nuit, coupure)
  affiche une puissance de 0 ; les productions restent.
- **Origine** : points d'accès observés, et lus dans l'intégration HACS `izypower_cloud`
  (StefanPlizga, licence MIT) ; aucun code repris. Réponses réelles capturées puis anonymisées
  (`fixtures/`).
- **Pas encore** : batteries et compteurs IzyPower (aucun dans l'installation de référence),
  réglages (limite d'injection…), historique repris du cloud.

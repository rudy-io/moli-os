# VELUX ACTIVE with NETATMO (passerelle KIX 300)

- Aucun code repris : le cloud de l'application VELUX ACTIVE (un serveur Netatmo) est décrit par
  ha-velux-active (IngmarStein, Apache-2.0, archivé en mars 2026 ; Niek, MIT) ; Moli le parle
  lui-même (`crates/moli-velux`).
- Connexion : `POST /oauth2/token` (`grant_type=password`, l'identifiant client public de
  l'application, `user_prefix=velux`, `scope=velux_scopes`) ; puis, avec le jeton,
  `POST /api/homesdata` (les modules, une fois) et `POST /api/homestatus` (`home_id` ; l'état,
  chaque minute).
- Modules : `NXG` passerelle (pluie `is_raining`, mode départ `locked`), `NXO` fenêtre, store ou
  volet (`velux_type` : `window`, `blind`, `shutter`… ; `current_position` et
  `target_position`, 0 fermé à 100 ouvert), `NXS` capteur de climat (ses mesures sont celles de
  sa pièce dans `homestatus` : `temperature` en dixièmes de degré, `humidity`, `co2`, `lux`,
  `air_quality`), `NXD` commande de départ (pile). Une fenêtre et son store partagent un
  `group_id`.
- Vérifié en vrai (sonde en lecture seule) : 1 passerelle, 7 fenêtres, 8 stores, un capteur, la
  commande de départ ; un store fermé et une fenêtre entrouverte depuis l'application se
  relisent avec leur nouvelle position.
- Fixture : `homesdata` et `homestatus` de cette installation, identifiants inventés, noms de
  pièces changés, sans adresse ni programme.
- Pas encore fait : les ordres. Stores et volets : `POST /syncapi/v1/setstate` avec
  `target_position`. Fenêtres de toit : le même ordre signé (HMAC-SHA512 avec une clé que la
  passerelle donne une fois, après un appui sur son bouton), selon Niek/ha-velux-active.
- Chemin écarté pour l'instant : HomeKit en local (la passerelle est compatible), parce qu'elle
  n'accepte qu'un seul système HomeKit et qu'elle est souvent déjà dans l'app Maison d'Apple.

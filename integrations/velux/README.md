# VELUX ACTIVE with NETATMO (passerelle KIX 300)

- Aucun code repris : le cloud de l'application VELUX ACTIVE (un serveur Netatmo) est décrit par
  ha-velux-active (IngmarStein, Apache-2.0, archivé en mars 2026 ; Niek, MIT) ; Moli le parle
  lui-même (`crates/moli-velux`).
- Connexion : `POST /oauth2/token` (`grant_type=password`, l'identifiant client public de
  l'application, `user_prefix=velux`, `scope=velux_scopes`) ; puis, avec le jeton,
  `POST /api/homesdata` (les modules, une fois) et `POST /api/homestatus` (`home_id` ; l'état,
  chaque minute). Le jeton dure 3 h : il est renouvelé par `grant_type=refresh_token` (le
  `refresh_token` de la réponse, rangé au coffre sous `refresh_token`, redémarrages compris),
  comme le fait l'application ; le mot de passe ne sert plus que si le cloud refuse ce jeton.
  Chaque connexion par mot de passe envoie un e-mail « nouvelle connexion » au compte.
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
- Ordres (`POST /syncapi/v1/setstate`, JSON, comme l'application) : un store avec
  `target_position` (−1 = arrêt) ; une fenêtre avec le même ordre signé (HMAC-SHA512, clé que la
  passerelle donne une fois après un appui sur son bouton), sans `force` ; l'arrêt d'une fenêtre
  passe par la passerelle (`stop_movements`). Format et signature selon Niek/ha-velux-active ;
  vérifiés par deux sondes : store de 0 à 50 % et retour (accepté en 1 s, arrivé en ~10 s ; le
  cloud montre la position visée tout de suite, la position atteinte à l'arrivée), fenêtre de
  0 à 10 % et retour (~7 s).
- Clé des fenêtres : `retrieve_key` par le cloud, puis le protocole local de la passerelle
  (port 25050, X25519 et ChaCha20-Poly1305), fait par une sonde ; Moli ne sait pas encore
  appairer lui-même (à faire : une commande `moli-os`, la clé allant droit au coffre).
- Pas encore vérifié en vrai : l'arrêt (−1 accepté par le cloud, mais le store était déjà
  arrivé) et `stop_movements`.
- Chemin écarté pour l'instant : HomeKit en local (la passerelle est compatible), parce qu'elle
  n'accepte qu'un seul système HomeKit et qu'elle est souvent déjà dans l'app Maison d'Apple.

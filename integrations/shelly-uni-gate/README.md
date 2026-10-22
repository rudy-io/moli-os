# Shelly Uni

- Protocole : API locale de première génération documentée par Shelly (`/status`, `/relay`, `/roller`) ; aucun code repris.
  `fixtures/status.json` est une réponse capturée sur un vrai appareil, adresse et MAC remplacées.
- Vérifié en lecture sur de vrais appareils. Les écritures (`gate` (une impulsion : le relais 1 se coupe seul après 0,2 s)) suivent la documentation et restent à tester physiquement, un humain présent, hors des pièces protégées.
- Reste : l'authentification Shelly (identifiant et mot de passe de l'appareil) n'est pas prise en charge ; les appareils de deuxième génération (`/rpc`) non plus.

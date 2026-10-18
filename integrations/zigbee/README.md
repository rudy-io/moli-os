# Zigbee natif

- Pilote sans Zigbee2MQTT ni broker : Moli parle EZSP (ASH sur série ou TCP) à un coordinateur
  Silicon Labs, `crates/moli-zigbee`. Dormant : rien ne tourne sans `[[driver]] kind = "zigbee"`.
- Réutilisé : la documentation Silicon Labs (UG100 pour EZSP, UG101 pour ASH, UG600 pour
  EmberZNet) ; les tables du protocole (identifiants de trames, statuts, valeurs de
  configuration d'un coordinateur) sont reprises de zigbee-herdsman (MIT, crédit dans `NOTICE`),
  avec leur provenance dans le code.
- État : **phase 2 en cours**. Le pilote ouvre le port, réinitialise la clé, négocie EZSP (13
  ou plus), la configure comme un coordinateur, relance le réseau qu'elle garde (`networkInit`)
  ou en forme un s'il n'y en a pas (clés au coffre, paramètres dans
  `<data_dir>/zigbee-<instance>.json`), et publie l'appareil « coordinateur » (`firmware`,
  `ezsp_version`, `network`), en lecture seule. Reprise d'un réseau Zigbee2MQTT :
  `moli-os zigbee import <coordinator_backup.json>` range ses clés au coffre et son réseau dans
  le fichier d'état ; si la clé n'a plus de réseau, le pilote le restaure au démarrage (même
  clé seulement, compteur de trames sauvegardé + 65 536), sans réappairage. Le pilote relève
  le compteur de trames de la clé (au démarrage du réseau, puis toutes les heures) : une clé
  effacée plus tard se restaure depuis ce relevé. Retour à Zigbee2MQTT :
  `moli-os zigbee export <fichier>` écrit une sauvegarde Open Coordinator Backup (clés lues au
  coffre, fichier privé à supprimer après usage). Appairage et appareils : étapes suivantes.
- Essayé sur : à remplir (clé, firmware, date).
- Les tests rejouent un NCP simulé (`src/sim.rs`) qui parle le vrai ASH. Fixture :
  `fixtures/coordinator_backup.json`, une sauvegarde **fictive** au format Open Coordinator
  Backup tel que l'écrit zigbee-herdsman (adaptateur ember).

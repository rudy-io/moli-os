# Zigbee natif

- Pilote sans Zigbee2MQTT ni broker : Moli parle EZSP (ASH sur série ou TCP) à un coordinateur
  Silicon Labs, `crates/moli-zigbee`. Dormant : rien ne tourne sans `[[driver]] kind = "zigbee"`.
- Réutilisé : la documentation Silicon Labs (UG100 pour EZSP, UG101 pour ASH, UG600 pour
  EmberZNet) ; les tables du protocole (identifiants de trames, statuts, valeurs de
  configuration d'un coordinateur) sont reprises de zigbee-herdsman (MIT, crédit dans `NOTICE`),
  avec leur provenance dans le code.
- État : **phases 1 à 3 livrées, essayées sur du vrai matériel**. Le pilote ouvre le
  port (en exclusivité), réinitialise la clé, négocie EZSP (13 ou plus), la configure comme un
  coordinateur, relance le réseau qu'elle garde (`networkInit`) ou en forme un s'il n'y en a
  pas (clés au coffre, paramètres dans `<data_dir>/zigbee-<instance>.json`), et publie
  l'appareil « coordinateur » (`firmware`, `ezsp_version`, `network` en lecture ; `permit_join`
  écrivable : ouvre l'appairage 254 s, accord humain pour un agent ; `joining` : le dernier
  appareil arrivé, noté dans le fichier d'état). Reprise d'un réseau Zigbee2MQTT :
  `moli-os zigbee import <coordinator_backup.json>` range ses clés au coffre et son réseau dans
  le fichier d'état ; si la clé n'a plus de réseau, le pilote le restaure au démarrage (même
  clé seulement, compteur de trames sauvegardé + 65 536), sans réappairage. Le pilote relève
  le compteur de trames de la clé (au démarrage du réseau, toutes les heures, et à l'arrêt) :
  une clé effacée plus tard se restaure depuis ce relevé, s'il date de 24 h au plus. Retour à
  Zigbee2MQTT : `moli-os zigbee export <fichier>` écrit une sauvegarde Open Coordinator Backup
  (clés lues au coffre, fichier privé à supprimer après usage).
- **Appareils (phase 3)** : un appareil qui arrive est interrogé (descripteurs, modèle,
  fabricant, capacités), retenu dans le fichier d'état, relié au coordinateur (rapports), et
  publié avec les clés et les unités de Zigbee2MQTT : `state`, `brightness`, `color_temp`,
  `color`, `temperature`, `humidity`, `contact`, `battery`, `power`, `linkquality`… Générique
  par cluster, rien par marque. Pas encore : familles propriétaires (Tuya, Xiaomi, Lixee),
  actions des télécommandes, retrait d'un appareil depuis Moli (phase 4).
- Essayé sur : clé Sonoff ZBDongle-E (EmberZNet 7.4.4.0, EZSP 13), Raspberry Pi 3 B+, ampoule
  Innr RB 287 C (firmware 1.9.64) : appairage, interview, rapports, ordres, redémarrage, le
  6 novembre 2026. Capteurs et prises : sur simulateur seulement à ce jour.
- Les tests rejouent un NCP simulé (`src/sim.rs`) qui parle le vrai ASH. Fixtures :
  `fixtures/coordinator_backup.json`, une sauvegarde **fictive** au format Open Coordinator
  Backup tel que l'écrit zigbee-herdsman (adaptateur ember) ; `fixtures/innr_rb287c.json`, les
  **vraies** trames d'une ampoule (son adresse courte remplacée).

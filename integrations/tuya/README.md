# Tuya (local)

- Aucun code repris : trames 3.3 / 3.4 / 3.5 vérifiées octet par octet contre la sortie
  d'implémentations existantes (localtuya, tinytuya), règle de tinytuya pour les en-têtes de version ; l'import lit le
  compte Tuya déjà lié à HA (`tools/home-assistant/tuya-export.py`).
- Pas de fixture d'appareil : les tests (`src/protocol.rs`) utilisent des trames produites
  par ces implémentations (vecteurs d'interopérabilité), pas des captures.
- Vérifié en vrai : import depuis HA, lecture et écriture locales sur des prises Tuya.
- Sous-appareils d'une passerelle : lus et commandés en local par elle (protocole 3.3, demande
  `{"cid", "t"}` et ordre `{"cid", "t", "dps"}` comme tinytuya), quand le fichier donne leur `cid`
  et l'identifiant de la passerelle. Vérifié en vrai : lecture de 9 modules fil pilote et 2
  capteurs derrière une passerelle 3.3.
- Reste : la version 3.1 n'est pas gérée ; les sous-appareils d'une passerelle 3.4+, ou sans
  `cid`, restent au cloud (lecture seule).
- Import sans Home Assistant : `tools/tuya-iot/tuya-iot-export.py`, par un projet cloud Tuya
  IoT (vérifié en vrai : 13 appareils dont 11 derrière une passerelle, clés, `cid`, codes
  standard des capteurs).

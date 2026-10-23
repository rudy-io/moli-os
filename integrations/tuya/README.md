# Tuya (local)

- Aucun code repris : trames 3.3 / 3.4 / 3.5 vérifiées octet par octet contre la sortie
  d'implémentations existantes (localtuya, tinytuya), règle de tinytuya pour les en-têtes de version ; l'import lit le
  compte Tuya déjà lié à HA (`tools/home-assistant/tuya-export.py`).
- Pas de fixture d'appareil : les tests (`src/protocol.rs`) utilisent des trames produites
  par ces implémentations (vecteurs d'interopérabilité), pas des captures.
- Vérifié en vrai : import depuis HA, lecture et écriture locales sur des prises Tuya.
- Sous-appareils d'une passerelle : lus en local par elle (protocole 3.3, demande `{"cid", "t"}`
  comme tinytuya), quand le fichier donne leur `cid` et l'identifiant de la passerelle.
- Reste : la version 3.1 n'est pas gérée ; les sous-appareils d'une passerelle ne sont pas
  encore commandés (ni en 3.4+, ni lus sans `cid`).
- Import sans Home Assistant : `tools/tuya-iot/tuya-iot-export.py`, par un projet cloud Tuya
  IoT (vérifié en vrai : 13 appareils dont 11 derrière une passerelle, clés, `cid`, codes
  standard des capteurs).

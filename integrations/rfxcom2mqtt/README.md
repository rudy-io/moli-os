# RFXcom 433 MHz (via rfxcom2mqtt)

- Réutilisé : le protocole MQTT de rfxcom2mqtt (états sur `devices/{id}` et
  `devices/{id}/{unité}`, ordres sur `cmd/{type}/{sous-type}/{id}/{unité}`, état du pont
  sur `bridge/status`) et sa découverte Home Assistant (une configuration retenue par
  entité). Aucun code repris.
- Identité : le sujet d'état sous `devices/` (`0x5A01`, ou `0x00ABCDE1/12` pour une unité
  d'un interrupteur ; le `/` devient `_`). Le nom donné dans rfxcom2mqtt n'est qu'une
  suggestion.
- **Seuls les appareils annoncés par la découverte deviennent des appareils.** Un récepteur
  433 MHz entend toute la rue : rfxcom2mqtt garde un sujet d'état pour chaque sonde du
  voisinage (plus de mille sur une maison réelle). Il faut donc
  `homeassistant.discovery: true` dans rfxcom2mqtt, et l'option `discovery_topic` du pilote
  doit valoir son `homeassistant.discovery_topic`.
- Un appareil qui n'a que son signal reste masqué : rfxcom2mqtt annonce un capteur
  « linkquality » sur l'identifiant nu d'un interrupteur (`devices/{id}`), où aucun état
  n'arrive jamais (ils vont sur `devices/{id}/{unité}`).
- Points : `temperature` (°C), `humidity` (%), `pressure` (hPa, champ `barometer`),
  `battery` (%, le niveau RFXtrx 0–9 converti : 9 → 100 %, 0 → 0 %), `rssi` (signal
  0–15, sans unité) en lecture ; `on` en écriture pour un interrupteur (`lighting1`…
  `lighting6`), envoyé sur le sujet de commande que donne sa configuration
  (`payload_on` / `payload_off`). Un autre champ de capteur devient un point en lecture
  seule sous son nom (`batteryVoltage` → `battery_voltage`). Un `binary_sensor` est lu
  tel quel, sans sens (le « on » d'une porte est « ouverte », le contact de Moli est
  « fermée » : rien n'est deviné). Volets RFY et `select` : pas encore couverts.
- `fixtures/discovery.json` (configurations de découverte : deux sondes Oregon, une prise
  Chacon `lighting2`, une entité du pont et une configuration Zigbee2MQTT du même broker,
  toutes deux ignorées) et `fixtures/states.json` (états : trames reçues, écho d'un ordre
  envoyé, sonde d'un voisin sans découverte) : captures réelles rendues anonymes, rejouées
  par les tests de `crates/moli-rfxcom2mqtt/src/catalog.rs`.
- **Vérifié sur du vrai matériel** (18 novembre 2026) : rfxcom2mqtt 1.2.1, RFXtrx433
  firmware 1025 (Type 2). Prise Chacon (`lighting2`, sous-type AC) : `On` puis `Off` publiés
  sur son sujet de commande, émis par le RFXtrx (« ACK - transmit OK »), la lumière s'allume
  et s'éteint ; l'écho publié ensuite sur `devices/{id}/{unité}` ne porte que `type`,
  `command`, `commandNumber` et `rfxFunction`. Sondes Oregon THGR810 et THGN132
  (`temperaturehumidity1`) : formes des trames et de leurs configurations relevées. Le pilote, lancé en mode
  miroir (aucun ordre envoyé) contre ce pont : 34 appareils annoncés, valeurs relues des
  messages retenus puis en direct, sondes du voisinage sans découverte ignorées.
- Pièges : le 433 MHz ne répond jamais. Un ordre réussi veut dire « remis au pont », pas
  « entendu par la prise » ; l'état publié est celui que le pont suppose. Une sonde Oregon
  change d'identifiant quand on change ses piles : c'est un nouvel appareil, à renommer, et
  l'ancien disparaît quand rfxcom2mqtt retire sa configuration.

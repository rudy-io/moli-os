# Zigbee2MQTT

- Réutilisé : le protocole MQTT de Zigbee2MQTT ; les points viennent des `exposes`, sans code
  par marque.
- `fixtures/bridge_devices.json` (7 appareils) et `state.json` : captures réelles, rejouées
  par les tests de `crates/moli-z2m/src/catalog.rs`.
- Identité : adresse IEEE (le `friendly_name` ne sert qu'aux sujets).
- Reste : Z2M ne rejoue pas les états, les appareils endormis ne sont connus qu'à leur
  prochain message (ou par le cache d'état).
- **Vérifié sur du vrai matériel** (octobre 2026) : capteur d'ouverture HOBEIAN ZG-102ZA, `contact`
  suit l'état réel (`true` = fermée) ; firmware non relevé.
- **Vérifié sur du vrai matériel** (octobre 2026) : ampoules Moes ZB-TD5-RCW-GU10 et Tuya
  CK-BL702-AL-01, luminosité en % (100 % envoie le niveau 254, 50 % le niveau 127) ; firmware non
  relevé.

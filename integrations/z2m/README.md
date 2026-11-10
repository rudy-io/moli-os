# Zigbee2MQTT

- Réutilisé : le protocole MQTT de Zigbee2MQTT ; les points viennent des `exposes`, sans code
  par marque.
- `fixtures/bridge_devices.json` (10 appareils, dont un volet anonymisé à partir d'un vrai Lonsonho QS-Zigbee-C03 et un Tuya ZG-2002-RF à deux relais en mode impulsion et un capteur de sol COOLO CS-201Z) et `state.json` : captures réelles, rejouées
  par les tests de `crates/moli-z2m/src/catalog.rs`.
- Identité : adresse IEEE (le `friendly_name` ne sert qu'aux sujets).
- Reste : Z2M ne rejoue pas les états, les appareils endormis ne sont connus qu'à leur
  prochain message (ou par le cache d'état).
- **Vérifié sur du vrai matériel** (octobre 2026) : capteur d'ouverture HOBEIAN ZG-102ZA, `contact`
  suit l'état réel (`true` = fermée) ; firmware non relevé.
- **Vérifié sur du vrai matériel** (octobre 2026) : ampoules Moes ZB-TD5-RCW-GU10 et Tuya
  CK-BL702-AL-01, luminosité en % (100 % envoie le niveau 254, 50 % le niveau 127) ; firmware non
  relevé.
- **Vérifié sur du vrai matériel** (octobre 2026) : volets SONOFF MINI-ZBRBS et Lonsonho
  QS-Zigbee-C03, ouvrir / stop / fermer et curseur depuis le tableau de bord, la position suit
  Z2M ; firmware non relevé. `moving` vaut `UP` / `DOWN` chez Lonsonho et `forward` / `reverse`
  chez SONOFF.
- **Vérifié sur du vrai matériel** (3 novembre 2026) : ampoule couleur Innr RB 287 C, firmware
  1.9.64, Zigbee2MQTT 2.14.2 : rouge, vert, bleu par le point `color` (« #rrggbb »), relus par
  l'ampoule ; `color_temp` vide en mode couleur, de retour au blanc (370 mired).
- **Vérifié sur du vrai matériel** (7 novembre 2026) : Tuya ZG-2002-RF (TS0002, `_TZ3000_nuenzetq`) posé sur la carte d'un moteur de portail, relais en mode `inching` (1 s) déclarés dans `impulse_points` : le tableau de bord affiche un bouton « Impulsion » par relais, sans état allumé/éteint ; « Relais 1 » ouvre le portail en mode piéton, « Relais 2 » en ouverture complète (mode voiture) ; firmware non relevé.
- **Vérifié sur du vrai matériel** (9 novembre 2026) : capteur de sol COOLO CS-201Z (HOBEIAN ZG-303Z), firmware `0103062026`, planté dans un pot : la pièce montre une pastille plante avec l'humidité de la terre, orange « à arroser » dès qu'elle passe sous `soil_warning` (50 %, réglé sur le capteur) ; la température et l'humidité du pot ne comptent plus dans celles de la pièce.

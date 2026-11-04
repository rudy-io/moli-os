# Onboarding : Tuya en local

1. Demander : les appareils sont-ils dans un compte Tuya / Smart Life déjà lié à Home
   Assistant ? (L'import passe par lui ; sans HA, non documenté.)
2. Contrôle sans secret : `docker exec -i homeassistant python3 - --summary < tuya-export.py`.
3. Import (clés jamais affichées) : `… python3 - < tuya-export.py | docker exec -i moli-os
   /moli-os tuya import`. Les clés vont au coffre, `tuya.json` sans clé.
4. Déclarer `kind = "tuya"` ; `moli-os check-config`, redémarrer. Corriger à la main `ip`,
   `version` ou `room` dans `tuya.json` si besoin (conservés aux imports suivants).
5. Tester une prise neutre, humain présent, jamais un radiateur de chambre.

## Aspirateurs robots : Rowenta et Tefal X-plorer Serie 75

Il faut la clé locale du robot, que l'application Rowenta ne donne pas.

1. Repérer le robot : il émet sur UDP 6667 ; le statut de `moli-os` ou le routeur donne son adresse.
2. **Action humaine** : associer le robot à l'application **Smart Life** ou **Tuya Smart** (il quitte
   l'application Rowenta, avec sa carte et ses programmes à refaire).
3. Lier ce compte à l'intégration Tuya de Home Assistant, puis, sans jamais afficher la sortie :
   `docker exec -i homeassistant python3 - < tools/home-assistant/tuya-export.py | docker exec -i moli-os /moli-os tuya import`.
   Sans Home Assistant : écrire le robot dans `tuya.json` (`id`, `name`, `category = "sd"`, `product = "X-plorer Serie 75"`, `ip`,
   `version = "3.3"`, sans `dps`) et ranger sa clé par un tube : `moli-os secrets set tuya key-<id>`.
4. Déclarer `kind = "tuya"` ; `moli-os check-config`, redémarrer. Sur un réseau isolé (sans les annonces UDP), ajouter `ip` et `version = "3.3"` dans `tuya.json`.
5. Tester sans démarrer le robot : `state` et `battery` se lisent, `locate` le fait sonner. Puis, robot
   surveillé et pièce dégagée : `clean`, `pause`, `dock`.

# Onboarding : Tuya en local

1. Demander : y a-t-il un projet cloud Tuya (plateforme IoT de Tuya, compte Smart Life lié
   au projet), ou les appareils sont-ils dans un compte déjà lié à Home Assistant ?
2. **Avec un projet cloud** (sans HA) : ses clés (Access ID, Access Secret) données sur la page
   Système (code demandé), `cloud_region` dans les options du pilote. Contrôle sans secret :
   `moli-os tuya import --cloud --summary`, puis l'import : `moli-os tuya import --cloud`.
3. **Par Home Assistant** : contrôle sans secret
   `docker exec -i homeassistant python3 - --summary < tuya-export.py`, puis l'import (clés
   jamais affichées) : `… python3 - < tuya-export.py | docker exec -i moli-os /moli-os tuya import`.
   Dans les deux cas, les clés vont au coffre, `tuya.json` sans clé.
4. Déclarer `kind = "tuya"` ; `moli-os check-config`, redémarrer. Corriger à la main `ip`,
   `version` ou `room` dans `tuya.json` si besoin (conservés aux imports suivants).
5. Tester une prise neutre, humain présent, jamais un radiateur de chambre.

## Aspirateurs robots : Rowenta et Tefal X-plorer Serie 75

Il faut la clé locale du robot, que l'application Rowenta ne donne pas.

1. Repérer le robot : il émet sur UDP 6667 ; le statut de `moli-os` ou le routeur donne son adresse.
2. **Action humaine** : associer le robot à l'application **Smart Life** ou **Tuya Smart** (il quitte
   l'application Rowenta, avec sa carte et ses programmes à refaire).
3. Lier ce compte au projet cloud Tuya puis `moli-os tuya import --cloud`, ou à l'intégration
   Tuya de Home Assistant, puis, sans jamais afficher la sortie :
   `docker exec -i homeassistant python3 - < tools/home-assistant/tuya-export.py | docker exec -i moli-os /moli-os tuya import`.
   Sans Home Assistant : écrire le robot dans `tuya.json` (`id`, `name`, `category = "sd"`, `product = "X-plorer Serie 75"`, `ip`,
   `version = "3.3"`, sans `dps`) et ranger sa clé par un tube : `moli-os secrets set tuya key-<id>`.
4. Déclarer `kind = "tuya"` ; `moli-os check-config`, redémarrer. Sur un réseau isolé (sans les annonces UDP), ajouter `ip` et `version = "3.3"` dans `tuya.json`.
5. Tester sans démarrer le robot : `state` et `battery` se lisent, `locate` le fait sonner. Puis, robot
   surveillé et pièce dégagée : `clean`, `pause`, `dock`.

## Centrales d'alarme : Daewoo SA501

1. Obtenir la clé locale par l'import cloud (`moli-os tuya import --cloud`) ou par l'export Home
   Assistant, comme les autres appareils Tuya. La centrale est en catégorie `mal`.
2. Écrire dans `tuya.json` (`id`, `name`, `category = "mal"`, `product = "Daewoo SA501"`, `ip`,
   `version = "3.3"`, `room` si on veut la voir dans une pièce (ou la régler ensuite dans
   l'Atelier, sans redémarrer), et les `dps` 101 à 121 — voir
   [docs/components/alarme.md](../../docs/components/alarme.md)). Ranger la clé par un tube :
   `moli-os secrets set tuya key-<id>`.
3. `moli-os check-config`, redémarrer. La centrale doit apparaître dans Atelier avec
   `system_arm_type`.
4. Tester **sans armer la maison** : lire le mode, régler un délai, vérifier la batterie et le GSM.
   Puis, un humain présent et aucune pièce protégée occupée : essayer `armed` et `disarmed`.
5. Ne pas reprendre dans `tuya.json` le mot de passe, les numéros de téléphone, l'IMEI ni les codes
   bruts : ils ne servent pas à Moli.

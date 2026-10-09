# Onboarding : Tuya en local

1. Demander : les appareils sont-ils dans un compte Tuya / Smart Life déjà lié à Home
   Assistant ? (L'import passe par lui ; sans HA, non documenté.)
2. Contrôle sans secret : `docker exec -i homeassistant python3 - --summary < tuya-export.py`.
3. Import (clés jamais affichées) : `… python3 - < tuya-export.py | docker exec -i moli-os
   /moli-os tuya import`. Les clés vont au coffre, `tuya.json` sans clé.
4. Déclarer `kind = "tuya"` ; `moli-os check-config`, redémarrer. Corriger à la main `ip`,
   `version` ou `room` dans `tuya.json` si besoin (conservés aux imports suivants).
5. Tester une prise neutre, humain présent, jamais un radiateur de chambre.

# Onboarding : box internet

1. Demander quelle box (Livebox, Freebox…) et si l'UPnP y est activé.
2. Aucun identifiant. Sans option, le pilote cherche lui-même (SSDP, 3 s).
3. Si rien ne répond : reprendre l'URL de description (`…/gatedesc.xml`) de la config HA,
   la mettre dans `location`.
4. Déclarer `kind = "igd"` ; `moli-os check-config`, redémarrer.
5. Tester : `connected`, `external_ip`, `line_down` ont une valeur. Lecture seule.

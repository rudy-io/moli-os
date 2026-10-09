# Onboarding : iopool

1. Demander si la sonde est reliée à l'appli iopool, et combien de bassins a le compte (seul
   le premier est lu).
2. Clé personnelle : appli iopool → Réglages → clé API. L'humain la range lui-même, par un
   tube : `moli-os secrets set <instance> api_key`. Elle ne part que vers `api.iopool.com`.
3. Déclarer `kind = "profile"`, `profile = "iopool"` ; `moli-os check-config`, redémarrer.
4. Tester : `temperature`, `ph`, `orp` ont une valeur ; sans clé, statut `waiting`.

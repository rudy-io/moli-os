# Onboarding : TP-Link Tapo

1. Demander : l'adresse IP de l'appareil (fixe de préférence), et s'il est alimenté (ampoule
   coupée au mur : le pilote attend).
2. Secret : `auth_hash` = base64 de `sha256(sha1(utilisateur) + sha1(mot de passe))` du compte
   TP-Link, la forme sous laquelle HA le garde. L'humain le range par un tube :
   `moli-os secrets set <instance> auth_hash` ; le mot de passe n'est jamais stocké.
3. Déclarer `kind = "tapo"`, `host` ; `moli-os check-config`, redémarrer.
4. Tester, humain présent, hors chambre : éteindre puis rallumer, changer la luminosité.

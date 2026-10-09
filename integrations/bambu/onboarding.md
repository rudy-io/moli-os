# Onboarding : Bambu Lab

1. Demander : l'adresse IP de l'imprimante et son numéro de série (affiché sur l'imprimante
   et dans Bambu Studio).
2. Code d'accès : affiché sur l'écran de l'imprimante. L'humain le range par un tube :
   `moli-os secrets set <instance> access_code`. Il faut une clé maître (empreinte `cert`).
3. Déclarer `kind = "bambu"`, `host`, `serial` (et `name` si voulu) ;
   `moli-os check-config`, redémarrer.
4. Tester imprimante allumée : `state` et températures ; allumer puis éteindre la lumière.
   Éteinte (prise coupée), le pilote attend et réessaie chaque minute.

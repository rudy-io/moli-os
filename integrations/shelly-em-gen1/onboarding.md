# Onboarding : Shelly EM (1re génération)

1. Demander l'adresse IP du compteur (de préférence fixe, réservée dans la box). Vérifier que
   `http://<ip>/shelly` répond `"type": "SHEM"` et `"auth": false` ; un appareil protégé par
   mot de passe n'est pas pris en charge.
2. Vérifier que `/status` donne une heure (`time` non vide) : sans heure (pas de SNTP
   joignable), les compteurs `total` restent à zéro. Le régler sur l'appareil si besoin.
3. Déclarer `kind = "profile"`, `profile = "shelly-em-gen1"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer.
4. Tester : `power_1`, `power_2` et `energy_1`, `energy_2` ont une valeur. Dans `[energy]`, la
   pince de l'arrivée sert de compteur `total` (`energy_1`, `power = …/power_1`) et
   d'injection `export` (`energy_returned_1`) ; une pince sur une production solaire, de
   compteur `solar`. Garder `reset = "auto"` (les compteurs peuvent repartir de zéro).

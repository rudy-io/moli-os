# Onboarding : alarme

1. À la reprise de HA, lire la configuration d'Alarmo (`.storage/alarmo.storage`) : modes
   actifs et leurs délais (`exit_time`, `entry_time`, `trigger_time`), capteurs et leurs
   modes, `use_entry_delay` (faux → `immediate = true`).
2. Trouver dans Moli le point de chaque capteur (zone Frigate : `frigate:zone_<zone>/<objet>`,
   porte : `…/contact`). Un capteur désactivé dans Alarmo n'est pas repris.
3. Déclarer `kind = "alarm"`, `[driver.options.modes.<mode>]` et un `[[driver.options.sensor]]`
   par capteur ; mettre l'appareil de l'alarme dans `[guard] protected_devices` (désarmer
   demande le code) ; `moli-os check-config`, redémarrer.
4. Reprendre en automatismes la programmation (heures d'armement) et les notifications
   d'Alarmo, puis les désactiver dans HA au moment de la bascule. Sans Alarmo à reprendre :
   au moins `automatisme-declenchee.json` (prévenir quand elle se déclenche).
5. Tester : armer avec un capteur actif (refusé, avec son nom), armer, passer devant un
   capteur, désarmer pendant le délai d'entrée.

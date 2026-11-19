# Onboarding : alarme

1. À la reprise de HA, lire la configuration d'Alarmo (`.storage/alarmo.storage`) : modes
   actifs et leurs délais (`exit_time`, `entry_time`, `trigger_time`), capteurs et leurs
   modes, `use_entry_delay` (faux → `immediate = true`).
2. Trouver dans Moli le point de chaque capteur (zone Frigate : `frigate:zone.<zone>/<objet>`,
   porte : `…/contact`). Un capteur désactivé dans Alarmo n'est pas repris.
3. Déclarer `kind = "alarm"`, `[driver.options.modes.<mode>]` et un `[[driver.options.sensor]]`
   par capteur (32 au plus) ; dans `[guard]`, la recommandation du README : le code à chaque
   fois pour `mode` = `off` et pour `setup`, et, si la maison arme à la voix, `agents_may` sur
   `mode` (`home`, `night`, `away`) ; donner une pièce à l'alarme si `protect_unassigned` ;
   `moli-os check-config`, redémarrer.
4. Reprendre en automatismes la programmation (heures d'armement) et les notifications
   d'Alarmo, puis les désactiver dans HA au moment de la bascule. Sans Alarmo à reprendre :
   au moins `automatisme-declenchee.json` (prévenir quand elle se déclenche).
5. Tester : armer avec un capteur actif (refusé, avec son nom), vérifier que `silent` est
   vide (sinon un point mal écrit ou un appareil hors ligne), armer, passer devant un capteur,
   désarmer pendant le délai d'entrée.

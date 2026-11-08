# Onboarding : aides de la maison

1. Demander quelles valeurs la maison doit retenir (mode, alarme, budget…) ; à la reprise de
   HA, lister ses `input_*` utilisés par les automatisations et garder leurs noms.
2. Pour chacune : `id` (jamais renommé), `name`, `kind` (`select`, `switch`, `number`,
   `text`, `aggregate`), `options` ou `min` / `max` / `step`, `initial`, `room` si elle
   appartient à une pièce.
3. Pour un `aggregate` (capteur virtuel, ex. température moyenne) : lister `points`
   (tous les points à combiner, format `instance:appareil/cle`), choisir `op` (`avg`, `min`,
   `max`), donner `unit` et `semantic` (ex. `°C`, `temperature`), et `stale_s` si un capteur
   sans relevé récent ne doit plus compter.
4. Déclarer `kind = "helpers"` et un `[[driver.options.helper]]` par aide ;
   `moli-os check-config`, redémarrer.
5. Tester : changer une valeur depuis le tableau de bord, redémarrer, elle est gardée ;
   pour un `aggregate`, vérifier que `value` suit la statistique attendue des points sources.

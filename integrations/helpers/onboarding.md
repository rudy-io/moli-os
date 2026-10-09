# Onboarding : aides de la maison

1. Demander quelles valeurs la maison doit retenir (mode, alarme, budget…) ; à la reprise de
   HA, lister ses `input_*` utilisés par les automatisations et garder leurs noms.
2. Pour chacune : `id` (jamais renommé), `name`, `kind` (`select`, `switch`, `number`,
   `text`), `options` ou `min` / `max` / `step`, `initial`, `room` si elle appartient à une pièce.
3. Déclarer `kind = "helpers"` et un `[[driver.options.helper]]` par aide ;
   `moli-os check-config`, redémarrer.
4. Tester : changer une valeur depuis le tableau de bord, redémarrer, elle est gardée.

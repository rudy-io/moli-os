# Onboarding : calendrier (week-end, jours fériés, vacances scolaires)

0. Avec une zone scolaire, dire à la personne que cette zone (`A`, `B`, `C`…) est envoyée une fois
   par jour au site de l'Éducation nationale (`data.education.gouv.fr`). Rien d'autre ne sort.
1. Demander la zone scolaire (celle de l'académie de la maison), et si la maison est en Alsace ou
   en Moselle (deux jours fériés de plus).
2. Déclarer `kind = "calendar"` avec `school_zone` (facultatif) et `alsace_moselle` (`false` par
   défaut) ; `moli-os check-config`, redémarrer.
3. Tester : l'appareil « Calendrier » dit `weekend`, `holiday`, `workday` et `quiet_day` pour
   aujourd'hui ; avec une zone, `school_holidays` a une valeur (oui ou non) quelques secondes après
   le démarrage. Si le pilote signale que les vacances n'ont pas pu être lues : vérifier que le
   serveur joint `data.education.gouv.fr` en HTTPS.

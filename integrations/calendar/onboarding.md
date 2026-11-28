# Onboarding : calendrier (jours fériés, calendriers .ics)

1. Demander si la maison est en Alsace ou en Moselle (deux jours fériés de plus).
2. Demander quels calendriers la maison veut suivre (vacances scolaires de sa zone, congés…) et
   lesquels rendent un jour « calme » (`quiet = true`).
3. Pour chacun, obtenir un fichier `.ics` et le déposer dans le dossier `data` (par exemple
   `data/calendars/vacances.ics`). Vacances scolaires : l'Éducation nationale publie un `.ics` par
   zone (jeu de données « Calendrier scolaire » sur data.education.gouv.fr) ; le télécharger une
   fois, le remplacer quand une nouvelle année est publiée.
4. Déclarer `kind = "calendar"`, `alsace_moselle`, et un `[[driver.options.calendar]]` par
   calendrier (`id`, `name`, `file`, `quiet`) ; `moli-os check-config`, redémarrer.
5. Tester : l'appareil « Jours » dit `weekend`, `holiday`, `workday` et `quiet_day` pour
   aujourd'hui ; chaque calendrier dit `on` et son prochain événement. Si le pilote signale un
   fichier absent : vérifier le chemin (relatif au dossier `data`).

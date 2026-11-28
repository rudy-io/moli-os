# Calendrier (jours fériés, calendriers .ics)

Le genre du jour et les calendriers de la maison, pour les automatismes qui ne tournent pas
pareil un jour calme : les annonces plus tard le matin, un autre programme de chauffage, un
réveil qui ne sonne pas… **Rien ne sort de la maison.**

**« Jours »**, toujours là :

- `weekend` : samedi ou dimanche ;
- `holiday` et `holiday_name` : un jour férié français (« Noël », ou vide), calculé sur place :
  les dates fixes, le lundi de Pâques, l'Ascension et le lundi de Pentecôte ; avec
  `alsace_moselle = true`, le Vendredi saint et la Saint-Étienne en plus ;
- `workday` : ni week-end ni férié ;
- `quiet_day` : week-end, férié, ou un événement aujourd'hui dans un calendrier `quiet = true`.
  Tant qu'un calendrier `quiet` n'a jamais pu être lu, un jour ouvré est inconnu (`null`).

**Un appareil par calendrier** (`[[driver.options.calendar]]`), lu dans un fichier iCalendar
(`.ics`) du dossier `data`, déposé à la main : un export d'agenda, le calendrier scolaire d'une
zone (l'Éducation nationale publie un `.ics` par zone)… Le fichier est relu dès qu'il change ;
pour ajouter un événement à la main, on l'ajoute au fichier.

- `on` : un événement aujourd'hui ; `event` : son nom (plusieurs : séparés par des virgules) ;
- `next`, `next_event`, `next_days` : le prochain événement qui commence après aujourd'hui (date
  `AAAA-MM-JJ`, nom, jours d'ici là).

Ce qui est lu : les `VEVENT` (`SUMMARY`, `DTSTART` en date ou en heure, avec `TZID` ou en UTC,
`DTEND` ou `DURATION`, `STATUS:CANCELLED` écarté), à la journée : un événement compte pour
chaque jour qu'il touche, la fin d'un événement à la journée étant exclue (comme le veut le
format). Un événement dont la fin égale le début compte pour ce jour-là. Répétition :
`RRULE:FREQ=YEARLY` (avec `INTERVAL`, `COUNT`, `UNTIL`), pour un anniversaire ; une autre règle
n'est pas suivie (l'événement compte une fois, le journal le dit).

Un fichier absent ou illisible est signalé par le pilote ; les événements déjà lus restent
valables tant que le fichier n'est pas relu avec succès.

Fixture : un extrait du calendrier scolaire de la zone A tel que publié (données publiques ; les `UID` en `@example.com`).

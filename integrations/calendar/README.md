# Calendrier (week-end, jours fériés, vacances scolaires)

Le genre du jour, pour les automatismes qui ne tournent pas pareil un jour calme : les annonces
plus tard le matin, un autre programme de chauffage, un réveil qui ne sonne pas…

Un appareil, « Calendrier », en lecture seule :

- `weekend` : samedi ou dimanche ;
- `holiday` et `holiday_name` : un jour férié français (« Noël », ou vide), calculé sur place,
  sans réseau : les dates fixes, le lundi de Pâques, l'Ascension et le lundi de Pentecôte ; avec
  `alsace_moselle = true`, le Vendredi saint et la Saint-Étienne en plus ;
- `workday` : ni week-end ni férié ;
- avec `school_zone` : `school_holidays` et `school_holidays_name` (« Vacances de Noël », ou
  vide), du premier jour sans école au dernier (le jour de la rentrée n'en est pas) ; le pont de
  l'Ascension en fait partie ; l'été est celui des élèves ;
- `quiet_day` : week-end, férié ou vacances scolaires.

Les points changent à minuit dans `timezone` (`Europe/Paris` par défaut) : ils sont recalculés
chaque minute.

**Vacances scolaires** : lues une fois par jour dans les données ouvertes de l'Éducation nationale
(`data.education.gouv.fr`, jeu `fr-en-calendrier-scolaire`), pour les 400 jours à venir.
`school_zone` : `A`, `B`, `C`, ou une autre zone du jeu de données (`Corse`, `Réunion`,
`Martinique`…). Tant que le calendrier n'a jamais pu être lu, `school_holidays` est inconnu, et
`quiet_day` aussi un jour ouvré ; le pilote le signale et réessaie toutes les heures. Une fois lu,
il reste valable même si une lecture suivante échoue (le calendrier est publié des années à
l'avance).

**Vie privée** : seule la zone scolaire quitte la maison, vers `data.education.gouv.fr`, une fois
par jour. Sans `school_zone`, rien ne sort.

Fixture : une vraie réponse du jeu de données (zone A, deux académies), données publiques.

# Enedis (compteur Linky)

- Passe par la [Conso API](https://conso.boris.sh), service gratuit et non officiel qui relaie Enedis
  avec le consentement du client. Lecture seule, dépendante d'Internet.
- Données de la veille, jamais du jour : le compteur envoie ses mesures une fois par jour, en général
  vers 8 h le lendemain. Pour le temps réel, il faut lire le compteur sur place (`z2m`).
- Une requête par jour, à une heure propre au compteur entre 6 h et 10 h, plus une vers 14 h si la
  veille manque encore : c'est ce que demande le service, dont le quota est partagé. Un redémarrage
  n'ajoute aucune requête. Chaque requête couvre 7 jours, un trou se répare donc tout seul.
- Une mesure manquante écarte son heure et son jour, pour qu'une réponse suivante les complète : une
  heure partielle resterait fausse une fois classée.
- `hours` alimente le compteur `history = true` de `[energy]` : chaque kWh est classé à son heure
  réelle. Le prix fixe du compteur (`price`) s'applique ; heures creuses et pleines ne sont pas
  distinguées.
- Il faut que la courbe de charge soit activée dans l'espace client Enedis ; sans elle, aucune donnée
  n'arrive (la consommation journalière seule n'est pas lue).
- Un jeton refusé (401, 403) arrête le pilote jusqu'à l'intervention d'un humain.
- `fixtures/` : la forme d'une vraie réponse, avec un compteur, une date et des valeurs fictifs.
- Vérifié sur un compteur réel (pas de 15 minutes, novembre 2026). Le passage à l'heure d'été ou
  d'hiver n'est testé que sur le code ; les pas de 30 ou 60 minutes aussi.
- Reste : production, consommation journalière sans courbe de charge, heures creuses et pleines.

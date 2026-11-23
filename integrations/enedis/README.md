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
- Il faut que la courbe de charge soit activée dans l'espace client Enedis. Si Enedis ne la sert pas
  (404, « la sous-ressource demandée n'existe pas »), la même relève lit la consommation quotidienne :
  le total de chaque jour (31 derniers jours, pour combler les trous) arrive, sans le pic, et
  l'avertissement donne le motif d'Enedis. `[energy]` reçoit chaque jour entier et ne le répartit
  sur ses heures que s'il n'en a encore aucune : un jour donné en partie par la courbe n'est pas
  complété. Vu en novembre 2026 : la courbe en 404 même sur un seul jour, collecte horaire active,
  consentement valide ; la consommation quotidienne répondait.
- Un jeton refusé (401, 403) arrête le pilote jusqu'à l'intervention d'un humain.
- `fixtures/` : la forme d'une vraie réponse, avec un compteur, une date et des valeurs fictifs.
- Vérifié sur un compteur réel (pas de 15 minutes, novembre 2026). Le passage à l'heure d'été ou
  d'hiver n'est testé que sur le code ; les pas de 30 ou 60 minutes aussi.
- Reste : production, heures creuses et pleines.

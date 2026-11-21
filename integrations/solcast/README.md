# Solcast (prévision solaire)

- La production attendue sur les toitures de la maison, par demi-heure, avec une estimation basse
  (10 %) et haute (90 %), d'après [Solcast](https://solcast.com) (compte gratuit « Home User »).
  Lecture seule, dépendante d'Internet.
- **Quota** : 10 appels par jour pour un compte récent (`calls_per_day`), chaque toiture demandée
  comptant pour un. Avec deux toitures, cinq mises à jour par jour. Elles sont réparties sur les heures
  de lumière, lues dans la prévision elle-même (première et dernière demi-heure qui produisent) : la
  première au lever, la dernière au coucher (elle apporte aussi celle du lendemain). Le compteur
  repart à minuit UTC, comme chez Solcast.
- **Rien n'est perdu au redémarrage** : les prévisions et le compte d'appels sont gardés dans l'état
  privé de l'instance (il faut la clé maître) ; un redémarrage ne coûte aucun appel.
- **La courbe de toute la journée** : chaque demi-heure garde la dernière prévision reçue avant de
  passer. La réponse du matin dessine la journée entière, les suivantes ne redessinent que ce qui reste.
  On peut ainsi comparer la production réelle à ce qui était prévu, même pour les heures passées, sans
  l'appel « estimated actuals ».
- `curve` tient dans un point texte (4 000 caractères au plus) : d'hier minuit à demain soir, par
  demi-heure, `{"start": ms, "step": 1800, "kw": [...], "lo": [...], "hi": [...]}`, `null` là où rien
  n'est connu (hier est abandonné s'il ne tient pas).
- Une clé refusée (401, 403) ou une toiture inconnue (404) : avertissement, nouvel essai une heure plus
  tard. Un quota épuisé (429) : plus rien avant le lendemain UTC. Réseau en panne : nouvel essai dans
  10 minutes (l'appel ne compte pas).
- `fixtures/` : réponses **reconstituées** d'après la documentation de l'API (deux toitures, une
  journée de novembre), pas encore comparées à un vrai compte.
- Reste : la correction des prévisions par la production réelle (`energy.db`), le « dampening » par
  heure de l'intégration Home Assistant.

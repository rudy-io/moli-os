# Mitsubishi Electric MELCloud

- API web non officielle du **MELCloud classique** (`app.melcloud.com`) ; MELCloud Home est un
  autre service, avec une autre API, que ce pilote ne couvre pas.
- Lecture et **commandes** : marche/arrêt, mode, consigne, ventilateur, ailettes. Température de la pièce en lecture.
- Un ordre relit l'état de l'unité (`Device/Get`) puis le renvoie avec le champ changé et son drapeau (`Device/SetAta`) ; l'unité l'applique à son prochain contact avec le service. La liste affiche encore l'ancienne valeur : ce qui a été commandé fait foi 150 s.
- La consigne est arrondie au pas de l'unité et refusée hors de la plage du mode en cours. Un invité ou un réglage verrouillé dans l'application reste en lecture.
- Dépend d'Internet et du service : les valeurs sont celles que l'unité a remontées en dernier.
- Une seule requête (`User/ListDevices`) décrit tout le compte, toutes les 5 minutes par défaut
  (60 s au minimum). Le service annonce parfois `Offline` le temps d'une réponse : une unité ne
  passe hors ligne qu'après 3 réponses de suite.
- `fixtures/list_devices.json` : vraie réponse réduite aux champs lus, noms et identifiants
  remplacés.
- Vérifié sur deux unités air-air réelles en lecture ; commandes essayées sur l'une (novembre 2026) : marche, arrêt, consigne, mode, ventilateur, ailette verticale, ailette horizontale, relus auprès du service, appliqués en moins de 30 s. L'ailette horizontale ne s'applique qu'unité allumée : à l'arrêt, le service répond 200 mais l'unité ne suit pas (essayé une fois dans chaque cas). Le pilote refuse donc cet ordre sur une unité éteinte.
- Reste : énergie, pompes à chaleur (ATW) et ventilation (ERV).

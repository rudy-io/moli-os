# Somfy Protect

- Origine : écrit pour Moli d'après le comportement de l'API cloud de l'appli Somfy Protect
  (ex-Myfox). [somfy-protect-api](https://github.com/Minims/somfy-protect-api) et
  [somfy-protect-api-rs](https://github.com/jeremiehuchet/somfy-protect-api-rs) (GPL-3.0) ont
  servi de référence pour les adresses, les champs et les messages ; aucun code n'en est repris.
- Connexion OAuth2 (`sso.myfox.io`, `grant_type=password`, puis `refresh_token`) ; le jeton est
  gardé dans le coffre.
- Lecture, toutes les minutes : les sites (une alarme chacun, avec son mode : `disarmed`,
  `partial`, `armed`, vus dans Moli comme `disarmed`, `home`, `away`, et si elle sonne) et leurs
  appareils (Link, détecteurs, sirènes, télécommandes, caméras) avec batterie, signal radio et
  température.
- En direct, par le websocket d'événements : changements de mode, alarmes (intrusion, panique,
  fumée, fin), statut des appareils.
- Écriture : le mode de l'alarme (`PUT /v3/site/<id>/security`) ; l'ordre d'un agent attend une
  personne.
- L'API ne donne pas l'état ouvert/fermé des IntelliTAG.

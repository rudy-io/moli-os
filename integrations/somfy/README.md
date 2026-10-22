# Somfy Protect

- Origine : écrit pour Moli d'après le comportement de l'API cloud de l'appli Somfy Protect
  (ex-Myfox). [somfy-protect-api](https://github.com/Minims/somfy-protect-api) (GPL-3.0) a servi
  de référence pour les adresses et les champs ; aucun code n'en est repris.
- Connexion OAuth2 (`sso.myfox.io`, `grant_type=password`, puis `refresh_token`) ; le jeton est
  gardé dans le coffre.
- Lecture, toutes les minutes : les sites (une alarme chacun, avec son mode : `disarmed`,
  `partial`, `armed`, vus dans Moli comme `disarmed`, `home`, `away`) et leurs appareils (Link,
  détecteurs, sirènes, télécommandes, caméras) avec batterie, signal radio et température.
- Aucune commande pour l'instant.
- Reste : le mode en écriture (sous la garde), les événements en direct (websocket), l'état des
  détecteurs d'ouverture.

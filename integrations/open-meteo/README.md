# Open-Meteo (météo)

- Réutilisé : l'API publique `/v1/forecast` d'Open-Meteo, gratuite et sans clé ; remplace
  aussi le `sun` de HA (lever, coucher, jour).
- Aucune fixture : aucune réponse réelle n'a été gardée dans les tests, les points ne sont
  vérifiés qu'en fonctionnement réel.
- Reste : capturer une réponse (`fixtures/forecast.json`).

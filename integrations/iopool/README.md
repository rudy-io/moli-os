# iopool EcO (piscine)

- Réutilisé : l'API cloud d'iopool (`/v1/pools`, en-tête `x-api-key`).
- Seul le premier bassin du compte est lu.
- Aucune fixture : aucune réponse réelle gardée dans les tests ; vérifié seulement en
  fonctionnement réel, sur un vrai compte.
- Reste : capturer une réponse anonymisée ; un compte à plusieurs bassins n'est pas géré.

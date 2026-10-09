# TP-Link Tapo

- KLAP v2 en HTTP local ; la session est vérifiée octet par octet contre python-kasa (avec un
  condensé factice). Le secret est le condensé `auth_hash`, jamais le mot de passe.
- Pas de fixture : le test de `get_device_info` lit une réponse écrite dans le code.
- Cible : l'ampoule L530 ; pas encore vue répondre en vrai.
- Reste : prises et autres ampoules non vérifiées ; identité de repli = l'adresse `host`.

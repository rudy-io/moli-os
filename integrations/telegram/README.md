# Telegram

- Un appareil « Telegram » avec un point `notify` (texte, écriture) : Moli envoie le texte aux
  `chats` configurés, par l'API des bots, en HTTPS, sans relais.
- Canal « Telegram » des automatismes = une commande sur ce point (journalisée, au nom de
  l'automatisme).
- Le jeton n'est jamais dans `moli.toml` : un gestionnaire de secrets → fichier tmpfs monté en
  lecture seule (`token_file`, par défaut `/run/secrets/telegram.token`).
- Vérifié en vrai avec un bot et un destinataire.
- Reste : la réception (parler à Moli depuis Telegram) n'est pas faite.

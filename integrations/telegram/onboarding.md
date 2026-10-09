# Onboarding : Telegram

1. La personne crée le bot avec @BotFather (`/newbot`) et colle le jeton **directement dans
   le gestionnaire de secrets** de la maison, qui le livre au conteneur dans un fichier tmpfs
   (`token_file`, par défaut `/run/secrets/telegram.token`). Jamais de capture d'écran ni de
   copie dans la conversation : un jeton vu ailleurs se révoque (`/revoke`).
2. Elle écrit `/start` au bot (Telegram n'autorise un bot à écrire qu'après ça).
3. Son identifiant de discussion = son identifiant d'utilisateur Telegram (un nombre positif).
4. `[[driver]] kind = "telegram"`, `[driver.options] chats = [<id>]` ; redémarrer Moli (le
   pilote lit le jeton au démarrage).
5. Tester : une commande `notify` avec un texte court ; la page Système montre le bot.

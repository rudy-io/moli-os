# Onboarding : Somfy Protect

Par le cloud Somfy : les alarmes (sites) du compte, leur mode (modifiable), si elles sonnent, et
leurs appareils avec batterie, signal radio et température ; les changements arrivent en direct.
Tester l'écriture en redonnant à l'alarme le mode qu'elle a déjà.

1. Faire créer à la personne un **compte Somfy Protect invité réservé à Moli** (appli Somfy
   Protect → utilisateurs du site), avec son propre e-mail.
2. Déclarer l'instance :

   ```toml
   [[driver]]
   id = "somfy"
   kind = "somfy"
   ```

3. La personne range le compte dans le coffre (jamais dans la conversation), puis le client
   OAuth de l'appli Somfy Protect (`client_id`, `client_secret` : ceux que publient les
   bibliothèques libres qui parlent à cette API), et redémarre :

   `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set somfy email`,
   pareil pour `password`, `client_id` et `client_secret`, puis `docker compose restart moli-os`.
4. Tester : l'alarme apparaît avec son mode, chaque appareil avec son nom Somfy et sa batterie.
   Les journaux disent `somfy: logged in`, `somfy: device seen` une fois par type d'appareil, puis
   `somfy: listening for events`. Changer le mode depuis l'appli Somfy : Moli le suit aussitôt.

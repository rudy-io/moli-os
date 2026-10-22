# Onboarding : Eufy Security

Par le cloud Eufy : les HomeBases, caméras et sonnettes du compte,
leur batterie, le mode de sécurité, et les détections en direct (notifications push, comme
l'appli). Le mode de sécurité se change en P2P local : Moli doit être sur le même réseau que les
stations (UDP 32108). Tester en redonnant à une station le mode qu'elle a déjà.

1. Faire créer à la personne un **compte Eufy invité réservé à Moli** (application Eufy → partage
   de la maison ou de chaque appareil), avec son propre e-mail. Eufy ne garde qu'une session par
   compte : avec le compte principal, l'application du téléphone serait déconnectée.
2. Demander le **pays** du compte (`FR`, `DE`, `US`…) : il choisit les serveurs d'Eufy.
3. Déclarer l'instance, puis la personne range le compte invité dans le coffre (jamais dans la
   conversation) et redémarre :

   ```toml
   [[driver]]
   id = "eufy"
   kind = "eufy"

   [driver.options]
   country = "FR"
   # language = "fr"
   ```

   `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set eufy email`,
   pareil pour `password`, puis `docker compose restart moli-os`.
4. Première connexion : Eufy envoie un **code par e-mail**. Le pilote attend et le dit (page
   Système, journaux). La personne range le code (`… secrets set eufy verify_code`) et redémarre :
   Moli devient un téléphone de confiance, les connexions suivantes se font seules. Parfois Eufy
   demande d'abord un **captcha** : son image est servie par Moli (lien dans le message
   d'attente), la réponse va dans `captcha`, puis redémarrer.
5. Tester : chaque appareil apparaît avec son nom Eufy, sa batterie et, pour une station, son mode
   de sécurité. Les journaux disent `eufy: listening for events` ; passer devant une caméra
   (pas dans une pièce où l'on dort) : `motion` passe à vrai 10 s. Aucune commande n'est envoyée.

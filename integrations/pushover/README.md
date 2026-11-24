# Pushover (notifications sur les téléphones)

- Moli envoie ses notifications à l'appli [Pushover](https://pushover.net) (Android, iOS, bureau) par
  son API publique : un appel HTTPS sortant vers `api.pushover.net`, sans relais, sans appli Moli sur le
  téléphone, sans accès entrant ni appairage.
- Un appareil « Pushover » par instance, avec un point `push` en écriture, comme un téléphone de l'appli
  Moli : le canal `telephone` des automatismes et les rappels de l'assistant le trouvent sans rien
  changer ailleurs. Le texte s'écrit « Titre », un saut de ligne, puis le message ; une seule ligne
  devient le message, titré « Moli ». Un point `remaining` donne les messages restants du mois.
- Secrets `token` (jeton de l'application créée sur pushover.net) et `user` (clé de l'utilisateur ou
  d'un groupe), les noms de l'API Pushover. Jamais dans `moli.toml`, jamais dans un message d'erreur.
- Au démarrage : les clés sont vérifiées (`users/validate.json`), ainsi que les appareils nommés dans
  `devices` ; un refus arrête le pilote jusqu'à ce qu'une personne corrige et redémarre. Le quota du
  mois est lu (`apps/limits.json`), puis mis à jour à chaque message (en-têtes `X-Limit-App-*`).
- Un plafond par instance (`per_minute`, 5 par défaut ; `per_hour`, 30 par défaut) : au-delà, le
  message est refusé et journalisé. Un automatisme qui s'emballe ou un agent ne vide pas le quota du
  mois en une soirée.
- Un refus de Pushover pendant les envois (4xx : clé révoquée, appareil supprimé) arrête les envois
  jusqu'au redémarrage : Pushover bloque un moment une adresse qui insiste. Un quota épuisé (429) les
  arrête jusqu'à sa remise à zéro, puis ils reprennent d'eux-mêmes. Une panne réseau ou une erreur
  5xx ne fait échouer que le message en cours.
- Le texte reste du texte : pas de HTML (`html=0`), pas d'URL, pas de pièce jointe. Titre coupé à 250
  caractères et message à 1 024 (limites de Pushover), avec « … », plutôt que refusés.
- Vérifié sur un faux serveur (envoi, options, coupure, appareils, clés refusées, quota, plafond,
  pannes). Compte réel : à confirmer.
- Reste : la priorité urgente (2, avec relance, accusé de réception et annulation), l'image jointe
  (instantané de caméra), l'URL, la durée de vie : il faudrait des options par message dans le nœud
  `notify`.

# Ring (sonnettes et caméras)

- Origine : API partenaire Amazon Vision (portail développeur Ring, `developer.amazon.com/ring/console/apps`) ;
  l'API est documentée par Ring/Amazon, rien n'est repris d'une autre intégration.
- **Lecture seule** : événements (sonnette, mouvement personne/véhicule/animal) et instantanés à la demande.
- Le compte est lié **dans un seul sens (Ring → Moli)**, le lien unidirectionnel de l'API partenaire :
  - `POST /api/ring/link` : le backend Ring POSTe le code d'autorisation à usage unique
    (form-encoded, 60 s) ; route publique — le code est inusable sans le `client_secret` du coffre ;
  - `GET /api/ring/linked?nonce=…&time=…` : l'appli Ring redirige le navigateur ; la confirmation
    exige une session humaine (code PIN) et un nonce HMAC-SHA256 signé par Ring (fenêtre 600 s,
    base64 URL-safe sans padding).
- Secrets (coffre, par instance) :
  - `client_id`, `client_secret`, `hmac_key` : rangés par l'humain (`moli-os secrets set ring …`) ;
  - `refresh_token` : écrit par l'API Moli au moment de la confirmation du lien (le pilote le lit au
    démarrage, et il **tourne** à chaque rafraîchissement) ;
  - `hardware_id` : UUID v4 généré et rangé par Moli (le pilote et l'API l'envoient dans l'en-tête
    `Hardware_Id`, vérifié par Ring sur chaque appel).
- Le pilote interroge les événements toutes les 15 s (4 types par caméra) et la liste des appareils
  toutes les 5 min ; le tick est le seul rafraîchisseur de jeton d'accès (~4 h), et range le refresh
  tourné **avant** de l'utiliser.
- Les instantanés ne lisent que le jeton en mémoire : un 303 renvoie une URL présignée (jamais
  journalisée), puis un GET de la JPEG.
- Piège connu : l'API peut répondre **406** depuis des adresses IP de centre de données/cloud, même
  avec les bons en-têtes (voir `onboarding.md`).

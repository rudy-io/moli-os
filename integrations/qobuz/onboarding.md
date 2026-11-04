# Onboarding : Qobuz

Moli joue la musique de l'abonnement Qobuz sur les enceintes de la maison
(point `play` des pilotes `cast` et `sonos`) : « joue du jazz », « mets
l'album Thriller »…

**À savoir avant de l'installer.** Qobuz ne propose aucune interface
officielle pour ça : Moli utilise celle du player web Qobuz, comme le font
qobuz-dl et streamrip. Elle peut changer ou cesser sans préavis.

## Identifiants

Il faut deux couples :

- **`app_id` + `app_secret`** : la paire que le player web Qobuz utilise pour
  signer ses appels. On lit `app_id` dans les devtools de
  <https://play.qobuz.com> (onglet Réseau → n'importe quel appel
  `api.qobuz.com` ou `www.qobuz.com/api.json` → en-tête `X-App-Id` ou
  paramètre `app_id`). Le `app_secret` se lit dans le bundle JavaScript du
  player (onglet Sources → chercher un hexadécimal de 32 caractères près de
  `getFileUrl`) ; les projets publics qobuz-dl/streamrip en gardent aussi
  des valeurs à jour.
- **`email` + `password`** : les identifiants de l'abonnement (le login
  donne un jeton, gardé en mémoire seulement).

## Configuration

1. Ajouter un bloc `[[driver]]` dans `moli.toml` :
   ```toml
   [[driver]]
   id = "qobuz"
   kind = "qobuz"

   [driver.options]
   format_id = 6   # 5 = MP3 320 kb/s, 6 = FLAC 16/44.1, 7/27 = hi-res 24-bit
   queue = 5       # pistes mises en file par recherche
   ```
2. Enregistrer les 4 secrets dans le coffre chiffré (valeur par un tube,
   jamais au clavier ni affichée) :
   - `read -rs V; printf %s "$V" | moli-os secrets set qobuz app_id; unset V`
   - `read -rs V; printf %s "$V" | moli-os secrets set qobuz app_secret; unset V`
   - `read -rs V; printf %s "$V" | moli-os secrets set qobuz email; unset V`
   - `read -rs V; printf %s "$V" | moli-os secrets set qobuz password; unset V`
3. Redémarrer Moli. La carte « Compte Qobuz » (Système) passe à
   `connected` ; un `waiting` indique un secret manquant ou un login refusé.

## Limites

- La qualité `format_id` est plafonnée par l'abonnement (une piste non
  éligible est simplement ignorée dans la file).
- `app_secret` peut changer quand Qobuz met à jour son player : le remettre
  dans le coffre puis redémarrer.

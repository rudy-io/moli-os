# Apple TV

- Protocole **Companion** (TCP 49153), celui de la télécommande Apple d'un iPhone, tel que
  pyatv (MIT) le décrit : appairage HomeKit avec le code à 4 chiffres affiché par l'Apple TV
  (SRP-6a 3072 bits / SHA-512, clés Ed25519), puis à chaque connexion une vérification
  (X25519) qui donne les deux clés ChaCha20-Poly1305 de la session. Messages en OPACK.
- Points : `power` (lu : veille ou non, et écrit : réveil / veille ; HDMI-CEC emmène l'écran
  quand la télé le permet), `key` et `hold` (touches, appuyées ou maintenues), `app` (écrit :
  ouvre une appli par son nom, son identifiant ou un lien), `apps` (les applis qu'elle sait
  ouvrir, `nom=identifiant` par ligne), `media_control` (`play`, `pause`).
- Pas encore : ce qui se joue (titre, image, position : protocole MediaRemote, à venir), le
  texte dans un champ, le volume réglable (touches `VolumeUp` / `VolumeDown` seulement, si la
  télé ou la barre de son suit l'Apple TV).
- Identité : l'identifiant d'appairage de l'Apple TV (stable, pas l'adresse).
- Rien de chiffré sans appairage : le pilote attend le code (statut `waiting`). L'Apple TV
  abandonne la demande en moins d'une minute ; le code ne s'affiche pas toujours de lui-même
  (écran Réglages › Télécommandes et appareils › App Télécommande et appareils). Trop tard ou
  échec : le pilote attend un redémarrage de Moli pour en afficher un autre (pas de code qui
  revient tout seul sur l'écran de la famille).
- Vérifié en vrai le 18 nov. 2026 sur une Apple TV 4K (3ᵉ génération) : appairage, veille lue,
  applis listées, touches suivies à l'écran.
- Essais : OPACK et TLV8 sur les vecteurs de pyatv, SRP octet par octet contre un portage
  indépendant des formules de srptools, vérification d'appairage et session chiffrée contre
  une fausse Apple TV.

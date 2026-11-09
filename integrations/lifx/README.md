# LIFX

Pilote natif `crates/moli-lifx`. Une instance = toutes les ampoules LIFX qu'elle trouve.

- **Transport** : protocole LAN de LIFX, UDP 56700, sans compte ni secret. Écrit d'après la
  documentation publique de LIFX ; les trames sont vérifiées par les tests de `src/proto.rs`.
- **Trouver les ampoules** : `GetService` envoyé aux adresses de `hosts` et, sauf
  `discovery = false`, en diffusion sur le réseau local. Nouvelle recherche chaque minute tant
  qu'une ampoule manque (connue avant, ou adresse de `hosts` muette), sinon toutes les 5 min.
- **Sécurité** : le protocole LAN de LIFX n'a aucune authentification : tout appareil du réseau
  local peut piloter les ampoules, comme le fait Moli, et le pilote n'y peut rien. Ce qu'il fait :
  ne croire une réponse que venue de l'adresse et du port interrogés (identifiant et séquence
  aléatoires, numéro de série vérifié), ne déplacer une ampoule que si elle est muette à son
  adresse, et borner ce que coûte un déluge de faux numéros (100 ampoules par instance, un
  numéro nouveau par adresse et par recherche, les ordres d'abord).
- **Identité** : le numéro de série (12 chiffres hexadécimaux, la MAC Wi-Fi moins 1), jamais
  l'adresse. Une ampoule qui change d'adresse est suivie ; une autre ampoule à la même adresse
  ne la remplace pas.
- **Ordres** : avec accusé de réception (`ack_required`) ; la valeur n'est publiée qu'après
  l'accusé, sans accusé l'ordre échoue. Seule la composante demandée change
  (`LightSetWaveformOptional`, sinon `LightSetColor`) ; une ampoule éteinte prend la valeur puis
  s'allume en fondu ; aucun relevé pendant un fondu.
- **Réseau** : un `LightGet` par ampoule et par relevé ; micrologiciel, modèle et groupe lus à la
  rencontre puis reposés à chaque recherche tant qu'ils manquent ; signal à chaque recherche.
- **Capacités** : lues sur l'ampoule (`GetVersion`, `GetHostFirmware`) puis cherchées dans la
  table des produits du pilote. Un modèle inconnu n'a que la marche et la luminosité
  (avertissement du pilote).

## Vérifié sur du vrai matériel

| Modèle | Produit | Micrologiciel | Vérifié | Date |
|---|---|---|---|---|
| LIFX White | 88 | 3.90 | lecture (capture rejouée en fixture) ; marche/arrêt et luminosité (`LightSetWaveformOptional`), luminosité sur ampoule éteinte | 2026-11-09 |
| LIFX Mini W | 51 | 3.90 | lecture (capture rejouée en fixture) ; marche/arrêt et luminosité (`LightSetWaveformOptional`), luminosité sur ampoule éteinte | 2026-11-09 |

- Maison de référence : 4 ampoules (2 White, 2 Mini W), adresses fixes dans `hosts`. Nom et
  groupe LIFX repris en nom et pièce. Une ampoule coupée à l'interrupteur au démarrage de Moli
  est trouvée seule à la recherche suivante, une fois remise sous tension.

- La LIFX White, blanc fixe 2700 K, annonce 3500 K dans son état : sans conséquence, la
  température n'est proposée que si la plage du modèle est réglable.
- Chaque ampoule envoie parfois deux fois sa réponse à `GetService` : le pilote ignore le double.
- **Couleur** (`color`) et **température réglable** (`color_temp`) : **non testées sur du vrai
  matériel** (aucune ampoule couleur ni à blanc réglable dans la maison de référence) ; couvertes
  seulement par des tests sur des trames construites d'après la documentation.
- **Reste** : effets, multizone zone par zone, matrices, infrarouge, cycle HEV, thèmes.
- Idées reprises de [gladys-lifx](https://github.com/guim31/gladys-lifx) (Apache-2.0), réécrites
  en Rust : voir `[origin]` dans `integration.toml`.

Non affilié à LIFX : LIFX est une marque de son propriétaire.

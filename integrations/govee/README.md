# Govee

Pilote natif `crates/moli-govee`. Une instance = toutes les lumières Govee qu'elle trouve.

> **Jamais essayé sur une vraie lumière.** Ce pilote est écrit d'après le manuel public de
> l'API locale de Govee ; aucun appareil Govee n'était disponible. Les deux fixtures sont des
> messages **construits d'après ce manuel**, pas des captures. Tout ce qui suit décrit ce que le
> pilote fait, pas ce qu'une lumière a montré.

- **Transport** : API locale officielle de Govee (« LAN API »), JSON en UDP, sans cloud, sans
  compte, sans clé. Recherche (`scan`) envoyée au groupe de multidiffusion 239.255.255.250:4001 ;
  les lumières répondent au port 4002 de Moli ; ordres et relevés (`devStatus`) vers leur port
  4003.
- **À activer dans l'appli Govee**, lumière par lumière (réglages de l'appareil, « LAN Control »
  ou « Contrôle LAN »). **Tous les modèles ne l'ont pas** : sans cette option dans l'appli, la
  lumière ne répond pas et ce pilote ne peut rien pour elle.
- **Trouver les lumières** : `scan` en multidiffusion (sauf `discovery = false`) et aux adresses
  de `hosts`. Nouvelle recherche chaque minute tant qu'une lumière manque (aucune connue, connue
  avant, hors ligne, adresse de `hosts` muette), sinon toutes les 5 min.
- **Le port 4002** : les lumières ne répondent qu'à ce port. Une seule instance `govee` par
  machine, et aucun autre programme qui l'écoute sur la même machine (Home Assistant avec son
  intégration Govee locale, par exemple). En conteneur : réseau de l'hôte.
- **Identité** : l'identifiant que la lumière donne au `scan` (ses chiffres hexadécimaux), jamais
  l'adresse.
- **Sécurité** : l'API locale n'a aucune authentification : tout appareil du réseau peut piloter
  les lumières, le pilote n'y peut rien. Ses réponses ne portent ni numéro de requête, ni (pour
  `devStatus`) l'identifiant de la lumière : une réponse est rattachée à une lumière par
  l'adresse d'où elle vient. Le pilote ne croit donc que l'adresse d'origine du datagramme (pas
  celle écrite dedans), ne déplace une lumière que si elle est muette là où elle était, ne donne
  une adresse à une autre lumière que si celle qui la tenait est hors ligne, et borne une
  instance à 100 lumières.
- **Relevés** : un `devStatus` par lumière toutes les 5 s (`poll_secs`), sans attendre la
  réponse. Hors ligne après 3 relevés sans réponse ; les dernières valeurs restent.
- **Ordres** : l'API n'a pas d'accusé de réception. Un ordre ne part que si la lumière est en
  ligne et a répondu récemment (3 relevés + 1 s) ; sinon il est refusé, jamais gardé pour plus
  tard. Après l'envoi, l'état est relu : c'est ce que la lumière répond qui est publié, pas ce
  qui a été demandé. Si la relecture ne montre pas l'ordre, il est renvoyé une fois. L'ordre
  réussit dès que la lumière a répondu après lui (2,7 s au plus) ; sans aucune réponse, il échoue.
- **Capacités** : l'API ne dit pas ce qu'un modèle sait faire et le pilote n'a pas de table de
  modèles : les quatre points sont proposés à toutes les lumières. Un modèle sans blanc réglable
  ou sans couleur ignorera l'ordre, et la relecture le montrera.

## Points

| Clé | Sens | Message |
|---|---|---|
| `on` | marche/arrêt | `turn` |
| `brightness` | 1-100 % | `brightness` |
| `color_temp` | 2000-9000 K | `colorwc` (`colorTemInKelvin`) |
| `color` | `#rrggbb` | `colorwc` (`color`, température 0) |

Quand la lumière affiche une couleur, elle annonce une température de 0 : `color_temp` garde
alors sa dernière valeur.

## Vérifié sur du vrai matériel

| Modèle | Micrologiciel | Vérifié | Date |
|---|---|---|---|
| aucun | | **pas encore** : ni lecture, ni ordre, ni recherche | |

Ce que seuls des essais diront : si chaque modèle répond bien à un `scan` envoyé à son adresse
(et pas seulement en multidiffusion), ce que `devStatus` annonce en couleur quand la lumière est
en blanc, si un ordre de luminosité allume une lumière éteinte, le délai avant que `devStatus`
reflète un ordre, et les modèles qui ignorent `colorwc`.

- **Reste** : segments, scènes, effets (hors du manuel de l'API locale) ; le nom donné dans
  l'appli (l'API locale ne le transmet pas : la lumière arrive nommée « Govee <modèle> (<fin de
  l'identifiant>) », à renommer dans Moli).
- Sources et licences : voir `[origin]` dans `integration.toml`. Aucun code ni aucune donnée
  repris.

Non affilié à Govee : Govee est une marque de son propriétaire.
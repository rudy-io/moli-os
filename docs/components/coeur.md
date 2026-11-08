# Le cœur : `moli-core` et `moli-runtime`

Tout passe par le cœur. Il tient l'état de la maison en mémoire, vérifie chaque ordre, le
route vers le bon pilote, l'inscrit au journal et diffuse chaque changement. Il ne contient
aucune logique d'appareil (c'est le rôle des [pilotes](pilotes.md)) et aucun LLM.

## `moli-core` : le vocabulaire (aucune entrée/sortie)

| Type | Rôle |
|---|---|
| `InstanceId` | une instance de pilote (`hue`, `tuya`, `clim-salon`…) |
| `DeviceId` | `<instance>:<identifiant natif stable>` (`z2m:0x00158d…`, `hue:<uuid>`, `tuya:<devid>`). **Jamais dérivé d'un nom.** |
| `PointId` | `<device>/<clé>` (`hue:<uuid>/on`) : une valeur d'un appareil |
| `Device` | nom natif, fabricant, modèle, pièce native, membres (groupes Hue), `points` |
| `PointSpec` | clé, libellé, `kind` (`binary` / `numeric{min,max,step}` / `enum{values}` / `text`), accès lecture/écriture, unité, sémantique (`on_off`, `brightness`, `power`…) |
| `Value` | `Null`, `Bool`, `Int`, `Float`, `Text` ; `validate(kind, value)` refuse ce qui ne colle pas |
| `Label` | nom et pièce choisis par l'humain : **une étiquette ne change jamais une identité** |
| `Event` | `device_upserted`, `device_removed`, `state`, `availability`, `driver_status`, `label_changed`, `journal`, `approval_requested`, `approval_resolved`, `notice` |
| `Origin` | `ui`, `api`, `mcp`, `cli`, `system`, `assistant`, `automation` : par où arrive une action |

## `moli-runtime` : le hub

`Hub` est un handle clonable (`Arc`) partagé par tout le monde.

- **Modèle** : appareils, états (`Sample { value, ts }`), étiquettes, sources d'images des
  caméras. Lu sous `RwLock` ; ⚠️ ne jamais reprendre le verrou qu'on tient déjà en lecture
  (blocage total déjà vécu).
- **Bus** : `broadcast` d'`Event`. Un état n'est émis **que s'il change**. Un abonné trop lent
  reçoit un nouvel instantané au lieu de bloquer les autres (SSE).
  La restauration d'un appareil depuis le sidecar émet `device_restored` avec sa définition
  en lecture seule,
  ses valeurs conservées et ses étiquettes, **sans** rejouer de mesures `state` aux
  automatismes, à l'énergie ou à l'historique. Le premier relevé frais de chaque point
  restauré émet ensuite un `state`, même si sa valeur est identique ; les suivants sont
  de nouveau dédupliqués.
- **Observation actuelle** : `hub.state` garde les valeurs utiles à l'affichage.
  `hub.current_state`, destiné aux consommateurs qui peuvent agir, exclut sous un même
  verrou les appareils hors ligne, les définitions restaurées non confirmées et les
  points qui attendent leur premier relevé frais. Une nouvelle définition ne rajeunit
  donc pas les valeurs ; un pilote qui n'annonce pas sa disponibilité reste compatible.
  Une condition solaire `jour` ou `nuit` reste fausse si la position est absente et
  qu'aucun relevé actuel `daylight` n'est disponible : l'absence d'information ne prouve
  pas qu'il fait nuit, et une ancienne valeur météo ne doit pas déclencher d'action.
- **Commandes** : `hub.command(point, value, origin, actor)` → validation contre le
  `PointSpec` (lecture seule, valeur invalide : refus) → **garde** → envoi au pilote
  (délai 5 s) → journal. La confirmation arrive ensuite comme un événement `state`.
- **Superviseur** : chaque pilote tourne dans sa tâche ; à chaque échec, sa file de commandes
  est vidée et ses appareils passent hors ligne ; un `panic` est capturé, relance avec
  attente croissante, statut exposé (`starting`, `running`, `waiting{reason}`, `backoff`,
  `stopped`). Une panne de pilote n'emporte jamais le cœur.
- **Journal** : chaque écriture (commande, étiquette, décision d'approbation, appareil
  retrouvé à une autre adresse) avec origine,
  acteur déclaré (borné à 64 caractères) et résultat ; persistant (`data/journal.jsonl`,
  réécrit avec ses 2 000 dernières lignes au-delà de 8 Mio) ; les échecs d'écriture sur
  disque sont comptés et visibles dans `/api/health` (`journal_failures`).
- **Persistance** : `data/labels.toml` (étiquettes), `data/state.json` (dernières valeurs,
  compatibles avec les anciens binaires), écritures périodiques ;
  `data/state.devices.json` (inventaire versionné et relevés de référence cohérents) et
  `data/state.observations.json` (derniers relevés, sans recopier les définitions).
  Les chemins sont dérivés du cache d'état configuré en remplaçant son extension par
  `.devices.json` et `.observations.json`. Un indicateur distinct suit les changements
  de définitions : seuls un ajout, un changement ou une suppression de fiche réécrivent
  l'inventaire. Republier une définition identique ne le réécrit pas. Les mises à jour
  d'état écrivent uniquement les observations et le miroir legacy.
  Les écritures sont sérialisées et chaque fichier est remplacé atomiquement.
  Une écriture commencée conserve son verrou jusqu’à la fin, même si son appelant annule
  son attente, pour éviter la concurrence entre deux remplacements temporaires. Un nouvel
  inventaire reçoit une génération aléatoire de 128 bits ; les observations ne remplacent
  ses relevés de référence que si leur génération correspond. Les anciennes observations
  et un `state.json` resté en retard ne peuvent donc pas ressusciter un appareil supprimé,
  ni associer les mesures d'une ancienne fiche à une nouvelle définition. Un fichier
  d'observations manquant ou corrompu laisse les relevés de référence disponibles.
  L'inventaire et les observations incluent les valeurs héritées encore non réclamées.
  Le format d'inventaire v1 reste lisible et migre au prochain flush ; `state.json` seul
  continue de fonctionner. Sauvegarder les trois fichiers conserve aussi les derniers
  relevés ; le snapshot restauré reste affichable avec ses horodatages d'origine.
  Une fiche est restaurée seulement quand une instance active de mêmes `kind` et
  `integration` s'enregistre. Elle reste **hors ligne, tous ses points en lecture seule**
  jusqu'à une nouvelle publication de sa définition par le pilote ; `ready()` et un
  simple changement de disponibilité ne suffisent pas. Cette publication fraîche enlève
  l'indisponibilité imposée par le cache et annonce l'appareil en ligne, pour les pilotes
  qui ne publient pas eux-mêmes de disponibilité. Un `false` explicitement annoncé par le
  pilote reste prioritaire. Les valeurs conservées attendent toujours leur propre relevé
  frais avant de devenir des observations actuelles.
  Une instance désactivée garde son cache dormant sans exposer ses appareils au redémarrage.
  `remove_device` purge définition et valeurs du prochain flush, sans supprimer les
  étiquettes ni l'historique. Le pont Home Assistant retire ainsi les entités supprimées
  de sa configuration explicite ; une entité encore configurée mais indisponible garde
  sa fiche hors ligne. Sans cette distinction, le cache conserverait indéfiniment une
  entité volontairement retirée.
  Les fichiers de cache sont limités à 8 Mio, 4 096 appareils et 1 024 points par appareil ;
  métadonnées et valeurs d'énumération à 8 Kio, textes de relevés à 64 Kio. Une définition
  invalide (identité, taille, nombre de choix ou borne numérique non finie) est écartée
  individuellement avec avertissement. Les relevés invalides et les entrées dépassant le
  budget sont omis sans bloquer les appareils valides ni relancer indéfiniment le flush.
  Une version inconnue ou un inventaire illisible est ignoré avec avertissement ; une
  définition fraîche reste nécessaire pour commander. Les erreurs d'entrée/sortie
  réelles conservent les indicateurs de modification pour réessayer. Le fichier faisant
  autorité est remplacé avant le miroir legacy : si ce dernier échoue, la suppression
  déjà enregistrée reste effective ; si l'écriture faisant autorité échoue, le précédent
  snapshot cohérent reste en place.
  L'arrêt du superviseur vide la file de commandes et marque ses appareils hors ligne ;
  `unregister_driver`, appelé après avoir attendu sa tâche, détache le pilote et retire
  les droits d'écriture des fiches conservées, sans effacer leurs noms ni leurs valeurs.
  `data/addresses.json` (où chaque pilote a joint son appareil, sa MAC, son identité : de quoi
  le retrouver si la box lui donne une autre adresse ; seulement appris : hors des sauvegardes,
  perdu il se réapprend).
  `DriverCtx::known_address`, `remember_address`, `moved` (journal), `known_devices`
  (appareils publiés avant un redémarrage).
- **Notices** : `hub.notice(...)` diffuse un message pour la maison (automatismes) : toast
  dans le dashboard.
- **Statistiques** : appareils, valeurs, pilotes, `rss_bytes` (lu dans `/proc/self/status`),
  uptime.

## La garde (`moli-runtime/src/guard.rs`)

Règle posée dès le premier jour : **là où dort un enfant, aucun agent ne touche à rien sans
un humain.**

- `protected_rooms` (dans `[guard]` de `moli.toml`) : par exemple « Chambre des enfants »,
  plus les autres noms que Hue ou HA donnent à la même pièce. Comparaison sans accents ni
  casse ; un appareil est protégé si **l'une** de ses pièces (étiquette ou pièce native,
  ou celles des membres d'un groupe) l'est.
- `protected_devices` : la même règle pour un appareil précis, quelle que soit sa pièce (la
  prise qui alimente la box), sur tous ses points. Un automatisme validé par un humain reste
  obéi ; son éditeur signale l'appareil.
- `quiet_hours` : une plage horaire (par exemple 21 h → 7 h 30, fuseau de la config), pour
  toutes les pièces ou celles listées dans `rooms`.
- **Commandes de machine** (sémantique `control` : mettre en pause ou annuler une impression
  3D) : retenues pour un humain **quelle que soit la pièce** dès que l'origine n'est pas de
  confiance (`hub.rs`, `is_control`). Une impression ratée ou un plateau abîmé ne se rattrape
  pas : un agent propose, un humain décide.
- Origines de confiance (jamais retenues) : `ui` (le dashboard avec une session PIN),
  `system`, `automation` (un automatisme validé par un humain = son intention).
- `household` (le dashboard du foyer **sans** code, mode `dashboard = "trusted"` : réseau
  local ou personne reconnue par Cloudflare Access) : obéi partout, heures calmes comprises,
  **sauf** dans une pièce protégée, sur une machine (`control`) et pour sortir un appareil
  d'une pièce protégée, où il est retenu comme un agent. N'importe quel programme du réseau
  peut se dire « dashboard » : seul le code le prouve. Le dashboard Maison ouvre alors la
  feuille du code, puis libère l'ordre.
- Les autres (`api`, `mcp`, `cli`, `assistant`) dans une pièce protégée ou en heures calmes :
  l'ordre **n'est pas exécuté**, il devient une **demande d'approbation** (202 + id), qui
  expire en 10 min. Seul un humain la libère (`/api/approvals/{id}`, en nommant le point).
  Une demande n'est remplacée que par une plus récente du **même appelant** (même point,
  même origine) ; 32 par appelant, 50 en tout, et c'est l'appelant qui en tient le plus qui
  cède sa plus ancienne (`api.md`, 1.1).
- Une pièce protégée qui ne correspond plus à aucun appareil (renommée) est **signalée**
  (dashboard, santé, MCP, log) : une protection qui ne s'applique plus ne doit pas passer
  inaperçue.
- Un agent ne peut pas sortir un appareil d'une pièce protégée en changeant son étiquette,
  ni renommer un appareil déjà nommé (il peut nommer un appareil sans nom). Avant de valider
  un automatisme, l'éditeur montre pour chaque appareil renommé son nom d'origine et son
  identifiant.

## Sessions humaines (`moli-api/src/session.rs`)

La famille commande sans code depuis le dashboard (décision D0), hors des pièces protégées
et des machines (resserré le 12 oct.). « Humain » au sens fort (valider un automatisme,
libérer ce qu'un agent a demandé, commander une chambre, voir sa caméra) = un
cookie de session obtenu avec le code **`MOLI_UI_PIN`** (ou `MOLI_UI_PIN_FILE` : de
préférence un fichier tmpfs livré par un gestionnaire de secrets). Lié à l'appelant
(adresse sur le réseau local, e-mail via Cloudflare Access), valable 30 jours, tous invalidés si le PIN change. Échecs comptés
par appelant : verrouillage 15 min qui double jusqu'à 24 h. Sans PIN configuré, personne
n'est humain (sûr par défaut), sauf l'installation d'une maison neuve (code d'installation dans
les journaux, voir [installation.md](installation.md)). Détail : [api.md § 1.2 bis et 1.3](api.md).

## Le coffre des secrets (`moli-runtime/src/secrets.rs`)

- `data/secrets.enc`, chiffré (ChaCha20-Poly1305 via `ring`), espace de noms par instance
  (`hue/app_key`, `tuya/<devid>`, `assistant/api_key`, `automations/telegram_webhook`…).
- Clé maître (64 chiffres hexadécimaux) : lue dans le fichier nommé par
  `MOLI_MASTER_KEY_FILE` (de préférence un fichier tmpfs, monté en lecture seule dans le
  conteneur), sinon dans `MOLI_MASTER_KEY`. Elle vit dans un gestionnaire de secrets, jamais
  sur le disque de Moli (voir [exploitation.md](exploitation.md)). ⚠️ **Ne jamais la
  régénérer** : le coffre deviendrait illisible.
- Écriture : `… | docker exec -i moli-os /moli-os secrets set <instance> <nom>` : **toujours
  par un tube**, jamais au clavier (refusé), jamais affiché. Le coffre relit le fichier avant
  d'écrire (Moli peut tourner pendant l'écriture) ; la valeur est prise en compte au
  redémarrage.
- Les secrets repris d'une installation Home Assistant passent de la même façon : par tube,
  sans jamais apparaître.

## Images des caméras (`moli-runtime/src/media.rs`)

Un pilote fournit une `SnapshotSource` par caméra (`ctx.provide_snapshots`). `hub.camera_image`
l'appelle (20 s maxi) sans tenir de verrou. Une caméra d'une pièce protégée n'est montrée
**qu'à une session humaine** ; l'outil MCP la refuse toujours ; l'assistant ne voit jamais
d'images.

## Tests

`crates/moli-runtime/tests/hub.rs` : pilote factice (prise), aller-retour de commande, panic
capturé, étiquettes persistantes, cache d'état, garde (pièces, heures calmes, approbations).

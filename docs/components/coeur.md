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
- **Commandes** : `hub.command(point, value, origin, actor)` → validation contre le
  `PointSpec` (lecture seule, valeur invalide : refus) → **garde** → envoi au pilote
  (délai 5 s) → journal. La confirmation arrive ensuite comme un événement `state`.
- **Superviseur** : chaque pilote tourne dans sa tâche ; à chaque échec, sa file de commandes
  est vidée et ses appareils passent hors ligne ; un `panic` est capturé, relance avec
  attente croissante, statut exposé (`starting`, `running`, `waiting{reason}`, `backoff`,
  `stopped`). Une panne de pilote n'emporte jamais le cœur.
- **Journal** : chaque écriture (commande, étiquette, décision d'approbation) avec origine,
  acteur déclaré (borné à 64 caractères) et résultat ; persistant (`data/journal.jsonl`,
  réécrit avec ses 2 000 dernières lignes au-delà de 8 Mio) ; les échecs d'écriture sur
  disque sont comptés et visibles dans `/api/health` (`journal_failures`).
- **Persistance** : `data/labels.toml` (étiquettes), `data/state.json` (dernières valeurs,
  rejouées au démarrage avec leur horodatage d'origine), écritures périodiques.
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
- `quiet_hours` : une plage horaire (par exemple 21 h → 7 h 30, fuseau de la config), pour
  toutes les pièces ou celles listées dans `rooms`.
- **Commandes de machine** (sémantique `control` : mettre en pause ou annuler une impression
  3D) : retenues pour un humain **quelle que soit la pièce** dès que l'origine n'est pas de
  confiance (`hub.rs`, `is_control`). Une impression ratée ou un plateau abîmé ne se rattrape
  pas : un agent propose, un humain décide.
- Origines de confiance (jamais retenues) : `ui` (le dashboard du foyer : réseau local
  ou personne reconnue par Cloudflare Access, sans code ; ou session PIN en mode
  `dashboard = "pin"`), `system`, `automation` (un automatisme validé par un humain = son
  intention).
- Les autres (`api`, `mcp`, `cli`, `assistant`) dans une pièce protégée ou en heures calmes :
  l'ordre **n'est pas exécuté**, il devient une **demande d'approbation** (202 + id), qui
  expire en 10 min. Seul un humain la libère (`/api/approvals/{id}`, en nommant le point).
- Une pièce protégée qui ne correspond plus à aucun appareil (renommée) est **signalée**
  (dashboard, santé, MCP, log) : une protection qui ne s'applique plus ne doit pas passer
  inaperçue.
- Un agent ne peut pas sortir un appareil d'une pièce protégée en changeant son étiquette,
  ni renommer un appareil déjà nommé (il peut nommer un appareil sans nom). Avant de valider
  un automatisme, l'éditeur montre pour chaque appareil renommé son nom d'origine et son
  identifiant.

## Sessions humaines (`moli-api/src/session.rs`)

La famille commande sans code depuis le dashboard (décision D0). « Humain » au sens fort
(valider un automatisme, libérer ce qu'un agent a demandé, voir une caméra de chambre) = un
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

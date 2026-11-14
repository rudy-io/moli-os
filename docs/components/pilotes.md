# Les pilotes
- **Aspirateurs robots** (catégorie `sd`, `src/vacuum.rs`, Rowenta et Tefal X-plorer Serie 75) : seule une table de points existe, sous les mêmes clés pour tous : `clean`, `pause`, `dock`, `mode`, `locate`, `state`, `battery`, `fan_speed`, `mop`, `volume` (selon le robot), `clean_time`, `clean_area`, `edge_brush`, `main_brush`, `fault`, `detail`. Effacer la carte, conduire à la main et nettoyer une zone n'existent pas, même si le cloud les dit écrivables. Ce qui fait bouger le robot est un `Control`. Le produit `X-Plorer Serie 75` reçoit toujours sa table, car la description du cloud nomme des points qu'il ne renvoie pas. Un robot a sa carte sur l'accueil (`web/src/maison/ui/VacuumCard.svelte`) : il passe par toutes les pièces, il n'appartient à aucune.

Un pilote relie un système extérieur (pont Zigbee, box, caméra, télé…) au hub. Chaque
pilote est une crate `crates/moli-<nom>` et un paquet du catalogue `integrations/<kind>/`
(manifeste, onboarding, fixtures : voir [profils.md](profils.md#le-catalogue-dintégrations)).
Les appareils HTTP simples n'ont pas besoin de pilote : ils passent par un
[profil déclaratif](profils.md).

## Comment un pilote marche

### Le contrat (`crates/moli-runtime/src/driver.rs`)

```rust
pub trait Driver: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>>;
}
```

`run` est censé tourner pour toujours. S'il rend la main (`Ok` ou `Err`) ou panique, le
superviseur (`crates/moli-runtime/src/supervisor.rs`) le relance :

- attente de 1 s, doublée à chaque échec, plafonnée à 60 s ;
- remise à 1 s si l'exécution précédente a duré au moins 60 s ;
- statut public : `starting`, `running`, `waiting{reason}`, `backoff{error, retry_in_ms}`, `stopped` ;
- à chaque échec, la file de commandes est **vidée** (chaque ordre en attente reçoit le refus
  `driver restarting`) et les appareils de l'instance passent **hors ligne** : rien ne part
  plus tard par surprise, et un automatisme ne raisonne pas sur des valeurs périmées.

### `DriverCtx` : ce qu'un pilote peut faire

Le contexte est limité à **sa** instance : un appareil d'une autre instance est refusé.

| Méthode | Rôle |
|---|---|
| `instance()`, `device_id(native)` | identité de l'instance, fabrique `<instance>:<native>` |
| `upsert_device(Device)` / `remove_device(id)` / `devices()` | publier, retirer, lister ses appareils |
| `set_state(device, key, value)` | enregistrer une valeur ; diffusée seulement si elle change ; clé inconnue ignorée |
| `pulse_state(...)` | idem mais toujours diffusée (appui de bouton : deux appuis = deux événements) |
| `set_availability(device, online)` | en ligne / hors ligne |
| `next_command()` | prochaine commande (annulable) ; `None` à l'arrêt |
| `ready()` / `wait_for(reason)` | « opérationnel » / « bloqué sur une action humaine » (affiché partout) |
| `warn(message)` / `clear_warning()` | avertissement non bloquant du pilote, indépendant du statut et des droits ; effacé à l’arrêt ou au redémarrage |
| `secret(name)`, `store_secret`, `forget_secret`, `can_store_secrets()`, `secrets_problem()` | coffre chiffré, rangé par instance |
| `provide_snapshots(device, source)` | l'appareil sait produire une image à la demande (caméras) |
| `cancelled()` | se résout à l'arrêt du système |

**Commandes.** Le hub valide avant le pilote : point existant, `access.write`, valeur
conforme au `kind`. Puis il pose un `CommandRequest { point, device, key, value }` dans une
file de 64 par instance (pleine : `Busy`). Le pilote répond `reply(Ok|Err)` dès que l'ordre
est remis à l'appareil. Le hub attend 5 s au plus ; chaque ordre porte cette **échéance**
(`CommandRequest::deadline`) : `next_command()` refuse lui-même (`expired`) un ordre que
personne n'attend plus, il n'atteint jamais le pilote (une lampe de chambre allumée des
heures après « échec », c'est pire qu'un refus). La confirmation arrive ensuite comme une
mise à jour d'état normale (Sonos et Tapo publient la valeur dès le succès).

**Images.** Une caméra déclare une `SnapshotSource` (`crates/moli-runtime/src/media.rs`).
Le hub l'appelle quand une surface demande une image : rien n'est récupéré pour rien.

### Identité et étiquettes

- `DeviceId` = `<instance>:<identifiant natif stable>` (adresse IEEE, UUID, numéro de série,
  MAC…). **Jamais dérivé d'un nom.** Un `/` dans l'identifiant natif devient `_`
  (`/` sépare l'appareil de la clé dans un `PointId` = `<device>/<clé>`).
- L'id d'instance (`[[driver]] id`) préfixe tous ses appareils **et** range ses secrets
  (`<instance>/<nom>`) : ne jamais le renommer.
- `native_name` et `native_room` sont des suggestions du système d'origine. Le nom et la
  pièce choisis par l'humain sont des **étiquettes** (`data/labels.toml`), qui gagnent
  toujours et ne changent jamais une identité.

### Déclarer un pilote (`crates/moli-os/src/config.rs`, `main.rs`)

```toml
[[driver]]
id = "enceinte"         # non vide, sans ':' ni '/', unique ; réservés : core, assistant, automations
kind = "sonos"          # z2m, zwave, hue, dirigera, tuya, reolink, sonos, philips, frigate, bambu, tapo, lifx, shelly, igd, helpers, profile
# enabled = false       # désactive l'instance sans la supprimer

[driver.options]        # s'applique au [[driver]] qui précède ; clés inconnues refusées
host = "192.168.1.x"
```

`moli-os check-config` construit chaque pilote (donc valide ses options) sans le lancer.
Les `kind` connus sont la liste `KINDS` à côté de `build_driver` (`crates/moli-os/src/main.rs`) :
un `kind` absent de la liste est inconnu, et un test vérifie que chacun est bien construit.
Ajouter un pilote = une crate, une branche du `match`, une entrée de `KINDS` et un paquet
`integrations/<kind>/` (le test `the_catalogue_describes_every_driver_and_built_in_profile`
l'exige).

### Secrets

- Coffre chiffré `data/secrets.enc` (ChaCha20-Poly1305). La clé maître vient uniquement de
  l'environnement : `MOLI_MASTER_KEY_FILE` (de préférence) ou `MOLI_MASTER_KEY`
  (fournie par un gestionnaire de secrets). Détails : [coeur.md](coeur.md).
- Un humain y range une valeur par un tube, jamais au clavier :
  `printf %s "$V" | moli-os secrets set <instance> <nom>` ; `moli-os secrets forget <instance> <nom>`.
  Redémarrer moli-os ensuite (les pilotes lisent leurs secrets au démarrage).
- Seuls `z2m` et `zwave` (`password_env`) et, en option, `hue` (`app_key_env`) lisent un
  secret dans une variable d'environnement ; la config ne contient que le **nom** de la
  variable.
- Aucune surface ne sert jamais un secret.

## Briques réseau partagées : `moli-net`

`crates/moli-net/src/lib.rs`, `upnp.rs`, `legacy.rs`.

| Brique | Usage |
|---|---|
| `PinnedHttps` | HTTPS vers un appareil du LAN au certificat auto-signé : empreinte SHA-256 **épinglée au premier contact** (TOFU), puis exigée. Construit sans empreinte, l'objet adopte celle de la première poignée de main (sous un seul verrou) et refuse ensuite toute autre (`PIN_MISMATCH`) ; une empreinte fournie est normalisée (minuscules, sans `:`) ; `pinned_fingerprint()` la donne. Délai 4 s par étape, keepalive TCP (60 s / 15 s), une connexion réutilisée avec un seul nouvel essai pour les requêtes rejouables. `send_once` (connexion, appui de touche : jamais rejoué), `send_slow` (caméra sur batterie), `stream` (SSE), `handshake` (épingle avant tout envoi de secret : le pilote stocke l'empreinte, puis construit l'objet qui porte le secret). En-tête `Host` **avec le port** sauf 443. **Corps bornés** : 4 Mio (`MAX_BODY`), 16 Mio pour `send_slow` (`MAX_SLOW_BODY`, images) ; au-delà, erreur `body too large`, jamais rejouée. `stream` n'a pas de fin par nature : l'appelant borne ce qu'il accumule. |
| `PinnedTls` | même épinglage pour un autre transport TLS (MQTT de Bambu) |
| `legacy.rs` | webpki ne sait pas lire un certificat **X.509 v1** (station Reolink). Le certificat est accepté sur son empreinte ; la signature de la poignée de main est vérifiée à la main avec la clé RSA, P-256 ou P-384 extraite du DER. |
| `public_tls()` | TLS vérifié contre les autorités publiques (liste Mozilla embarquée : l'image n'a pas de magasin système) |
| `plain()` / `plain_with_headers()` | HTTP en clair, une connexion par requête (UPnP, API JSON locales), corps borné à 4 Mio |
| `web(host, port, tls, req, limit, max_body)` + `WebUrl::parse` | une requête vers une API web, délai global et taille de réponse bornés ; `WebUrl` n'accepte que `http(s)://` et refuse `user@host`. Utilisé par l'assistant et les webhooks des automatismes. |
| `raop` | AirPlay 1 vers un récepteur qui prend le PCM en clair : `plain` (le critère, lu dans le TXT `_raop._tcp`), `announced` (ce TXT, demandé au seul récepteur, celui de l'instance dont le SRV donne le port configuré), `Session` (RTSP, réponses appariées par `CSeq`, puis le son en RTP avec SYNC, avance bornée, renvois et horloge, en ne répondant qu'au récepteur), et trois gestes d'une session chacun, `read_volume`, `set_volume`, `say` (le volume relu, le niveau de l'annonce, le son, le volume remis). Un refus d'une étape de la session est une erreur typée (`Refused`), distincte d'un récepteur absent ; un `TEARDOWN` raté n'est qu'un avertissement. Voir [`airplay`](#airplay--annonces-sur-un-récepteur-airplay-1-cratesmoli-airplay). |
| `upnp` | `discover` (SSDP `M-SEARCH` vers 239.255.255.250:1900), `fetch` (description), `soap` (action), `control_url`, lecture XML minimale par nom de balise |

En cas d'empreinte différente, l'erreur contient `PIN_MISMATCH` : les pilotes passent alors
en `waiting` avec la commande `moli-os secrets forget <instance> cert` (ou `cert_sha256`).

## `airzone` : Airzone Local API (`crates/moli-airzone`)

Le pilote natif couvre les 12 opérations de la Local API 1.78 : zones, systèmes,
eau chaude sanitaire, capteurs de qualité d'air et informations du Webserver.
Le [registre de couverture](../../integrations/airzone/coverage.json) recense les
118 schémas de paramètres, dont 31 explicitement écrivables, et les 17 mesures
d'énergie. Chaque point dépend des capacités effectivement annoncées par le
contrôleur ; cette couverture ne garantit pas la compatibilité de tous les firmwares.

Une instance par contrôleur, avec `host` explicite. Le défaut TOML historique
`read_only = false` est conservé pour ne pas désactiver les automatismes existants.
Le défaut `write = "legacy"` conserve seulement `on`, `target_temperature`, `mode`
et `fan_speed` (paramètre Airzone `speed`) sur les zones et leurs contrôles partagés
historiques. `write = "extended"` active explicitement les nouvelles commandes :
modes système, eau chaude, renommage, réglages installateur, grilles, ventilation,
qualité de l’air et libellé d’intégration. Un essai d’observation configure
explicitement `read_only = true`, prioritaire sur les deux modes d’écriture.
Les passerelles ajoutées depuis Moli démarrent en lecture seule avec le périmètre
`legacy` ; l'autorisation par code Moli ne modifie pas ce périmètre. Les modifications
de connexion et réactivations rétablissent la lecture seule.
Ces restrictions sont appliquées aux capacités publiées, à l’exécuteur et au
transport. Les commandes restent soumises aux gardes, aux membres affectés et à
une nouvelle lecture des capacités. Aucun PUT n'est envoyé au
démarrage, et une commande n'est jamais rejouée. Les nouvelles zones déjà associées
au contrôleur sont découvertes lors des lectures ; l'appairage physique n'appartient
pas à cette API.

HTTP utilise le port 3000 par défaut. HTTPS demande un port explicite et
`tls_fingerprint`, une empreinte SHA-256 vérifiée ; aucun repli en clair. Si l'eau
chaude ne rapporte pas son unité, `dhw_units = 0` (°C) ou `1` (°F) permet de la
déclarer explicitement. Sans unité connue, sa consigne reste en lecture seule.

`moli-os airzone-inspect --instance airzone <opération>` donne un diagnostic
strictement en lecture seule, avec identifiants et textes libres expurgés. Les
opérations de diagnostic comprennent les variantes GET, la démonstration synthétique
et la disponibilité de la page thermostat ; le HTML distant n'est pas affiché.
Les programmations Cloud, l'installation du réseau, l'appairage et les mises à jour
du firmware ne sont pas des opérations de la Local API.

Configuration, garde des groupes, limites des réponses, commandes et restrictions :
[guide Airzone](../../integrations/airzone/README.md).

## Scènes et rythme de la journée (`crates/moli-scenes`)

Pas un pilote à déclarer : dès que `moli.toml` a un `[[scene]]` ou un `[rhythm]`, Moli lance
l'instance `scenes` et publie **un** appareil, `scenes:maison`.

```toml
[[scene]]
id = "soiree"          # stable : c'est la valeur de scenes:maison/scene
name = "Soirée"
cycle = true           # fait partie de la ronde du bouton (toutes, si aucune ne le dit)
lights = [
  { id = "lumieres:suspensions", off = true },
  { id = "z2m:0x…", color = "#ffb060", brightness = 22, effect = "flame" },
  { id = "hue:…", white = 2400, brightness = 30 },
]

[rhythm]
lights = ["lumieres:suspensions", "hue:…"]
# curve = [{ at = "07:00", white = 2700, brightness = 60 }, …]   # facultatif
```

- **Points** : `scene` (énumération : `none` et les identifiants ; l'écrire met la scène),
  `order` (`next` / `previous`, en écriture seule : la ronde depuis un bouton mural ou un
  automatisme), `rhythm` (interrupteur, seulement avec un `[rhythm]`). Tout passe par le hub :
  page, automatisme (`set`), agent, assistant.
- **Une scène** dit, lumière par lumière : éteinte (`off`), ou une couleur (`color`, « #rrggbb »),
  un blanc (`white`, kelvins : pris par les lampes sans couleur, ou quand aucune couleur n'est
  donnée), une luminosité (`brightness`, %), une animation (`effect`, jouée là où la lampe l'a ;
  pour un luminaire, par ses ampoules). Ce qui n'est pas dit reste tel quel ; une lumière que la
  scène ne nomme pas n'est pas touchée. Un simple interrupteur est allumé ou éteint. Les ordres
  partent l'un après l'autre, au nom de la maison (comme ceux d'un luminaire) ; choisir une autre
  scène interrompt celle qui s'installe.
- **Le reste de la maison** : `orders = [{ point = "tele:…/ambilight_mode", value = "relax" }]`,
  des ordres simples (texte, nombre, vrai/faux) envoyés après les lumières : l'Ambilight d'une
  télé, une prise. Un ordre refusé (télé éteinte) est noté au journal, la scène reste en place.
  Une scène ne peut pas en commander une autre.
- **Fin d'une scène** : `scene` revient à `none` quand toutes ses lumières allumées sont éteintes
  (jamais dans les 30 premières secondes), ou quand on écrit `none` (les lumières ne bougent pas).
- **Rythme de la journée** : une lumière de `lights` qu'on allume, sans scène en place, prend le
  blanc et le niveau de l'heure (heure de la maison), puis les suit toutes les dix minutes. La
  courbe par défaut : 05:00 2200 K 30 %, 07:00 2700 K 60 %, 09:00 4300 K 100 %, 16:00 4000 K 100 %,
  19:00 3000 K 80 %, 21:30 2400 K 50 %, 23:30 2200 K 30 %, en glissant de l'une à l'autre. Une
  lumière qu'une personne a réglée (niveau à plus de 4 points de celui donné, ou une couleur)
  n'est plus suivie jusqu'à sa prochaine extinction. Le rythme n'allume jamais rien.
- Les identifiants de scène `none`, `next` et `previous` sont réservés. `moli-os check-config`
  refuse une scène mal écrite (lumière nommée deux fois, `off` avec un réglage, couleur invalide…).

## `z2m` : Zigbee via Zigbee2MQTT (`crates/moli-z2m`)

- **Couvre** : tous les appareils Zigbee que Zigbee2MQTT sait décrire (~4 000), sans code par marque.
- **Protocole** : MQTT local (1883 par défaut), abonnement à `{base}/#`. Commandes publiées sur `{base}/{friendly_name}/set`.
- **Configuration** :

```toml
[[driver]]
id = "z2m"
kind = "z2m"
[driver.options]
host = "127.0.0.1"          # défaut
port = 1883                 # défaut
base_topic = "zigbee2mqtt"  # défaut
# username = "moli-os"
# password_env = "MOLI_Z2M_PASSWORD"   # NOM de la variable, jamais le mot de passe
# client_id = "…"                      # défaut : moli-os-<instance>
# impulse_points = ["z2m:0x…/state_l1"]  # relais câblés en déclencheur (portail, garage)
```

- **Secrets** : mot de passe MQTT éventuel dans la variable d'environnement nommée par `password_env`.
- **Points** : construits depuis les `exposes` du message retenu `bridge/devices` (`src/catalog.rs`). Clé = propriété Z2M ; les composites sont aplatis (`color.x`). Écriture si le bit `set` de Z2M est présent, **sauf** membres d'un composite (Z2M refuse un membre seul). Une exposition de catégorie `config` a la sémantique `config`. Les énumérations numériques sont exposées en texte et reconverties en nombre à l'envoi. **Lumières** : la luminosité Zigbee (0–254, `value_max`) est exposée en % (0–100, unité `%`), comme les autres pilotes ; conversion arrondie à la lecture et à l'envoi, une ampoule allumée n'est jamais affichée à 0 %. La température de blanc reste en mired ; une ampoule en mode couleur (`color_mode` autre que `color_temp`) la publie vide (`null`), comme Hue, pour que les pages montrent la couleur. **Couleur** : un composite `color {x, y}` réglable donne en plus un point `color` réglable (texte « #rrggbb », sens `color`, comme Hue), envoyé en CIE xy (`moli_core::color`) et relu depuis `color.x` / `color.y` ; les membres restent en lecture seule pour l'historique. Le tableau de bord et les luminaires proposent alors couleurs et blancs. **Flamme** (`src/flame.rs`) : la plupart des lampes Zigbee n'ont pas d'animation à elles (l'`effect` Zigbee est un clignotement pour repérer la lampe). Une lampe qui prend une couleur et se règle en luminosité gagne donc deux valeurs dans son point `effect` (créé si elle n'en a pas, et rendu lisible) : `flame` et `no_effect`. `flame` est jouée par le pilote : toutes les 1,3 à 2,3 s, un fondu lent (`transition` de 1 à 2 s) vers un ambre un peu différent et un niveau entre 0,82 et 1,22 fois la luminosité choisie, avec un léger creux une fois sur dix. Volontairement doux, et un seul ordre à la fois par lampe (envoi sans file : un fondu qui ne passe pas est sauté) pour laisser le réseau Zigbee libre. Pendant la flamme, les retours de niveau et de couleur de la lampe ne sont pas publiés : la luminosité enregistrée reste celle que la personne a choisie, et l'historique ne se remplit pas. Le curseur de luminosité règle le niveau autour duquel la flamme bouge. Une couleur, un blanc, un autre effet ou l'extinction (par Moli ou au bouton de la lampe) arrêtent la flamme ; `no_effect` l'arrête et remet la luminosité choisie (la lampe reste ambre). Après un redémarrage, une lampe enregistrée en `flame` et encore allumée la reprend. La ronde des couleurs de la lampe elle-même (`colorloop`, Zigbee) tourne jusqu'à `stop_colorloop` et la lampe ne dit jamais qu'elle la joue : sur ces lampes, le pilote la tient en mémoire dans `effect` et traduit `no_effect` en `stop_colorloop`, pour qu'une ambiance ou une scène l'arrête avant de poser autre chose. **Migration** : au déploiement, l'historique `brightness` des ampoules Z2M passe de 0–254 à 0–100 (les anciennes valeurs restent sur 254 dans les graphiques) ; une automatisation écrite avec un seuil brut (`> 200`) est à reprendre. **Volets** (`cover`) : `position` a le sens `position` (0–100 %, 100 = ouvert), posé par le pilote pour un expose `cover` et jamais déduit du nom ; `state` (`OPEN` / `CLOSE` / `STOP`) est un ordre, de sens `other`, jamais un interrupteur ; `moving` (`UP` / `STOP` / `DOWN`) est lu tel quel. **Migration** : au déploiement, `state` d'un volet cesse d'être un interrupteur (`on_off` → `other`) et `position` passe de `other` à `position` ; l'historique et les automatisations qui lisaient `state` comme un interrupteur sont à reprendre. **Contacts** : `contact` est repris tel que Z2M le publie (`true` = fermée), alors que Z2M le déclare avec `value_on: false`. **Relais à impulsion** (`impulse_points`) : un relais qui pilote un contact sec (portail, porte de garage, ZG-2002-RF en mode `inching`) ressemble à un interrupteur dont l'état retombe à éteint ; Z2M ne dit pas à quoi il est câblé, et le retour à `OFF` est aussi celui d'une minuterie, donc rien n'est déduit : le propriétaire liste les identifiants de points (`z2m:<IEEE>/state_l1`). Un point listé (interrupteur réglable seulement) devient une énumération à une seule valeur, `cycle`, de sens `control` (un agent demande toujours l'accord d'une personne), en écriture seule : l'ordre part en `state_lN: "ON"`, l'état renvoyé n'est jamais publié ni relu. Un identifiant inconnu ou non éligible est signalé dans les journaux. Le tableau de bord montre un bouton « Impulsion » par relais (« Relais 1 », « Relais 2 ») ; la fonction de chaque relais se dit par le nom de l'appareil. **Migration** : un relais déclaré cesse d'être un interrupteur (`on_off` → `control`) ; une règle, un favori ou un historique qui lisait son état est à reprendre. **Boutons et télécommandes** : `action` (`single`, `double`, `hold`…) est un appui, pas un état : chaque appui est transmis, même deux fois le même de suite (`pulse_state`, comme Hue et Dirigera), donc un automatisme se déclenche à chaque fois. Un message retenu, rejoué par le broker quand Moli se connecte, n'est pas un appui ; un `action` vide (Z2M l'efface juste après) non plus, et le dernier appui reste affiché (#198). **Plantes** : `soil_moisture` a le sens `soil_moisture` (humidité de la terre, %) ; le `dry` d'un capteur de sol Tuya (COOLO CS-201Z…) prend le sens `soil_dry` (la plante a soif), seulement à côté d'une humidité du sol : un `dry` seul garde le sens `other`. Ce n'est jamais une alerte de sécurité. Le tableau de bord montre une pastille « plante » dans la pièce, et la température et l'humidité du pot ne comptent pas dans celles de la pièce ; l'assistant ne les lit pas non plus comme celles de la pièce. **Migration** : `soil_moisture` et `dry` passent de `other` à leur sens ; une automatisation qui lisait `dry` garde la même clé. **Météo** (station Shelly WS90, capteur de vent générique) : un point en lecture seule reçoit son sens météo d'après le nom que Z2M lui donne et son unité, jamais d'après le nom seul : `wind_speed` → `wind_speed`, `gust_speed` → `wind_gust` (en m/s chez Z2M : convertis en km/h, × 3,6, bornes comprises), `wind_direction` en ° → `wind_direction`, `rain_rate` en mm/h → `rain_rate`, `uv_index` sans unité → `uv_index`. Une autre unité, ou un point réglable (la vitesse d'un ventilateur), garde son sens d'avant. Le `precipitation` de la WS90 est un compteur sans fin : ce n'est pas `rain_today`. **Migration** : le vent d'une station Z2M passe de m/s à km/h ; son historique et un seuil d'automatisme en m/s sont à reprendre.
- **Identité** : adresse IEEE. Le `friendly_name` ne sert qu'au routage des sujets.
- **Découverte** : chaque nouveau `bridge/devices` recharge le catalogue (appareils disparus retirés ; coordinateur et appareils désactivés exclus). Disponibilité via `<nom>/availability` (`{"state":"online"}` ou `online`).
- **Pièges** : Z2M ne rejoue pas les états aux nouveaux abonnés. Les appareils sur secteur (routeurs) sont donc interrogés une fois par exécution via `/get`, un toutes les 500 ms pour ne pas saturer le réseau ; les appareils endormis restent couverts par le cache d'état du hub. Taille de paquet portée à 4 Mio (`bridge/devices` grossit avec le réseau). Les propres `/set` et `/get` de Moli reviennent par `#` : ignorés. Une commande reçue avant que le broker ait accepté la connexion (`ConnAck`) est **refusée** (`broker injoignable`) au lieu d'être acquittée puis perdue ; toute erreur de connexion termine l'exécution (le superviseur vide la file et reconnecte).
- **Tests** : `src/catalog.rs` sur des fixtures réelles `integrations/z2m/fixtures/bridge_devices.json` (12 appareils) et `state.json` : typage, décodage, encodage des commandes, composites, lectures à la demande.

## `zwave` : Z-Wave via zwave-js-ui (`crates/moli-zwave`)

- **Couvre** : tous les nœuds Z-Wave décrits par le gateway zwave-js-ui (lumières, prises,
  capteurs sur pile) ; on/off et niveau écrivables, tout le reste en lecture seule ; aucun
  code par marque.
- **Protocole** : MQTT local (1883 par défaut), abonnement à `{base}/#`. Commandes publiées
  sur `{base}/{nom}/{cc}/{endpoint}/{prop}/set` (payload JSON brut : `true`/`false` pour
  CC 32 sur `currentValue` et CC 37 sur `targetValue`, nombre pour CC 38 sur
  `targetValue` : le niveau `0`–`99`, ou `255` pour `on`, qui rallume au niveau
  précédent) ; les mises à jour d'état arrivent enveloppées `{time, value}` (défaut de la
  passerelle, déballées par le pilote) ; disponibilité sur `{base}/{nom}/status`
  (`true`/`false`) ; bootstrap par l'API `getNodes` du gateway (topic
  `{base}/_CLIENTS/ZWAVE_GATEWAY-{mqtt_name}/api/getNodes/set`, payload `{"args":[]}`,
  réponse sur le même topic sans le `/set`).
- **Configuration** :

```toml
[[driver]]
id = "zwave"
kind = "zwave"
[driver.options]
host = "127.0.0.1"          # défaut
port = 1883                 # défaut
base_topic = "zwave"        # défaut
# mqtt_name = "zwave-js-ui" # défaut — nom du client MQTT de la passerelle (champ « Name » du panneau MQTT ;
                           # la variable d'environnement MQTT_NAME de la passerelle le remplace si définie)
# username
# password_env = "MOLI_ZWAVE_PASSWORD"   # NOM de la variable, jamais le mot de passe
# client_id
```

- **Secrets** : mot de passe broker éventuel dans la variable d'environnement nommée par
  `password_env` ; jamais dans `moli.toml`.
- **Points** : `on` (CC 32 Basic, 37 Switch Binary, 38 Switch Multilevel : le number de
  CC 38 est décodé en booléen) et, écrivable, le niveau CC 38 — lu sur `currentValue`, le
  vrai niveau (`targetValue` ne garde que le dernier ordre, `255` après un `on`, et un
  appareil qui ne rapporte pas sa cible le laisse là), écrit sur `targetValue`, et pris sur
  le seul endpoint où il s'écrit. Il est exposé sous la clé `brightness` (la clé standard
  des lumières, celle que l'interface « maison » et les ambiances cherchent, comme Hue et
  Dirigera) quand la classe de device est un vrai variateur : le niveau Z-Wave (0–99)
  y est en % (0–100, unité `%`), comme les autres pilotes ; arrondi vers le bas à la
  lecture et vers le haut à l'envoi, chaque pourcentage revient tel quel sauf 99 % (relu
  100 %), et un variateur allumé n'est jamais affiché à 0 %. Sinon `level`, à l'échelle
  de l'appareil (0–99 ; volets, et le mode accumulation sans `getNodes`, où le premier
  `currentValue` CC 38 suffit à créer `on` et `level`) ; `battery` (CC 53 `batteryLevel`
  ou CC 128 `level`, %), `temperature` / `humidity` (CC 31/49 : « Air temperature »,
  « Relative humidity »),
  `power` (CC 50, clé 66049, W) en lecture ; un (cc, property) inconnu est ignoré proprement.
  Sémantique du point de niveau : `Brightness` uniquement si la classe de device du
  nœud est Multilevel Switch (generic 0x11) avec specific 0x01/0x02 — sinon `Other`,
  sinon la garde ne traite pas le nœud comme une lumière (un module de volet CC 38
  nommé « Portail » doit rester vérifiable par le nom). `native_name` conserve le nom
  brut du gateway (accents et espaces intacts) ; seul le sujet MQTT est assaini.
- **Identité** : `nodeId` zwave (`<instance>:<nodeId>`), jamais le nom ; le nom et la
  localisation zwave-js servent de suggestions (`native_name`, `native_room`).
- **Pièges** : le gateway ne rejoue pas les états sans le flag retain → bootstrap `getNodes`
  à chaque connexion (fenêtre initiale de 15 s), puis réessai toutes les 30 s jusqu'à
  la première réponse acceptée, puis rafraîchissement périodique toutes les 300 s (les
  nœuds appairés ou renommés ensuite apparaissent sans redémarrage ; si la première
  fenêtre de 15 s passe sans réponse, un avertissement journalise le sujet interrogé —
  vérifier `base_topic` et `mqtt_name`) ; le fallback tolérant par
  accumulation des topics vus n'est armé qu'après cette première fenêtre (pas de fantômes
  `zwave:<nom>` si la réponse arrive après quelques secondes) ; un nœud
  n'est marqué hors ligne que sur `status` `false` après avoir été vu en ligne (un capteur
  sur pile jamais vu reste disponible) ; le shape de `getNodes` et la sérialisation des
  propertyKeys (CC 49) sont confirmés sur la cible (capture réelle + test
  on/off sur nœud neutre) ; les sujets supposent la passerelle en mode `VALUEID` avec
  `nodeNames` (`"gateway": {"type": 0, "nodeNames": true}` dans `settings.json`) — sans
  cela la passerelle reste en mode `NAMED` et **ne publie aucun sujet de valeur** ; ne pas
  cocher « Ignore location » dans la passerelle : le pilote préfixe chaque sujet par la
  localisation du nœud quand elle existe (cf. `getNodes`), et un sujet sans préfixe ne
  serait plus associé à son appareil ; les mises à jour d'état arrivent enveloppées
  `{"time", "value"}` (défaut de la passerelle, déballées par le pilote) ; ne jamais
  journaliser les `MqttOptions` (le `Debug` affiche le mot de passe) ; une commande avant
  `ConnAck` est refusée (`broker injoignable`), jamais acquittée puis perdue ; le succès
  d'une écriture n'est pas vérifié (la passerelle n'acquitte pas les commandes).
- **Tests** : `src/catalog.rs` sur les fixtures `integrations/zwave/fixtures/get_nodes.json`
  (6 nœuds synthétiques, dont un moteur de volet CC 38), `value_messages.json` et
  `get_nodes_real.json` (capture réelle anonymisée) : découverte, typage, décodage,
  encodage on/niveau, tolérance (enveloppe, propertyKey, bornes), règles de disponibilité
  et de config.

## `zigbee` : Zigbee natif, clé EZSP (`crates/moli-zigbee`)

- **Couvre** (phases 1 à 3, essayées sur une clé ZBDongle-E et une ampoule Innr RB 287 C ;
  capteurs et prises sur simulateur à ce jour) : la clé coordinateur et son réseau (formé s'il
  n'existe pas, repris sinon, restauré depuis une sauvegarde importée), l'appairage ouvert depuis
  Moli, l'import et l'export d'une sauvegarde Zigbee2MQTT, et les appareils (voir **Appareils**).
  Pas encore : familles propriétaires (Tuya, Xiaomi, Lixee), actions des télécommandes, retrait
  d'un appareil depuis Moli (phase 4) : d'ici là, `z2m` reste le pilote d'une maison.
- **Appareils** (`src/fleet.rs`, `src/interview.rs`, `src/devices.rs`, `src/zcl.rs`) :
  - **Interview** à l'arrivée, une à la fois, 2 s après l'annonce : descripteur de nœud, endpoints,
    descripteurs simples (3 essais chacun : la 1re requête après un appairage se perd souvent),
    `Basic` attribut par attribut, capacités de couleur, type de zone et inscription IAS (les deux
    procédures de herdsman, puis vérification de `zoneState`), diviseurs de mesure. Un silence à
    n'importe quelle étape l'arrête, ce qu'elle a appris gardé dans le `store` : `ctx.warn` le dit,
    et la suivante repart de là, sans délai, au prochain message de l'appareil (1 min d'écart au
    moins) ou toutes les 10 min. Une réponse sans l'attribut est une réponse. Après 3 échecs, seuls
    les descripteurs et l'inscription IAS retiennent l'appareil : une lampe sans modèle vaut mieux
    que pas de lampe. Un appareil du
    `store` pas encore interrogé l'est au démarrage. Réussie, elle n'est refaite que si l'appareil
    rejoint « de rien » (`STANDARD_SECURITY_UNSECURED_JOIN`) : il a été réinitialisé et a tout
    oublié. Un appareil qui part sans avoir été interrogé est oublié.
  - **Points** : génériques par cluster, mêmes clés et mêmes unités que `moli-z2m`
    (`brightness` en %, `contact` vrai = fermé, `color` en `#rrggbb` avec `color.x`, `color.y`
    en lecture). Un 2e endpoint porte son numéro (`state_2`). `color_temp` vaut `Null` tant que
    la lampe est en couleur : elle continue d'en rapporter une, au bout de sa plage. Sa plage
    est bornée à 150-600 mired : une lampe peut déclarer bien plus qu'elle ne fait.
  - **Rapports** : bind vers l'endpoint 1 du coordinateur et configure reporting (valeurs de
    `reporting.ts` de herdsman-converters ; pour une prise, 5 W, 5 V, 50 mA et 0,1 kWh, ramenés aux
    diviseurs de l'appareil, comme `genericMeter`, et `power` lu sur Electrical s'il existe) ; un
    refus est une réponse, un silence se retente (1 h d'écart). Un rapport qui demande une réponse
    par défaut la reçoit, jamais un broadcast.
  - **Ordres** : remis à la clé dans l'ordre reçu par la boucle, puis suivis à part : un appareil
    muet ne retient ni la boucle ni les autres. Réponse par défaut en erreur = refus rapporté à
    l'auteur ; accusé radio sans réponse = pris ; rien à 300 ms de l'échéance du hub = échec dit à
    l'auteur. Un appareil dormant : « remis » dès que la clé l'a pris (il le recevra à son réveil,
    bien après les 5 s du hub), et un avis à la maison s'il ne l'a finalement pas. Valeur optimiste affichée dès que l'appareil l'a, confirmée par son
    rapport. Échec : ses valeurs sont relues.
  - **Disponibilité** (valeurs de Z2M) : tout message = en ligne ; à l'écoute et muet 10 min → une
    lecture (une par silence de 10 min, pas plus), sans réponse → hors ligne ; dormant muet 25 h →
    hors ligne (sa lecture manquée ne compte pas : il dort) ; parti du réseau → hors ligne, gardé
    dans le `store`. Silence noté toutes les heures et à l'arrêt : il continue à travers un
    redémarrage, sans compter le temps où Moli était arrêté. Lectures cadencées à 500 ms, toutes refaites au retour du réseau.
- **Protocole** : EZSP (UG100) dans des trames ASH (UG101), sur le port série de la clé (ou
  `tcp://hôte:port` pour un adaptateur réseau). Le pilote réinitialise la clé (RST), négocie
  EZSP **13 ou plus** (EmberZNet 7.4+), envoie la configuration d'un coordinateur
  (`setConfigurationValue`, liste et valeurs du pilote `ember` de zigbee-herdsman, provenance
  ligne par ligne dans `src/coordinator.rs` ; une valeur refusée par la clé est journalisée
  et la sienne gardée), l'endpoint 1 du coordinateur (`addEndpoint` : profil Home Automation,
  identifiant et clusters de `FIXED_ENDPOINTS` de herdsman ; refusé, le démarrage échoue),
  les politiques du centre de confiance (`setPolicy`, comme `initTrustCenter` de herdsman),
  puis `networkInit` :
  - réseau gardé par la clé → attente de `stackStatusHandler` « réseau levé » (10 s), puis
    `getNetworkParameters` + `getEui64` : le réseau n'est repris (et recopié dans le `store`,
    canal et `nwk_update_id` compris) que si la clé en est le coordinateur et que c'est le
    réseau du `store` (même PAN et PAN étendu), ou, `store` vide, si le coffre a déjà une
    `network_key` (première formation interrompue). Sinon le pilote attend en le disant
    (`pilotes.zigbee.pas_coordinateur`, `autre_reseau`, `reseau_inconnu`) **sans rien
    changer** : ni le réseau de la clé (jamais quitté ni effacé), ni le `store` ;
  - pas de réseau et `store` vide → **formation** (`src/network.rs`, séquence `formNetwork` de
    herdsman) : clé réseau et clé de lien du centre de confiance aléatoires (`ring`), rangées
    au coffre **avant** toute commande de sécurité (reprises si le coffre les a déjà, après un
    essai interrompu), `setInitialSecurityState`, masque de sécurité étendu, table des clés
    vidée, `formNetwork` (canal `channel` ou 15, PAN et PAN étendu aléatoires), attente du
    réseau levé, puis paramètres écrits dans le `store` ;
  - pas de réseau mais un réseau connu du `store` → **restauration** (`src/backup.rs`,
    `formNetwork` « depuis une sauvegarde » de herdsman) avec les clés du coffre, sur la clé
    d'origine seulement, depuis un compteur de trames frais seulement (+ 65 536), sinon le
    pilote attend en le disant ; jamais de formation par-dessus. Un relevé de plus de 24 h
    (ou sans date) n'est pas restauré non plus (`pilotes.zigbee.compteur_ancien`) : une clé
    restée sous tension, Moli arrêté, a pu dépasser la marge.
  - **compteur de trames** : dès que le réseau tourne (formé, restauré ou repris et reconnu),
    puis toutes les heures tant qu'il est levé, le pilote lit le compteur de trames NWK sortant
    de la clé (`getNetworkKeyInfo`, comme `backup()` de l'adaptateur ember de herdsman ; statut
    `SlStatus` sur 32 bits en EZSP 13 comme 14) et l'écrit dans le `store`
    (`frame_counter`, `frame_counter_fresh`, jamais à la baisse, daté en secondes unix dans
    `frame_counter_recorded_at`, comme l'import, daté de son heure). À l'arrêt propre de Moli,
    sur un réseau levé et vérifié, le pilote referme l'appairage s'il est ouvert et relève le
    compteur une dernière fois (3 s au plus : le superviseur lui laisse 5 s), pour qu'un
    export juste après parte de là. La lecture horaire laisse les
    rappels de la clé être lus ; une lecture ratée est journalisée et retentée l'heure
    suivante, sans effacer le relevé. Une restauration ou un export part de ce relevé
    + 65 536 (`FRAME_COUNTER_MARGIN` : la clé émet en continu, le relevé peut dater d'une heure).
    Un réimport d'une sauvegarde au compteur inférieur au relevé est refusé. Un réseau repris
    qui reste « down » au démarrage n'est pas vérifié : le relevé du `store` cesse d'être
    restaurable (valeur gardée, `frame_counter_fresh` à faux), sauf si la clé se lit comme un
    autre réseau (ce relevé n'est alors pas le sien) ; quand il se lève, il est vérifié comme
    au démarrage (même réseau, coordinateur ; sinon le pilote attend en le disant, rien n'est
    adopté), puis son compteur est relevé aussitôt et toutes les heures. Tant que cette
    vérification n'a pas pu lire la clé (nouvel essai à l'heure), l'appairage est refusé
    (`pilotes.zigbee.reseau_non_verifie`) et aucune arrivée n'est notée dans le `store`.
- **Configuration** :

```toml
[[driver]]
id = "zigbee"
kind = "zigbee"
[driver.options]
port = "/dev/ttyUSB0"   # ou "tcp://192.168.1.x:6638"
# baud = 115200         # défaut
# flow = "software"     # défaut (XON/XOFF, ZBDongle-E) ; "hardware" ou "none"
# channel = 15          # canal d'un nouveau réseau (11 à 26, sinon Moli refuse de démarrer) ; 15 par défaut
```

- **Secrets** : `network_key` et `tc_link_key` (32 chiffres hexadécimaux), écrites dans le
  coffre (jamais dans un fichier ni un journal, `Debug` masqué) par le pilote lui-même quand il
  forme le réseau, ou par `moli-os zigbee import` (Moli arrêté) depuis une sauvegarde
  Zigbee2MQTT ; il faut donc une clé maître. Sans coffre, aucun réseau n'est formé : le
  pilote attend (`pilotes.zigbee.coffre_indisponible`).
- **Fichier** : `<data_dir>/zigbee-<instance>.json` (le `store`) : version, paramètres du
  réseau (PAN, PAN étendu, canal, IEEE du coordinateur, `nwk_update_id`, compteur de trames
  et numéro de séquence de la clé, **sans clé**), appareils. Écrit dans `<fichier>.tmp` puis
  renommé. Aucun fichier de sauvegarde avec la clé n'est tenu à jour : la clé ne vit qu'au
  coffre.
- **Retour à Zigbee2MQTT** : `moli-os zigbee export <fichier> [--instance <id>] [--force]`
  (Moli arrêté) écrit une sauvegarde Open Coordinator Backup au format de l'adaptateur ember
  de herdsman (`toUnifiedBackup`) : réseau et appareils du `store`, `network_key` et
  `hashed_tclk` lus au coffre à ce moment, compteur = relevé + 65 536. Fichier privé (0600),
  écrit atomiquement, jamais par-dessus un fichier existant sans `--force` ; rien n'est écrit
  sans les deux clés au coffre ni sans compteur frais relevé depuis 24 h au plus
  (`pilotes.zigbee.export_compteur_ancien`). La clé n'est jamais affichée ; un
  avertissement (`pilotes.zigbee.export_avertissement`) rappelle de supprimer le fichier après
  usage. `moli-os zigbee import` relit ce fichier.
- **Points** : appareil `coordinator` : `firmware` (texte, `7.4.4.0`), `ezsp_version`
  (entier), `network` (`up`, `down`, `none`, suit `stackStatusHandler`), en lecture seule ;
  `permit_join` (booléen, **écrivable**) : vrai ouvre l'appairage 254 s comme le `permitJoin`
  de herdsman (clé transitoire « ZigBeeAlliance09 », politique d'arrivée, `permitJoining`
  sur la clé, puis `Mgmt_Permit_Joining_req` diffusé à `0xFFFC`), et retombe seul à faux au
  bout de 254 s ; faux le referme tout de suite ; refusé si le réseau n'est pas levé
  (`pilotes.zigbee.reseau_absent`) ou pas encore reconnu comme celui du `store`
  (`pilotes.zigbee.reseau_non_verifie`) ; sémantique `control` : un agent qui le demande attend
  l'accord d'un humain ; une ouverture qui échoue en route est refermée sur la clé (au mieux)
  avant que l'erreur soit rendue ; `joining` (texte) : l'IEEE du dernier appareil arrivé
  (`trustCenterJoinHandler` ou annonce ZDO), noté dans le `store` (`interviewed: false`) ;
  un appareil qui part reste dans le `store` (v1).
- **Pièges** : un firmware plus ancien qu'EZSP 13 met le pilote en `waiting` (message
  `pilotes.zigbee.firmware_ancien` avec la version trouvée) **sans relance en boucle** : seul
  un nouveau firmware y fait quelque chose. Une clé qui redémarre seule ou un lien perdu
  termine l'exécution (le superviseur relance, tout recommence au RST). Un ordre reçu avant
  que le coordinateur soit prêt est refusé (`pilotes.zigbee.pas_pret`). Les rappels de la clé
  sont lus en continu (au-delà de 256 en attente, le client les jetterait). Un réseau repris
  qui ne se lève pas en 10 s laisse le pilote prêt avec `network = down`, sans relance en
  boucle : le `stackStatusHandler` suivant remet le point à jour (un « réseau tombé » pendant
  l'attente est ignoré, comme herdsman). Une clé qui porte un autre réseau que celui du
  `store`, un réseau inconnu sans clé au coffre ou un réseau dont elle n'est pas le
  coordinateur n'est jamais adoptée : le `store` et le coffre restent ceux du même réseau. Le
  port série est pris en exclusivité (`flock`, comme Zigbee2MQTT, puis `TIOCEXCL`, rendu à la
  fermeture) : un port déjà tenu par un autre programme est refusé en le nommant
  (`pilotes.zigbee.port_occupe`), le superviseur réessaie.
- **Tests** : un NCP simulé (`src/sim.rs`) qui parle le vrai ASH ; le pilote complet tourne
  sur un vrai hub (`src/lib.rs`) : prêt avec son coordinateur, firmware trop ancien, ordres
  refusés avant d'être prêt, lien perdu, formation (clé au coffre avant la clé, canal 15,
  `store` sans clé), clé du coffre reprise, sans coffre pas de réseau, réseau connu jamais
  reformé, réseau repris, réseau repris qui reste tombé (horloge tokio en pause), réseau
  levé tard vérifié puis relevé, autre réseau, réseau inconnu sans clé ou clé non coordinateur jamais
  adoptés, formation interrompue reprise ; restauration d'un réseau importé (compteur inconnu,
  usé, épuisé, de plus de 24 h, autre clé, import interrompu : rien n'est touché), import
  (`moli-os zigbee import` : clés au coffre seulement, `--force`, sauvegarde plus ancienne
  refusée), export (format de herdsman, fichier privé, compteur trop ancien refusé),
  appairage (accord humain pour un agent, ouverture et fermeture, échec refermé, refusé sur
  un réseau pas encore vérifié), arrivées notées, relevé horaire du compteur, arrêt propre
  (appairage refermé, dernier relevé, jamais bloquant), port série exclusif (pty).
- **Onboarding** : `integrations/zigbee/onboarding.md` (chemin `/dev/serial/by-id/…`,
  `--device`, groupe du port, mise à jour du firmware).

## `hue` : Philips Hue (`crates/moli-hue`)

- **Couvre** : lumières, interrupteurs (boutons), détecteurs (mouvement, température, luminosité, contact), batteries, pièces et zones Hue.
- **Protocole** : HTTPS CLIP v2 local (443), certificat épinglé. Chargement complet `GET /clip/v2/resource`, puis flux d'événements SSE `/eventstream/clip/v2`. Commandes en `PUT /clip/v2/resource/light/<id>` (ou `grouped_light/<id>`).
- **Configuration** :

```toml
[[driver]]
id = "hue"
kind = "hue"
[driver.options]
host = "192.168.1.x"
# app_key_env = "MOLI_HUE_KEY"   # optionnel : clé fournie par l'environnement au lieu de l'appairage
# cert_sha256 = "…"              # optionnel : empreinte du pont si le coffre est indisponible
```

- **Secrets** (instance) : `app_key`, `client_key`, `cert_sha256` et `app_key_cert_sha256` (l'empreinte sous laquelle la clé a été donnée), écrits par le pilote lui-même lors de l'appairage. Il faut donc une clé maître. **La clé est liée à son certificat** : empreinte oubliée (après un « certificat changé »), la clé est oubliée aussi, avant tout contact, et l'appairage recommence ; une clé donnée sous une autre empreinte n'est jamais envoyée. Une clé enregistrée avant ce changement est adoptée si l'empreinte en place est celle enregistrée avec elle (pas de nouvel appui) ; sinon elle est oubliée. Tant qu'une clé `app_key_env` est présente, la clé du coffre n'est ni lue, ni liée, ni oubliée : le tri se fait au premier démarrage sans elle. Avec `app_key_env`, la clé ne peut pas être oubliée : renseigner `cert_sha256`.
- **Appairage** : statut `waiting` « Appuie sur le bouton du pont Hue » ; le pilote redemande toutes les 2 s (erreur Hue 101) jusqu'à l'appui. L'empreinte est stockée **avant** la clé, sur tous les chemins : sans empreinte connue (ni coffre, ni `cert_sha256`), poignée de main nue et `cert_sha256` stocké avant l'appairage comme avant l'usage d'une clé `app_key_env`. Clé révoquée (401/403) : la clé est oubliée et l'appairage recommence (sauf clé venue de `app_key_env` : `waiting`).
- **Points** : appareil Moli = `device` Hue, points issus de ses services. Lumière : `on`, `brightness` (%), `color_temp` (mired), `color` (« #rrggbb », envoyé au pont en CIE xy, `moli_core::color`) **écrivables** ; `color.x`, `color.y` en lecture (historique). En mode couleur le pont rend `mirek: null` : `color_temp` vaut alors `null`. `effect` (enum, écrivable) sur les ampoules qui ont des animations (gen 3 : `fire`, `candle`, `prism`, `sparkle`, `underwater`, `cosmos`…, `no_effect` pour arrêter). `button<N>` (enum des événements, en `pulse`), `battery`, `motion`, `temperature`, `illuminance` (lux, converti depuis `10000·log10(lux)+1`), `contact`. Un second service du même type est préfixé (`light2.on`). Disponibilité via `zigbee_connectivity`.
- **Pièces et zones** : publiées comme appareils avec `on` et `brightness`. **Pièces écrivables**, **zones en lecture seule** (une zone peut chevaucher une chambre protégée). Les `members` permettent à la garde de vérifier la pièce de chaque lampe.
- **Identité** : UUID du `device` Hue (ou de la pièce/zone). Le pont lui-même n'est pas publié.
- **Pièges** : un renommage ou un changement de pièce/zone déclenche un rechargement complet. Flux muet 10 min : réouverture et rechargement. Un message SSE (ou une ligne) qui dépasse 1 Mio sans fin : flux fermé, rouvert et rechargement complet. Les commandes tournent à côté du flux avec un budget de 4,5 s (sous les 5 s du hub). Le message « certificat changé » montre l'empreinte épinglée et celle présentée, et donne la commande `secrets forget <instance> cert_sha256` avec l'identifiant réel de l'instance (la clé part avec l'empreinte). Avec `app_key_env` configuré (même si la variable manque) ou une empreinte de la configuration, il ne conseille pas d'oublier l'empreinte : il dit de refaire la clé, ou de changer `cert_sha256` (les empreintes complètes sont dans le journal). `Bridge` n'affiche jamais la clé en `Debug`.
- **Tests** : `src/model.rs` sur `integrations/hue/fixtures/resources.json` (pont réel) : pièces commandables, zones jamais, boutons, capteurs, événements partiels, encodage ; `src/bridge.rs` : parseur SSE découpé en morceaux, plafond de 1 Mio (ligne sans fin, `data:` sans ligne vide).

## `melcloud` : climatiseurs Mitsubishi Electric (`crates/moli-melcloud`)

- **Couvre** : les unités air-air du MELCloud classique, en lecture et en commande. MELCloud Home est un autre service, non couvert.
- **Config** : `kind = "melcloud"`, `poll_seconds` en option (300 par défaut, de 60 à 3600). Secret `token` : la clé de contexte du compte, jamais le mot de passe.
- **Points** : `on`, `mode`, `target_temperature`, `fan_speed`, `vertical_vane`, `horizontal_vane` **écrivables** selon ce que l'unité annonce ; `temperature` en lecture. Un invité ou un réglage verrouillé reste en lecture.
- **Commandes** : `Device/Get`, puis `Device/SetAta` avec tout l'état et le drapeau du seul champ changé. L'unité applique l'ordre à son prochain contact ; la liste retarde, donc ce qui a été commandé fait foi 150 s. Budget de 4,5 s, sous les 5 s du hub. La consigne est arrondie au pas et refusée hors de la plage du mode en cours.
- **Identité** : `DeviceID` du service.
- **Pièges** : API web non officielle, dépendante d'Internet ; `Offline` varie d'une réponse à l'autre, d'où 3 réponses de suite avant de passer hors ligne. Une clé refusée (401, 403) met le pilote en attente d'un humain.
- **Tests** : `src/model.rs` sur `integrations/melcloud/fixtures/list_devices.json`, plus les emplacements (zones, étages), les doublons et les valeurs absurdes.

## `overkiz` : Somfy TaHoma en local (`crates/moli-overkiz`)

- **Couvre** : volets, stores, rideaux, écrans, pergolas et auvents compatibles avec
  l’API locale du mode développeur ; favoris « My » selon les commandes annoncées.
  Portails et fenêtres sont observés en lecture seule par défaut. Les scénarios
  enregistrés, programmations et autres fonctions cloud ne sont pas implémentés ici.
- **Configuration** :

```toml
[[driver]]
id = "somfy"
kind = "overkiz"
[driver.options]
host = "gateway-example.local"
read_only = true
# port = 8443
# poll_secs = 5
```

- **Secrets** : jeton du mode développeur dans `token`, empreinte dans `cert_sha256`,
  rangés sous l’identifiant d’instance. Le certificat est épinglé avant l’envoi du
  jeton ; le premier contact TOFU exige un réseau de confiance ou une empreinte
  vérifiée configurée à l’avance. Un jeton refusé ou un certificat changé place
  le pilote en `waiting`, appareils hors ligne.
- **Points et garde** : `percent_control` est le pourcentage ouvert (100 = ouvert),
  de sens `Position` pour les ouvrants ordinaires ; `control` propose seulement les
  commandes natives qualifiées, `favourite_position` reste une configuration.
  RTS ne reçoit jamais de position mesurée inventée. Toute écriture d’un portail
  ou d’une fenêtre exige l’autorisation explicite `writable_openers` par URL exacte
  et classe ; tous ses points inscriptibles gardent le sens `Control`, curseur
  compris. `read_only` refuse aussi les commandes admises sur des métadonnées anciennes.
- **Identité et disponibilité** : SHA-256 de l’URL native exacte, indépendante du
  nom. Une absence met immédiatement l’appareil hors ligne ; sa suppression exige
  trois lectures réussies consécutives sans lui. Un retour conserve ses étiquettes
  et remet le compteur à zéro ; une lecture échouée ne compte pas.
- **Commandes** : prélecture des capacités et de la disponibilité, budget commun
  de 4,5 s maximum, un seul `exec/apply` sans rejeu. Un refus isolé ne redémarre pas
  le pilote. L’accusé natif ne confirme pas le mouvement physique.
- **Tests et limites** : fixtures synthétiques, vrai Hub et serveur TLS simulé ;
  inventaire historique anonymisé rejoué hors réseau. Les commandes matérielles
  restent à qualifier pour cette implémentation.
  [Détails et convergence](../../integrations/overkiz/README.md),
  [couverture restante](../../integrations/overkiz/COVERAGE.md).

## `voltalis` : radiateurs Voltalis (`crates/moli-voltalis`)

- **Couvre** : les radiateurs d'un site Voltalis, en lecture et en commande.
- **Config** : `kind = "voltalis"`, `poll_seconds` en option (300 par défaut, de 60 à 3600). Secrets `username` et `password` : le service n'a pas de clé durable ; le jeton de session reste en mémoire.
- **Points** : `on`, `mode` (les modes que le radiateur annonce), `target_temperature` (s'il a le mode `temperature`), `automatic` (sur le programme du service) **écrivables** ; `fault` (tout diagnostic autre que `OK`) en lecture.
- **Commandes** : `GET manualsetting` pour trouver le réglage du radiateur, puis `PUT manualsetting/{id}`, jusqu'à nouvel ordre ; `automatic = true` ferme le réglage manuel. Un mode ou une consigne allume le radiateur ; éteindre garde le mode. Session finie : reconnexion et un second essai ; refus à la reconnexion : le pilote s'arrête. Budget de 4,5 s, sous les 5 s du hub.
- **Identité** : `id` de l'appareil dans le service.
- **Pièges** : API web non officielle, dépendante d'Internet. Un compte refusé arrête le pilote jusqu'à l'intervention d'un humain, pour ne pas bloquer le compte ; une session expirée est rouverte au relevé suivant.
- **Tests** : `src/model.rs` sur `integrations/voltalis/fixtures/`, plus les statuts inconnus et les valeurs absurdes.

## `dirigera` : hub IKEA Dirigera (`crates/moli-dirigera`)

- **Couvre** : lampes (marche, luminosité, température de couleur, couleur), prises (marche,
  puissance, courant, tension, énergie), capteurs (température, humidité, PM2,5, COV, CO₂,
  mouvement, éclairement, ouverture, fuite d'eau), boutons de raccourci (appuis), stores
  (position, ouvrir / stop / fermer, mouvement ; non vérifiés sur un vrai store) et batteries.
  Pas encore : purificateurs, enceintes, pièces et scènes IKEA.
- **Protocole** : API locale **non officielle**, HTTPS sur **8443**, certificat épinglé (un
  certificat d'usine P-384 valable jusqu'en 2071). Chargement complet `GET /v1/home`, puis
  WebSocket `wss://<hub>:8443/v1` (événements `deviceStateChanged`, `remotePressEvent`…), un
  événement `ping` toutes les 30 s (le hub répond `pong`). Commandes en
  `PATCH /v1/devices/<id>` avec `[{"attributes": {…}}]`.
- **Configuration** :

```toml
[[driver]]
id = "ikea"
kind = "dirigera"
[driver.options]
host = "192.168.1.x"
# cert_sha256 = "…"   # optionnel : empreinte du hub si le coffre est indisponible
```

- **Secrets** (instance) : `cert_sha256`, `token` (un JWT ES256 valable 10 ans) et
  `token_cert_sha256` (l'empreinte sous laquelle le jeton a été donné), écrits par le pilote
  lui-même lors de l'appairage. Il faut donc une clé maître. Plusieurs jetons cohabitent
  (celui de Moli et celui d'un autre client restent valables). **Le jeton est lié à son
  certificat** : sans empreinte connue (oubliée après un « certificat changé »), ou avec une
  autre dans les options, il est oublié **avant** tout contact et l'appairage recommence ;
  celui qui répond à la place du hub ne le reçoit jamais.
- **Appairage** (OAuth PKCE `S256`) : statut `waiting` « Appuie sur le bouton d'action sous le
  hub Dirigera », affiché **seulement** quand Moli tient un code. Le pilote demande un code
  (`/v1/oauth/authorize`, valable 60 s), puis le
  jeton toutes les 2 s (`/v1/oauth/token`) : `403 Button not pressed…` = pas encore,
  `403 Pairing request timed out` ou `401 Invalid authorization code` = code périmé, un autre
  est demandé. Le hub n'appaire qu'un client à la fois (`409 Already one ongoing pairing
  request`) : le statut dit alors de **ne pas** appuyer (l'appui pourrait profiter à l'autre
  client), et le pilote attend que l'autre demande expire ; tant que Moli attend l'appui, le
  hub refuse donc tout autre appairage. Une réponse lente ne fait pas perdre l'appui
  (nouvel essai ; 16 en 10 h d'attente sur un vrai hub). L'empreinte est
  stockée **avant** le premier code. Jeton refusé (401/403 sur `/v1/home`) : oublié, et
  l'appairage recommence. Certificat changé : statut `waiting` avec l'empreinte épinglée et
  celle présentée, et la commande pour oublier l'empreinte (le jeton part avec elle).
- **Points** : selon les attributs présents (deux appareils d'un même type peuvent différer).
  Lampe : `on`, `brightness` (1-100 %), `color_temp` (mired, converti depuis les kelvins ;
  bornes en mireds acceptées, KAJPLATS), `color` (« #rrggbb », envoyé en teinte et saturation)
  ; **écrivables** si l'attribut est dans `capabilities.canReceive`. En mode couleur,
  `color_temp` vaut `null` ; en mode blanc (`colorMode: temperature`), `color` vaut `null`. Prise : `on` ✎, `power`, `current`, `voltage`, `energy` (kWh).
  Capteurs : `temperature`, `humidity`, `pm25`, `voc_index`, `co2`, `motion` (`isDetected`),
  `illuminance` (lux, converti depuis la mesure Matter ou Zigbee `10000·log10(lux)+1` comme
  pour Hue ; 0, trop sombre pour mesurer, donne 0 lx ; 0xFFFF, aucune mesure, `null`),
  `contact` (vrai = fermé, comme Hue), `leak`. Bouton de raccourci : `button`
  (`single`, `double`, `long`, en `pulse`). Store : `position` ✎ (sens `position`, 0-100 %,
  100 = ouvert ; le hub compte à l'envers, 0 = en haut : lue dans `blindsCurrentLevel`,
  envoyée en `blindsTargetLevel`, inversée), `control` ✎ (`open` / `stop` / `close`, un ordre
  jamais relu, envoyé en `blindsState` `up` / `stopped` / `down`), `moving` (`UP` / `DOWN` /
  `STOP` depuis `blindsState`, `null` pour une valeur inconnue) ; reconnu comme volet par le
  tableau de bord et l'assistant. `battery` partout où il y a une pile.
  Disponibilité : `isReachable` (aussi dans chaque événement). **Migration** : au
  déploiement, l'historique `illuminance` des capteurs Dirigera passe de la mesure brute
  (jusqu'à 40001) aux lux (jusqu'à 10 000) ; les anciennes valeurs restent brutes dans les
  graphiques. Une automatisation écrite avec un seuil brut (`< 20000`, environ 100 lx) est à
  reprendre.
- **Identité** : `id` du Dirigera (un UUID suivi de `_1`, `_2`… : un produit Matter comme le
  MYGGSPRAY se présente en deux appareils). Nom natif = `customName` (sinon le modèle), pièce
  native = la pièce IKEA, sans l'espace insécable que le hub laisse parfois au bout. Le hub peut
  ne donner cette pièce qu'à un appareil d'un produit (un MYGGSPRAY ou un VALLHORN : la présence,
  pas la lumière) : un appareil en lecture seule sans pièce reprend celle de son `relationId`,
  sauf si le produit est réparti sur deux pièces. Un appareil qu'on peut commander ne reprend
  rien : une pièce dit aussi qui peut le commander. Comme toute pièce native, celle-ci s'efface à
  l'affichage devant une pièce choisie dans Moli, mais la garde et l'accès des invités comptent
  les deux. Le hub, les répéteurs et les télécommandes de lampes (qui pilotent leurs
  lampes elles-mêmes) ne publient que leur pile, ou rien.
- **Pièges** : un renommage, un changement de pièce, un ajout ou un retrait recharge tout. Le
  WebSocket coupé (redémarrage du hub) ou muet 95 s : rechargement complet puis reconnexion ;
  refermé en moins de 10 s : le superviseur reprend la main (attente croissante). Hub
  redémarré : appareils hors ligne le temps du démarrage, puis rechargés sans nouvel appairage. Les
  commandes tournent à côté des événements avec un budget de 4,5 s (le hub répond `202` en
  0,3 s environ) ; la confirmation revient par le WebSocket, valeurs intermédiaires comprises
  (une luminosité varie en fondu : 60 puis 30). **Un attribut par `PATCH`** : `isOn` et
  `lightLevel` ensemble, le hub répond `202` mais ignore la luminosité. Le hub arrondit teinte
  et saturation (pas de 1/254). `Hub` et `Grant` n'affichent jamais le jeton en `Debug`.
- **Tests** : `src/pkce.rs` (vecteur de l'annexe B de la RFC 7636, base64url de la RFC 4648,
  formulaire du jeton) ; `src/hub.rs` (réponses d'appairage vues sur un hub) ; `src/lib.rs`,
  avec le vrai coffre du runtime : un jeton sans son empreinte est oublié avant tout
  contact, et « Appuie sur le bouton » ne vient qu'après un code (jamais pendant un 409) ;
  `src/model.rs` sur `integrations/dirigera/fixtures/home.json` (un hub réel anonymisé : lampes
  TRADFRI et ORMANAS, détecteur MYGGSPRAY, le hub lui-même) et `events.json` (événements
  réels : marche/arrêt, fondu, couleur, blanc), plus chaque famille, bornes en mireds, contact
  inversé, couleur partielle, éclairement en lux (0, bornes 1 et 40001, un relevé réel, 0xFFFF), pièce partagée par un produit (ordre, deux pièces, lecture seule, déplacement), store (points et sens, position inversée dans les deux sens,
  ordres, mouvement, valeur inconnue), types inconnus ignorés, encodage des commandes.

## `enedis` : compteur Linky par la Conso API (`crates/moli-enedis`)

- **Couvre** : la consommation d'un compteur Linky, heure par heure, avec un jour de retard. Lecture seule.
- **Config** : `kind = "enedis"`, `prm` (les 14 chiffres du compteur). Secret `token` : le jeton de la Conso API.
- **Points** : `last_day` (date du dernier jour complet), `last_day_kwh`, `last_day_peak`, `hours` (les heures reçues, en JSON, pour un compteur `history = true` de `[energy]`).
- **Identité** : un appareil `linky` par instance.
- **Pièges** : service gratuit, non officiel, au quota partagé : une requête par jour à une heure propre au compteur, une seconde vers 14 h si la veille manque, aucune de plus au redémarrage (sauf au tout premier). Une mesure manquante écarte son heure et son jour. Les heures sont locales et nomment la fin de la mesure ; au passage à l'heure d'hiver, la même heure revient deux fois. Un jeton refusé arrête le pilote jusqu'à l'intervention d'un humain.
- **Tests** : `src/model.rs` sur `integrations/enedis/fixtures/`, plus les changements d'heure, les mesures absurdes et le calendrier des requêtes.

## `tuya` : Tuya Wi-Fi en local (`crates/moli-tuya`)

- **Couvre** : interrupteurs, prises, volets et tout appareil Tuya Wi-Fi joignable directement. **Par le cloud** (`src/cloud.rs`, lecture seule), quand les clés du projet cloud du foyer sont là : les capteurs **sur pile** sans adresse (catégories `mcs` porte, `sj` fuite, `ywbj` fumée, `pir`, `wsdcg`, `rqbj`, `cobj`, `sos`), qui ne parlent jamais sur le réseau local, et les **sous-appareils derrière une passerelle** Tuya. Mêmes identifiants Moli qu'en local (historique, étiquettes, portes de l'Accueil inchangés). **En local, par la passerelle** : un sous-appareil qui a dans `tuya.json` son `node_id` et son `parent` (passerelle joignable avec sa clé) passe par la connexion de sa passerelle (`Child` dans `src/device.rs`) : trames d'état routées par leur `cid`, requête d'état par `cid` à chaque connexion, ordres avec `cid` (3.3 : `{t, cid, dps}` ; 3.4+ : `data: {cid, ctype: 0, dps}`) ; en 3.3, l'état d'un sous-appareil déchiffré avec la clé de la passerelle vaut preuve (la passerelle répond « data unvalid » pour elle-même) ; disponibilité = celle de la passerelle. Il prime alors sur le cloud. **Ampoules** (`src/light.rs`, catégories de lumière et de variateur `light::LAMPS` seulement : ailleurs, un `bright_value` garde son code et son sens pour la garde) : `bright_value(_v2)`, `temp_value(_v2)`, `colour_data_v2` deviennent `brightness` (%), `color_temp` (mired, 2700–6500 K supposés, répartis en mired), `color` (`#rrggbb`, HSV Tuya lu en hexadécimal `HHHHSSSSVVVV` ou en JSON, écrit en hexadécimal) ; un ordre de couleur passe `work_mode` à `colour`, un blanc à `white`, une luminosité en couleur règle la valeur de la couleur (état courant lu par `ctx.current`) ; en couleur, la luminosité lue est celle de la couleur. **Appuis** (interrupteurs à scènes `wxkg`, points `switch_mode…`) : un appui poussé par l'appareil (trame `STATUS`) est une occurrence (`Up::Pressed` → `pulse_state`), diffusée même quand elle répète le dernier appui ; ce qui répond à la requête d'état de la connexion n'est qu'un état, même en `STATUS` (une passerelle répond ainsi pour ses sous-appareils) : tout ce qui arrive pendant `REPLY` (5 s), puis le premier message de chaque `cid`, au plus `QUIET_CAP` (30 s) après la connexion (`Answers` dans `src/device.rs`) ; une reconnexion ne rejoue rien, et un interrupteur qui ne répond jamais redevient actif après le plafond. Le cloud ne rapporte jamais un appui (`readings`). **Repli** (`Fallback` dans `src/lib.rs`) : avec les clés cloud, ces sous-appareils sont aussi suivis par le cloud, sur un canal à part, écouté seulement quand le lien local de la passerelle est coupé (jamais établi ou perdu) alors qu'elle parle encore au cloud ; une passerelle débranchée coupe aussi le cloud : états du cloud appliqués, disponibilité = la dernière vue par le cloud ; le lien rétabli, la passerelle reprend la main. Des clés données plus tard redémarrent le pilote pour eux aussi.
- **Cloud** : clés dans `/run/secrets/tuya-cloud` (Access ID puis Access Secret, une ligne chacun ; écrit par le lanceur du conteneur depuis un gestionnaire de secrets ; vide = pas de cloud), **ou** données par une personne dans Système → « Cloud Tuya » (code demandé, `PUT /api/integrations/tuya-cloud`), rangées chiffrées dans le coffre de Moli (`tuya/cloud_access_id`, `tuya/cloud_access_secret`), jamais renvoyées ; sans clés au démarrage, le pilote regarde toutes les 20 s et redémarre quand elles arrivent. Mise en place conseillée : un projet cloud Tuya, les appareils liés **un par un** en lecture (capteurs sur pile, sous-appareils des passerelles, passerelles), les prises restant locales (quota de messages) ; règle de messages de production : `devicePropertyMessage`, `deviceOnline`, `deviceOffline`, chiffrement AES-GCM. Région `cloud_region` (`eu` = Central Europe, défaut ; `eu-west`, `us`, `us-east`, `in`, `cn`). Quand le cloud a des appareils à suivre, une région inconnue est dite sur l'intégration (en attente, avec les régions connues) : les appareils locaux continuent, ceux que seul le cloud lit passent hors ligne, et une passerelle injoignable n'a plus de repli. Un cloud qui ne démarre pas (rien à suivre, région inconnue) ne fait jamais redémarrer le pilote : son canal ne se ferme que si sa tâche s'arrête. OpenAPI signée (HMAC-SHA256 : `client_id` + jeton + `t` + `nonce` + méthode, empreinte du corps, chemin) : jeton renouvelé 5 min avant ses 2 h, états `iot-03/devices/status` (20 par appel) et présence au démarrage puis **toutes les 30 min**. En direct : **service de messages** (Pulsar sur websocket `wss://mqe…:8285`, mot de passe `md5(id + md5(secret))[8..24]`, chaque message acquitté), contenu chiffré avec `secret[8..24]` (AES-128-ECB, ou AES-128-GCM quand la propriété `em` vaut `aes_gcm`), messages d'état (`devId` + `status`) et métier (`bizCode` `online` / `offline` / propriétés). Quota de l'essai gratuit : ~1 500 appels par mois sur 26 000. Commandes refusées (« lecture seule : vu par le cloud »).
- **Protocole** : 100 % local après import. TCP **6668** par appareil, connexion gardée ouverte. Versions **3.3** (trames `0x55AA`, CRC32, AES-128-ECB), **3.4** (HMAC-SHA256, clé de session négociée), **3.5** (trames `0x6699`, AES-128-GCM, même négociation). « 3.2 » est traité comme 3.3 ; **3.1 n'est pas géré**.
- **Configuration** :

```toml
[[driver]]
id = "tuya"
kind = "tuya"
[driver.options]
devices_file = "/data/tuya.json"   # défaut : data/tuya.json
# discovery = true                 # défaut : écoute des diffusions UDP
```

- **Secrets** : une clé locale par appareil, `key-<device_id>` (16 caractères), rangée par l'import.
- **Import sans Home Assistant** : `moli-os tuya import --cloud [--summary]` lit le cloud Tuya avec les clés du projet cloud que Moli garde pour le repli (`src/import.rs`, `cloud::import`) : la liste des appareils liés au projet, puis le modèle de chaque appareil (ses DP). Même fichier et même fusion que l'export par HA ci-dessous.
- **Import** (une fois, puis à chaque nouvel appareil) : `tools/home-assistant/tuya-export.py` tourne **dans le conteneur Home Assistant** et lit, via le compte Tuya déjà lié à HA, la liste des appareils, leurs DP et leurs clés locales.

```sh
docker exec -i homeassistant python3 - --summary < tuya-export.py      # contrôle, sans secret
docker exec -i homeassistant python3 - < tuya-export.py \
  | docker exec -i moli-os /moli-os tuya import                        # clés jamais affichées
```

  Le script ne rafraîchit jamais le jeton de HA (cela révoquerait celui de HA) et refuse de tourner s'il expire dans moins de 15 min. `moli-os tuya import [--instance tuya] [--devices-file …]` refuse un terminal, range les clés dans le coffre et écrit `tuya.json` **sans clé**. Les champs `ip`, `version` et `room` modifiés à la main sont conservés. Redémarrer ensuite.
- **`tuya.json`** : `id`, `name`, `category`, `product`, `ip`, `version` (`"3.3"`, `"3.4"`, `"3.5"`), `room`, `gateway`, `dps[]` (`id`, `code`, `type`, `writable`, `unit`, `scale`, `min`, `max`, `step`, `range`).
- **Points** : clé = code du DP (`switch_1`, `cur_power`). Booléen → binaire ; entier → numérique divisé par `10^scale` ; enum → enum ; texte → texte ; `Json`, `Raw`, `Bitmap` ignorés. Unités : `mA` affiché en A, `℃` en °C, `度` en kWh. Enum dont l'appareil parle ses propres mots (prise Woox R6128 : `relay_status` vaut `off` / `on` en local, `power_off` / `power_on` / `last` au cloud) : affiché avec le mot du cloud, et une fois ce dialecte vu, l'ordre part avec le mot de l'appareil (`on`, `off` ; `last` part tel quel, son mot local n'ayant jamais été vu), jusqu'à ce que l'appareil reparle les mots du cloud. Écrivable si `writable` **et** type booléen, entier ou enum (jamais le texte libre). À l'envoi, un entier est arrondi au pas de l'appareil (si `step` > 1).
- **Découverte** : écoute UDP **6666, 6667** (jusqu'à 3.3) et **7000** (3.4+), sockets partagées (`SO_REUSEPORT`) avec localtuya de HA. L'adresse retenue est celle d'où vient la diffusion, sauf si elle semble venir de la machine elle-même (`127.0.0.1` : Moli dans un conteneur sur Mac, Docker Desktop ou OrbStack) : c'est alors l'`ip` annoncée par l'appareil, crue seulement si c'est une adresse de réseau privé, sinon la diffusion est ignorée ; sans cette règle, chaque annonce remplaçait l'adresse trouvée par la recherche par clé et la prise restait hors ligne. `ip` et `version` servent au premier essai ; une connexion validée et saine reste intacte, un appareil hors ligne peut suivre une nouvelle annonce. La connexion doit prouver sa clé avant de devenir disponible ou de recevoir un ordre : état `dps` déchiffré en 3.3, négociation authentifiée en 3.4/3.5, avec un délai global de 5 s après la connexion TCP. Un simple heartbeat ou un retour d'erreur ne suffit jamais. Ni une diffusion ni une recherche par clé ne peuvent descendre sous la version 3.4/3.5 du fichier ou celle prouvée depuis le démarrage.
- **Recherche par clé** (`src/seek.rs`, `seek = true` par défaut) : si les diffusions ne sont pas entendues, Moli recherche les appareils locaux hors ligne dans le /24 des adresses connues, même si une ancienne `ip` figure dans le fichier. Après 12 s laissées aux premières connexions, puis 60 s après chaque recherche, les adresses sont vérifiées avec les clés connues : état chiffré en 3.3, preuve de négociation en 3.4/3.5. Seules les adresses des connexions actuellement validées sont exclues ; deux appareils hors ligne peuvent donc retrouver leurs IP après un échange de baux DHCP. Les échecs sont retentés à la recherche suivante. Les capteurs sur pile sans IP et les enfants des passerelles ne sont jamais recherchés. Une preuve retrouvée réveille immédiatement la reconnexion, même à adresse inchangée. Les adresses apprises restent en mémoire ; le fichier d'import est conservé.
- **Pièges** : heartbeat toutes les 10 s, connexion jugée morte après 35 s de silence, reconnexion avec attente jusqu'à 5 min. Une adresse retrouvée, même identique, remet l'attente à 2 s avant le nouvel essai : un refus transitoire après la découverte est ainsi retenté rapidement. Seule une minuterie écoulée augmente l'attente. Liens et recherches réservent chaque IP avant d'ouvrir TCP, y compris pendant l'authentification : une ancienne configuration ne doit pas évincer la connexion d'un autre appareil. La réservation est libérée à la fermeture, sur erreur, délai dépassé ou annulation ; les autres IP restent indépendantes. Les prises ne publient la puissance que sur demande : DP 18/19/20 redemandés toutes les 30 s. Un ordre de plus de 5 s n'est jamais envoyé (« un radiateur allumé des heures plus tard est pire qu'un refus ») ; un ordre non acquitté en 5 s est signalé. Appareil sans clé : ignoré avec un avertissement (relancer l'import).
- **Tests** : `src/protocol.rs` vérifié **octet par octet** contre pytuya (localtuya) pour les trois versions, négociation contre un appareil simulé, trames tronquées ou altérées ; `src/model.rs` (unités, échelle, écriture) ; `src/lib.rs` (fusion d'import sans clé dans le fichier ; pilote lancé avec des clés cloud et une région de test sur la machine même, rien ne part chez Tuya : maison lue par le seul cloud, maison sans appareil, région inconnue dite, appareils du cloud hors ligne et sans redémarrage en boucle) ; appareils TCP simulés dans `src/device.rs` et `src/seek.rs` (heartbeat ou mauvaise clé refusés, délai de négociation borné, commande après preuve, file de reconnexion rejetée, échange DHCP, nouvel essai après échec, plancher protocole, connexions saines épargnées).

## `reolink` : caméras Reolink (`crates/moli-reolink`)

- **Couvre** : caméras et sonnette derrière un Home Hub (ou une caméra / un NVR seul). **Lecture seule** + images. Un canal hors ligne est gardé s'il a un nom **ou** l'identifiant d'une caméra (une caméra sur pile endormie) ; seul un emplacement vide est ignoré. Au premier inventaire, chaque canal est décrit dans le journal (`reolink channel` : en ligne, endormi, nommé, caméra), pour expliquer une caméra absente.
- **Protocole** : API HTTPS locale (443 par défaut), `POST /api.cgi?cmd=…` en lots JSON. Événements interrogés **chaque seconde** sur une connexion gardée ; liste des canaux rafraîchie toutes les 5 min.
- **Configuration** :

```toml
[[driver]]
id = "reolink"
kind = "reolink"
[driver.options]
host = "192.168.1.x"
# port = 443
```

- **Secrets** : `username`, `password` posés par l'humain (`moli-os secrets set reolink username`, puis `password`), et `cert` (empreinte) écrit par le pilote.
- **Points** (binaires, par canal, selon ce que le canal annonce) : `motion`, `person`, `vehicle`, `animal`, `package`, `doorbell`. Images JPEG via `cmd=Snap`.
- **Identité** : UID de la caméra (stable même si elle change de canal) ; à défaut `ch<N>` (numéro de canal, moins stable).
- **Pièges** : le certificat est épinglé par une poignée de main nue **avant** tout envoi d'identifiants ; c'est un **X.509 v1**, vérifié à la main par `moli-net`. Une seule session (jeton) est gardée même à travers les redémarrages du pilote : les stations limitent les sessions simultanées. Codes `-6`/`-7` : nouvelle connexion. Le jeton voyage dans l'URL : aucune erreur ne cite l'URL. Une caméra sur batterie peut mettre ~10 s à se réveiller (14 s accordées à l'image). Station injoignable : toutes les détections passent à `null` (jamais de « personne vue » périmée).
- **Tests** : `src/lib.rs` (événements lus aux deux niveaux `md`/`visitor` et `ai`, erreurs par commande, jeton masqué au débogage) ; `crates/moli-net/src/legacy.rs` sur le certificat public réel du Home Hub (`integrations/reolink/fixtures/home-hub-cert.b64`).

## `sonos` : enceintes Sonos (`crates/moli-sonos`)

- **Couvre** : un lecteur Sonos par instance.
- **Protocole** : UPnP/SOAP en HTTP local, port **1400** (`AVTransport`, `RenderingControl`). Description lue sur `/xml/device_description.xml`. Interrogation toutes les 2 s (4 appels SOAP).
- **Configuration** : `host` seul (pas de découverte SSDP).

```toml
[[driver]]
id = "sonos"
kind = "sonos"
[driver.options]
host = "192.168.1.x"
```

- **Secrets** : aucun.
- **Points** : `playing` (lecture/pause), `volume` (0-100 %), `mute` **écrivables** ; `state`, `title`, `artist` en lecture (métadonnées DIDL échappées).
- **Identité** : UDN UPnP. Nom et pièce natifs = `roomName` Sonos.
- **Annonces vocales** (`[driver.options.announce]` : `media_base`, `tts`, `voice`, `volume`) :
  un point `announce` (texte, écriture seule), et `announce_volume` (0-100 %, **écrivable**) :
  le niveau auquel une annonce est dite. Il part du `volume` de la configuration, puis garde la
  dernière valeur écrite, redémarrage compris ; l'écrire passe par la garde comme le reste. Le
  tableau de bord en fait le curseur « Pendant une annonce » de la carte de l'enceinte, montré
  seulement si le point est inscriptible. Une annonce est une commande comme une autre
  (garde et journal ; un agent ne parle pas la nuit sans accord). Le texte (500 caractères au
  plus) est dit par **Piper en local** (protocole Wyoming, `moli-net::wyoming`), le son est
  publié 5 minutes sous un nom imprévisible (`GET /api/media/<128 bits>.wav`), puis joué en
  **clip audio** par l'API locale du Sonos (WebSocket TLS sur 1443, `moli-net::ws`) : la
  musique baisse, l'annonce passe, la musique revient d'elle-même, comme le script HA
  `announce_sonos`. Le certificat du Sonos est épinglé au premier contact (secret
  `sonos/ws_cert`). Une annonce à la fois, dans l'ordre ; la commande répond tout de suite,
  un échec est journalisé dans les logs. `speaking` (texte, lecture) tient les mots de
  l'annonce pendant que l'enceinte les dit (dès que le clip est `ACTIVE`), vide une fois dits :
  un [satellite vocal](satellite-vocal.md) qui répond par l'enceinte (`speaker`) attend la fin
  avant de se remettre à écouter.
  Tout ce qui n'est pas propre au Sonos (le texte vérifié, la file de 3 au plus, l'abandon
  après 30 s d'attente, `quiet`, `announce_volume`, `speaking`, la synthèse et la publication du son) est
  dans `crates/moli-announce`, pour toute enceinte qui parle ; le Sonos n'y ajoute que le clip
  (`moli_announce::Speaker`), comme [`airplay`](#airplay--annonces-sur-un-récepteur-airplay-1-cratesmoli-airplay) le RAOP.
- **Pièges** : description injoignable au démarrage = échec du pilote, relancé par le superviseur. Pas de gestion des groupes Sonos.
- **Tests** : lecture du SOAP et du DIDL doublement échappé.

## `airplay` : annonces sur un récepteur AirPlay 1 (`crates/moli-airplay`)

- **Couvre** : un récepteur AirPlay 1 par instance, pour des **annonces** seulement (pas de
  musique). Seulement un récepteur sans chiffrement, sans mot de passe ni appairage, lu dans son
  TXT `_raop._tcp` (`moli_net::raop::plain`, la règle de pyatv) : `et` et `cn` contiennent 0,
  pas de bit 38 ni 48 dans `ft` (AirPlay 2), ni `pw=true` ni `0x8`, `0x80` ou `0x200` dans
  `sf` (mot de passe, appairage). Ni AirPlay 2 (HomePod, Apple TV récente, Mac : eux aussi ont
  0 dans `et` et `cn`), ni AirPort Express.
  IPv4 seulement.
- **Protocole** : RAOP, dans `moli-net::raop`. RTSP sur **5000** (`ANNOUNCE` L16/44100/2,
  `SETUP`, `RECORD`, `SET_PARAMETER`/`GET_PARAMETER volume`, `TEARDOWN`), puis du PCM 16 bits
  stéréo **petit-boutiste** en RTP par paquets de 352 trames ; un **SYNC** chaque seconde sur
  le port de contrôle du récepteur, qui avance jusqu'à la fin ; une avance sur sa latence
  (2 s), bornée à 4× le temps réel ; les paquets réclamés (type 85) renvoyés ; ses questions
  d'horloge répondues. Le son de Piper est rééchantillonné à 44,1 kHz (`moli-audio`).
- **Configuration** : `host`, et `port` (5000 ; le recopier de l'enregistrement SRV
  `_raop._tcp` quand il diffère), `tts` (`127.0.0.1:10200`), `voice`, `volume` (le niveau de
  départ des annonces, 40).

```toml
[[driver]]
id = "salon"
kind = "airplay"
[driver.options]
host = "192.168.1.x"
```

- **Secrets** : aucun.
- **Points** : ceux de `moli-announce`, comme pour le Sonos (`announce`, `announce_volume`,
  `quiet`, `quiet_until`, `speaking` dès que le son part), et `volume` (0-100 %, **écrivable**) : le niveau du récepteur,
  relu dans la session de chaque annonce et remis après elle, même quand le son a échoué. 0 %
  est muet (-144 dB). Un volume relu qui n'est pas un nombre fini (`nan`, `inf`) est ignoré ;
  les autres sont bornés entre -144 et 0 dB. Un récepteur qui refuse le volume de l'annonce la
  dit quand même, à son propre volume, avec un avertissement : rien n'est remis après. Des volumes écrits en rafale : seul le dernier en attente part.
- **Pièges** : ⚠️ **toute session RTSP coupe ce que le récepteur joue**, même lire le volume :
  `volume` est lu au démarrage et dans la session de chaque annonce, puis suivi d'après ce que
  Moli écrit. Un réglage fait ailleurs (téléphone, télécommande) n'est vu qu'à l'annonce
  suivante, et le libellé du point le dit. Au démarrage, le pilote demande au récepteur son TXT
  (une question DNS-SD envoyée à son port 5353, pas à tout le réseau), celui de l'instance dont
  le SRV donne `port` (un hôte peut en annoncer deux) : un récepteur qui ne
  passe pas le critère, ou qui **refuse** la session, arrête le pilote avec une phrase qui dit
  quoi faire. Un volume que le récepteur ne veut pas donner n'est pas un refus. La présence est
  testée par une simple connexion TCP, qui ne coupe rien. Le récepteur ne dit **rien** quand il
  jette ce qu'il reçoit (SYNC absent, retard, mauvais boutisme) : seule l'oreille prouve.
- **Identité** : l'adresse `host`.
- **Tests** : une session entière contre un faux récepteur (étapes RTSP, paquets, SYNC,
  renvoi), le critère sur les TXT mesurés (Delta Player, HomePod, Apple TV, Mac), la lecture
  d'une réponse DNS (coupée ou bouclée : rien ; deux instances : celle du port configuré ; le
  TXT et le SRV en deux réponses), l'ordre des volumes d'une annonce, un volume refusé qui ne
  coupe pas l'annonce, un volume relu `nan` ou hors bornes, les
  réponses appariées par `CSeq`, le refus distingué de l'absence, le son tiré d'un WAV ; dans
  un vrai `Hub`, le volume lu au démarrage puis écrit, une rafale de volumes, et le refus au
  démarrage, par le TXT ou par la session.

## `philips` : télé Philips Android (`crates/moli-philips`)

- **Couvre** : alimentation, volume, sourdine, Ambilight, application en cours, touches de télécommande (appuyées ou **maintenues**) ; côté Android : **ouvrir une appli** et **ce qui se joue** (titre, image, lecture/pause ; avec ADB, quelle que soit l'appli).
- **Protocole** : API JointSpace en HTTPS local, port **1926**, authentification **digest** MD5 avec les identifiants d'appairage, certificat épinglé. Interrogation toutes les 5 s. Wake-on-LAN (UDP broadcast, port 9) pour allumer.
- **Configuration** :

```toml
[[driver]]
id = "tele"
kind = "philips"
[driver.options]
host = "192.168.1.x"
mac = "aa:bb:cc:dd:ee:ff"   # pour le Wake-on-LAN
# api_version = 6           # défaut
# adb = true                # ce que joue toute appli (débogage réseau de la télé allumé)
# overlay = true            # notifications par-dessus l'image (appli TvOverlay sur la télé)
```

- **Côté Android** (le cœur `moli_androidtv::tv`, réutilisable pour toute Google TV ; `src/android.rs` du pilote n'y ajoute que les touches JointSpace, appuyées ou maintenues) : Android TV Remote v2 (TLS **6466**, certificat client, messages protobuf préfixés d'un varint ; la télé parle d'abord, ping toutes les 5 s, connexion morte après 16 s de silence) et Google Cast (TLS **8009**, enveloppe protobuf autour de JSON : état du récepteur puis canal média de l'appli). Deux connexions tenues par ce cœur, que le pilote fait tourner à côté de JointSpace, reprises de 5 s à 60 s (veille profonde = ports fermés). Un ordre donné pendant une coupure est refusé, jamais rejoué.
- **ADB** (`adb = true`, `moli_androidtv::adb`) : sur une Google TV, Cast ne dit rien de ce qu'une appli ouverte sur la télé joue (vu sur une Philips le 3 nov. : 10 jours de Netflix, YouTube, Disney+ sans un titre). Les sessions média d'Android le disent toutes : `dumpsys media_session` toutes les 3 s, par ADB sur TCP **5555** (« débogage réseau » dans les options pour les développeurs de la télé). Client ADB natif (en-têtes de 24 octets, `CNXN`/`AUTH`/`OPEN`/`WRTE`/`CLSE`) ; la clé RSA 2048 de Moli est créée au premier démarrage (secret `adb_key`), le jeton de la télé signé en PKCS #1 v1.5 comme le fait `adb`. Clé inconnue : envoyée au format Android, la télé demande sur son écran (avertissement sur le pilote : cocher « Toujours autoriser ») ; sans réponse, pas de nouvelle question avant 10 min. Titre, sous-titre (chaîne, série), état, position ramenée à l'heure de la télé (`/proc/uptime`) ; ni durée ni image (`dumpsys` ne les donne pas) : celles de Cast quand Cast voit le même titre. Une position n'est republiée que si elle saute (> 3 s d'écart avec la prévue : le tableau de bord la fait avancer seul). ADB sans TLS seulement (le débogage sans fil d'Android 11, appairé par code, n'est pas pris).
- **Incrustations** (`overlay = true`, `moli_androidtv::overlay`) : l'appli **TvOverlay** (Play Store, gratuite) dessine des notifications par-dessus ce qui passe, dans un coin, quelques secondes : titre, message, image, ou **vidéo en direct** (RTSP, HLS). API REST en HTTP sur le port **5001**. Point `notify` en écriture : un texte (le message), ou un objet JSON de ses champs (`title`, `message`, `image`, `video`, `corner` = `bottom_end`…, `duration` en s, `smallIcon` = `mdi:…`) ; un champ inconnu est refusé tout de suite. Télé éteinte : refusé sans attendre. Envoi hors de la boucle du pilote. Installation : depuis le Play Store, puis par ADB `appops set com.tabdeveloper.tvoverlay SYSTEM_ALERT_WINDOW allow` (aucun menu de la télé ne le permet) et `dumpsys deviceidle whitelist +com.tabdeveloper.tvoverlay` ; son horloge à l'écran se cache par `POST /set/overlay {"clockOverlayVisibility": 0}`. Exemple vérifié le 3 nov. : la sonnette en direct, flux du go2rtc de Frigate en vidéo seule (`#media=video`, sinon son par-dessus le programme), `{"title": "Sonnette", "video": "rtsp://<nvr>:8554/sonnette", "corner": "bottom_end", "duration": 30}`.
- **Secrets** : `username`, `password` (identifiants d'appairage JointSpace, posés par l'humain), `cert` écrit par le pilote ; `atv_cert` / `atv_key` (PEM, l'appairage Android TV Remote **repris de HA**, `.storage/androidtv_remote_*.pem`, par tube) ; `atv_pin`, `cast_pin`, `adb_key` écrits par le pilote. Le pilote ne fait **pas** l'appairage lui-même (provenance des identifiants : à vérifier, probablement repris de HA).
- **Points** : `power`, `volume` (0 à max lu sur la télé, 60 par défaut), `mute`, `ambilight` **écrivables** ; `app` : l'appli au premier plan (paquet Android, dit par Android TV Remote : JointSpace ne la connaît pas sur une Google TV), **écrire** un paquet, un lien ou le nom d'une appli connue (Netflix, YouTube, Disney+, Prime Video, Jellyfin, Moonlight) l'ouvre : son lien web d'abord (une Google TV peut refuser `market://launch?id=…`, vu sur une Philips le 9 oct.), puis celui du Store (`moli_androidtv::remote::app_links`). L'ordre attend la réponse de la télé : l'appli à l'écran ou 1 s sans refus = ouverte ; tous les liens refusés = erreur « la télé a refusé d'ouvrir … », dite à qui a demandé ; `media_app`, `media_state` (`playing`, `paused`, `buffering`, `idle`), `media_title`, `media_subtitle`, `media_image` (URL), `media_duration`, `media_position` (s) en lecture (ADB d'abord quand il voit un titre, sinon Cast ; `media_app` vide pour ADB : le point `app` dit l'appli) ; `media_control` (`play`, `pause`, `stop`) en écriture seule : par Cast s'il voit ce qui joue, sinon touche média par la télécommande Android ; `key` en **écriture seule** (enum de 24 touches : curseurs, `Confirm`, `Back`, `Home`, lecture, volume, chaînes, `Source`, `WatchTV`…) ; `hold` en **écriture seule** : une flèche, `FastForward` ou `Rewind` **maintenue** (`RemoteDirection` `START_LONG` d'Android TV Remote) jusqu'à `release` (`END_LONG`), comme sur la vraie télécommande : dans YouTube, droite maintenue fait défiler la vidéo avec l'aperçu (vu le 3 nov. : 0:22 → 0:52 en 4 s). Une autre touche maintenue lâche la première ; lâchée d'office au bout de 20 s (téléphone parti) ou si la télécommande se coupe. La télécommande du tableau de bord maintient une flèche gardée 450 ms (bouton, ou glissé sur le pavé tactile sans lever le doigt).
- **Ambilight** : `ambilight` (allumé / éteint), `ambilight_mode` (énumération, les mots de Moli pour `styleName` / `menuSetting` de `ambilight/currentconfiguration` : `standard`, `natural`, `vivid`, `game`, `comfort`, `relax` suivent l'image ; `hot_lava`, `deep_water`, `fresh_nature`, `warm_white` (ISF) sont les ambiances fixes de la télé ; `music` et `party` suivent le son) et `ambilight_color` (« #rrggbb » : une seule couleur tout autour, `FOLLOW_COLOR` / `MANUAL_HUE`, teinte, saturation et luminosité de 0 à 255). Écrire un mode ou une couleur allume l'Ambilight s'il est éteint. Lus à chaque tour : un réglage que Moli ne sait pas nommer laisse `ambilight_mode` vide ; la configuration brute est écrite une fois au journal du pilote (`ambilight`) à chaque changement, pour ajouter un mot. Une scène les règle par ses `orders`.
- **Pièges** : la télé route sur l'en-tête `Host` **complet** : sans le port, elle répond 404 (d'où le `Host` avec port de `PinnedHttps`). Allumage : WoL puis 5 essais espacés de 2 s (l'API se réveille après). Extinction : `powerstate = Standby`, sinon touche `Standby` **seulement si** la télé est allumée (la touche bascule). Un appui de touche n'est jamais rejoué. En veille profonde l'API dort : appareil hors ligne et `power = false`.
- **Identité** : ⚠️ l'id natif est **l'adresse `host`** (`ctx.device_id(&config.host)`), pas un identifiant de l'appareil : un changement d'IP créerait un nouvel appareil.
- **Tests** : digest vérifié sur l'exemple de la RFC 2617.

## `frigate` : NVR Frigate (`crates/moli-frigate`)

- **Couvre** : caméras de Frigate : mouvement, objets vus, interrupteurs de Frigate, images. Et le **serveur** (appareil « Serveur Frigate », `<instance>:frigate-server`, déclaré au premier `frigate/stats`) : `running`, `version`, `uptime` (h), `inference_speed` (ms, premier détecteur), `detection_fps`, `gpu_load` et `cpu_load` (%).
- **Protocole** : sujets MQTT publiés par Frigate (`frigate/<caméra>/…`), sur le même broker que Z2M. Images par l'API HTTP interne de Frigate (`/api/<caméra>/latest.jpg?h=720`, port interne non authentifié).
- **Configuration** (tout a une valeur par défaut) :

```toml
[[driver]]
id = "frigate"
kind = "frigate"
# [driver.options]
# host = "127.0.0.1"
# port = 1883
# base_topic = "frigate"
# api = "http://127.0.0.1:5000"
```

- **Secrets** : aucun. ⚠️ Pas d'option d'identifiants MQTT (contrairement à `z2m`).
- **Points** : `motion`, `status` (texte), compteurs `person`, `car`, `dog`, `cat`, `bird`, `all` ; `detection`, `recording`, `snapshots` **écrivables** (publiés sur `…/<switch>/set` en `ON`/`OFF`).
- **Découverte** : une caméra apparaît quand elle publie `motion` ou l'état de `detect` (les zones publient aussi des comptes : elles sont ignorées). Disponibilité globale via `frigate/available`.
- **Identité** : nom de la caméra dans la config Frigate (c'est l'identifiant de Frigate).
- **Pièges** : abonnements ciblés, jamais les JPEG retenus (`…/snapshot`) ni `stats`. Broker injoignable : commandes refusées (pas mises en file), nouvel essai après 5 s.
- **Tests** : correspondance sujets → points.

## `bambu` : imprimante Bambu Lab (`crates/moli-bambu`)

- **Couvre** : A1, P1, X1. Écritures : la lumière et `control` (`pause`, `resume`, `stop`, envoyés comme ha-bambulab : `{"print": {"command": …}}`), de sémantique `control` : **un agent qui les demande attend un humain** (garde du hub). Pas encore vu en vrai.
- **Protocole** : MQTT local en **TLS 8883**, utilisateur `bblp`, mot de passe = code d'accès. Sujets `device/<série>/report` et `device/<série>/request`.
- **Filament** (`src/ams.rs`) : chaque unité AMS devient un appareil `<série>_AMS_<n>` (emplacements 1 à 4 en clair, « PLA Basic · #FF6A13 · 85 % » ou « vide », indice d'humidité, température, en cours d'utilisation) et la bobine externe `<série>_ExternalSpool` (bobine, en cours d'utilisation), déclarés au premier rapport qui les cite. Les rapports partiels sont fusionnés clé par clé (un delta de l'AMS n'efface pas le reste). Format d'après le rapport MQTT documenté que lit ha-bambulab ; **pas encore vu en vrai**.
- **Configuration** :

```toml
[[driver]]
id = "bambu"
kind = "bambu"
[driver.options]
host = "192.168.1.x"
serial = "<numéro de série>"   # affiché sur l'imprimante et dans Bambu Studio
name = "Bambu A1"              # optionnel
```

- **Secrets** : `access_code` (code affiché sur l'imprimante, posé par l'humain), `cert` écrit par le pilote.
- **Adresse qui change** (`src/find.rs`) : l'imprimante annonce elle-même où elle est, toutes les quelques secondes (`NOTIFY` diffusé sur UDP 2021, comme l'écoute Bambu Studio : `USN` = numéro de série, `Location` = adresse). Injoignable à son adresse, elle est suivie à celle qu'elle annonce **une fois son certificat épinglé vérifié là-bas** (une annonce seule ne prouve rien ; refusée, l'adresse n'est pas revérifiée avant 10 min). Le déménagement est journalisé et retenu dans `data/addresses.json` pour le prochain démarrage ; un `host` changé dans la configuration l'emporte.
- **Points** : `state`, `progress` (%), `remaining` (min), `nozzle_temperature`, `nozzle_target`, `bed_temperature`, `bed_target`, `job`, `layer`, `total_layers`, `error`, `wifi_signal` (dBm), `printing` ; `light` (lumière de chambre) **écrivable**.
- **Identité** : numéro de série.
- **Pièges** : certificat épinglé au premier contact, **avant** l'envoi du code ; imprimante souvent éteinte (prise connectée) : nouvel essai d'épinglage chaque minute, puis pause de 30 s entre reconnexions. Les rapports sont des **deltas** fusionnés dans le dernier état ; rapport complet (`pushall`) demandé toutes les 5 min. Ne jamais journaliser les `MqttOptions` (le `Debug` affiche le mot de passe).
- **Tests** : rapport fusionné → points.

## `moonraker` : imprimantes 3D Klipper (`crates/moli-moonraker`)

- **Couvre** : toute imprimante Klipper avec l'API Moonraker (vue sur un Snapmaker U1 et une Creality K2 Combo ; Voron, Prusa sous Klipper…). Paquet `integrations/klipper` ; le profil `moonraker` (lecture seule) reste pour qui n'a besoin que de l'état.
- **Protocole** : HTTP local, sans authentification (port 7125). Au premier contact : `printer/info` (nom d'hôte = identité), `printer/objects/list` (la **forme** : têtes `extruder…`, plateau, capteur d'enceinte `temperature_sensor cavity|chamber|chamber_temp`, lumière `led …`/`neopixel …`, table de filaments Snapmaker `print_task_config`, firmware Creality reconnu à sa sonde `prtouch…`), `machine/system_info` (modèle, s'il le dit ; une Creality ne le dit pas : « Creality K2 » est tiré de son nom d'hôte `K2-…`). Puis une seule requête `objects/query` toutes les **5 s en impression, 15 s sinon, 30 s éteinte** ; métadonnées du fichier à chaque nouveau travail (estimation du trancheur, vignette) ; totaux de l'historique toutes les 10 min.
- **Configuration** : `host`, `port` (7125), `name` (par défaut le modèle annoncé, sinon le nom d'hôte).
- **Points** : `state` (`standby`, `printing`, `paused`, `complete`, `cancelled`, `error` ; Klipper arrêté = `error` avec sa raison dans `message`), `printing`, `progress` (%), `remaining` (min : moyenne de l'estimation du trancheur et de l'avancée du fichier), `job`, `layer`, `total_layers`, `print_duration` (min), `filament_used` (m), `nozzle_temperature`/`nozzle_target` (tête active), `active_tool`, `tool<N>_temperature`/`_target`/`_filament` (« PLA Basic · Snapmaker », « vide »)/`_color` (`#rrggbb`), `bed_*`, `chamber_temperature`, `message`, `jobs_total`, `print_hours_total`, `filament_total` (m). **Écrivables** : `light` (`SET_LED … RED/GREEN/BLUE/WHITE`), `control` (`pause`, `resume`, `cancel` → `POST /printer/print/<ordre>`), de sémantique `control` : un agent attend un humain.
- **Creality (K2)** : son `display_status.progress` est très en retard sur l'impression (19 % pour 44 %) : pendant l'impression du fichier que `virtual_sdcard.cur_print_data` décrit (même nom de fichier que `print_stats`), l'avancement est `virtual_sdcard.progress`, celui de son écran. Moonraker n'y lit pas le trancheur (`slicer: Unknown`, ni estimation ni vignette) : l'estimation vient de `cur_print_data.metadata.estimated_time` (même condition), les couches de `virtual_sdcard.layer`/`layer_count`.
- **Image** : la vignette du travail (la plus grande que le trancheur a mise dans le G-code) sert d'image de l'appareil (`/api/devices/{id}/snapshot`).
- **Caméra d'une Creality** (`src/creality.rs`) : WebRTC seulement, aucune image fixe (`/server/webcams/list` vide, le `/webcam/` de Fluidd sans rien derrière). Le **navigateur** lit la vidéo : Moli passe son offre au service caméra de l'imprimante (`POST :8000/call/webrtc_local`, `{type, sdp}` en JSON puis base64, comme go2rtc la lit) et rapporte la réponse (`/api/devices/{id}/live`, appareil `live: true`). La caméra ne lit une adresse que si elle est écrite comme go2rtc l'écrit (`… typ host ufrag … generation 0`) : Moli réécrit les lignes `a=candidate` de l'offre avec l'adresse du navigateur qu'il voit (`LiveSource::answer(offer, viewer)`, le nom `.local` du navigateur remplacé ; TCP et IPv6 retirés). La vidéo va de l'imprimante au navigateur : au réseau de la maison seulement (Moli ouvert par son adresse locale), et le navigateur doit joindre l'imprimante (UDP compris). La carte de l'imprimante la montre pendant une impression, à la demande sinon.
- **Pas fait** : lancer une impression (le plateau doit être libre : un humain sur place), la caméra du U1 (son flux ne répond pas en HTTP local), le flux en direct par websocket (le relevé toutes les 5 s suffit).
- **Tests** : réponses réelles du U1 et de la K2 (`integrations/klipper/fixtures/u1.json`, `k2.json`) → forme, points, valeurs ; estimation du temps restant ; Klipper arrêté ; ordres → requêtes.

## `shelly` : Shelly Gen2 et suivants (`crates/moli-shelly`)

- **Couvre** : Shelly Plus, Pro, Gen3, Gen4 alimentés (sorties `switch`, compteurs `em`/`emdata`
  du Pro 3EM, `em1`/`em1data`, `pm1`, entrées, température). Pas encore : volets, lumières,
  capteurs sur pile, Gen1.
- **Protocole** : websocket local `ws://<hôte>/rpc` (JSON-RPC 2.0). Au départ
  `Shelly.GetDeviceInfo`, `GetConfig`, `GetStatus` ; ensuite l'appareil pousse ses changements
  (`NotifyStatus`, `NotifyFullStatus` ; un Pro 3EM toutes les 5 s) ; état complet relu chaque
  minute en filet. Commande : `Switch.Set` sur la même connexion.
- **Configuration** : une instance par Shelly.

```toml
[[driver]]
id = "pince"
kind = "shelly"
[driver.options]
host = "192.168.1.x"
# port = 80
# mac = "aa:bb:cc:dd:ee:ff"   # pour le retrouver même avant qu'il ait répondu une fois
```

- **Secrets** : `password` (facultatif) si l'appareil est protégé (digest SHA-256, utilisateur
  `admin`). Sans mot de passe rangé, l'instance attend avec la commande à taper.
- **Appareils** : le boîtier `<mac>` (signal, mise à jour disponible, entrées, température, et
  le compteur d'un Pro 3EM), `<mac>_switch_<n>` par sortie (`on` ✎, puissance, énergie
  consommée et restituée en kWh, alertes), `<mac>_em1_<n>` / `<mac>_pm1_<n>` par voie.
  Pro 3EM : phases câblées reconnues (tension ou énergie comptée) ; une seule = pas de points
  par phase ; puissance **signée** (positive = soutirée, négative = injectée).
- **Adresse qui change** : suivi par MAC (`moli_net::lan::relocate`), identité vérifiée par
  `Shelly.GetDeviceInfo` avant d'adopter la nouvelle adresse.
- **Tests** : `src/model.rs` sur les captures réelles `integrations/shelly/fixtures/`
  (Pro 4PM, Pro 3EM, notification) ; `src/rpc.rs` (digest).

## `tapo` : ampoules et prises TP-Link Tapo (`crates/moli-tapo`)

- **Couvre** : marche/arrêt, luminosité, température de couleur, couleur.
- **Protocole** : 100 % local, HTTP (port 80) avec le chiffrement **KLAP v2** : `handshake1` / `handshake2`, cookie `TP_SESSIONID`, AES-128-CBC + signature SHA-256. Interrogation toutes les 5 s.
- **Configuration** : `host`, `port` (80 par défaut), `mac` (facultative : pour retrouver l'ampoule si la box lui donne une autre adresse avant que Moli ait appris sa MAC).
- **Secrets** : `auth_hash` = `sha256(sha1(utilisateur) + sha1(mot de passe))` en base64, comme HA le garde ; jamais le mot de passe. `moli-os secrets set <instance> auth_hash`.
- **Points** (selon ce que l'appareil annonce), **tous écrivables** : `on`, `brightness` (1-100 %), `color_temp` (2500-6500 K), `hue` (0-360), `saturation` (%).
- **Identité** : `device_id` renvoyé par l'appareil (à défaut : l'adresse `host`). Nom = `nickname` décodé.
- **Adresse qui change** : comme les profils (`profils.md`, « Une adresse qui change », même `moli_net::lan::relocate`, même `data/addresses.json`) : injoignable (3 relevés en marche, ou au démarrage), l'ampoule est cherchée par sa MAC (retenue, sinon celle de `mac`) ; une candidate n'est prise que si elle réussit la poignée de main KLAP avec la clé du compte (`handshake1` prouve que l'appareil la connaît avant que Moli envoie quoi que ce soit) **et** donne le même `device_id`. Journal « retrouvé à une nouvelle adresse ». Une autre ampoule à une adresse apprise n'est jamais prise ; à l'adresse de la config, jamais seule quand la MAC de la sienne est connue (coupée au mur des heures, c'est normal, et toutes les ampoules du compte réussissent la poignée de main) : pour remplacer une ampoule à la même adresse, donner sa MAC dans `mac`. Une adresse ou une MAC changée par une personne est obéie. Tant qu'elle est introuvable, chaque minute : l'adresse où elle a répondu la dernière fois, puis celle de la config. En marche, un autre `device_id` à l'adresse arrête le pilote (relancé, il cherche).
- **Pièges** : ampoule coupée au mur : statut `waiting` et nouvel essai chaque minute. Session expirée : une nouvelle poignée de main, une fois. Un changement de teinte/saturation remet `color_temp` à 0 (et inversement côté appareil).
- **Tests** : session KLAP identique **octet par octet** à python-kasa ; lecture de `get_device_info`.

## `lifx` : ampoules Wi-Fi LIFX (`crates/moli-lifx`)

- **Couvre** : marche/arrêt, luminosité, et selon le modèle température de couleur et couleur. Une instance pilote **toutes** les ampoules qu'elle trouve (100 au plus).
- **Protocole** : 100 % local, protocole LAN de LIFX en UDP (port 56700), sans compte ni secret ; écrit d'après la documentation publique de LIFX. Interrogation toutes les 5 s : **un seul `LightGet` par ampoule et par relevé**, envoyé une fois (le relevé suivant tient lieu de nouvel essai), et aucun pendant un fondu. Le relevé ne fait que mettre une lecture par ampoule dans la file : chaque lecture est une pièce de travail, une ampoule muette ne retient un ordre que 500 ms.
- **Configuration** (tout est facultatif) :

```toml
[[driver]]
id = "lifx"
kind = "lifx"
[driver.options]
hosts = ["192.168.1.x"]   # adresses interrogées directement
# discovery = true        # diffusion GetService sur le réseau local
# poll_secs = 5           # 1 à 3600
# transition_ms = 0       # fondu de chaque changement, 0 à 60 000
```

  Sans `hosts` et avec `discovery = false`, la configuration est refusée.
- **Secrets** : aucun.
- **Ce que le pilote croit** : le protocole n'a aucune authentification (tout appareil du réseau peut piloter les ampoules, le pilote n'y peut rien). Une réponse ne compte que venue de l'adresse **et** du port interrogés, avec notre identifiant (aléatoire), la séquence (départ aléatoire) et le numéro de série demandés. Avec `discovery = false`, seules les réponses des adresses de `hosts` comptent à la recherche.
- **Recherche** : `GetService` aux adresses de `hosts` et en diffusion, deux fois, réponses écoutées 0,6 s à chaque fois ; puis chaque minute tant qu'une ampoule manque (aucune connue, publiée avant, adresse de `hosts` muette, ou hors ligne : une ampoule qui passe hors ligne avance la recherche), sinon toutes les 5 min. Ce qu'elle entend est mis en file et traité **une pièce à la fois, entre les ordres** (ordres d'abord, puis la recherche quand elle est due, puis le relevé, puis la file : une file jamais vide ne retient pas la recherche) :
  - **rencontre** d'un numéro nouveau : il doit répondre à un `LightGet` en 2 s (envois à 0, 0,4 et 1,2 s) ; 8 rencontres en attente au plus, **un seul numéro nouveau par adresse et par recherche** (une ampoule a un numéro), un numéro qui ne répond pas n'est pas redemandé avant 5 min ; au-delà de 100 ampoules, les suivantes sont ignorées avec un avertissement ;
  - **déménagement** : une ampoule connue entendue ailleurs n'y est adoptée que si elle **ne répond plus** à son adresse (`LightGet` avec renvois, 2 s : un datagramme perdu ne suffit pas) et répond à la nouvelle ; une ampoule qui répond ne peut pas être détournée (ligne `lifx found at a new address` dans les traces) ;
  - **complément** : micrologiciel, modèle (redemandé seulement si le micrologiciel a changé ou s'il est inconnu, et gardé tant qu'une réponse ne le remplace pas) et groupe, chacun une fois, reposés à chaque recherche tant qu'ils manquent ; l'appareil est republié une fois complet. À la rencontre, le groupe est demandé avec renvois : la garde connaît par lui une pièce protégée ;
  - **signal** Wi-Fi de chaque ampoule en ligne.
- **Identité** : numéro de série de l'ampoule (cible des trames, 12 chiffres hexadécimaux, la MAC Wi-Fi moins 1). Une autre ampoule qui répond à une adresse connue est une autre ampoule et ne remplace jamais la première.
- **Capacités** : `GetVersion` (produit) et `GetHostFirmware` (version) cherchés dans la table du pilote (`src/products.rs`, faits de la liste publique des produits LIFX : couleur, plage en kelvins et ses évolutions selon le micrologiciel). Modèle inconnu : marche et luminosité seulement, avec un avertissement. Un message que le micrologiciel dit ne pas connaître (`StateUnhandled`) n'est plus envoyé.
- **Points** : `on`, `brightness` (1-100 %), `color_temp` (K, bornes du modèle, si la plage est réglable), `color` (`#rrggbb`, modèles couleur), `rssi` (dBm, lecture seule).
- **Ordres** : avec accusé de réception (`ack_required`), renvoyés à 0, 0,4, 1,2 et 2,7 s dans 4 s au plus ; la valeur n'est publiée qu'une fois l'accusé reçu, sans accusé l'ordre échoue. Luminosité, température et couleur partent en `LightSetWaveformOptional` qui ne change **que** la composante demandée (une couleur changée dans l'appli LIFX depuis le dernier relevé n'est pas écrasée) ; un micrologiciel qui ne le connaît pas reçoit la couleur entière (`LightSetColor`). La température remet la saturation à 0 sur une ampoule couleur. Une ampoule éteinte prend la nouvelle valeur aussitôt puis s'allume en fondu, sans éclair de l'ancienne.
- **Nom et pièce** : le nom LIFX devient `native_name` (suivi s'il change dans l'appli), le groupe LIFX `native_room` (relu tant qu'il manque, sinon au redémarrage).
- **Pièges** : une ampoule LIFX manque souvent un relevé : elle ne passe hors ligne qu'après **3 relevés manqués de suite** (comme HA). Hors ligne (coupée au mur), elle n'est plus interrogée qu'un relevé sur six pour ne pas retarder les autres et les ordres. Chaque ampoule envoie parfois deux fois sa réponse à `GetService` : le double est ignoré.
- **Tests** : trames et lecture des messages (`src/proto.rs`, dont l'exemple `SetColor` de la documentation LIFX à l'octet), conversions, table des produits, ordres selon le modèle ; et le pilote entier à travers le hub face à de fausses ampoules sur `127.0.0.1`, un port chacune (`src/tests.rs`) : publication seulement après accusé, accusé venu d'ailleurs refusé, réponses d'ailleurs ignorées sans découverte, un seul `LightGet` par relevé, dix ampoules muettes sans bloquer les ordres, groupe obtenu malgré une réponse perdue, ampoule qui répond non détournée (même après un relevé perdu), modèle gardé quand le micrologiciel arrive tard, ampoule muette suivie sans qu'une autre prenne sa place, déluge de numéros sans bloquer les ordres, plafond, modèle redemandé, ampoule éteinte, micrologiciel ancien, aucun relevé pendant un fondu, et les deux captures réelles rejouées. Numéros de série factices (`02:00:00:00:00:0x`).

## `igd` : box internet (`crates/moli-igd`)

- **Couvre** : toute passerelle UPnP IGD (Livebox, Freebox…) : connexion, IP publique, durée, débit de ligne. **Lecture seule.**
- **Protocole** : UPnP local : SSDP puis SOAP (`WANIPConnection`, `WANCommonInterfaceConfig`). Interrogation toutes les 10 s.
- **Configuration** :

```toml
[[driver]]
id = "box"
kind = "igd"
[driver.options]
location = "http://<box>:<port>/<chemin>/gatedesc.xml"   # optionnel : saute la découverte
```

- **Secrets** : aucun.
- **Points** : `connected`, `external_ip`, `uptime` (s), `line`, `line_down` et `line_up` (Mbit/s). **Aucun débit instantané** : les compteurs UPnP des box sont faux (une Livebox annonce des Go/s).
- **Identité** : numéro de série de la box (`serialNumber`), à défaut son UDN, à défaut l'adresse. Une Livebox tire un nouvel UDN à chaque redémarrage : l'UDN seul coupait la box en deux. Changé le 14 oct. 2026 (avant : l'UDN) : l'appareil de la box change d'identifiant une fois à la mise à jour (étiquette, pièce et historique de la box à reprendre).
- **Découverte** : dans l'ordre, la description où la box a été trouvée la dernière fois (`data/addresses.json`, tant que `location` n'a pas changé), l'URL configurée, tout ce qui répond à `upnp:rootdevice` (3 s). Si une box est déjà connue, seule celle qui a le même numéro de série est prise pendant environ une heure, puis une autre (box remplacée) ; l'URL configurée reste la parole d'une personne. Introuvable : le pilote **écoute les annonces SSDP** (`NOTIFY ssdp:alive`, port 1900 partagé avec les autres logiciels de la machine, socket lié au groupe SSDP et gardé ouvert tant qu'elle est introuvable : rien n'est manqué entre deux essais) et recommence chaque minute ; une box qui redémarre annonce sa nouvelle description. Seules les annonces d'un appareil du réseau privé, dont la description est sur lui-même, sont retenues. En marche, après 6 échecs (1 min) : nouvelle recherche, sinon le pilote repart en écoute.
- **Retrouvée ailleurs** : une ligne au journal (« retrouvé à une nouvelle adresse », ancienne → nouvelle description) ; `location` n'est jamais réécrite.
- **Pièges** : la Livebox ne répond aux recherches SSDP que pour un autre de ses services (`WFADevice`, port 1990), jamais pour sa passerelle (port 60000, chemin tiré au redémarrage) : elle n'est connue que par ses annonces, peu fréquentes hors redémarrage. Fixer `location` accélère le premier démarrage.
- **Tests** : conversion en Mbit/s, identité (numéro de série, UDN, adresse), annonces retenues ; parsing UPnP et annonces dans `crates/moli-net/src/upnp.rs`.
## `ipp` : imprimante réseau (`crates/moli-ipp`)

- **Couvre** : toute imprimante IPP Everywhere / AirPrint (Canon, Epson, HP, Brother depuis ~2013) : état en mots, niveau de chaque cartouche, file, dernière impression ; **imprimer depuis Moli** (photo, pages d'un PDF rendues par le tableau de bord), « Faire clignoter », « Annuler ».
- **Protocole** : IPP 2.0 en HTTP sur 631 (`/ipp/print`), codé dans le crate (RFC 8010, collections ignorées entières). État toutes les 30 s, 4 s pendant une impression.
- **Configuration** : `host` ; `port` (631) et `path` (`/ipp/print`) facultatifs.
- **Secrets** : aucun.
- **Points** : `state` (`idle`/`processing`/`stopped`), `status` (texte), `ink_N` (%, `null` = inconnu), `ink_low`, `jobs`, `job` ; boutons `identify`, `cancel` (Control : un agent passe par un humain).
- **Impression** : `POST /api/devices/<id>/print` (JPEG, 16 Mo par page), tableau de bord de la maison seulement, vérifié avant de lire le corps, 40 pages / 10 s. File en mémoire bornée à 32 Mo (Moli vit dans 128 Mo), envoyée page par page par une tâche à part ; « Annuler » vide la file (époque) et annule la page en cours ; la file attend quand le papier manque ou bourre.
- **Pièges** : une jet d'encre lit le document en imprimant (envoi jusqu'à 15 min) ; une Canon dont les cartouches sont rechargées annonce 0 % ; éteinte au bouton = hors ligne (normal), sauf « Auto power on ».
- **Tests** : réponse réelle d'une Canon MG3600 (`integrations/ipp/fixtures/`), file bornée, annulation, pause sans papier.

## `host` : la machine de Moli et les ordinateurs (`crates/moli-host`)

- **Couvre** : la machine où tourne Moli (l'hôte) par `/proc` et `/sys`, sans privilège : processeur, charge, mémoire, échange, températures (paquet CPU, NVMe), réseau, durée ; et les **ordinateurs de la maison** : allumés (port, ou adresse retrouvée par la MAC dans `/proc/net/arp`), réveillés par Wake-on-LAN (`moli_net::wol`), et avec l'**agent Moli** (`agent/windows/`) tout ce qu'ils font et des ordres en retour.
- **Configuration** :

```toml
[[driver]]
id = "machines"
kind = "host"
[driver.options]
name = "serveur"
[[driver.options.machines]]
id = "pc-bureau"
name = "PC de bureau"
host = "192.168.1.x"
mac = "aa:bb:cc:dd:ee:ff"
agent_sha256 = "<sha256 du jeton de l'agent : le jeton reste sur le PC>"
```

- **Agent** (`agent/windows/`) : `POST /api/machines/<id>/report` toutes les 5 s (jeton porteur) → `{ orders, every }` ; rien n'écoute sur le PC. Ordres : `shutdown` (30 s d'avis), `restart`, `sleep`, `remote` : l'appli Claude (Remote Control) et l'appli Codex (exec-server vers Codex Cloud) ouvertes pour le téléphone, aussi à chaque ouverture de session. Le détail (processus, disques) : `GET /api/machines/<device>` ; le mode à distance : `POST /api/machines/<device>/remote`, **code PIN à chaque fois**, la machine réveillée d'abord si elle dort (après un arrêt, pas de session Windows : la mettre en veille).
- **Points** : serveur : `cpu`, `load`, `ram`, `ram_used`, `ram_total`, `swap`, `cpu_temperature`, `disk_temperature`, `net_down`, `net_up`, `uptime`, `cores`. Ordinateur : `power` (Control) et, avec l'agent, `agent`, `cpu`, `cpu_temperature`, `ram…`, `gpu_load`, `gpu_temperature`, `gpu_memory…`, `gpu_power`, `disk`, `net…`, `uptime`, `claude_app`, `codex_app` ; boutons `restart`, `sleep` (Control).
- **Disques et conteneurs de l'hôte** : un script côté hôte (non fourni ici, lancé chaque minute par cron) écrit `data/infra.json`, servi par `/api/infra`. Moli ne tient pas le socket Docker.
- **Tests** : `/proc` et `hwmon` sur fichiers écrits par les tests, table ARP, jeton et ordres de l'agent (`agents.rs`).

## `helpers` : aides de la maison (`crates/moli-helpers`)

- **Couvre** : les valeurs que la maison garde pour elle, comme les `input_select` /
  `input_boolean` / `input_number` / `input_text` de HA : mode de la maison, alarme active,
  budget… Chaque aide est un appareil `<instance>:<id>` avec un point inscriptible :
  `state` (choix, `select`), `on` (interrupteur, `switch`), `value` (`number`, `text`).
- **Configuration** : `[[driver.options.helper]]` avec `id` (jamais renommé), `name`,
  `kind`, `options` (choix), `min`/`max`/`step` (nombre), `initial`, `room` (facultatif).
- Une commande devient la nouvelle valeur (garde et journal comme pour tout appareil) ; la
  valeur survit aux redémarrages (cache d'état) ; une aide jamais vue démarre à `initial`.
- Exemple : une instance `maison` avec `mode_maison` (Normal, Absent, Nuit, Vacances),
  `alarme_active`, `budget_elec_mensuel`, aux mêmes noms et valeurs que les `input_*` de HA
  qu'elles remplacent.

## `lumieres` : luminaires (`crates/moli-lights`, D12)

Pilote interne, lancé par Moli dès que `moli.toml` déclare un `[[fixture]]` (pas de
`[[driver]]`, l'id `lumieres` est réservé). Un luminaire réunit des ampoules (`bulbs`, ids
d'appareils) et/ou le relais qui les alimente (`power`, point `appareil/clé`) en un appareil
`lumieres:<id>` : `on` ✎, `powered` (sous tension : relais allumé et au moins une ampoule qui
répond), `brightness` ✎ (%), `color_temp` ✎ (mireds, ce que toutes les ampoules acceptent),
`color` ✎. Ses `members` = ampoules + relais : la garde juge un ordre au luminaire sur leurs
pièces ; il commande ensuite les membres en origine `system` (journal : `luminaire « Nom »`).

- **Lecture** : chaque événement d'un membre recalcule l'état (une ampoule injoignable n'est
  jamais allumée ; le luminaire est allumé si une ampoule joignable l'est et que le relais
  n'est pas coupé). Membres de toute marque, par sens (`OnOff`, `Brightness` en % ou sur 254,
  `ColorTemp`, `Color` texte ; « ON »/« OFF » ou booléen).
- **Ordres** (fonction pure `plan`, testée) : éteindre → les ampoules joignables, le relais
  seulement si aucune ne répond ; allumer ou régler → le relais d'abord s'il est coupé
  (réponse dès qu'il a obéi, puis les ampoules réglées en tâche de fond quand elles
  reviennent, 15 s au plus), sinon les ampoules (allumées puis réglées) ; aucune ampoule
  joignable sans relais → refus « coupée à l'interrupteur ».

## `dehors` : la météo dehors, mesurée ou prévue (`crates/moli-outdoor`, #210)

Pilote interne, lancé par Moli dès que `moli.toml` déclare `[outdoor]` (pas de `[[driver]]`,
l'id `dehors` est réservé). Sans cette section, rien ne change : la page Dehors lit
`outdoor.weather` de `home.json` comme avant.

```toml
[outdoor]
forecast = ["meteo:api.open-meteo.com:443", "air:air-quality-api.open-meteo.com:443"]  # un id ou une liste
forecast_stale_after = "1h"   # facultatif : 30 min par défaut

[[outdoor.station]]           # les stations, par ordre de préférence
device = "z2m:0x…"
stale_after = "15m"           # facultatif : 30 min par défaut

[[outdoor.station]]
device = "infoclimat:…"       # une station lente écrit sa propre durée
stale_after = "2h"
```

Il publie **un** appareil, `dehors:meteo`, en lecture seule (il n'accepte aucun ordre et n'en
transmet aucun). Ses `members` sont les sources.

- **Les sens** : `temperature`, `humidity`, `pressure`, `illuminance`, `wind_speed`,
  `wind_gust`, `wind_direction`, `rain_rate`, `rain_today`, `uv_index`, chacun sous la clé de
  son nom, s'il a au moins une source. Dans une source, le point retenu pour un sens est celui
  qui porte ce sens **et** le nom du sens (`temperature`), sinon le seul qui porte ce sens
  (`wind_gusts` d'Open-Meteo pour `wind_gust`). Plusieurs sans le bon nom (une température,
  une ressentie, un point de rosée) : la source ne donne pas ce sens, et le journal le dit.
- **Une unité par sens** : celle du sens (`Semantic::fixed_unit`), sinon celle de la première
  source qui le donne. Rien n'est converti : une source dans une autre unité est écartée pour
  ce sens, et le démarrage le dit. Les pilotes convertissent chez eux.
- **Le choix**, sens par sens : la mesure **fraîche** de la première station qui l'a, sinon
  la prévision fraîche, sinon la dernière valeur connue, quelle que soit sa source. Il y a donc
  une valeur dès qu'une source a parlé une fois. Frais = relevé depuis moins de `stale_after`.
- **L'âge** : la valeur garde l'heure du relevé de sa source, pas celle de la recopie ; la règle
  `stale` des automatismes marche telle quelle. Quand une source relit la même valeur, l'heure
  avance aussi (dans la minute).
- **La source** : `<clé>_source` (texte, lecture seule, sens `config`) porte l'identifiant de
  l'appareil source. `stations` et `forecast` (texte, sens `config`) listent les sources
  (ids séparés par des virgules) : la page Dehors s'en sert pour « mesuré » ou « prévu ».
- **Joignable** tant qu'une de ses sources l'est.
- **Rien à relire en boucle** : le pilote suit les changements de ses sources ; une horloge
  par minute fait seulement passer une source de fraîche à ancienne.

**Le repli vers la prévision est un choix, pas une sécurité.** Une prévision peut annoncer
20 km/h pendant une rafale à 70. Un automatisme qui protège quelque chose exige la mesure :

```json
{ "type": "if", "all": true, "rules": [
  { "kind": "state", "point": "dehors:meteo/wind_speed", "op": "gt", "value": 40 },
  { "kind": "state", "point": "dehors:meteo/wind_speed_source", "op": "eq", "value": "z2m:0x…" }
] }
```

Sans la seconde règle, la prévision suffit à relever le store ; avec, seule la station le fait.

## `presence` : qui est à la maison (`crates/moli-presence`)

- **Couvre** : la présence de chaque personne d'après le Wi-Fi de son téléphone, en local
  (sans appli ni cloud), comme les trackers `ping`/`nmap` de HA.
- **Protocole** : toutes les `every` s (60), un datagramme UDP vers chaque adresse du réseau
  (`network`, /24 à /30), puis lecture de `/proc/net/arp` (table des voisins de l'hôte :
  Moli est en réseau `host`) ; présent dès qu'une adresse Wi-Fi de la personne répond, absent
  après `away_after` min (10) sans signe. Rien n'est déclaré absent pendant ce délai au
  démarrage.
- **Configuration** : `[[driver.options.person]]` avec `id`, `name`, `macs` (adresses Wi-Fi
  des téléphones ; iPhone : adresse privée « Fixe » pour le réseau de la maison).
- **Points** : `home` (binaire, sémantique occupation), lecture seule.
- **Machines** : `[[driver.options.machine]]` avec `id`, `name`, `ip` (sur le réseau déclaré) : appareil « Machine » (`description` = son adresse), point `online` (allumée tant qu'elle répond, éteinte après deux relevés sans réponse). C'est l'équivalent de l'intégration `ping` de HA pour un ordinateur.

## `esphome` : satellites vocaux et appareils ESPHome (`crates/moli-esphome`)

- **Couvre** : les appareils ESPHome. Les satellites vocaux (Home Assistant Voice Preview
  Edition, ou un ESP32-S3 monté à la main) : le mot de réveil, la conversation, la voix. Les
  autres (prise mesurée, lecteur de compteur P1…) : leurs capteurs, capteurs binaires, capteurs
  texte et interrupteurs.
- **Protocole** : l'API native d'ESPHome, TCP **6053**, chiffrée par **Noise** — écrite depuis la
  spécification, **sans bibliothèque ESPHome** (`noise.rs`, `frame.rs`, `api.rs` ; identifiants et
  numéros de champs de `api.proto`, ESPHome 2026.3, version d'API 1.14). Une connexion tenue
  ouverte, ping toutes les 20 s, silence de 90 s = connexion morte ; l'appareil redémarre après
  15 min sans aucun client.
- **Configuration** : `host`, et **`media_base` sans laquelle il n'y a pas de voix** : l'appareil
  va chercher lui-même le WAV que Moli publie. Le détail (firmware compris) est dans
  [satellite-vocal.md](satellite-vocal.md).
- **Secrets** : `api_key` (base64 : la clé `api: encryption:` du firmware). Un appareil **sans**
  clé en reçoit une de Moli — si `mac` correspond, et seulement s'il n'en tient aucune déjà.
- **Points** : un satellite (l'appareil annonce une voix ou un `media_player`) : `state`
  (`idle`, `listening`, `thinking`, `speaking`), `volume` et `muted` (le `media_player` de
  l'appareil), `wake_word`, `say` (texte, 500 caractères). Tout autre appareil : un point par
  entité, de clé l'`object_id` de l'entité (sinon son nom), réduit en clé : minuscules, accents
  repliés, tout autre caractère (`-`, espace, `__`…) devient un seul `_`. Deux types qui
  partagent une clé : le premier de capteur, capteur binaire, capteur texte, interrupteur la
  garde, les autres prennent leur type en suffixe (`<clé>_switch`), quel que soit l'ordre de la
  liste. Les interrupteurs s'écrivent (`OnOff`), le reste se lit. Les réglages de l'appareil
  (catégorie `config` : `restart`, `safe_mode`, `factory_reset`…), ses diagnostics (catégorie
  `diagnostic` : Wi-Fi, uptime, adresse…) et les entités cachées par défaut sont laissés de
  côté ; au-delà de 500 entités, la connexion est refusée. Une valeur arrive telle quelle (les
  décimales annoncées ne sont qu'une indication d'affichage), sauf un capteur en `kW`, qui
  devient des `W` arrondis aux décimales annoncées moins trois ; une valeur absente
  (`missing_state`, `NaN`) n'est pas publiée.
- **Identité** : la MAC de l'appareil (`ctx.device_id(&hello.mac)`).
- **Pièges** : ⚠️ l'assistant vocal d'ESPHome n'accepte **qu'un client**, et Home Assistant s'y
  abonne dès qu'il gère l'appareil : si les deux sont branchés, **HA gagne et Moli se tait sans
  le dire**. Un seul `media_player` (le pilote retient le dernier annoncé). Les états, eux, vont à
  chaque client : une prise ou un P1 se lit dans Moli et dans Home Assistant à la fois, ce qui
  permet une migration progressive.
- **Tests** : `src/lib.rs` (un appareil qui parle en clair ne remplace jamais la clé que Moli
  tient ; provisionnement borné par la MAC ; un point par entité, kW → W, diagnostics écartés),
  `src/api.rs` (numéros de champs des listes et des états).

## `solarman` : onduleurs derrière un enregistreur Solarman (`crates/moli-solarman`)

- **Couvre** : les micro-onduleurs Deye (`model = "deye"`, par défaut) et les onduleurs string
  Solis S6-GR1P (`model = "solis"`) derrière leur enregistreur Wi-Fi. **Lecture seule** : seules
  les fonctions Modbus de lecture 0x03 (registres de maintien, Deye) et 0x04 (registres d'entrée,
  Solis) sont envoyées.
- **Transport** : TCP 8899, une connexion par lecture. `transport = "v5"` (par défaut) : trame
  Modbus RTU dans l'enveloppe V5 de l'enregistreur, numéro de série obligatoire.
  `transport = "modbus_tcp"` : Modbus TCP nu (en-tête MBAP), pour les clés qui ne parlent pas V5
  (une clé Solis en est) ; ces clés ne servent qu'un client à la fois.
- **Configuration** : `host`, `serial` (V5), `slave` (1), `model`, `transport`, `timezone`.
- **Registres** : cartes de ha-solarman (MIT, voir `NOTICE`) : `deye_micro.yaml` et
  `solis_s6-gr1p.yaml`. Solis : registres 3004 à 3043 en deux blocs.
- **Compteurs** : la production totale (`energy`) ne recule jamais, et un compteur qui saute plus
  loin que ce que l'onduleur a pu produire depuis la lecture précédente est écarté. Solis : le
  total à vie de l'onduleur est en kWh entiers ; Moli le complète au dixième avec le compteur du
  jour. Une lecture tout à zéro ne publie pas de total. Les valeurs hors de ce que l'onduleur
  peut lire (puissance, tensions, courants, fréquence, température) sont écartées.
- **La nuit** : l'onduleur dort et l'enregistreur avec lui : l'appareil passe hors ligne sans
  avertissement.
- **Tests** : `src/v5.rs` (trames V5 et Modbus TCP, trame courte refusée), `src/deye.rs`,
  `src/solis.rs` (valeurs comparées à Home Assistant au même instant, compteurs).

## `kasa` : prises TP-Link Kasa, protocole local historique (`crates/moli-kasa`)

- **Couvre** : les prises Kasa qui parlent encore le protocole local d'origine (HS100, HS110…),
  d'abord une HS110 matériel 1.0 (prise avec mesure). Les firmwares récents en KLAP (port 80)
  relèvent de `tapo`.
- **Protocole** : JSON sur TCP **9999**, chaque message chiffré par XOR à clé glissante (171),
  précédé de sa longueur (4 octets gros-boutiens) ; une connexion par requête. Aucun compte.
- **Configuration** : `host`, `port` (9999), `poll_seconds` (10, de 2 à 3600).
- **Points** : `on` (relais, la même clé que `tapo`, `shelly` et `melcloud`) et `led` (voyant)
  écrits par `set_relay_state` / `set_led_off` (le code d'erreur de la prise est vérifié) ; avec
  mesure (`feature` contient `ENE`) : `power` W, `voltage` V, `current` A, `energy` kWh (compteur
  à vie). Matériel 1 en W/V/A/kWh (essayé sur une HS110 1.0) ; matériel 2 en mW/mV/mA/Wh, lu
  d'après sa description, **non essayé**. Une mesure qui ne répond pas ne rend pas la prise
  indisponible.
- **Identité** : la MAC de la prise, réduite à ses 12 chiffres hexadécimaux (toute autre forme
  est refusée).
- **Tests** : `src/lib.rs` (chiffrement XOR aller-retour, `get_sysinfo`, les deux générations de
  mesure, période refusée), fixtures anonymisées de `integrations/kasa/fixtures/`.
- **Vérifié sur du vrai matériel** : HS110(EU) matériel 1.0, firmware 1.2.6 : lecture (relais,
  voyant, puissance, tension, courant, énergie), voyant allumé puis éteint, relais allumé (la
  charge mesurée à 2,2 kW) puis éteint, depuis Moli. Matériel 2 : pas encore.

## `recycleapp` : collectes de déchets (`crates/moli-recycleapp`)

- **Couvre** : le calendrier des collectes d'une adresse belge, d'après RecycleApp (Fost Plus).
- **Protocole** : l'API web publique de recycleapp.be (`api.fostplus.be/recyclecms/public/v1`,
  adresse lue dans `www.recycleapp.be/config/app.settings.json`, en-tête `x-consumer:
  recycleapp.be`), sans compte. Code postal → `zipcodes?q=`, rue → `POST streets?q=&zipcodes=`,
  puis `collections?zipcodeId=&streetId=&houseNumber=&fromDate=&untilDate=` (8 semaines).
- **Configuration** : `zipcode` (4 chiffres), `street`, `house_number` ; `language` (`fr`),
  `timezone` (`Europe/Brussels`).
- **Points** : par type connu (identifiant du logo de la fraction), `<type>` = prochaine date
  (`AAAA-MM-JJ`) et `<type>_days` = jours d'ici là ; `today`, `tomorrow` = ce qui est collecté
  (noms, ou vide). Une collecte remplacée (`exception.replacedBy`) est écartée.
- **Rythme** : calendrier toutes les 6 h, jours recomptés toutes les 15 min.
- **Tests** : `src/model.rs` (adresse, prochaine collecte par type, collecte remplacée, type
  inconnu, nom manquant dans la langue), `src/lib.rs` (adresse vérifiée, requêtes encodées).

## `engie` : compte ENGIE Belgique (`crates/moli-engie`)

- **Couvre** : un compte client ENGIE Belgique : prix du contrat, pics du tarif capacitaire,
  véhicules de la recharge intelligente ENGIE.
- **Protocole** : l'API de l'appli ENGIE Smart (`api.engie.be`, HTTPS, jeton Bearer), non
  officielle. Connexion OAuth 2 + PKCE sur `account.engie.be` : la personne se connecte sur la vraie
  page (mot de passe et code SMS/e-mail chez ENGIE) et colle l'adresse de fin
  (`be.engie.smart://login-callback/…?code=…`) dans la carte du tableau de bord (point `sign_in`).
  Le jeton de rafraîchissement **tourne** à chaque usage : le nouveau est rangé aussitôt.
- **Configuration** : rien d'obligatoire ; `language` (page de connexion, `fr`), `timezone`
  (`Europe/Brussels` : jours et mois des pics, heures de départ lues et écrites dans ce fuseau), `vehicles` (les véhicules suivis,
  par nom ou numéro ; vide : tous ; les autres ne sont jamais lus et leur appareil est retiré).
- **Secrets** : `refresh_token`, posé par la connexion, jamais tapé.
- **Points** : compte : `status`, `sign_in_url`, `sign_in`, `reward_balance` (solde de
  récompenses disponible), `reward_payout_threshold`. Contrat : un prix par énergie, sens et
  plage de la période en cours (`electricity_offtake_peak`… €/kWh TVA comprise ; la plage mêlée
  `EN` est écartée, comme dans Home Assistant), `peak_month_kw`, `peak_month_at`,
  `peak_today_kw`, `peak_previous_month_kw`. Véhicule : `soc`, `range_km`, `charge_power_kw`,
  `charge_status`, `plugged`, `reachable`, `departure`, `target_level`, `direct` (les trois
  derniers s'écrivent), `session_kwh`, `session_cost`, `session_reward`, `session_outcome`,
  `reward_month`, `reward_total`. Unités données telles quelles (`€/kWh`, `kW`, `€`) : jamais
  confondues avec la puissance ou l'énergie de la maison.
- **Rythme** : véhicules toutes les 5 min ; prix, pics et gains du mois toutes les heures ;
  gains des mois d'avant une fois par jour. En panne, un essai après 1 min, puis deux fois plus
  tard à chaque fois, jusqu'à 30 min. Le pilote attend une personne (`wait_for`) tant que le
  compte n'est pas connecté. Un 403 laisse de côté la seule ressource refusée (dit une fois) ;
  seul un 401 après renouvellement redemande une connexion.
- **Pièges** : ⚠️ une connexion avec le même compte dans l'appli ou sur engie.be révoque la
  session de Moli (`status = sign_in` à nouveau). Une écriture renvoie les trois réglages de la
  session (départ, objectif, `direct`) : ENGIE les veut ensemble. Départ à 7 jours au plus.
- **Tests** : `src/auth.rs` (page PKCE, adresse collée vérifiée par son `state`, réponses du jeton),
  `src/model.rs` (fixtures fictives de `integrations/engie/fixtures/`), `src/lib.rs` (heures de
  départ, jours et mois dans le fuseau du compte).

## `vicare` : pompes à chaleur et chaudières Viessmann (`crates/moli-vicare`)

- **Couvre** : un appareil Viessmann relié par sa passerelle Wi-Fi (Vitoconnect) : la PAC
  (températures, compresseur, COP, eau chaude) et un appareil par circuit de chauffage (mode,
  programme, consignes, pompe). Une page **Chauffage** apparaît dans le tableau de bord dès qu'un
  tel appareil existe ; ces appareils n'ont **pas** de carte dans une pièce, ils vivent sur cette page.
- **Protocole** : le cloud Viessmann (`api.` et `iam.viessmann-climatesolutions.com`, deux hôtes
  écrits en dur, HTTPS seulement). L'API est celle que Viessmann documente ; la connexion **sans
  navigateur** (e-mail et mot de passe en Basic sur `/authorize`, puis code PKCE) est celle de
  l'appli ViCare et **n'est pas le parcours documenté** par Viessmann : elle peut changer sans
  préavis. Pas de double authentification, pas de compte Google ou Apple.
- **Configuration** : `installation` (numéro, seulement s'il y en a plusieurs), `redirect_uri`
  (seulement pour une clé d'API qui en déclare une autre).
- **Secrets** : `client_id` (clé d'API du portail développeur, client public), `username`,
  `password`. Voir `integrations/vicare/onboarding.md`.
- **Rythme** : une lecture toutes les 2 min 30 (le cloud limite à environ 1 450 appels par jour et
  120 par 10 minutes). Un refus pour quota, en lecture comme sur un ordre, suspend les appels
  jusqu'à l'heure de reprise donnée par le cloud.
- **Identifiants refusés** : le pilote attend une personne (`wait_for`) et **ne réessaie pas** :
  corriger le secret puis redémarrer Moli (retenter un mauvais mot de passe ferait bloquer le
  compte). Une coupure réseau, elle, ne refait jamais une connexion par mot de passe : le jeton
  de rafraîchissement est gardé pour l'essai suivant.
- **Commandes** : mode du circuit, consignes confort / normale / réduite, consignes d'eau chaude,
  charge unique. Les bornes viennent des commandes du cloud ; sans bornes, le point reste en
  lecture seule. Un mode, une phase ou un programme que Moli ne connaît pas s'affiche avec son
  identifiant brut.
- **Tests** : `src/model.rs` (fixtures anonymisées de `integrations/vicare/fixtures/`), `src/lib.rs`
  (PKCE, attente du quota).

## `profile` : profils déclaratifs (`crates/moli-profile`)

Un seul pilote générique pour tout appareil HTTP décrit en TOML. Voir [profils.md](profils.md).

## Tests : où et comment

- Tout passe par `.\scripts\dev.ps1 sh scripts/check.sh` (fmt, clippy `-D warnings`, `cargo test --workspace`, dans Docker ; voir [exploitation.md](exploitation.md)). Une crate seule : `.\scripts\dev.ps1 cargo test -p moli-tuya`.
- Les tests de pilotes sont des tests unitaires purs (sans réseau), souvent sur des captures réelles : fixtures Z2M et Hue, réponses Reolink, Meross, Daikin, Moonraker.
- `crates/moli-net` : épinglage (premier certificat adopté puis exigé, empreinte `AB:CD…` normalisée ; le vérificateur ne fait que hacher les octets, aucun vrai certificat nécessaire) et corps bornés.
- `crates/moli-runtime/tests/hub.rs` couvre le contrat côté cœur : aller-retour de commande, refus, clés inconnues ignorées, pilote qui panique relancé, appareils étrangers refusés.
- `crates/moli-os/src/config.rs` vérifie que `moli.example.toml` est valide.
- Sur du vrai matériel : propriétés inoffensives, jamais dans une pièce protégée, état initial remis (voir `AGENTS.md`).

## Outil annexe : pièces depuis Home Assistant

`tools/home-assistant/ha-areas-import.py` lit (en lecture seule) les registres de pièces et
d'appareils de HA, retrouve chaque appareil Moli par identité (identifiants Z2M, Hue,
Tuya…, ou MAC), et propose `room = <pièce HA>` pour les appareils **sans pièce** (ni étiquette,
ni pièce native). Il ne déplace jamais rien. Sans argument : plan à blanc ; `--apply` :
écrit les étiquettes via `PUT /api/labels/<id>` (origine `cli`) sur `127.0.0.1:8790`.

## Tableau récapitulatif

« Vérifié » = le pilote a tourné contre ce modèle réel (au moins en lecture) ; « pas encore »
= écrit d'après le protocole documenté, jamais vu répondre.

| Pilote | Crate | Protocole | Écriture possible | Vérifié sur du vrai matériel |
|---|---|---|---|---|
| `z2m` | `moli-z2m` | MQTT local (Zigbee2MQTT) | oui, selon les `exposes` | Zigbee2MQTT (appareils variés) |
| `zigbee` | `moli-zigbee` | EZSP sur ASH (série ou TCP), clé Silicon Labs | oui : `permit_join` du coordinateur (sémantique `control` : accord humain pour un agent), `state`, `brightness`, `color_temp`, `color`, thermostat | ZBDongle-E (EmberZNet 7.4.4.0) + Innr RB 287 C |
| `zwave` | `moli-zwave` | MQTT local (zwave-js-ui) | on/off, niveau | zwave-js-ui (capture réelle anonymisée + on/off sur nœud neutre) |
| `hue` | `moli-hue` | HTTPS CLIP v2 + SSE, local, épinglé | oui : lumières, pièces ; zones non | pont Philips Hue (CLIP v2) |
| `melcloud` | `moli-melcloud` | HTTPS vers le MELCloud classique, clé de contexte | oui : marche, mode, consigne, ventilateur, ailettes | deux unités air-air (lecture) ; commandes sur l'une |
| `overkiz` | `moli-overkiz` | HTTPS local 8443, certificat épinglé | selon capacités ; portails/fenêtres explicitement revus et gardés | modèle rejoué hors réseau ; transport et commandes sur simulateur TLS |
| `voltalis` | `moli-voltalis` | HTTPS vers le service, compte puis jeton de session | oui : marche, mode, consigne, programme | quatre radiateurs `VX_WIRE` (lecture) ; commandes sur l'un |
| `dirigera` | `moli-dirigera` | HTTPS 8443 + WebSocket, local, épinglé (API non officielle) | oui : lampes, prises, stores | hub Dirigera (firmware 2.975) : lampes TRADFRI et ORMANAS (commandes comprises), détecteur MYGGSPRAY (lecture), redémarrage du hub ; prises, boutons, stores, autres capteurs : pas encore |
| `enedis` | `moli-enedis` | HTTPS vers la Conso API, jeton du client | non | un compteur Linky (lecture, pas de 15 minutes) |
| `tuya` | `moli-tuya` | TCP 6668 chiffré 3.3/3.4/3.5, UDP 6666/6667/7000 ; cloud en lecture | oui : DP booléens, entiers, enum (local seulement) | prises Tuya (local, écriture comprise) ; capteurs sur pile et passerelles (cloud) |
| `reolink` | `moli-reolink` | HTTPS API locale, épinglé (X.509 v1) | non (+ images) | Reolink Home Hub (caméras, sonnette) |
| `sonos` | `moli-sonos` | UPnP/SOAP HTTP 1400 ; annonces en WebSocket TLS 1443 | oui : lecture, volume, sourdine, annonces | Sonos Beam |
| `airplay` | `moli-airplay` | RAOP : RTSP 5000, RTP/UDP | oui : annonces, volume | Freebox Delta Player |
| `philips` | `moli-philips` | JointSpace HTTPS 1926, digest, WoL ; Android TV Remote, Cast | oui : alimentation, volume, sourdine, Ambilight, touches, appli | Philips Google TV (lecture) |
| `frigate` | `moli-frigate` | MQTT + HTTP interne 5000 | oui : détection, enregistrement, instantanés | Frigate |
| `bambu` | `moli-bambu` | MQTT TLS 8883, épinglé | lumière, `control` (humain pour un agent) | pas encore (cible : Bambu Lab A1) |
| `moonraker` | `moli-moonraker` | HTTP local 7125 | lumière, `control` (humain pour un agent) | Snapmaker U1, Creality K2 Combo |
| `tapo` | `moli-tapo` | HTTP 80 + KLAP v2 | oui : tout | pas encore (cible : Tapo L530) |
| `lifx` | `moli-lifx` | UDP 56700 (protocole LAN), diffusion | oui : marche, luminosité, température, couleur (selon le modèle) | LIFX White et Mini W (micrologiciel 3.90) : lecture et ordres ; couleur et température non testées |
| `shelly` | `moli-shelly` | WebSocket local 80 (JSON-RPC Gen2), notifications poussées, digest SHA-256 | oui : sorties (`on`) | Pro 4PM, Pro 3EM (une phase câblée) : lecture identique à HA au Wh près ; ordre sur une sortie : pas encore |
| `igd` | `moli-igd` | UPnP IGD (SSDP + SOAP) | non | Livebox |
| `ipp` | `moli-ipp` | IPP 2.0 HTTP 631 | identifier, annuler, imprimer (tableau de bord) | Canon MG3600 |
| `host` | `moli-host` | /proc, /sys ; WoL ; agent HTTP sortant | allumer, éteindre, veille, Claude et Codex pour le téléphone (PIN) | hôte Linux ; PC Windows avec l'agent |
| `presence` | `moli-presence` | UDP + table ARP de l'hôte | non | téléphones sur le Wi-Fi |
| `esphome` | `moli-esphome` | API native ESPHome, TCP 6053, Noise écrite à la main | oui : volume, sourdine, `say`, interrupteurs | pas encore (cible : satellite ESP32-S3) |
| `phones` | `moli-phones` | rapports HTTP de l'appli (jeton) | `notify` (notification) | iPhone (appli Moli) |
| `telegram` | `moli-telegram` | HTTPS, API des bots | `notify` (message) | oui (API des bots) |
| `solarman` | `moli-solarman` | V5 ou Modbus TCP sur TCP 8899, lecture seule | non | micro-onduleurs Deye 2 et 4 entrées ; Solis S6-GR1P (Modbus TCP) |
| `vicare` | `moli-vicare` | HTTPS, cloud Viessmann (deux hôtes fixes) | oui : modes, consignes, eau chaude | PAC Viessmann (régulation CU401B_S, passerelle Vitoconnect) |
| `kasa` | `moli-kasa` | JSON sur TCP 9999, XOR, local | relais, voyant | HS110(EU) matériel 1.0 (lecture, relais, voyant) ; matériel 2 : pas encore |
| `profile` | `moli-profile` | HTTP/HTTPS GET, Meross signé | oui si le profil déclare `[[write]]` (Daikin) | Daikin BRP069 (lecture), Meross EM06, Open-Meteo, Tempo, iopool EcO (voir [profils.md](profils.md#les-profils-du-catalogue)) |
| `helpers` | `moli-helpers` | aucun (valeurs gardées par Moli) | oui | sans objet |
| `lumieres` (interne) | `moli-lights` | aucun (lit et commande d'autres appareils via le hub) | oui | ampoules Hue derrière un relais |

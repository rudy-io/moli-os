# Les pilotes

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
kind = "sonos"          # z2m, zwave, hue, dirigera, tuya, reolink, sonos, philips, frigate, bambu, tapo, shelly, igd, helpers, profile
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
```

- **Secrets** : mot de passe MQTT éventuel dans la variable d'environnement nommée par `password_env`.
- **Points** : construits depuis les `exposes` du message retenu `bridge/devices` (`src/catalog.rs`). Clé = propriété Z2M ; les composites sont aplatis (`color.x`). Écriture si le bit `set` de Z2M est présent, **sauf** membres d'un composite (Z2M refuse un membre seul). Une exposition de catégorie `config` a la sémantique `config`. Les énumérations numériques sont exposées en texte et reconverties en nombre à l'envoi. **Lumières** : la luminosité Zigbee (0–254, `value_max`) est exposée en % (0–100, unité `%`), comme les autres pilotes ; conversion arrondie à la lecture et à l'envoi, une ampoule allumée n'est jamais affichée à 0 %. La température de blanc reste en mired. **Migration** : au déploiement, l'historique `brightness` des ampoules Z2M passe de 0–254 à 0–100 (les anciennes valeurs restent sur 254 dans les graphiques) ; une automatisation écrite avec un seuil brut (`> 200`) est à reprendre. **Volets** (`cover`) : `position` a le sens `position` (0–100 %, 100 = ouvert), posé par le pilote pour un expose `cover` et jamais déduit du nom ; `state` (`OPEN` / `CLOSE` / `STOP`) est un ordre, de sens `other`, jamais un interrupteur ; `moving` (`UP` / `STOP` / `DOWN`) est lu tel quel. **Migration** : au déploiement, `state` d'un volet cesse d'être un interrupteur (`on_off` → `other`) et `position` passe de `other` à `position` ; l'historique et les automatisations qui lisaient `state` comme un interrupteur sont à reprendre. **Contacts** : `contact` est repris tel que Z2M le publie (`true` = fermée), alors que Z2M le déclare avec `value_on: false`.
- **Identité** : adresse IEEE. Le `friendly_name` ne sert qu'au routage des sujets.
- **Découverte** : chaque nouveau `bridge/devices` recharge le catalogue (appareils disparus retirés ; coordinateur et appareils désactivés exclus). Disponibilité via `<nom>/availability` (`{"state":"online"}` ou `online`).
- **Pièges** : Z2M ne rejoue pas les états aux nouveaux abonnés. Les appareils sur secteur (routeurs) sont donc interrogés une fois par exécution via `/get`, un toutes les 500 ms pour ne pas saturer le réseau ; les appareils endormis restent couverts par le cache d'état du hub. Taille de paquet portée à 4 Mio (`bridge/devices` grossit avec le réseau). Les propres `/set` et `/get` de Moli reviennent par `#` : ignorés. Une commande reçue avant que le broker ait accepté la connexion (`ConnAck`) est **refusée** (`broker injoignable`) au lieu d'être acquittée puis perdue ; toute erreur de connexion termine l'exécution (le superviseur vide la file et reconnecte).
- **Tests** : `src/catalog.rs` sur des fixtures réelles `integrations/z2m/fixtures/bridge_devices.json` (8 appareils) et `state.json` : typage, décodage, encodage des commandes, composites, lectures à la demande.

## `zwave` : Z-Wave via zwave-js-ui (`crates/moli-zwave`)

- **Couvre** : tous les nœuds Z-Wave décrits par le gateway zwave-js-ui (lumières, prises,
  capteurs sur pile) ; on/off et niveau écrivables, tout le reste en lecture seule ; aucun
  code par marque.
- **Protocole** : MQTT local (1883 par défaut), abonnement à `{base}/#`. Commandes publiées
  sur `{base}/{nom}/{cc}/{endpoint}/{prop}/set` (payload JSON brut : `true`/`false` pour
  CC 32 sur `currentValue` et CC 37 sur `targetValue`, nombre `0`–`255` pour CC 38 sur
  `targetValue`) ; les mises à jour d'état arrivent enveloppées `{time, value}` (défaut de la
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
  CC 38 est décodé en booléen) et, écrivables, le niveau CC 38 `targetValue` (0-100 %) —
  exposé sous la clé `brightness` (la clé standard des lumières, celle que l'interface
  « maison » et les ambiances cherchent, comme Hue et Dirigera) quand la classe de
  device est un vrai variateur, sinon `level` (volets, et le mode accumulation sans
  `getNodes`) ; `battery` (CC 53 `batteryLevel` ou CC 128 `level`, %),
  `temperature` / `humidity` (CC 31/49 : « Air temperature », « Relative humidity »),
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

## `dirigera` : hub IKEA Dirigera (`crates/moli-dirigera`)

- **Couvre** : lampes (marche, luminosité, température de couleur, couleur), prises (marche,
  puissance, courant, tension, énergie), capteurs (température, humidité, PM2,5, COV, CO₂,
  mouvement, éclairement, ouverture, fuite d'eau), boutons de raccourci (appuis) et batteries.
  Pas encore : stores, purificateurs, enceintes, pièces et scènes IKEA.
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
  `illuminance`, `contact` (vrai = fermé, comme Hue), `leak`. Bouton de raccourci : `button`
  (`single`, `double`, `long`, en `pulse`). `battery` partout où il y a une pile.
  Disponibilité : `isReachable` (aussi dans chaque événement).
- **Identité** : `id` du Dirigera (un UUID suivi de `_1`, `_2`… : un produit Matter comme le
  MYGGSPRAY se présente en deux appareils). Nom natif = `customName` (sinon le modèle), pièce
  native = la pièce IKEA, sans l'espace insécable que le hub laisse parfois au bout. Le hub, les répéteurs et les télécommandes de lampes (qui pilotent leurs
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
  inversé, couleur partielle, types inconnus ignorés, encodage des commandes.

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
- **Import** (une fois, puis à chaque nouvel appareil) : `tools/home-assistant/tuya-export.py` tourne **dans le conteneur Home Assistant** et lit, via le compte Tuya déjà lié à HA, la liste des appareils, leurs DP et leurs clés locales.

```sh
docker exec -i homeassistant python3 - --summary < tuya-export.py      # contrôle, sans secret
docker exec -i homeassistant python3 - < tuya-export.py \
  | docker exec -i moli-os /moli-os tuya import                        # clés jamais affichées
```

  Le script ne rafraîchit jamais le jeton de HA (cela révoquerait celui de HA) et refuse de tourner s'il expire dans moins de 15 min. `moli-os tuya import [--instance tuya] [--devices-file …]` refuse un terminal, range les clés dans le coffre et écrit `tuya.json` **sans clé**. Les champs `ip`, `version` et `room` modifiés à la main sont conservés. Redémarrer ensuite.
- **`tuya.json`** : `id`, `name`, `category`, `product`, `ip`, `version` (`"3.3"`, `"3.4"`, `"3.5"`), `room`, `gateway`, `dps[]` (`id`, `code`, `type`, `writable`, `unit`, `scale`, `min`, `max`, `step`, `range`).
- **Points** : clé = code du DP (`switch_1`, `cur_power`). Booléen → binaire ; entier → numérique divisé par `10^scale` ; enum → enum ; texte → texte ; `Json`, `Raw`, `Bitmap` ignorés. Unités : `mA` affiché en A, `℃` en °C, `度` en kWh. Écrivable si `writable` **et** type booléen, entier ou enum (jamais le texte libre). À l'envoi, un entier est arrondi au pas de l'appareil (si `step` > 1).
- **Découverte** : écoute UDP **6666, 6667** (jusqu'à 3.3) et **7000** (3.4+), sockets partagées (`SO_REUSEPORT`) avec localtuya de HA. L'adresse retenue est celle d'où vient la diffusion. `ip` et `version` servent au premier essai ; une connexion validée et saine reste intacte, un appareil hors ligne peut suivre une nouvelle annonce. La connexion doit prouver sa clé avant de devenir disponible ou de recevoir un ordre : état `dps` déchiffré en 3.3, négociation authentifiée en 3.4/3.5, avec un délai global de 5 s après la connexion TCP. Un simple heartbeat ou un retour d'erreur ne suffit jamais. Ni une diffusion ni une recherche par clé ne peuvent descendre sous la version 3.4/3.5 du fichier ou celle prouvée depuis le démarrage.
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
  un échec est journalisé dans les logs.
  Tout ce qui n'est pas propre au Sonos (le texte vérifié, la file de 3 au plus, l'abandon
  après 30 s d'attente, `quiet`, `announce_volume`, la synthèse et la publication du son) est
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
  `quiet`, `quiet_until`), et `volume` (0-100 %, **écrivable**) : le niveau du récepteur,
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

- **Couvre** : alimentation, volume, sourdine, Ambilight, application en cours, touches de télécommande ; côté Android : **ouvrir une appli** et **ce qui se joue** (titre, image, lecture/pause).
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
```

- **Côté Android** (`crates/moli-androidtv`, réutilisable pour toute Google TV) : Android TV Remote v2 (TLS **6466**, certificat client, messages protobuf préfixés d'un varint ; la télé parle d'abord, ping toutes les 5 s, connexion morte après 16 s de silence) et Google Cast (TLS **8009**, enveloppe protobuf autour de JSON : état du récepteur puis canal média de l'appli). Deux connexions tenues par le pilote, reprises de 5 s à 60 s (veille profonde = ports fermés). Un ordre donné pendant une coupure est refusé, jamais rejoué.
- **Secrets** : `username`, `password` (identifiants d'appairage JointSpace, posés par l'humain), `cert` écrit par le pilote ; `atv_cert` / `atv_key` (PEM, l'appairage Android TV Remote **repris de HA**, `.storage/androidtv_remote_*.pem`, par tube) ; `atv_pin`, `cast_pin` écrits par le pilote. Le pilote ne fait **pas** l'appairage lui-même (provenance des identifiants : à vérifier, probablement repris de HA).
- **Points** : `power`, `volume` (0 à max lu sur la télé, 60 par défaut), `mute`, `ambilight` **écrivables** ; `app` : l'appli au premier plan (paquet Android, dit par Android TV Remote : JointSpace ne la connaît pas sur une Google TV), **écrire** un paquet, un lien ou le nom d'une appli connue (Netflix, YouTube, Disney+, Prime Video, Jellyfin, Moonlight) l'ouvre : son lien web d'abord (une Google TV peut refuser `market://launch?id=…`, vu sur une Philips le 9 oct.), puis celui du Store (`moli_androidtv::remote::app_links`). L'ordre attend la réponse de la télé : l'appli à l'écran ou 1 s sans refus = ouverte ; tous les liens refusés = erreur « la télé a refusé d'ouvrir … », dite à qui a demandé ; `media_app`, `media_state` (`playing`, `paused`, `buffering`, `idle`), `media_title`, `media_subtitle`, `media_image` (URL), `media_duration`, `media_position` (s) en lecture (Cast) ; `media_control` (`play`, `pause`, `stop`) en écriture seule ; `key` en **écriture seule** (enum de 24 touches : curseurs, `Confirm`, `Back`, `Home`, lecture, volume, chaînes, `Source`, `WatchTV`…).
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
- **Creality (K2)** : son `display_status.progress` est très en retard sur l'impression (19 % pour 44 %) : quand `virtual_sdcard.cur_print_data` est là, l'avancement est `virtual_sdcard.progress`, celui de son écran. Moonraker n'y lit pas le trancheur (`slicer: Unknown`, ni estimation ni vignette) : l'estimation vient de `cur_print_data.metadata.estimated_time`, les couches de `virtual_sdcard.layer`/`layer_count`.
- **Image** : la vignette du travail (la plus grande que le trancheur a mise dans le G-code) sert d'image de l'appareil (`/api/devices/{id}/snapshot`).
- **Pas fait** : lancer une impression (le plateau doit être libre : un humain sur place), la caméra (le flux du U1 ne répond pas en HTTP local), le flux en direct par websocket (le relevé toutes les 5 s suffit).
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

## `esphome` : satellites vocaux (`crates/moli-esphome`)

- **Couvre** : les appareils ESPHome, d'abord les satellites vocaux (Home Assistant Voice
  Preview Edition, ou un ESP32-S3 monté à la main) : le mot de réveil, la conversation, la voix.
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
- **Points** : `state` (`idle`, `listening`, `thinking`, `speaking`), `volume` et `muted` (le
  `media_player` de l'appareil), `wake_word`, `say` (texte, 500 caractères).
- **Identité** : la MAC de l'appareil (`ctx.device_id(&hello.mac)`).
- **Pièges** : ⚠️ l'assistant vocal d'ESPHome n'accepte **qu'un client**, et Home Assistant s'y
  abonne dès qu'il gère l'appareil : si les deux sont branchés, **HA gagne et Moli se tait sans
  le dire**. Un seul `media_player` (le pilote retient le dernier annoncé).
- **Tests** : `src/lib.rs` (un appareil qui parle en clair ne remplace jamais la clé que Moli
  tient ; provisionnement borné par la MAC).

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
| `zwave` | `moli-zwave` | MQTT local (zwave-js-ui) | on/off, niveau | zwave-js-ui (capture réelle anonymisée + on/off sur nœud neutre) |
| `hue` | `moli-hue` | HTTPS CLIP v2 + SSE, local, épinglé | oui : lumières, pièces ; zones non | pont Philips Hue (CLIP v2) |
| `dirigera` | `moli-dirigera` | HTTPS 8443 + WebSocket, local, épinglé (API non officielle) | oui : lampes, prises | hub Dirigera (firmware 2.975) : lampes TRADFRI et ORMANAS (commandes comprises), détecteur MYGGSPRAY (lecture), redémarrage du hub ; prises, boutons, autres capteurs : pas encore |
| `tuya` | `moli-tuya` | TCP 6668 chiffré 3.3/3.4/3.5, UDP 6666/6667/7000 ; cloud en lecture | oui : DP booléens, entiers, enum (local seulement) | prises Tuya (local, écriture comprise) ; capteurs sur pile et passerelles (cloud) |
| `reolink` | `moli-reolink` | HTTPS API locale, épinglé (X.509 v1) | non (+ images) | Reolink Home Hub (caméras, sonnette) |
| `sonos` | `moli-sonos` | UPnP/SOAP HTTP 1400 ; annonces en WebSocket TLS 1443 | oui : lecture, volume, sourdine, annonces | Sonos Beam |
| `airplay` | `moli-airplay` | RAOP : RTSP 5000, RTP/UDP | oui : annonces, volume | Freebox Delta Player |
| `philips` | `moli-philips` | JointSpace HTTPS 1926, digest, WoL ; Android TV Remote, Cast | oui : alimentation, volume, sourdine, Ambilight, touches, appli | Philips Google TV (lecture) |
| `frigate` | `moli-frigate` | MQTT + HTTP interne 5000 | oui : détection, enregistrement, instantanés | Frigate |
| `bambu` | `moli-bambu` | MQTT TLS 8883, épinglé | lumière, `control` (humain pour un agent) | pas encore (cible : Bambu Lab A1) |
| `moonraker` | `moli-moonraker` | HTTP local 7125 | lumière, `control` (humain pour un agent) | Snapmaker U1, Creality K2 Combo |
| `tapo` | `moli-tapo` | HTTP 80 + KLAP v2 | oui : tout | pas encore (cible : Tapo L530) |
| `shelly` | `moli-shelly` | WebSocket local 80 (JSON-RPC Gen2), notifications poussées, digest SHA-256 | oui : sorties (`on`) | Pro 4PM, Pro 3EM (une phase câblée) : lecture identique à HA au Wh près ; ordre sur une sortie : pas encore |
| `igd` | `moli-igd` | UPnP IGD (SSDP + SOAP) | non | Livebox |
| `ipp` | `moli-ipp` | IPP 2.0 HTTP 631 | identifier, annuler, imprimer (tableau de bord) | Canon MG3600 |
| `host` | `moli-host` | /proc, /sys ; WoL ; agent HTTP sortant | allumer, éteindre, veille, Claude et Codex pour le téléphone (PIN) | hôte Linux ; PC Windows avec l'agent |
| `presence` | `moli-presence` | UDP + table ARP de l'hôte | non | téléphones sur le Wi-Fi |
| `esphome` | `moli-esphome` | API native ESPHome, TCP 6053, Noise écrite à la main | oui : volume, sourdine, `say` | pas encore (cible : satellite ESP32-S3) |
| `phones` | `moli-phones` | rapports HTTP de l'appli (jeton) | `notify` (notification) | iPhone (appli Moli) |
| `telegram` | `moli-telegram` | HTTPS, API des bots | `notify` (message) | oui (API des bots) |
| `profile` | `moli-profile` | HTTP/HTTPS GET, Meross signé | oui si le profil déclare `[[write]]` (Daikin) | Daikin BRP069 (lecture), Meross EM06, Open-Meteo, Tempo, iopool EcO (voir [profils.md](profils.md#les-profils-du-catalogue)) |
| `helpers` | `moli-helpers` | aucun (valeurs gardées par Moli) | oui | sans objet |
| `lumieres` (interne) | `moli-lights` | aucun (lit et commande d'autres appareils via le hub) | oui | ampoules Hue derrière un relais |

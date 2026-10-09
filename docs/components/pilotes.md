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
kind = "sonos"          # z2m, hue, tuya, reolink, sonos, philips, frigate, bambu, tapo, igd, helpers, profile
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
- Seuls `z2m` (`password_env`) et, en option, `hue` (`app_key_env`) lisent un secret dans une
  variable d'environnement ; la config ne contient que le **nom** de la variable.
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
- **Points** : construits depuis les `exposes` du message retenu `bridge/devices` (`src/catalog.rs`). Clé = propriété Z2M ; les composites sont aplatis (`color.x`). Écriture si le bit `set` de Z2M est présent, **sauf** membres d'un composite (Z2M refuse un membre seul). Une exposition de catégorie `config` a la sémantique `config`. Les énumérations numériques sont exposées en texte et reconverties en nombre à l'envoi.
- **Identité** : adresse IEEE. Le `friendly_name` ne sert qu'au routage des sujets.
- **Découverte** : chaque nouveau `bridge/devices` recharge le catalogue (appareils disparus retirés ; coordinateur et appareils désactivés exclus). Disponibilité via `<nom>/availability` (`{"state":"online"}` ou `online`).
- **Pièges** : Z2M ne rejoue pas les états aux nouveaux abonnés. Les appareils sur secteur (routeurs) sont donc interrogés une fois par exécution via `/get`, un toutes les 500 ms pour ne pas saturer le réseau ; les appareils endormis restent couverts par le cache d'état du hub. Taille de paquet portée à 4 Mio (`bridge/devices` grossit avec le réseau). Les propres `/set` et `/get` de Moli reviennent par `#` : ignorés. Une commande reçue avant que le broker ait accepté la connexion (`ConnAck`) est **refusée** (`broker injoignable`) au lieu d'être acquittée puis perdue ; toute erreur de connexion termine l'exécution (le superviseur vide la file et reconnecte).
- **Tests** : `src/catalog.rs` sur des fixtures réelles `integrations/z2m/fixtures/bridge_devices.json` (7 appareils) et `state.json` : typage, décodage, encodage des commandes, composites, lectures à la demande.

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

- **Secrets** (instance) : `app_key`, `client_key`, `cert_sha256`, écrits par le pilote lui-même lors de l'appairage. Il faut donc une clé maître.
- **Appairage** : statut `waiting` « Appuie sur le bouton du pont Hue » ; le pilote redemande toutes les 2 s (erreur Hue 101) jusqu'à l'appui. L'empreinte est stockée **avant** la clé, sur tous les chemins : sans empreinte connue (ni coffre, ni `cert_sha256`), poignée de main nue et `cert_sha256` stocké avant l'appairage comme avant l'usage d'une clé `app_key_env`. Clé révoquée (401/403) : la clé est oubliée et l'appairage recommence (sauf clé venue de `app_key_env` : `waiting`).
- **Points** : appareil Moli = `device` Hue, points issus de ses services. Lumière : `on`, `brightness` (%), `color_temp` (mired), `color` (« #rrggbb », envoyé au pont en CIE xy, `moli_core::color`) **écrivables** ; `color.x`, `color.y` en lecture (historique). En mode couleur le pont rend `mirek: null` : `color_temp` vaut alors `null`. `effect` (enum, écrivable) sur les ampoules qui ont des animations (gen 3 : `fire`, `candle`, `prism`, `sparkle`, `underwater`, `cosmos`…, `no_effect` pour arrêter). `button<N>` (enum des événements, en `pulse`), `battery`, `motion`, `temperature`, `illuminance` (lux, converti depuis `10000·log10(lux)+1`), `contact`. Un second service du même type est préfixé (`light2.on`). Disponibilité via `zigbee_connectivity`.
- **Pièces et zones** : publiées comme appareils avec `on` et `brightness`. **Pièces écrivables**, **zones en lecture seule** (une zone peut chevaucher une chambre protégée). Les `members` permettent à la garde de vérifier la pièce de chaque lampe.
- **Identité** : UUID du `device` Hue (ou de la pièce/zone). Le pont lui-même n'est pas publié.
- **Pièges** : un renommage ou un changement de pièce/zone déclenche un rechargement complet. Flux muet 10 min : réouverture et rechargement. Un message SSE (ou une ligne) qui dépasse 1 Mio sans fin : flux fermé, rouvert et rechargement complet. Les commandes tournent à côté du flux avec un budget de 4,5 s (sous les 5 s du hub). Le message « certificat changé » donne la commande `secrets forget <instance> cert_sha256` avec l'identifiant réel de l'instance. `Bridge` n'affiche jamais la clé en `Debug`.
- **Tests** : `src/model.rs` sur `integrations/hue/fixtures/resources.json` (pont réel) : pièces commandables, zones jamais, boutons, capteurs, événements partiels, encodage ; `src/bridge.rs` : parseur SSE découpé en morceaux, plafond de 1 Mio (ligne sans fin, `data:` sans ligne vide).

## `tuya` : Tuya Wi-Fi en local (`crates/moli-tuya`)

- **Couvre** : interrupteurs, prises, volets et tout appareil Tuya Wi-Fi joignable directement. **Par le cloud** (`src/cloud.rs`, lecture seule), quand les clés du projet cloud du foyer sont là : les capteurs **sur pile** sans adresse (catégories `mcs` porte, `sj` fuite, `ywbj` fumée, `pir`, `wsdcg`, `rqbj`, `cobj`, `sos`), qui ne parlent jamais sur le réseau local, et les **sous-appareils derrière une passerelle** Tuya. Mêmes identifiants Moli qu'en local (historique, étiquettes, portes de l'Accueil inchangés).
- **Cloud** : clés dans `/run/secrets/tuya-cloud` (Access ID puis Access Secret, une ligne chacun ; écrit par le lanceur du conteneur depuis un gestionnaire de secrets ; vide = pas de cloud), **ou** données par une personne dans Système → « Cloud Tuya » (code demandé, `PUT /api/integrations/tuya-cloud`), rangées chiffrées dans le coffre de Moli (`tuya/cloud_access_id`, `tuya/cloud_access_secret`), jamais renvoyées ; sans clés au démarrage, le pilote regarde toutes les 20 s et redémarre quand elles arrivent. Mise en place conseillée : un projet cloud Tuya, les appareils liés **un par un** en lecture (capteurs sur pile, sous-appareils des passerelles, passerelles), les prises restant locales (quota de messages) ; règle de messages de production : `devicePropertyMessage`, `deviceOnline`, `deviceOffline`, chiffrement AES-GCM. Région `cloud_region` (`eu` = Central Europe, défaut ; `eu-west`, `us`, `us-east`, `in`, `cn`). OpenAPI signée (HMAC-SHA256 : `client_id` + jeton + `t` + `nonce` + méthode, empreinte du corps, chemin) : jeton renouvelé 5 min avant ses 2 h, états `iot-03/devices/status` (20 par appel) et présence au démarrage puis **toutes les 30 min**. En direct : **service de messages** (Pulsar sur websocket `wss://mqe…:8285`, mot de passe `md5(id + md5(secret))[8..24]`, chaque message acquitté), contenu chiffré avec `secret[8..24]` (AES-128-ECB, ou AES-128-GCM quand la propriété `em` vaut `aes_gcm`), messages d'état (`devId` + `status`) et métier (`bizCode` `online` / `offline` / propriétés). Quota de l'essai gratuit : ~1 500 appels par mois sur 26 000. Commandes refusées (« lecture seule : vu par le cloud »).
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
- **Découverte** : écoute UDP **6666, 6667** (jusqu'à 3.3) et **7000** (3.4+), sockets partagées (`SO_REUSEPORT`) avec localtuya de HA. L'adresse retenue est celle d'où vient la diffusion. Un appareil avec `ip` **et** `version` dans le fichier n'est pas modifié par la découverte. Sans `ip`, version par défaut 3.3.
- **Recherche par clé** (`src/seek.rs`, `seek = true` par défaut) : quand HA tourne sur la même machine, il tient 6666/6667 sous un autre utilisateur, donc `SO_REUSEPORT` ne partage rien et la découverte n'entend rien. Pour chaque appareil sans `ip`, Moli cherche qui écoute sur 6668 dans le /24 des appareils connus (adresses déjà prises exclues), puis demande à chaque adresse, avec la clé de l'appareil : en 3.3 un état chiffré par cette clé (`dps` lisible), en 3.4/3.5 la preuve de la négociation. Seule la bonne clé ouvre la conversation : aucune adresse n'est devinée. Au démarrage puis toutes les 15 min pour ceux qui manquent ; une paire adresse/appareil déjà essayée n'est jamais redemandée. Les capteurs Wi-Fi sur pile (portes `mcs`) n'écoutent jamais : jamais trouvés, c'est normal (ils ne parlent qu'au cloud).
- **Pièges** : heartbeat toutes les 10 s, connexion jugée morte après 35 s de silence, reconnexion avec attente jusqu'à 5 min. Les prises ne publient la puissance que sur demande : DP 18/19/20 redemandés toutes les 30 s. Un ordre de plus de 5 s n'est jamais envoyé (« un radiateur allumé des heures plus tard est pire qu'un refus ») ; un ordre non acquitté en 5 s est signalé. Appareil sans clé : ignoré avec un avertissement (relancer l'import).
- **Tests** : `src/protocol.rs` vérifié **octet par octet** contre pytuya (localtuya) pour les trois versions, négociation contre un appareil simulé, trames tronquées ou altérées ; `src/model.rs` (unités, échelle, écriture) ; `src/lib.rs` (fusion d'import sans clé dans le fichier).

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
  un point `announce` (texte, écriture seule). Une annonce est une commande comme une autre
  (garde et journal ; un agent ne parle pas la nuit sans accord). Le texte (500 caractères au
  plus) est dit par **Piper en local** (protocole Wyoming, `moli-net::wyoming`), le son est
  publié 5 minutes sous un nom imprévisible (`GET /api/media/<128 bits>.wav`), puis joué en
  **clip audio** par l'API locale du Sonos (WebSocket TLS sur 1443, `moli-net::ws`) : la
  musique baisse, l'annonce passe, la musique revient d'elle-même, comme le script HA
  `announce_sonos`. Le certificat du Sonos est épinglé au premier contact (secret
  `sonos/ws_cert`). Une annonce à la fois, dans l'ordre ; la commande répond tout de suite,
  un échec est journalisé dans les logs.
- **Pièges** : description injoignable au démarrage = échec du pilote, relancé par le superviseur. Pas de gestion des groupes Sonos.
- **Tests** : lecture du SOAP et du DIDL doublement échappé.

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
- **Points** : `power`, `volume` (0 à max lu sur la télé, 60 par défaut), `mute`, `ambilight` **écrivables** ; `app` : l'appli au premier plan (paquet Android, dit par Android TV Remote : JointSpace ne la connaît pas sur une Google TV), **écrire** un paquet, un lien ou le nom d'une appli connue (Netflix, YouTube, Disney+, Prime Video, Jellyfin, Moonlight : `moli_androidtv::remote::package_of`) l'ouvre ; `media_app`, `media_state` (`playing`, `paused`, `buffering`, `idle`), `media_title`, `media_subtitle`, `media_image` (URL), `media_duration`, `media_position` (s) en lecture (Cast) ; `media_control` (`play`, `pause`, `stop`) en écriture seule ; `key` en **écriture seule** (enum de 24 touches : curseurs, `Confirm`, `Back`, `Home`, lecture, volume, chaînes, `Source`, `WatchTV`…).
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
- **Points** : `state`, `progress` (%), `remaining` (min), `nozzle_temperature`, `nozzle_target`, `bed_temperature`, `bed_target`, `job`, `layer`, `total_layers`, `error`, `wifi_signal` (dBm), `printing` ; `light` (lumière de chambre) **écrivable**.
- **Identité** : numéro de série.
- **Pièges** : certificat épinglé au premier contact, **avant** l'envoi du code ; imprimante souvent éteinte (prise connectée) : nouvel essai d'épinglage chaque minute, puis pause de 30 s entre reconnexions. Les rapports sont des **deltas** fusionnés dans le dernier état ; rapport complet (`pushall`) demandé toutes les 5 min. Ne jamais journaliser les `MqttOptions` (le `Debug` affiche le mot de passe).
- **Tests** : rapport fusionné → points.

## `moonraker` : imprimantes 3D Klipper (`crates/moli-moonraker`)

- **Couvre** : toute imprimante Klipper avec l'API Moonraker (vue sur un Snapmaker U1 ; Voron, Prusa sous Klipper…). Paquet `integrations/klipper` ; le profil `moonraker` (lecture seule) reste pour qui n'a besoin que de l'état.
- **Protocole** : HTTP local, sans authentification (port 7125). Au premier contact : `printer/info` (nom d'hôte = identité), `printer/objects/list` (la **forme** : têtes `extruder…`, plateau, capteur d'enceinte `temperature_sensor cavity|chamber`, lumière `led …`/`neopixel …`, table de filaments Snapmaker `print_task_config`), `machine/system_info` (modèle, s'il le dit). Puis une seule requête `objects/query` toutes les **5 s en impression, 15 s sinon, 30 s éteinte** ; métadonnées du fichier à chaque nouveau travail (estimation du trancheur, vignette) ; totaux de l'historique toutes les 10 min.
- **Configuration** : `host`, `port` (7125), `name` (par défaut le modèle annoncé, sinon le nom d'hôte).
- **Points** : `state` (`standby`, `printing`, `paused`, `complete`, `cancelled`, `error` ; Klipper arrêté = `error` avec sa raison dans `message`), `printing`, `progress` (%), `remaining` (min : moyenne de l'estimation du trancheur et de l'avancée du fichier), `job`, `layer`, `total_layers`, `print_duration` (min), `filament_used` (m), `nozzle_temperature`/`nozzle_target` (tête active), `active_tool`, `tool<N>_temperature`/`_target`/`_filament` (« PLA Basic · Snapmaker », « vide »)/`_color` (`#rrggbb`), `bed_*`, `chamber_temperature`, `message`, `jobs_total`, `print_hours_total`, `filament_total` (m). **Écrivables** : `light` (`SET_LED … RED/GREEN/BLUE/WHITE`), `control` (`pause`, `resume`, `cancel` → `POST /printer/print/<ordre>`), de sémantique `control` : un agent attend un humain.
- **Image** : la vignette du travail (la plus grande que le trancheur a mise dans le G-code) sert d'image de l'appareil (`/api/devices/{id}/snapshot`).
- **Pas fait** : lancer une impression (le plateau doit être libre : un humain sur place), la caméra (le flux du U1 ne répond pas en HTTP local), le flux en direct par websocket (le relevé toutes les 5 s suffit).
- **Tests** : réponses réelles du U1 (`integrations/klipper/fixtures/u1.json`) → forme, points, valeurs ; estimation du temps restant ; Klipper arrêté ; ordres → requêtes.

## `tapo` : ampoules et prises TP-Link Tapo (`crates/moli-tapo`)

- **Couvre** : marche/arrêt, luminosité, température de couleur, couleur.
- **Protocole** : 100 % local, HTTP (port 80) avec le chiffrement **KLAP v2** : `handshake1` / `handshake2`, cookie `TP_SESSIONID`, AES-128-CBC + signature SHA-256. Interrogation toutes les 5 s.
- **Configuration** : `host`, `port` (80 par défaut).
- **Secrets** : `auth_hash` = `sha256(sha1(utilisateur) + sha1(mot de passe))` en base64, comme HA le garde ; jamais le mot de passe. `moli-os secrets set <instance> auth_hash`.
- **Points** (selon ce que l'appareil annonce), **tous écrivables** : `on`, `brightness` (1-100 %), `color_temp` (2500-6500 K), `hue` (0-360), `saturation` (%).
- **Identité** : `device_id` renvoyé par l'appareil (à défaut : l'adresse `host`). Nom = `nickname` décodé.
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
- **Identité** : UDN de la box (à défaut l'adresse).
- **Découverte** : l'URL configurée est essayée en premier, puis tout ce qui répond à `upnp:rootdevice` (3 s d'écoute, faites même quand `location` est donnée). Après 6 échecs d'interrogation, nouvelle recherche, acceptée seulement si l'UDN est le même.
- **Pièges** : la Livebox ne répond pas aux recherches SSDP ciblées ; le code suppose qu'elle répond à la recherche générique, ce qui reste à vérifier. Pour une box qui ne fait que s'annoncer, fixer `location` (l'URL que HA connaît déjà, par exemple), à mettre à jour si la box change de chemin après un redémarrage.
- **Tests** : conversion en Mbit/s ; parsing UPnP dans `crates/moli-net/src/upnp.rs`.

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
| `hue` | `moli-hue` | HTTPS CLIP v2 + SSE, local, épinglé | oui : lumières, pièces ; zones non | pont Philips Hue (CLIP v2) |
| `tuya` | `moli-tuya` | TCP 6668 chiffré 3.3/3.4/3.5, UDP 6666/6667/7000 ; cloud en lecture | oui : DP booléens, entiers, enum (local seulement) | prises Tuya (local, écriture comprise) ; capteurs sur pile et passerelles (cloud) |
| `reolink` | `moli-reolink` | HTTPS API locale, épinglé (X.509 v1) | non (+ images) | Reolink Home Hub (caméras, sonnette) |
| `sonos` | `moli-sonos` | UPnP/SOAP HTTP 1400 ; annonces en WebSocket TLS 1443 | oui : lecture, volume, sourdine, annonces | Sonos Beam |
| `philips` | `moli-philips` | JointSpace HTTPS 1926, digest, WoL ; Android TV Remote, Cast | oui : alimentation, volume, sourdine, Ambilight, touches, appli | Philips Google TV (lecture) |
| `frigate` | `moli-frigate` | MQTT + HTTP interne 5000 | oui : détection, enregistrement, instantanés | Frigate |
| `bambu` | `moli-bambu` | MQTT TLS 8883, épinglé | lumière, `control` (humain pour un agent) | pas encore (cible : Bambu Lab A1) |
| `moonraker` | `moli-moonraker` | HTTP local 7125 | lumière, `control` (humain pour un agent) | Snapmaker U1 |
| `tapo` | `moli-tapo` | HTTP 80 + KLAP v2 | oui : tout | pas encore (cible : Tapo L530) |
| `igd` | `moli-igd` | UPnP IGD (SSDP + SOAP) | non | Livebox |
| `ipp` | `moli-ipp` | IPP 2.0 HTTP 631 | identifier, annuler, imprimer (tableau de bord) | Canon MG3600 |
| `host` | `moli-host` | /proc, /sys ; WoL ; agent HTTP sortant | allumer, éteindre, veille, Claude et Codex pour le téléphone (PIN) | hôte Linux ; PC Windows avec l'agent |
| `presence` | `moli-presence` | UDP + table ARP de l'hôte | non | téléphones sur le Wi-Fi |
| `phones` | `moli-phones` | rapports HTTP de l'appli (jeton) | `notify` (notification) | iPhone (appli Moli) |
| `telegram` | `moli-telegram` | HTTPS, API des bots | `notify` (message) | oui (API des bots) |
| `profile` | `moli-profile` | HTTP/HTTPS GET, Meross signé | oui si le profil déclare `[[write]]` (Daikin) | Daikin BRP069 (lecture), Meross EM06, Open-Meteo, Tempo, iopool EcO (voir [profils.md](profils.md#les-profils-du-catalogue)) |
| `helpers` | `moli-helpers` | aucun (valeurs gardées par Moli) | oui | sans objet |
| `lumieres` (interne) | `moli-lights` | aucun (lit et commande d'autres appareils via le hub) | oui | ampoules Hue derrière un relais |

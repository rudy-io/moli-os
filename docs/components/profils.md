# Les profils déclaratifs

Un profil décrit en TOML un appareil (ou une API web) parlant HTTP : quelles requêtes
faire, à quel rythme, dans quel format, et comment chaque réponse devient des points typés.
Un seul pilote générique (`kind = "profile"`, `crates/moli-profile`) exécute n'importe quel
profil. Ajouter une marque = ajouter un fichier, qu'un agent peut écrire, que
`moli-os profile check` valide et qu'un humain approuve (`moli-os profile trust`) avant usage.

- Format et validation : `crates/moli-profile/src/profile.rs` (pur, sans entrée/sortie).
- Exécution : `crates/moli-profile/src/lib.rs`.
- Approbations : `crates/moli-profile/src/trust.rs`.
- Profils intégrés au binaire : `integrations/*/profile.toml` (liste `BUILTIN`), chacun dans
  son paquet du catalogue (voir « Le catalogue d'intégrations »).

**Un profil ne change un appareil que par ses `[[write]]` déclarés**, et chacun est une
commande : il passe par le hub (garde, journal) comme celles de tout pilote. Un point sans
`[[write]]` est en lecture seule, toute commande dessus est refusée. Les lectures ne peuvent
pas cacher une écriture (voir « Les lectures ne changent rien »).

## Le format

Quatre sections : `[profile]`, au moins une `[[request]]`, au moins un `[[point]]`, et des
`[[write]]` facultatifs. Toute clé inconnue est refusée.

### `[profile]`

| Clé | Obligatoire | Rôle |
|---|---|---|
| `id` | oui | identifiant du profil, non vide, sans `/` ni `:` |
| `name` | oui | nom lisible ; nom de l'appareil par défaut et sa description |
| `manufacturer`, `model` | non | affichés sur l'appareil |
| `https` | non (`false`) | HTTPS avec certificat **public** (API web, liste Mozilla embarquée) |
| `host` | non | hôte par défaut (API web) ; un appareil du LAN reçoit le sien dans la config |
| `port` | non | port par défaut ; sinon 443 avec `https`, 80 sans |
| `identity` | non | `<requête>.<chemin>` d'une valeur qui identifie l'appareil pour de bon (MAC, numéro de série). Sans elle : `host:port` |
| `name_from` | non | `<requête>.<chemin>` du nom que l'appareil se donne |
| `secret` | non | nom du secret (par instance, dans le coffre chiffré) dont le profil a besoin. Un profil **fichier** qui en déclare un doit aussi déclarer `host` |

### `[[request]]`

| Clé | Rôle |
|---|---|
| `id` | identifiant unique dans le profil, référencé par les points |
| `path` | chemin (`/v1/x/{lat}`) : commence par un seul `/`, ASCII imprimable (encoder espaces et accents), URI valide. Une query (`?a=b`) seulement sur un point d'accès listé comme sûr (voir « Les lectures ne changent rien ») |
| `format` | `json` ou `kv` (`clé=valeur,clé=valeur`, valeurs décodées des `%XX`, à la Daikin) |
| `every` | rythme : `30s`, `5m`, `1h` ; borné entre **5 s et 1 jour** |
| `transport` | `get` (défaut) ou `meross` (message JSON signé posté, API locale Meross) |
| `namespace` | Meross uniquement : `Appliance.System.All`… (lettres, chiffres, points) |
| `payload` | Meross uniquement : objet JSON en texte (défaut `"{}"`) |
| `headers` | en-têtes ajoutés : `{ "x-api-key" = "{secret}" }`. Noms : lettres, chiffres, `-` ; valeurs ASCII imprimable. Interdits : ceux que pose le moteur (`host`, `cookie`, `content-length`, `content-type`, `transfer-encoding`, `connection`, `accept`) et les changements de méthode (`x-http-method-override`, `x-http-method`, `x-method-override`) |

Il n'y a pas de transport « https » séparé : HTTP ou HTTPS dépend de `[profile] https`.

### `[[point]]`

| Clé | Rôle |
|---|---|
| `key` | clé du point, unique, non vide, sans `/` |
| `label` | libellé non vide |
| `from` | `<requête>.<chemin>` dans la réponse (voir ci-dessous) |
| `kind` | `binary`, `numeric`, `text` ou `enum` |
| `values` | valeurs de l'enum (obligatoires pour `enum`, distinctes, non vides) |
| `map` | valeur brute (en texte) → sens, appliqué **avant** tout le reste ; chaque valeur doit coller au `kind` |
| `unit` | unité (voir ci-dessous) |
| `semantic` | `on_off`, `brightness`, `color_temp`, `color`, `power`, `apparent_power`, `energy`, `voltage`, `current`, `temperature`, `humidity`, `pressure`, `illuminance`, `battery`, `battery_low`, `signal_strength`, `contact`, `occupancy`, `water_leak`, `smoke`, `tamper`, `config`, `other`. Par défaut : déduite de la clé et de l'unité |
| `scale` | multiplicateur (numérique seulement ; fini, non nul, \|s\| ≤ 1e9) |
| `round` | décimales gardées après `scale` (numérique seulement, 6 au plus) |
| `min`, `max` | bornes d'une commande (numérique seulement, finies, `min < max`) : le hub refuse un ordre hors bornes. Les lectures ne sont pas bornées |
| `step` | pas proposé aux humains (numérique seulement, > 0) |

**Unités acceptées** : celles du cœur (`W kWh Wh VA V mV A °C % lx hPa dB ppm s min h`) et
`mired lqi dBm Hz L m³ µg/m³ ppb rpm km/h mm ° UV`. Une faute de frappe (`°c`, `mn`) est refusée.

### Chemins dans les réponses

- Segments séparés par des points : `q.result.status.extruder.temperature`.
- Dans un tableau : un nombre choisit par position (`daily.temperature_2m_max.0`) ;
  `[champ=valeur]` choisit l'élément dont le champ vaut cette valeur
  (`electricity.[channel=1].power`), insensible à l'ordre. `1` correspond à `1` et à `"1"`.
- Réponse `kv` : un seul niveau (`control.pow`).
- Pas de segment vide, sélecteur mal formé refusé (`[channel]`, `[=1]`).

### Lecture d'une valeur

1. `map` d'abord (valeur brute en texte).
2. `""`, `-`, `--`, `null`, `nan`, `NaN` : **pas de mesure** (`null`).
3. Selon le `kind` :
   - `numeric` : nombre ou texte numérique, × `scale`, arrondi ; jamais de NaN ni d'infini publié ;
   - `binary` : booléen JSON, ou `1/true/on/ON`, `0/false/off/OFF` ;
   - `enum` : doit être une des `values`, sinon `null` + avertissement (nouveau mode de firmware) ;
   - `text` : texte brut.
4. Une clé absente de la réponse laisse la dernière valeur en place.

### `[[write]]`

Rend un point écrivable. Une commande sur ce point devient un `GET` vers `path`.

```toml
[[write]]
point = "target_temperature"
path = "/aircon/set_control_info?pow={control.pow}&mode={control.mode}&stemp={value}&shum={control.shum}&f_rate={control.f_rate}&f_dir={control.f_dir}"
decimals = 1
expect = "ret=OK"
refresh = "control"
```

| Clé | Obligatoire | Rôle |
|---|---|---|
| `point` | oui | clé d'un `[[point]]` ; un seul `[[write]]` par point |
| `path` | oui | chemin envoyé ; doit contenir `{value}` (voir ci-dessous) ; même forme d'URI que `[[request]] path` |
| `map` | non | valeur commandée (en texte) → texte envoyé : `{ true = "1", false = "0" }`, `{ froid = "3", … }`. Sur un point binaire ou enum, s'il est donné il doit couvrir **toutes** les valeurs (sinon le libellé humain partirait vers l'appareil) |
| `decimals` | non | numérique seulement : `22` part en `22.0` avec `decimals = 1`. Par défaut, la forme la plus courte (`22`, `21.5`) |
| `expect` | non | texte que la réponse doit contenir pour que l'écriture compte comme faite (Daikin répond 200 même quand il refuse : `ret=PARAM NG`) |
| `refresh` | non | requête relue juste après l'écriture : le tableau de bord montre ce que l'appareil a fait, pas ce qu'on a demandé |

Dans `path` :

- `{value}` : la valeur commandée (déjà validée par le hub : type, enum, bornes), passée par
  `map`, encodée en `%XX` (une valeur ne peut jamais ajouter un paramètre).
- `{<requête>.<chemin>}` : la valeur telle que l'appareil l'a donnée dans sa **dernière
  réponse** à cette requête (même syntaxe de chemin que `from`), encodée. Sert aux appareils
  qui veulent tous les paramètres à chaque écriture.
- `{a.x|b.y}` : la première présente. Le chemin peut contenir `{value}` (seul placeholder
  imbriqué permis) : `{control.dt{value}|control.stemp}` lit `dt3` si la valeur envoyée est `3`.
- `{secret}` et les `{var}` : mêmes règles que pour les requêtes.

Validé à la compilation : point connu, `{value}` présent, chaque requête citée connue, chemins
bien formés (un seul niveau dans une réponse `kv`), `refresh` connue, `map` cohérent avec le
point, `decimals` sur un numérique.

### Les lectures ne changent rien

Une `[[request]]` est relue toutes les quelques secondes, hors garde : elle ne doit jamais
pouvoir écrire. D'où, à la compilation :

- **pas de query** (`?…`) dans une lecture, sauf sur un point d'accès documenté comme lecture
  seule et listé dans `SAFE_QUERIES` (`profile.rs`), avec ses seuls paramètres, et des valeurs
  inertes (lettres, chiffres, `_ . , -`, `{var}`). Un `GET /aircon/set_control_info?pow=1`,
  `/relay/0?turn=on` ou `/cm?cmnd=Power%20On` déguisé en lecture est refusé ;
- aucun en-tête de changement de méthode (`X-HTTP-Method-Override`…) ;
- le transport `meross` n'envoie que des messages `GET`.

Liste blanche actuelle (de quoi garder les profils fournis valides, rien de plus) :

| Point d'accès | Paramètres | Pourquoi c'est sûr |
|---|---|---|
| `/v1/forecast` | `latitude`, `longitude`, `current`, `daily`, `timezone`, `forecast_days` | API publique Open-Meteo, lecture seule |
| `/v1/air-quality` | `latitude`, `longitude`, `current`, `timezone` | idem |
| `/printer/objects/query` | `print_stats`, `heater_bed`, `extruder`, `display_status` | Moonraker : chaque paramètre nomme un objet de l'imprimante à lire |

Ajouter une entrée = modifier le code (revue humaine), en disant pourquoi l'API est en
lecture seule. Une clé d'API passée en query (`?appid={secret}`) demande aussi une entrée.

### Substitutions `{secret}` et `{var}`

- Un `{nom}` dans `path` ou dans une valeur d'en-tête : lettres minuscules, chiffres, `_`.
- `{secret}` = la valeur du secret nommé par `[profile] secret`. Règles de sûreté :
  - refusé si le profil n'a pas `https = true` (**un secret ne voyage qu'en HTTPS**) ;
  - refusé sans `[profile] secret` ;
  - un profil qui a un secret (envoyé, ou qui signe ses messages comme Meross) et un `host`
    ne peut pas être pointé vers un autre `host` que le sien ;
  - sans `[profile] host`, le secret part vers l'hôte de l'instance : permis **seulement**
    pour un profil intégré (appareil du LAN avec sa propre clé, comme Meross). Un profil
    fichier avec un secret et sans `host` est refusé à la compilation ;
  - les erreurs et journaux citent le **modèle** de chemin, jamais la valeur substituée.
- Tout autre `{nom}` est une variable que l'instance doit fournir dans `vars`, sinon le pilote
  refuse de se construire (`check-config` échoue).
- Dans `path`, les valeurs sont encodées en `%XX` ; dans les en-têtes, elles sont brutes.
- Transport `meross` : la clé ne voyage pas. Le message est signé
  `md5(messageId ‖ clé ‖ horodatage)`, envoyé en HTTP local, et la réponse doit être un `GETACK`.

## Exécution

- Cible : `host` de l'instance, sinon celui du profil, sinon erreur. Port : instance, puis profil, puis 443/80.
- Profil fichier non approuvé : statut `waiting` avec `moli-os profile check <p>` puis
  `moli-os profile trust <p>` ; aucune requête n'est faite (voir « Confiance »).
- Secret requis absent : statut `waiting` avec la commande `moli-os secrets set <instance> <nom>`.
- Premier tour : chaque requête une fois. Celles dont dépendent `identity` et `name_from`
  doivent réussir (sinon échec du pilote, relancé par le superviseur) ; les autres sont
  retentées dans 15 s. Une `identity` déclarée mais absente de la réponse est une erreur :
  jamais de repli sur l'adresse, qui couperait l'appareil en deux le jour où l'identité revient.
- Ensuite chaque requête a son propre rythme et son propre compteur d'échecs. Après un échec :
  nouvel essai dans `min(every, max(15 s, every/4))`. Après **3 échecs
  consécutifs**, les points de cette requête passent à `null`. L'appareil n'est hors ligne que
  si **toutes** les requêtes échouent.
- Chaque échange : 5 s au plus, réponse limitée à 1 Mio, `GET` (ou `POST` JSON pour Meross),
  `Accept: application/json, text/plain, */*`, `Host` avec le port sauf port par défaut
  (80 en HTTP, 443 en HTTPS).
- ⚠️ En-têtes en **Title-Case** (`Host:`) : l'adaptateur Daikin BRP069 répond 403 à `host:`.

### Une commande

1. Point sans `[[write]]` : refus « ce point est en lecture seule ».
2. Les requêtes citées par `path` sont **relues** d'abord (comme pydaikin) : renvoyer une
   valeur vieille de 30 s annulerait un réglage fait entre-temps à la télécommande. Relecture
   impossible : refus, rien n'est envoyé.
3. `path` est rempli ; une référence absente de la réponse : refus (jamais d'ordre partiel).
4. `GET` sur la même cible que les lectures (mêmes en-têtes `Host`, `Accept`). Réponse 2xx
   (et `expect` présent si déclaré) : `Ok` ; sinon l'erreur (statut HTTP, ou
   « refusé par l'appareil : … » avec le début de la réponse).
5. Tout cela tient dans l'échéance de la commande (5 s, celle du hub) : au-delà, la commande
   est abandonnée et rien de plus ne part vers l'appareil.
6. `refresh` est relue après la réponse au hub.

Les erreurs citent le **modèle** de chemin, jamais le chemin rempli.

## Confiance

Un profil intégré a été relu avec le binaire. Un **fichier** (écrit par un agent, téléchargé)
ne tourne que si un humain l'a approuvé : son SHA-256 doit figurer dans
`<data_dir>/trusted-profiles.toml`.

```toml
# Profile files a human approved (moli-os profile trust <file>).
[[trusted]]
path = "/data/profiles/capteur.toml"   # pour les humains : seule l'empreinte compte
sha256 = "685c945a…"
```

- `moli-os profile trust <fichier>` : valide le profil, **affiche chaque chemin lu et écrit**
  (hôte, secret, requêtes avec leurs en-têtes, points, écritures), puis enregistre l'empreinte.
  Redémarrer ensuite. Il lit `moli.toml` (`--config`) pour trouver le dossier de données.
- Toute modification du fichier change l'empreinte : le pilote repasse en `waiting` jusqu'à
  une nouvelle approbation. Approuver la nouvelle version d'un même chemin retire l'ancienne.
- Le chemin du fichier d'approbations est donné au pilote par `moli-os` (dossier de données),
  jamais par les options du pilote : le pilote et `profile trust` lisent forcément la même liste.

## Déclarer une instance

```toml
[[driver]]
id = "clim-salon"            # jamais renommé : préfixe des appareils et des secrets
kind = "profile"
[driver.options]
profile = "daikin-brp069"    # id intégré, ou chemin vers un fichier .toml lisible par moli-os
host = "192.168.1.x"         # appareil du LAN (absent pour une API web : host du profil)
# port = 80                  # sinon celui du profil, sinon 443/80
# vars = { lat = "45.0", lon = "4.0" }
```

Une instance = un appareil. Un profil hors binaire se donne par son chemin, et ne tourne
qu'approuvé (`moli-os profile trust <chemin>`, voir « Confiance »).

## `moli-os profile list`, `profile check` et `profile trust`

- `moli-os profile list` : les profils intégrés.
- `moli-os profile check <id|chemin>` : sans configuration, n'importe où. Valide le profil et
  affiche ce qu'il déclare : fichier et SHA-256, hôte, secret, `needs vars`, chaque requête
  (rythme, format, méthode, chemin, en-têtes), chaque point (clé, sémantique, unité, kind,
  bornes), puis la section **writes** (chaque écriture avec son chemin complet, la requête
  relue après, le texte attendu ; `writes: none (read-only)` sinon). C'est le passage obligé
  de tout profil écrit par un agent. Il ne vérifie ni l'hôte ni les `vars` d'une instance :
  c'est le rôle de `moli-os check-config`.
- `moli-os profile trust <chemin>` : la même chose, puis l'empreinte dans
  `<data_dir>/trusted-profiles.toml`. Refusé pour un profil intégré (rien à approuver).

## Le catalogue d'intégrations

Chaque intégration, pilote natif ou profil, est un **paquet** `integrations/<id>/` (forme
décidée dans [ARCHITECTURE.md](../ARCHITECTURE.md#le-paquet-dintégration)) :

| Fichier | Rôle |
|---|---|
| `integration.toml` | le manifeste (ci-dessous) |
| `profile.toml` | le profil, si `driver = "profile"` ; absent pour un pilote natif |
| `fixtures/` | réponses réelles capturées (anonymisées), citées par le manifeste |
| `README.md` | 3 à 10 lignes : ce qui a été réutilisé, ce qui est deviné, ce qui reste |
| `onboarding.md` | questions à poser, appuis physiques, où trouver les identifiants, comment tester |

18 paquets : 11 pilotes natifs (`z2m`, `hue`, `tuya`, `reolink`, `sonos`, `philips`,
`frigate`, `bambu`, `tapo`, `igd`, `helpers`) et les 7 profils ci-dessous.

### `integration.toml`

```toml
[integration]
id = "daikin-brp069"          # = nom du dossier : minuscules, chiffres, -
name = "Daikin (adaptateur Wi-Fi BRP069)"
brands = ["Daikin"]
models = ["BRP069"]
transport = "http"            # http | https | mqtt | upnp | tcp | native
driver = "profile"            # profile | un kind natif (z2m, hue…)
# crate = "moli-hue"          # pilote natif : obligatoire, moli-<kind>

[discovery]                   # ce que le code fait réellement
mdns = []                     # services, ex. "_hue._tcp"
ssdp = []                     # cibles de recherche, ex. "upnp:rootdevice"
ports = [80]                  # ports parlés sur le LAN (vide : API web)

[needs]
secrets = []                  # noms qu'un humain range dans le coffre de l'instance
vars = []                     # profil : ses {placeholders} (`vars` de l'instance)
options = ["host"]            # clés de [driver.options] sans valeur par défaut

[origin]
reused = "aucun code ni donnée repris : protocole documenté par pydaikin (…)"
author = "agent"              # agent | human

[[fixture]]
file = "fixtures/control_info.kv"
request = "control"           # profil : la requête dont c'est la réponse (rejouée)
```

- `native` : rien sur le réseau, Moli tient les valeurs lui-même (`helpers`).
- `reused` reste vide quand rien n'est noté dans le code ni la doc ; le README le dit.
- Les secrets qu'un pilote écrit lui-même (empreintes `cert`, clés d'appairage Hue) ne sont
  pas dans `needs.secrets` (rien à fournir) ; un commentaire du manifeste les cite.
- Ajouts au format décrit dans l'architecture : `options` (ce que l'onboarding doit
  demander) et `[[fixture]]` (le check doit savoir quelle requête une réponse rejoue).

### `moli-os catalogue check [dossier]`

Dossier par défaut : `integrations`. Sans configuration ni réseau ; rien n'est construit.
Pour chaque paquet (un sous-dossier) :

- manifeste lu strictement (clé inconnue refusée), `id` = nom du dossier, `transport` et
  `author` connus, `README.md` et `onboarding.md` présents ;
- `driver` : `profile` ou un `kind` de `KINDS` (la liste de ce que construit `build_driver`,
  `crates/moli-os/src/main.rs`) ; un pilote natif nomme `crate = "moli-<kind>"`, n'a pas de
  `profile.toml`, et n'a qu'un paquet ;
- profil : compilé comme par `profile check`, en profil intégré si le binaire embarque
  exactement ce texte, sinon en fichier (colonne « needs profile trust ») ; son `id`, son
  transport (`https`), `needs.secrets` (`[profile] secret`), `needs.vars` (ses
  placeholders) et `host` dans `needs.options` (s'il n'a pas d'hôte) doivent lui correspondre ;
- fixtures : chaque fichier cité existe dans le paquet (pas de `..`) ; un fichier de
  `fixtures/` non cité est signalé. Pour un profil, chaque fixture avec `request` est lue au
  format de la requête et **rejouée** sur ses points ; `identity` et `name_from` doivent s'y
  trouver (erreur sinon). Un point jamais lu avec une valeur, ou un profil sans fixture :
  avertissement.

Il imprime un tableau, puis les avertissements et les erreurs ; code 1 s'il y a une erreur.
`scripts/check.sh` le lance après les tests. Les fixtures des pilotes natifs sont rejouées
par les tests de leur crate (`include_bytes!` vers le paquet).

État au 4 octobre 2026 : 18 paquets, aucune erreur, 6 avertissements (4 profils web sans
fixture : `open-meteo`, `open-meteo-air`, `tempo`, `iopool` ; Daikin : consigne, humidité et
code erreur jamais lus avec une valeur ; Meross : canaux 3 à 6 jamais vus).

### Profils intégrés (`BUILTIN`)

`BUILTIN` (`profile.rs`) liste les ids à la main ; une macro inclut
`integrations/<id>/profile.toml` (l'id nomme le fichier, ils ne peuvent pas diverger).
Plutôt qu'un `build.rs` : un profil embarqué est de confiance sans `profile trust`, l'ajouter
doit donc être une ligne relue, pas un fichier déposé dans un dossier ; et pas de code
généré. Le test `builtin_is_every_profile_of_the_catalogue` vérifie la liste contre le
dossier dans les deux sens.

`/api/system` donne pour chaque pilote son paquet (`integration`, voir
[api.md](api.md) § 2.3).

### Les profils du catalogue

| Profil | Appareil | Accès | Requêtes | Secret | Vérifié en vrai |
|---|---|---|---|---|---|
| `daikin-brp069` | climatiseur Daikin, adaptateur Wi-Fi BRP069 | HTTP local, sans auth, `kv` | `basic` 1 h, `sensor` 60 s, `control` 30 s | aucun | oui, en lecture (écritures d'après le protocole documenté par pydaikin) |
| `meross-em06` | compteur d'énergie Meross EM06/EM06P, 6 circuits | HTTP local signé (`meross`) | `all` 1 h, `e` 15 s | `key` | oui (EM06P) |
| `moonraker` | imprimante 3D Klipper + Moonraker | HTTP local, sans auth | `info` 1 h, `q` 15 s | aucun | oui (Snapmaker U1) ; remplacé depuis par le pilote natif `moonraker` (paquet `klipper`) |
| `open-meteo` | météo du moment et du jour | HTTPS `api.open-meteo.com`, vars `lat`, `lon` | `w` 15 min | aucun | oui (sans fixture) |
| `open-meteo-air` | qualité de l'air et UV (modèles CAMS) | HTTPS `air-quality-api.open-meteo.com`, vars `lat`, `lon` | `a` 30 min | aucun | oui (sans fixture) |
| `tempo` | couleur EDF Tempo du jour et du lendemain | HTTPS `www.api-couleur-tempo.fr` | `today` 1 h, `tomorrow` 30 min | aucun | oui (sans fixture) |
| `iopool` | sonde de piscine iopool EcO | HTTPS cloud `api.iopool.com`, en-tête `x-api-key` | `pools` 15 min | `api_key` | oui (sans fixture) |

Seul `daikin-brp069` écrit (4 points) ; les autres sont en lecture seule.

#### `daikin-brp069`

- Identité : `basic.mac` ; nom : `basic.name` (nom donné dans l'appli Daikin).
- Points : `on`, `mode` (`auto`, `déshumidification`, `froid`, `chaud`, `ventilation`),
  `target_temperature` (°C, sémantique `config`, commandes de 16 à 30 °C par pas de 0,5),
  `temperature`, `outdoor_temperature`, `humidity` (`-` = pas de mesure),
  `fan` (`auto`, `silence`, `1` à `5`), `error`.
- Écritures (`on`, `mode`, `target_temperature`, `fan`) : `GET /aircon/set_control_info` avec
  **tous** les paramètres (`pow`, `mode`, `stemp`, `shum`, `f_rate`, `f_dir`), repris de
  `control` relue juste avant ; un seul change. Correspondances : `on` `1`/`0` ; `mode`
  `auto` `0`, `déshumidification` `2`, `froid` `3`, `chaud` `4`, `ventilation` `6` ; `fan`
  `auto` `A`, `silence` `B`, `1` à `5` → `3` à `7` ; consigne envoyée avec une décimale (`22.0`).
- Changer de mode reprend la consigne, l'humidité et la ventilation que l'unité garde pour ce
  mode (`dt<mode>`, `dh<mode>`, `dfr<mode>`, comme pydaikin) ; un mode qui n'en a pas
  (ventilation : pas de `dt6`) garde les valeurs actuelles. Changer de mode n'allume pas
  l'unité (`pow` repris tel quel).
- L'adaptateur répond 200 même quand il refuse : écriture faite seulement si la réponse
  contient `ret=OK`. `control` est relue après chaque écriture.
- Champs vérifiés sur une réponse capturée sur un vrai BRP069 ; écritures pas encore
  essayées sur une vraie unité. Si `f_dir` manque (unités à `f_dir_ud`/`f_dir_lr`), l'écriture est refusée
  avec « control.f_dir absent… », sans rien envoyer.

#### `meross-em06`

- Identité : `uuid` matériel ; nom : type matériel. Requêtes `POST /config`,
  `Appliance.System.All` et `Appliance.Control.ElectricityX` (canaux 1 à 6).
- Points par canal `N` : `power_N` (W), `current_N` (A), `voltage_N` (V), `power_factor_N`,
  `energy_today_N` (Wh), `energy_month_N` (Wh). Sur le fil : mA, mV, mW, Wh. Canal choisi par
  `[channel=N]`. Un canal absent reste absent (jamais 0).
- Clé de l'appareil (celle du compte Meross qui l'a installé) :
  `moli-os secrets set <instance> key`. Les points `power_N` et `energy_month_N` alimentent
  la section `[energy]`.

#### `moonraker`

- Identité et nom : `hostname` de la machine (un nom d'hôte : stable en pratique, mais à garder fixe).
- Points : `state` (`standby`, `printing`, `paused`, `complete`, `cancelled`, `error`),
  `progress` (%), `file`, `print_duration` (min), `nozzle_temperature`, `nozzle_target`,
  `bed_temperature`, `bed_target`. Le profil n'a pas de port par défaut : préciser `port`
  (Moonraker écoute souvent sur 7125).

#### `open-meteo`

- Points : `temperature`, `feels_like`, `humidity`, `precipitation`, `cloud_cover`,
  `wind_speed`, `wind_gusts`, `weather_code` (WMO), `daylight`, `today_max`, `today_min`,
  `today_rain`, `today_rain_chance`, `sunrise`, `sunset` (texte). Remplace le `sun` de HA.
- Pas d'`identity` : l'appareil est `<instance>:<hôte de l'API>:443` (`host:port`).

#### `open-meteo-air`

- Points : `aqi` (indice européen), `pm2_5`, `pm10`, `no2`, `ozone` (µg/m³), `uv_index`.

#### `tempo`

- Points : `today`, `tomorrow`, enum `inconnu`, `bleu`, `blanc`, `rouge`
  (`0` = pas encore connu : la couleur du lendemain sort vers 11 h).

#### `iopool`

- Identité : `pools.0.id` ; nom : `pools.0.title`. Seul le **premier** bassin du compte est lu.
- Points : `temperature` (°C), `ph`, `orp` (mV), `measured_at` (texte), `action_required`,
  `filtration` (h conseillées), `mode`.
- Clé personnelle (appli iopool → Réglages → clé API) : `moli-os secrets set <instance> api_key`.

## Comment écrire un nouveau profil

1. Capturer les vraies réponses de l'appareil (`curl`) et repérer une valeur d'identité
   stable (MAC, numéro de série). Un appareil du LAN **doit** avoir une `identity` : sans elle,
   un changement d'IP crée un nouvel appareil. Jamais un nom.
2. Écrire le fichier. Exemple minimal pour un capteur JSON local **fictif** qui répondrait
   `{"mac": "…", "temp": 21.5, "relay": "on"}` sur `/api/status` :

```toml
[profile]
id = "exemple-capteur"
name = "Capteur d'exemple"
manufacturer = "Exemple"
identity = "status.mac"

[[request]]
id = "status"
path = "/api/status"
format = "json"
every = "30s"

[[point]]
key = "temperature"
label = "Température"
from = "status.temp"
kind = "numeric"
unit = "°C"
round = 1

[[point]]
key = "relay"
label = "Relais"
from = "status.relay"
kind = "binary"
```

3. Valider : `moli-os profile check ./exemple-capteur.toml`, corriger jusqu'au `✓`.
   Pour écrire, déclarer un `[[write]]` (jamais une query dans une lecture).
4. Faire approuver par un humain : `moli-os profile trust ./exemple-capteur.toml` (il relit
   chaque chemin affiché).
5. Déclarer l'instance avec `profile = "<chemin du fichier>"` et `host`, lancer
   `moli-os check-config`, redémarrer, vérifier les valeurs dans le tableau de bord ou par l'API.
6. Pour en faire un profil intégré : créer le paquet `integrations/<id>/` (`profile.toml`,
   `integration.toml`, `README.md`, `onboarding.md`, les réponses réelles dans `fixtures/`
   citées avec leur `request`), ajouter l'id à `BUILTIN` (`crates/moli-profile/src/profile.rs`),
   puis `moli-os catalogue check` jusqu'à zéro erreur. `builtin_profiles_are_valid` vérifie
   qu'il compile et que seuls les points prévus sont écrivables (liste à mettre à jour s'il
   écrit) ; un test de décodage sur la fixture (comme `daikin_answers_decode`) reste utile
   pour les valeurs exactes.

Règles à retenir : rythme minimal 5 s, pas de secret en clair dans le fichier (`[profile]
secret` + coffre), `{secret}` seulement en HTTPS, `host` obligatoire dans un fichier qui a
un secret, pas de query dans une lecture hors liste blanche, enum fermées (un code inconnu
lit `null`).

## Tests

`crates/moli-profile/src/profile.rs` : tous les profils intégrés compilent et seuls les 4
points Daikin déclarés sont écrivables ; `BUILTIN` = les `profile.toml` du catalogue, dans les
deux sens ; décodage sur des réponses réelles (fixtures des paquets Daikin, Moonraker, Meross
EM06, y compris canaux réordonnés) ; profils cassés expliqués (rythme,
référence, kind, clé en double, champ inconnu, arrondi, échelle, bornes, chemin d'URI, unité,
libellé, segment vide) ; sélecteurs mal formés ; substitutions ; Meross sans secret ;
écritures refusées à la compilation (sans `{value}`, point inconnu, requête inconnue, chemin
trop profond en `kv`, `refresh` inconnue, imbrication autre que `{value}`, `{secret}` en
HTTP, `map` incomplète ou étrangère, `decimals` hors numérique, doublon) ; découpage des
modèles d'écriture et encodage d'une valeur texte ; lectures qui cacheraient une écriture
(query hors liste, paramètre non listé, `;`, en-tête de changement de méthode) ; profil
fichier avec secret sans `host`. `src/lib.rs` : signature Meross (la clé ne voyage jamais,
identifiants jamais répétés), en-têtes `Host`, chemins d'écriture Daikin rendus depuis une
réponse fixée (dont changement de mode et repli), refus sans réponse, profil fichier non
approuvé qui reste en `waiting` sans rien publier, paquet annoncé (`integration`) : l'id
d'un profil intégré, `profile` pour un fichier. `src/trust.rs` : approbation liée au texte
exact (modification = nouvelle approbation, l'ancienne retirée), fichier illisible.
`crates/moli-os/src/catalogue.rs` : le check nomme chaque erreur (pilote inconnu, `crate`
faux, id ≠ dossier, manifeste absent, pilote natif en double, transport ou `vars` qui ne
collent pas au profil, fixture absente, hors du paquet ou d'une requête inconnue, fichier non
cité) et compte les points rejoués. `src/main.rs` : chaque `kind` de `KINDS` est construit
par `build_driver`, un autre est inconnu ; le vrai catalogue passe sans erreur et décrit
chaque pilote natif et chaque profil intégré.
Aucun test ne touche le réseau.

# Surfaces externes : REST, SSE, MCP, CLI (`crates/moli-api`, `crates/moli-os`)

Référence de tout ce qu'un humain, un script ou un agent peut appeler de l'extérieur.
Sources : `crates/moli-api/src/*.rs`, `crates/moli-core/src/event.rs`,
`crates/moli-runtime/src/{hub.rs,guard.rs}`, `crates/moli-os/src/{main.rs,config.rs}`.
Les autres composants sont décrits à côté : [automatismes.md](automatismes.md),
[moli.md](moli.md), [historique-energie.md](historique-energie.md), [dashboard.md](dashboard.md).

Conventions de ce document : tous les horodatages sont en **millisecondes depuis l'epoch**
(UTC) ; un « point » s'écrit `<device_id>/<clé>` (ex. `z2m:0x00124b0000000001/state`) ; un
`device_id` s'écrit `<instance>:<id natif>` (le `/` y est interdit, remplacé par `_`).

---

## 1. Principes

### 1.1 Un seul routeur, un seul port

`crates/moli-api/src/lib.rs` (`router()`) sert **toutes** les surfaces sur le même port
(`[server] listen`, défaut `0.0.0.0:8790`) :

| Préfixe | Surface |
|---|---|
| `/api/*` | REST (JSON) |
| `/api/events`, `/api/automations/live` | flux SSE (server-sent events) |
| `/mcp` | Model Context Protocol, transport « streamable HTTP » (rmcp) |
| tout le reste | dashboard Svelte embarqué dans le binaire (`web/dist`, repli SPA sur `index.html`) ; un chemin `/api/...` inconnu répond `404` sans corps |

Détails transverses :

- HTTP/1.1 uniquement (axum compilé avec la seule fonctionnalité `http1`), compression
  gzip/brotli sur les réponses.
- Pas d'en-têtes CORS. Les routes JSON exigent `content-type: application/json` (sinon
  `415`, rejet natif d'axum) : avec le cookie `SameSite=Strict`, c'est ce qui empêche une
  page web étrangère d'écrire dans la maison (CSRF).
- Taille de corps : **256 Ko** pour tout le JSON ; 4 Mo pour `/api/assistant/listen`
  (voix) et `/api/automations/import` (export HA). Au-delà : `413`.
- Bornes : un point fait au plus 256 caractères, un acteur déclaré 64 (au journal, sans
  caractère de contrôle), un texte de valeur 4 000 (`MAX_TEXT`) ; une commande hors bornes
  est refusée et journalisée sans son contenu.
- **Débit** (`limit.rs`), par appelant et par tranche de 10 s, sur ce qui écrit seulement :
  `POST /api/command` et `PUT /api/labels/*` 30 (100 depuis le dashboard, un curseur envoie
  beaucoup de valeurs), `POST /api/approvals/*` 30, `POST /mcp` 120. Au-delà : `429`.
- Demandes d'autorisation : 50 en attente au plus ; une nouvelle demande pour le même point
  et la même origine remplace l'ancienne (la carte affichée est toujours la dernière).
- À l'arrêt (SIGTERM / Ctrl-C), les flux SSE se ferment : l'arrêt n'attend jamais un
  tableau de bord ouvert.
- Il n'y a **pas d'authentification en lecture** : tout client qui passe la garde `Host`
  lit l'état, le journal, l'historique, l'énergie. Seules certaines écritures exigent un
  humain (voir 1.3).

### 1.2 Garde `Host` (anti DNS-rebinding)

`crates/moli-api/src/guard.rs`, middleware posé sur **chaque** route (REST, SSE, MCP,
dashboard). Une page web peut faire envoyer des requêtes à ce serveur par un navigateur du
réseau local, mais ne peut pas forger l'en-tête `Host`.

- Acceptés : `localhost`, `127.0.0.1`, `::1`, toute adresse IP écrite telle quelle (le
  rebinding DNS passe par un **nom** que l'attaquant contrôle, jamais par une IP), plus chaque
  entrée de `[server] allowed_hosts` dans `moli.toml`.
- Comparaison insensible à la casse, port retiré des deux côtés (`maison.lan:8790`,
  `[fe80::1]:8790` et `maison.lan` sont équivalents).
- `Host` absent ou inconnu : `403` texte brut `host not allowed (add it to
  server.allowed_hosts)`.
- La validation d'hôte propre à rmcp est désactivée (`disable_allowed_hosts()`) : c'est
  cette garde unique qui protège aussi `/mcp`.

Ce n'est pas une authentification : c'est une protection contre les sites tiers.

### 1.2 bis Qui appelle (`crates/moli-api/src/caller.rs`, `access.rs`)

Juste derrière la garde `Host`, le middleware `identify` établit **une fois par requête**
qui est au bout du fil (`Caller`) :

| `via` | Quand | Clé de session (`Caller::key`) |
|---|---|---|
| `local` | client en boucle locale, **sans** en-tête Cloudflare | l'adresse |
| `lan` | adresse privée, lien local ou ULA (IPv4 mappée ramenée en IPv4) | l'adresse |
| `access` | requête venue par le tunnel (en-têtes `cf-ray` / `cf-connecting-ip` **depuis la machine elle-même**) **et** assertion `Cf-Access-Jwt-Assertion` valide : signature RS256 vérifiée contre les clés publiques de l'équipe (`https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`, rafraîchies au plus une fois par minute, gardées si Cloudflare est injoignable), audience = l'application, émetteur = l'équipe, non expirée (60 s de tolérance), e-mail présent | `access:<e-mail>` |
| `outside` | tout le reste : tunnel sans assertion valide, adresse publique, appareil du réseau qui imite Cloudflare. **Refusé (403) sur toutes les routes.** | `cf:<ip>` ou l'adresse réelle |

Le tunnel se termine sur la machine elle-même : sans assertion valide, une requête venue
par Cloudflare est une **inconnue**, jamais `local` ni `cli`. Sans `[server.access]`
dans `moli.toml`, personne n'est reconnu par Cloudflare. Les en-têtes de Cloudflare ne
comptent que venus de la machine elle-même (le tunnel) : un appareil du réseau local qui
en ajoute est une inconnue, refusée, et sa clé reste son adresse réelle (il ne peut pas
s'en choisir une pour échapper au blocage du PIN). Exemple : `maison.example.org`, une
application Access dont la politique n'admet que les e-mails du foyer.

### 1.3 Origine d'une action

Chaque écriture porte une `Origin` (`crates/moli-core/src/event.rs`), décidée **par le
serveur**, jamais déclarée librement :

| Origine | Attribuée quand | Surface |
|---|---|---|
| `ui` | en-tête `x-moli-origin: ui` **et** : le foyer (`lan`, `local`, `access`) si `[guard] dashboard = "trusted"` (défaut), **ou** une session humaine valide (cookie `moli_session` ouvert par PIN, même `Caller::key`, moins de 30 jours) | dashboard |
| `cli` | en-tête `x-moli-origin: cli` **et** `via = local` (jamais à travers le tunnel) | scripts sur la machine |
| `api` | tout le reste en REST : pas d'en-tête, valeur inconnue, `ui` sans session, `cli` depuis le réseau, `mcp`… | scripts, applications |
| `mcp` | tout appel d'outil MCP | agents |
| `assistant` | ordres donnés par Moli pendant `POST /api/assistant`, **quel que soit** l'interlocuteur (acteur `Moli`) | conversation du dashboard |
| `automation` | actions d'un automatisme dont un humain a validé cette version exacte (acteur = nom de l'automatisme) | moteur d'automatismes |
| `system` | expiration d'un ordre retenu, opérations internes | cœur |

La fonction `origin()` de `crates/moli-api/src/rest.rs` ne s'applique qu'à
`POST /api/command` et `PUT /api/labels/{id}` (acteur au journal : `tableau de bord
(<e-mail ou adresse>)`). **Ce qui engage** exige une session humaine (PIN), même depuis un
dashboard de confiance : `POST /api/approvals/{id}` (libérer ce qu'un agent a demandé),
valider / allumer / lancer / restaurer / supprimer un automatisme, l'image d'une caméra
de pièce protégée. Décision D0 du 3 oct. : la famille commande sans code, le PIN sert à
valider et à autoriser.

`GET /api/session` renvoie `human`, `pin_configured`, `locked`, et aussi `trusted` (ce
dashboard commande sans code), `via` et `who` (l'e-mail reconnu par Access).

**Sessions humaines** (`crates/moli-api/src/session.rs`) :

- Le code : celui choisi dans le tableau de bord (gardé chiffré, il l'emporte), sinon
  `MOLI_UI_PIN` ou `MOLI_UI_PIN_FILE`, jamais dans un fichier de config. Moins de 4 caractères :
  sessions humaines désactivées (personne n'est humain, les ordres retenus ne peuvent
  qu'expirer).
- Une maison **qui n'a jamais eu de code** (aucun choisi, rien dans l'environnement, aucune
  session jamais gardée), avec un coffre : un code d'installation à usage unique, valable une
  heure, écrit dans les journaux ; `POST /api/session/setup {code, pin}` choisit le code de la
  maison (6 chiffres au moins sans propriétaire Access, 4 sinon) et ouvre la session ; la
  maison en est avertie. Une maison qui a perdu son code ne se rouvre jamais ainsi.
- Changer le code (`PUT /api/session/pin {pin, current?}`) : le propriétaire Access librement ;
  dans une maison sans propriétaire, quiconque prouve le code actuel (mêmes verrous). La
  maison en est avertie, toutes les sessions se ferment.
- Cookie `moli_session` : `HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000` (30 jours),
  lié à la clé de l'appelant qui l'a ouvert : son adresse sur le réseau local, son e-mail
  à travers Access (l'adresse publique d'un téléphone change sans arrêt). Un cookie volé
  est inutile ailleurs.
- Seul le SHA-256 du jeton est conservé, dans le coffre chiffré (`core/ui_sessions`), avec
  une empreinte du PIN : changer le PIN ferme toutes les sessions. 20 sessions au plus
  (la plus ancienne est évincée).
- 5 échecs depuis un même appelant : blocage 15 min, doublé à chaque récidive jusqu'à
  24 h. Le blocage est **par appelant** : un agent ne peut ni deviner le PIN, ni bloquer
  l'humain qui se connecte depuis son propre appareil.
- En plus, **20 codes faux en une heure, tous appelants confondus** : la connexion par code
  est suspendue une heure et la maison reçoit un avis (les commandes du foyer, elles,
  marchent toujours). Personne ne devine le code en changeant d'adresse.
- Débits : `/api/session` 10 essais / 10 s par appelant ; Moli (`/api/assistant`) et les
  images de plan 20 / 10 s. Le budget large du tableau de bord (100 commandes / 10 s) ne
  vaut que pour le foyer, pas pour un simple en-tête.

### 1.4 Qui la garde croit

`crates/moli-runtime/src/guard.rs` (`GuardPolicy::check`). La politique vit dans
`[guard]` de `moli.toml`, éditée par des humains seulement : aucune surface ne la modifie.

| Origine | Verdict |
|---|---|
| `ui`, `system`, `automation` | toujours obéi |
| `api`, `mcp`, `cli`, `assistant` | **retenu** (approbation humaine) si l'une des règles ci-dessous s'applique, sinon exécuté |

Règles, dans l'ordre :

1. **Pièce protégée** (`protected_rooms`) : l'appareil, ou un membre s'il s'agit d'un
   groupe, est dans une pièce protégée. Toutes ses pièces comptent : l'étiquette de
   l'utilisateur **et** la pièce native (Hue…). Comparaison insensible à la casse, aux
   espaces multiples et aux accents (« chambre bebe » = « Chambre bébé »).
   Raison affichée : `pièce protégée (<pièce>)`.
2. **Appareil sans pièce** (`protect_unassigned = true`) : raison `appareil sans pièce
   connue`.
3. **Heures calmes** (`quiet_hours` : `from`, `to` en `HH:MM`, `timezone` IANA, `rooms`
   facultatif ; liste vide = toute la maison) : la fenêtre peut passer minuit, `from == to`
   = toute la journée, l'heure d'été est suivie. Raison `heures calmes (21:00 → 07:30)`.

Règles annexes :

- **Étiquettes** : une origine autre que `ui`/`system` ne peut pas changer la **pièce**
  d'un appareil situé dans une pièce protégée, sans pièce (si `protect_unassigned`), ou
  dans une pièce visée par des heures calmes ciblées (sinon un agent renommerait la pièce
  d'une lampe de chambre, puis la commanderait). **Nom** : un agent peut nommer un appareil
  qui n'en a pas encore (onboarding), jamais **renommer** un appareil déjà nommé (la phrase
  qu'un humain valide parle de ce nom). Refus : `403`. Nom et pièce sont nettoyés (pas de
  caractère de contrôle, espaces simples) et bornés à 60 caractères (`MAX_LABEL`).
- **Images** : caméra dans une pièce protégée = image réservée à une session humaine
  (REST) et toujours refusée en MCP. Les heures calmes et `protect_unassigned` ne
  s'appliquent pas aux images (voir § 7).
- **Pièces protégées orphelines** : une pièce protégée qui ne correspond à aucun appareil
  est signalée dans `guard.unmatched_protected_rooms` et dans les logs (protection qui ne
  s'applique plus, après un renommage côté Hue par exemple).

### 1.5 Ordres retenus et approbation humaine

Chemin d'une commande (`Hub::command`, `crates/moli-runtime/src/hub.rs`) :

1. **Validation d'abord** : le point existe, il est inscriptible, la valeur respecte sa
   spécification (type, bornes, valeurs d'énumération ; les nombres sont normalisés en
   flottant). Une valeur invalide est refusée même dans une pièce protégée.
2. **Garde** : si la commande doit être retenue, une demande est créée :
   - `id` unique, jamais réutilisé d'un redémarrage à l'autre (≥ horodatage ms courant) ;
   - `expires` = +10 minutes ;
   - nom de l'appareil, nom natif et pièces **figés au moment de la demande** (renommer
     l'appareil ensuite ne déguise pas ce que l'humain approuve) ;
   - événement `approval_requested`, entrée de journal au résultat `pending`.

   Réponse REST : **`202 Accepted`** `{"ok": false, "pending": <id>, "reason": "..."}`.
   Réponse MCP : succès avec `status: "awaiting_human_approval"` (voir § 4).
3. **Sinon** : envoi au pilote propriétaire, attente de sa réponse (5 s), entrée de
   journal. Le pilote confirme l'**envoi** ; la confirmation d'état arrive par l'événement
   `state` suivant.

Décision humaine : `POST /api/approvals/{id}` avec `x-moli-origin: ui`, le cookie de
session, et le corps `{"approve": true|false, "point": "<point affiché sur la carte>"}`.

| Contrôle | Échec |
|---|---|
| origine `ui` (session valide, même IP) | `403` `only a human, from the dashboard, can decide` |
| la demande existe encore | `410` `unknown or already decided request` |
| le point envoyé est celui de la demande | `409` `this request is about another point than the one shown` |
| la demande n'a pas expiré | `410` `request expired` |
| (approuvée) l'ordre part maintenant | erreurs de commande (`422`, `503`, `504`, `502`…), demande consommée quand même |

Puis événement `approval_resolved` et entrée de journal `approval` (origine `ui`, acteur
`tableau de bord`). Toutes les 15 s, la maintenance expire les demandes échues
(`decision: "expired"`, origine `system`). Les demandes en attente vivent **en mémoire** :
un redémarrage les efface.

### 1.6 Journal

Toute écriture est journalisée, réussie ou non (`JournalEntry`, `crates/moli-core/src/event.rs`) :
**qui** (`origin` + `actor`), **par quelle surface** (`origin`), **quoi** (`action`),
**résultat** (`outcome`), durée.

| Champ | Type | Sens |
|---|---|---|
| `id` | entier | croissant, continu après redémarrage |
| `ts` | ms | fin de l'opération |
| `origin` | `ui` `api` `mcp` `cli` `system` `assistant` `automation` | surface |
| `actor` | texte, facultatif | nom déclaré (corps REST `actor`, paramètre MCP `actor`, `Moli`, nom d'automatisme, `tableau de bord`) |
| `action` | objet, voir ci-dessous | quoi |
| `outcome` | `{"result":"ok"}`, `{"result":"err","error":"…"}`, `{"result":"pending","error":"en attente d'autorisation humaine #<id> : <raison>"}` | résultat |
| `duration_ms` | entier | durée |

`action` (étiqueté par `kind`) :

- `{"kind":"command","point":"…","value":…}`
- `{"kind":"label","device":"…","label":{"name":"…","room":"…"}}` (label vide si refus)
- `{"kind":"approval","request":<id>,"point":"…","value":…,"decision":"approved|denied|expired"}`

Stockage : `data/journal.jsonl` (ajout seul) + les 500 dernières entrées en mémoire.
Lecture : `GET /api/journal`, outil MCP `get_journal`, événements SSE `journal`.

---

## 2. REST

Base : `http://<hôte>:8790`. Corps et réponses en JSON. Les erreurs émises par les
handlers ont la forme `{"error": "<message>"}` ; les rejets d'extracteurs axum (JSON mal
formé, champ manquant, requête invalide) répondent en **texte brut** (`400`, `415`,
`422`).

Légende « Accès » : **libre** = aucune condition au-delà de la garde `Host` ;
**humain** = session humaine valide (cookie `moli_session` depuis la même IP).

### 2.1 Tableau de toutes les routes

**État, appareils, système**

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/health` | libre | aucune | `{name:"moli-os", version, stats, guard, drivers}` | aucune |
| GET | `/api/devices` | libre | aucune | `Snapshot` : `{devices:[DeviceView], drivers:[DriverView], approvals:[ApprovalRequest], guard}` | aucune |
| GET | `/api/devices/{id}` | libre | `id` = device_id | `DeviceView` | `404` unknown device |
| GET | `/api/devices/{id}/snapshot` | libre ; **humain** si la caméra est dans une pièce protégée | aucune | image brute, `content-type` de la caméra, `cache-control: no-store` | `403` pièce protégée ; `404` appareil inconnu / pas une caméra ; `502` pas d'image en 20 s ou échec |
| GET | `/api/home` | libre | aucune | contenu de `data/home.json` (disposition du dashboard famille), `{}` si absent | `500` JSON invalide |
| GET | `/api/system` | libre | aucune | `{version, stats, drivers, bricks, surfaces, perf, coverage}` (carte du système, § 2.3) | aucune |
| GET | `/api/bench` | libre | aucune | `data/bench.json` tel quel (comparaison avec Home Assistant, écrite par un script côté hôte, hors de ce dépôt) | `404` no comparison available yet |

**Commandes, étiquettes, approbations, journal**

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/integrations/tuya-cloud` | libre | aucune | `{instances:[{instance, configured}]}` (jamais les clés) | aucune |
| PUT | `/api/integrations/tuya-cloud` | **humain** | `{access_id, access_secret, instance?}` (lettres et chiffres) | `{stored, instance}` ; le pilote redémarre avec elles en moins d'une minute | `403` ; `404` pas de pilote tuya ; `422` pas des clés Tuya ; `500` coffre |
| GET | `/api/ambiances` | libre | aucune | `[{id, name, icon, colors:["#rrggbb"], white (K), brightness (%), effect?}]` (`crates/moli-ambiance`) | aucune |
| POST | `/api/ambiance` | libre ; chaque ordre passe par la garde comme `/api/command` | `{lights:[id…] (1 → 32, un groupe vaut ses membres), ambiance? \| color? ("#rrggbb") \| white? (1500 → 10000 K), actor?}` | `{done:[id], held:[{device, point, approval, reason}], failed:[{device, error}], skipped:[id]}` | `400` ; `404` ambiance inconnue |
| POST | `/api/command` | libre ; origine via `x-moli-origin` (§ 1.3) | `{point, value, actor?}` | `200 {ok:true}` ; **`202 {ok:false, pending:<id>, reason}`** si retenu | `404` point inconnu ; `403` point en lecture seule ; `422` valeur invalide ; `503` pilote indisponible / file pleine ; `504` pas de réponse en 5 s ; `502` erreur du pilote |
| PUT | `/api/labels/{id}` | libre ; changer la pièce d'un appareil gardé exige `ui` | `{name?, room?}` : absent = inchangé, `""` = effacé | `Label` `{name?, room?}` | `404` unknown device ; `403` changement de pièce gardé ; `500` écriture de `labels.toml` |
| GET | `/api/journal` | libre | `?limit=` (défaut 100) | `[JournalEntry]`, plus récent d'abord (500 au plus en mémoire) | `400` requête invalide |
| GET | `/api/approvals` | libre | aucune | `[ApprovalRequest]`, plus ancien d'abord | aucune |
| POST | `/api/approvals/{id}` | **humain** + `x-moli-origin: ui` | `{approve: bool, point}` | `{ok:true}` | `403` ; `409` mauvais point ; `410` inconnue / expirée ; erreurs de commande si l'ordre approuvé échoue ; `400` id non numérique |

**Session humaine**

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/session` | libre | cookie éventuel | `{human, pin_configured, locked}` (`locked` = cette IP est bloquée) | aucune |
| POST | `/api/session` | libre | `{pin}` | `{human:true}` + `Set-Cookie: moli_session=…` | `401` code incorrect ; `429` trop d'essais depuis cet appareil ; `503` aucun PIN configuré |
| DELETE | `/api/session` | libre | cookie | `{human:false}` + cookie effacé (`Max-Age=0`) | aucune |

**Historique et énergie**

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/history` | libre | `?point=` (obligatoire), `hours` (défaut 24, borné 1 min → 366 j), `points` (défaut 300, borné 10 → 2 000) | `Series` (§ 2.3) | `404` history is disabled ; `400` requête ; `500` |
| GET | `/api/energy` | libre | aucune | `Summary` : puissance instantanée, aujourd'hui, hier, mois, projection, mois dernier, par compteur | `404` no [energy] meters configured ; `500` |
| GET | `/api/energy/series` | libre | `?step=hour\|day\|month` (obligatoire), `count` (défaut 24 / 30 / 12 ; borné 1 → 8 784 / 3 660 / 600) | `Report` `{step, from, to, currency, timezone, meters, buckets:[{start, meters:{<id>:{kwh, cost?}}}]}` | `400` pas invalide ; `404` ; `500` |

**Assistant (Moli)**

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/assistant` | libre | aucune | `{ready, model, listen}` ; non configuré : `{ready:false, listen:false, configured:false}` | aucune |
| POST | `/api/assistant` | libre ; Moli agit en origine `assistant` | `Turn` : `{messages:[{role:"user"\|"assistant", content}], surface?: "bubble"\|"page"}`, dernier message = utilisateur | `Reply` : `{reply, cards:[…], actions:[{point, device, value, status:"done"\|"held"\|"failed", request?, reason?, error?}], model, usage:{prompt_tokens, completion_tokens, steps}}` | `400` ; `429` occupé (2 tours simultanés, 60 tours / 10 min tous clients confondus) ; `502` fournisseur du modèle ; `503` non configuré / pas de clé |
| POST | `/api/assistant/listen` | libre | corps = audio brut, `content-type: audio/*`, ≥ 200 octets | `{text}` (transcription) | `400` pas de l'audio / trop court ; `429` ; `502` ; `503` voix désactivée |

Limites de `Turn` : 14 derniers messages renvoyés au modèle, 2 000 caractères chacun,
6 allers-retours modèle par tour, 8 cartes au plus (types `device`, `room`, `energy`,
`weather`, `remote`, `cameras`, `history`). Détails : [moli.md](moli.md).

**Automatismes** (règles complètes : [automatismes.md](automatismes.md))

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/automations` | libre | aucune | `{automations:[Vue], abilities:{telegram, writer}}`, plus récemment modifié d'abord | `503` |
| POST | `/api/automations` | libre ; sans session = brouillon d'auteur `agent` | `Automation` : `name`, `graph`, `mode?` ; `id` facultatif (donné par Moli depuis le nom) | `Vue` (jamais validé, jamais allumé) | `422` nom vide, > 60 nœuds, > 120 liens, > 400 automatismes, JSON invalide ; `500` écriture ; `503` |
| GET | `/api/automations/{id}` | libre | aucune | `Vue` + `runs` (20 dernières exécutions) | `404` |
| PUT | `/api/automations/{id}` | libre ; sans session, ne peut pas allumer | `Automation` (`id` du corps ignoré) | `Vue` ; un changement de logique fait perdre la validation | `404` ; `422` ; `500` |
| DELETE | `/api/automations/{id}` | **humain** | aucune | `{ok:true}` | `403` ; `404` |
| POST | `/api/automations/{id}/approve` | **humain** | `{fingerprint}` = empreinte de la version affichée | `Vue` (validée et allumée) | `403` ; `404` ; **`409`** empreinte différente (relire) ; `422` problèmes à corriger |
| POST | `/api/automations/{id}/enabled` | éteindre : libre ; allumer : **humain** | `{on, fingerprint?}` (empreinte obligatoire pour `on:true`, qui équivaut à `approve`) | `Vue` | `403` ; `404` ; `409` ; `422` |
| POST | `/api/automations/{id}/restore` | **humain** | aucune | `Vue` revenue à la dernière version validée | `403` ; `404` ; `422` pas de version validée |
| POST | `/api/automations/{id}/run` | **humain**, ou le tableau de bord de la maison (`x-moli-origin: ui`, même règle qu'une commande : une routine validée se lance sans code) | `{fingerprint}` | `Run` démarrée (`status:"running"`, `steps:[]`) ; suivi par `/api/automations/live` | `403` ; `404` ; `409` ; `422` non validée / problèmes / pas de déclencheur |
| POST | `/api/automations/{id}/test` | libre | aucune | `Run` à blanc, terminée (`dry:true`) : conditions lues pour de vrai, aucune action, aucune attente | `404` ; `422` pas de déclencheur |
| POST | `/api/automations/check` | libre | `{graph}` | `Check` : `{summary (phrase française), problems:[{node?, level:"error"\|"warning", message}], protected:[pièces protégées touchées]}` | `422` ; `503` |
| POST | `/api/automations/draft` | libre | `{request, current?: Automation}` (`request` coupée à 2 000 caractères ; `current` borné comme à l'enregistrement) | `{name, mode, graph, check, note}` ; **rien n'est enregistré** | `400` (dont `current` > 60 nœuds ou > 120 liens) ; `429` ; `502` ; `503` Moli non configuré |
| POST | `/api/automations/import` | **humain** (vérifié avant le corps) | `{automations:[{alias, description?, mode?, yaml, entities?, ha_id?}]}` (150 au plus) | **`202 {started:<n>}`** : traduction en tâche de fond, chaque traduction par le sas de Moli, brouillons d'auteur `import` dont la note cite l'automatisation HA (`ha_id`) à désactiver ; déjà importés (par nom enregistré ou SHA-256 du YAML) ignorés | `403` sans session ; `400` > 150, ou existants + à traduire > 400 ; `429` import déjà en cours ; `503` |
| GET | `/api/automations/runs` | libre | `?automation=<id>&limit=` (défaut 50, max 500) | `[Run]`, plus récente d'abord | `503` |
| GET | `/api/automations/live` | libre | aucune | flux SSE (§ 3.2) | `503` |

**Flux et MCP**

| Méthode | Chemin | Accès | Entrée | Réponse | Erreurs |
|---|---|---|---|---|---|
| GET | `/api/events` | libre | aucune | flux SSE : `snapshot` puis `event` (§ 3.1) | aucune |
| POST (GET, DELETE selon le client) | `/mcp` | libre ; origine `mcp` | JSON-RPC 2.0 | voir § 4 | `403` hôte |

`503` sur les routes d'automatismes = `automations are not running` ; en pratique
`moli-os serve` démarre toujours le moteur.

### 2.1 bis Le plan vivant (`crates/moli-api/src/plan.rs`)

| Méthode | Route | Accès | Détail |
|---|---|---|---|
| GET | `/api/plan` | libre | `{ skin?, floors: [...] }`. Étage **image** : `{ id, name, image, devices: [{ id, x, y }] }`, coordonnées 0–1 sur l'image. Étage **dessiné** : `{ id, name, size: [l, h], elevation?, offset?: [x, y], height?, roof?: { ridge: "x"|"y", rise }, walls: [{ x, y, w, h, r?, height? }], rooms: [{ id, name, kind, room?, poly: [[x, y]…] }], openings: [{ kind, x, y, w, h }], items: [{ kind, x, y, w, h, r?, height?, label? }], devices: [{ id, x, y }] }`, tout en **cm** (`room` = la pièce Moli, pour le halo des lumières et « poser par pièce » ; `height` d'étage = ses murs en 3D, 250 par défaut ; `height` d'un mur ou d'un élément = sa hauteur (muret, garde-corps) ; `r` = rotation en degrés autour du centre, un mur peut être de biais ; `roof` = toit à deux pans au-dessus des murs de l'étage, faîtage le long de x ou y, `rise` cm au-dessus des égouts : regarder cet étage en 3D montre la maison de dehors ; un élément a son dos (tête de lit, dossier, écran) sur son bord haut) |
| PUT | `/api/plan` | **session humaine** | même forme ; 8 étages, 400 appareils, 400 murs/ouvertures/éléments, 100 pièces (3 à 64 points) par étage au plus ; tout dans l'étage (100 cm de marge), 100 m de côté au plus ; types connus (`skin` : plan, blueprint, nuit, aquarelle ; `kind` de pièce, d'ouverture, d'élément : listes de `plan.rs`) ; ids `[a-z0-9-]`, noms ≤ 60 ; image connue ; écriture atomique (nom temporaire unique) de `data/plan.json` |
| POST | `/api/mobile/register` | **le foyer** (LAN, ou Cloudflare Access : la vue web de l'appli) | appairage d'un téléphone (`{ name, platform: ios|android, model, push_token? }`) → `{ id, token, home }` ; le jeton n'est montré qu'ici, Moli n'en garde que le SHA-256 (`data/phones.json`) ; 20 téléphones au plus |
| POST | `/api/phones/{id}/report` | **jeton du téléphone** (`Authorization: Bearer`) ; seule route joignable sans réseau de la maison ni Access (`Via::Phone`, application Access « bypass » sur `/api/phones`) | `{ battery?, charging?, latitude?, longitude?, accuracy?, wifi?, app?, reason?, push_token? }` bornés → `{ home }` (la maison, apprise sur son Wi-Fi) ; 401 = jeton refusé, l'appli se réappaire |
| DELETE | `/api/phones/{id}` | **session humaine** | oublie un téléphone (perdu, vendu) : son jeton cesse de marcher |
| POST | `/api/plan/images` | **session humaine** | corps = l'image (PNG, JPEG, WebP, SVG ; 8 Mo au plus) ; réponse `{ image: "<16 hex>.<ext>" }` (nom tiré du contenu) |
| GET | `/api/plan/images/{name}` | libre | l'image, `immutable`, avec une CSP `sandbox` (un SVG ouvert seul ne peut rien exécuter) |

Notifier un téléphone : `POST /api/command` sur `telephones:<id>/push` (texte : une ligne de titre, puis le message), ou le canal « telephone » d'un automatisme. Envoi par le service push d'Expo (jeton de l'appli), qui relaie vers Apple ou Google.

### 2.2 Formes JSON principales

**`DeviceView`** (`GET /api/devices/{id}`, éléments de `Snapshot.devices`) : l'appareil
« aplati » plus son état.

| Champ | Type | Sens |
|---|---|---|
| `id` | texte | `device_id`, stable, jamais dérivé d'un nom |
| `instance` | texte | instance de pilote propriétaire |
| `native_name` | texte | nom dans le système source (peut changer) |
| `manufacturer`, `model`, `description`, `native_room` | texte, facultatifs | |
| `members` | `[device_id]`, facultatif | pour un groupe : appareils atteints (la garde vérifie leurs pièces) |
| `points` | `[PointSpec]` | |
| `label` | `{name?, room?}` | étiquettes de l'utilisateur (priment sur le natif) |
| `online` | booléen ou `null` | `null` = disponibilité inconnue |
| `state` | `{<clé>: {value, ts}}` | dernière valeur par point |
| `camera` | `true`, présent seulement pour une caméra | image via `/snapshot` |

**`PointSpec`** : `{key, label, kind, access:{read, write}, unit?, semantic}`.

- `kind` : `{"type":"binary"}`, `{"type":"numeric","min"?,"max"?,"step"?}`,
  `{"type":"enum","values":[…]}`, `{"type":"text"}`.
- `unit` : symbole (`W`, `kWh`, `Wh`, `VA`, `V`, `mV`, `A`, `°C`, `%`, `lx`, `hPa`, `dB`,
  `ppm`, `s`, `min`, `h`, ou tout autre texte).
- `semantic` : `on_off`, `brightness`, `color_temp`, `color`, `power`, `apparent_power`,
  `energy`, `voltage`, `current`, `temperature`, `humidity`, `pressure`, `illuminance`,
  `battery`, `battery_low`, `signal_strength`, `contact`, `occupancy`, `water_leak`,
  `smoke`, `tamper`, `config`, `other`.
- `value` : scalaire JSON (`true`, `42`, `21.5`, `"auto"`, `null`). Binaire → booléen,
  numérique → nombre, énumération → texte exact.

**`DriverView`** : `{instance, kind, integration, status}` (`integration` : voir § 2.3) avec `status` = `{"state":"starting"}`,
`{"state":"running"}`, `{"state":"backoff","error","retry_in_ms"}`,
`{"state":"waiting","reason"}` (attend un geste humain, ex. bouton d'appairage),
`{"state":"stopped"}`.

**`guard`** (`GuardStatus`) : `{protected_rooms:[…], unmatched_protected_rooms?:[…],
protect_unassigned, quiet_hours?:{from, to, timezone, rooms, active}}`.

**`stats`** : `{uptime_ms, devices, points, drivers, drivers_running, rss_bytes}`
(`rss_bytes` : Linux seulement, sinon `null`).

**`ApprovalRequest`** : `{id, ts, expires, point, value, origin, actor?, reason,
device_name, native_name, rooms}`.

**`Series`** (`/api/history`) : `{point, from, to, total (changements dans la fenêtre),
before:[ts, valeur]|null, first:[ts, valeur]|null, last:[ts, valeur]|null,
raw:[[ts, valeur]…], buckets:[{ts, avg, min, max, count}], truncated}`. `raw` si les
changements tiennent dans `points`, sinon `buckets` pour un point numérique (ou les
changements les plus récents et `truncated:true` pour un point non numérique).

**`Summary`** (`/api/energy`) : `{currency, timezone, since?, period?, price_now?, period_point?,
meters:[{id, name, role:"grid"|"total"|"circuit", point, power?, power_w?, power_ts?}],
live?:{point, max, warn, unit?, value?, ts?}, today, yesterday, month, last_month, month_projection?, monthly_fee?, budget_point?}` ; chaque période =
`{from, to, total:{kwh, cost?}, meters:{<id>:{kwh, cost?}}, unmeasured?}`.

**Vue d'automatisme** (réponses de `/api/automations*`) :

| Champ | Sens |
|---|---|
| `automation` | l'objet `Automation` : `{id, name, enabled, mode:"restart"\|"single"\|"queued", graph:{nodes, edges}, author:"human"\|"assistant"\|"agent"\|"import", approved?, approved_version?, note?, created, updated}` |
| `summary`, `problems`, `protected` | verdict du vérificateur (`Check`) |
| `approved` | la version actuelle est celle qu'un humain a validée |
| `live` | `enabled` **et** `approved` : tournera au prochain déclenchement |
| `fingerprint` | SHA-256 (hex complet) du mode, des nœuds (id + étape) et des liens ; ni le nom ni la position sur le canevas n'en font partie |
| `can_restore` | une version validée existe et diffère de l'actuelle |
| `last_run` | `{id, status, started, why, dry}` ou `null` |

Nœuds du graphe (`type` + paramètres, `x`/`y` pour le canevas) : déclencheurs `when_state`
(`point`, `to?`, `from?`, `for_s`), `when_threshold` (`point`, `above?`, `below?`,
`for_s`), `at_time` (`at` « HH:MM », `days` 1 = lundi … 7), `at_sun` (`event`
`rise|set`, `offset_min`, `days`), `every` (`minutes`), `on_start`, `manual` ; logique
`if` (`rules`, `all`, ports `yes`/`no`) ; actions `set` (`point`, `value`), `toggle`
(`point`), `wait` (`seconds`), `wait_for` (`rule`, `timeout_s`, ports `ok`/`timeout`),
`notify` (`title?`, `message`, `channels` `maison|telegram`), `write` (`prompt`). Règles :
`{kind:"state", point, op:"eq|ne|gt|ge|lt|le", value}`, `{kind:"time", after?, before?,
days}`, `{kind:"sun", is:"day|night"}`. Liens : `{from, port (défaut "out"), to}`.

**`Run`** : `{id, automation, name, trigger, why, started, ended?,
status:"running"|"done"|"failed"|"cancelled", dry, steps:[{node, at,
status:"done"|"failed"|"simulated"|"yes"|"no"|"timeout"|"waiting", detail?}]}`.

### 2.3 `/api/system`

Description du système tel qu'il tourne (rien n'est écrit à la main) :

- `drivers` : `[{instance, kind, integration, status, devices, points, offline, cameras}]` ;
  `integration` = le paquet du catalogue qu'il exécute (`integrations/<id>/`) : le `kind`
  d'un pilote natif, l'id du profil intégré, `profile` pour un profil fichier ;
- `bricks` : `guard` (pièces protégées, heures calmes, demandes en attente), `history`
  (taille de `history.db`), `energy` (compteurs, kWh du jour, taille de `energy.db`),
  `automations` (`live`, `drafts`, `total`, `runs_24h`), `assistant` (statut de Moli),
  `cameras` ; chacune `{id, name, active, facts}` ;
- `surfaces` : liste fixe (`maison`, `atelier`, `mcp`, `rest`) ;
- `perf` : 20 dernières lignes de `data/perf.jsonl` ; `coverage` : couverture lue dans
  `bench.json` ou `null`.

### 2.4 Exemples `curl`

L'en-tête `Host` est celui de l'URL : utiliser un nom présent dans `allowed_hosts` (ou
`localhost` sur la machine).

**1. Lire toute la maison**

```sh
curl -s http://localhost:8790/api/devices | jq '.devices[] | {id, name: (.label.name // .native_name), online}'
```

**2. Donner un ordre (script, origine `api`)**

```sh
curl -s -X POST http://localhost:8790/api/command \
  -H 'content-type: application/json' \
  -d '{"point": "z2m:0x00124b0000000001/state", "value": true, "actor": "script-arrosage"}'
# 200 {"ok":true}
# 202 {"ok":false,"pending":1759500000123,"reason":"pièce protégée (Chambre des enfants)"}
```

**3. Approuver un ordre retenu (humain, depuis la machine qui a ouvert la session)**

```sh
read -rs PIN                                   # saisi sans écho, hors historique
printf '{"pin":"%s"}' "$PIN" | curl -s -c /tmp/moli.cookies \
  -H 'content-type: application/json' --data @- http://localhost:8790/api/session
unset PIN
curl -s -b /tmp/moli.cookies -X POST http://localhost:8790/api/approvals/1759500000123 \
  -H 'x-moli-origin: ui' -H 'content-type: application/json' \
  -d '{"approve": true, "point": "z2m:0x00124b0000000001/state"}'
curl -s -b /tmp/moli.cookies -X DELETE http://localhost:8790/api/session; rm /tmp/moli.cookies
```

**4. Historique d'un point sur 48 h**

```sh
curl -s 'http://localhost:8790/api/history?point=z2m:0x00124b0000000002/temperature&hours=48&points=200' \
  | jq '{total, last, n: (.raw | length), buckets: (.buckets | length)}'
```

**5. Suivre la maison en direct**

```sh
curl -sN http://localhost:8790/api/events
# event: snapshot
# data: {"devices":[…],"drivers":[…],"approvals":[],"guard":{…}}
# event: event
# data: {"type":"state","point":"z2m:0x00124b0000000001/state","value":true,"ts":1759500001234}
```

---

## 3. SSE

### 3.1 `GET /api/events`

`crates/moli-api/src/sse.rs`. Abonnement au bus **avant** la photo : rien ne tombe entre
les deux.

| Nom d'événement SSE | `data` | Quand |
|---|---|---|
| `snapshot` | `Snapshot` complet (comme `GET /api/devices`) | à la connexion ; de nouveau si l'abonné a pris trop de retard (bus de 1 024 événements) : il reçoit une photo fraîche au lieu des événements perdus |
| `event` | un `Event` (ci-dessous) | à chaque changement |
| `error` | aucun | la photo n'a pas pu être sérialisée (ne devrait pas arriver) |

Commentaires keep-alive périodiques (valeur par défaut d'axum). Le flux se ferme à l'arrêt
du serveur. Depuis un navigateur : `new EventSource('/api/events')` avec
`addEventListener('snapshot' | 'event', …)`.

`Event` est étiqueté par `type` (snake_case) :

| `type` | Champs | Sens |
|---|---|---|
| `device_upserted` | `device` : l'appareil (`id`, `instance`, `native_name`, `manufacturer?`, `model?`, `description?`, `native_room?`, `members?`, `points`) | appareil publié ou redéfini par son pilote |
| `device_removed` | `device` : device_id | appareil retiré |
| `state` | `point`, `value`, `ts` | nouvelle valeur (seulement si elle change, sauf points « impulsion » type bouton, diffusés à chaque occurrence) ; aussi à la restauration du cache |
| `availability` | `device`, `online` (booléen) | joignable / injoignable |
| `driver_status` | `instance`, `status` (`DriverStatus`, § 2.2) | état d'un pilote |
| `label_changed` | `device`, `label` `{name?, room?}` | étiquette modifiée (label complet après fusion) |
| `journal` | `entry` : `JournalEntry` (§ 1.6) | toute écriture journalisée |
| `approval_requested` | `request` : `ApprovalRequest` (`id`, `ts`, `expires`, `point`, `value`, `origin`, `actor?`, `reason`, `device_name`, `native_name`, `rooms`) | un ordre attend un humain |
| `approval_resolved` | `id`, `decision` : `approved` \| `denied` \| `expired` | décision ou expiration |
| `notice` | `notice` : `{title?, message, from?, ts}` (`from` = nom de l'automatisme) | message pour la maison (nœud `notify`, canal `maison`) |

Exemple :

```json
{"type":"approval_requested","request":{"id":1759500000123,"ts":1759500000122,"expires":1759500600122,"point":"hue:12/on","value":true,"origin":"mcp","actor":"claude-code","reason":"pièce protégée (Chambre des enfants)","device_name":"Veilleuse","native_name":"Hue go 1","rooms":["Chambre des enfants"]}}
```

### 3.2 `GET /api/automations/live`

`crates/moli-api/src/automations.rs` (`live`). Aucune photo initiale : relire
`GET /api/automations` à la connexion. Nom d'événement SSE : `live`. `data` étiqueté
par `type` :

| `type` | Champs | Sens |
|---|---|---|
| `run_started` | `run` : `Run` | une exécution démarre (réelle ou à blanc) |
| `step` | `run` (id), `automation` (id), `step` : `{node, at, status, detail?}` | une étape (l'éditeur allume le nœud) |
| `run_ended` | `run` : `Run` complet | fin (`done`, `failed`, `cancelled`) |
| `changed` | `automation` (id) | automatisme enregistré, validé, allumé/éteint, restauré ou supprimé : relire |

Canal de 256 messages ; un abonné en retard **perd silencieusement** les messages manqués
(pas de resynchronisation, contrairement à `/api/events`).

---

## 4. MCP

### 4.1 Point d'entrée

`http://<hôte>:8790/mcp`, transport « streamable HTTP » (`rmcp` 3.5,
`crates/moli-api/src/mcp.rs`) :

- `StreamableHttpService` avec `LocalSessionManager` (sessions en mémoire),
  `json_response(true)` : les réponses arrivent en `application/json` plutôt qu'en flux ;
  `with_legacy_session_mode(false)` (sens exact à vérifier dans rmcp). Les tests
  (`crates/moli-api/tests/http.rs`) appellent `tools/list` et `tools/call` en POST isolés,
  sans `initialize` préalable ni en-tête de session : ça fonctionne.
- Le client envoie `content-type: application/json` et
  `accept: application/json, text/event-stream`.
- Garde `Host` comme partout, aucune authentification. Toute écriture a l'origine `mcp`
  et passe par la garde (§ 1.4).
- Nom du serveur : `moli-os`. Instructions envoyées au client, telles quelles :

> Moli OS controls a real home. Point ids look like `<device_id>/<key>`. Call
> list_devices first; only points marked writable accept send_command. Every write is
> journaled with your actor name. Prefer harmless points when testing and restore the
> previous value.

Résultats : un succès est **un bloc texte contenant du JSON compact** ; un échec métier
est un résultat `isError: true` avec un message texte (pas une erreur JSON-RPC). Seule
exception : `camera_snapshot` renvoie un bloc image (base64 + type MIME).

Brancher un client : `claude mcp add --transport http moli http://<hôte>:8790/mcp`
(Claude Code), ou l'équivalent « serveur MCP HTTP » de tout autre agent.

### 4.2 Les 14 outils

| Outil | Paramètres | Renvoie | Notes |
|---|---|---|---|
| `system_status` | aucun | `{stats, drivers, guard, pending_approvals, comparison_with_home_assistant:{coverage, ha_memory_bytes, moli_memory_bytes, measured_at}\|null}` | dit d'avance à l'agent où et quand il lui faudra un humain |
| `list_devices` | `room?` (insensible à la casse ; pièce de l'étiquette, sinon native), `search?` (sous-chaîne de l'id, du nom natif, du nom d'étiquette ou du modèle) | `{count, devices:[{id, name, room, model, description, online, points:[{key, value, unit?, writable?}]}]}` | `writable: true` seulement sur les points inscriptibles |
| `get_device` | `device` (device_id) | `DeviceView` complet (spécifications, bornes, valeurs permises, état) | `unknown device` |
| `camera_snapshot` | `device` | bloc image (base64, type MIME de la caméra) | **refusé** pour une pièce protégée : `protected room « … »: images are for humans in the dashboard only` ; délai 20 s |
| `send_command` | `point`, `value` (booléen, nombre ou texte d'énumération ; objet/tableau → texte JSON), `actor?` (nom de l'agent, journalisé) | `{ok:true, point}` ; retenu : `{ok:false, status:"awaiting_human_approval", request:<id>, reason, message:"Not executed. A human must approve this request from the Moli OS dashboard (it expires in 10 minutes). Do not retry; tell the user."}` | l'ordre retenu n'est **pas** une erreur : l'agent doit relayer, pas réessayer. Erreurs : `unknown point`, `point is read-only`, `invalid value: …`, `driver …` |
| `set_label` | `device`, `name?`, `room?` (`""` efface), `actor?` | `{ok:true, device, label}` | changer la pièce d'un appareil gardé est refusé (`seul un humain peut changer la pièce de cet appareil (…)`) |
| `get_journal` | `limit?` (défaut 20) | `[JournalEntry]`, plus récent d'abord | |
| `history` | `point`, `hours?` (défaut 24, borné 1 min → 366 j) | `{point, from, to, changes, value_before_window, stats:{min, max, avg}\|null, first, last, series (≤ 60 points, brut ou seaux), truncated}` | moyenne pondérée (seaux pesés par leur nombre d'échantillons, valeur d'avant la fenêtre incluse) ; `history is disabled on this instance` |
| `energy` | `step?` (`hour`/`day`/`month`), `count?` (défaut 24 / 30 / 12) | sans `step` : `Summary` ; avec : `Report` | jours et mois en heure locale ; `no energy meters are configured on this instance` |
| `history_sql` | `sql` : **une** requête SQLite en lecture seule | `{columns, rows, truncated}` | voir 4.3 |
| `list_automations` | aucun | `{automations:[{id, name, summary, enabled, approved, live, author}]}` | `live` = allumé **et** validé par une personne |
| `get_automation` | `id` | `{automation, check, runs (5 dernières, étape par étape)}` | `unknown automation` |
| `save_automation_draft` | `automation` : objet comme `get_automation` le renvoie (`name`, `mode`, `graph` ; `id` pour retravailler un existant ; `id` et `enabled` complétés par défaut) | `{id, status:"draft: a person must approve it in the dashboard", check}` | toujours un **brouillon** d'auteur `agent` ; modifier la logique d'un automatisme validé lui fait perdre sa validation ; ne peut pas allumer |
| `test_automation` | `id` | `Run` à blanc (chemin suivi, étape par étape) | conditions lues pour de vrai, rien n'agit, rien n'attend |

Il n'existe volontairement **aucun** outil MCP pour approuver un ordre, valider, allumer,
lancer pour de vrai, restaurer ou supprimer un automatisme : c'est réservé à l'humain du
dashboard.

### 4.3 Règles de `history_sql`

Vue interrogeable : `history(point TEXT, ts INTEGER /* ms */, kind TEXT /* b i f t z */,
num REAL /* booléen 0/1, entier, flottant */, txt TEXT)`.

- Connexion SQLite dédiée, ouverte en **lecture seule** + `PRAGMA query_only`.
- Une seule instruction ; elle doit être en lecture seule (`stmt.readonly()`), sinon
  `only a single read-only statement is accepted`.
- Un « authorizer » refuse `ATTACH`, `DETACH`, `PRAGMA`, transactions et savepoints.
- Budget : 2 s par requête (`query took longer than 2s`), 200 lignes, environ 1 Mo de
  réponse (`truncated: true` au-delà), 64 Kio maximum pour toute chaîne ou blob construit.
- Les blobs sont rendus `"<N bytes>"`.

Exemple de la description de l'outil : `SELECT COUNT(*) FROM history WHERE point LIKE
'%/contact' AND num = 0 AND ts > strftime('%s','now','-7 days') * 1000`.

### 4.4 Exemple `curl`

```sh
curl -s http://localhost:8790/mcp \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_command","arguments":{"point":"z2m:0x00124b0000000001/state","value":true,"actor":"mon-agent"}}}'
```

---

## 5. CLI `moli-os`

Définition : `crates/moli-os/src/main.rs` (clap ; les sous-commandes s'écrivent en
kebab-case).

```
moli-os [-c|--config <fichier>] [COMMANDE]
```

| Option / variable | Rôle |
|---|---|
| `-c`, `--config` (env `MOLI_CONFIG`, défaut `moli.toml`) | fichier de configuration, global à toutes les sous-commandes |
| `--version`, `--help` | standard clap |
| `MOLI_MASTER_KEY_FILE` (préféré : fichier tmpfs) ou `MOLI_MASTER_KEY` | clé maître du coffre `data/secrets.enc` (64 chiffres hexadécimaux) |
| `MOLI_UI_PIN_FILE` ou `MOLI_UI_PIN` | PIN du dashboard (≥ 4 caractères) |
| `RUST_LOG` | filtre de logs (défaut `info,rumqttc=warn,rmcp=warn`) |

La configuration est chargée et validée pour toutes les commandes **sauf** `profile` :
`moli.toml` doit exister et être valide même pour `secrets` ou `energy import`.

| Commande | Arguments | Effet |
|---|---|---|
| *(aucune)* ou `serve` | aucun | lance le hub, les pilotes, l'historique, l'énergie, les automatismes, Moli et toutes les surfaces |
| `check-config` | aucun | valide la configuration et chaque pilote, affiche `configuration OK (N driver(s))` |
| `secrets set <instance> <name>` | instance de pilote (ou `assistant`, `automations`), nom du secret | lit la valeur sur **l'entrée standard**, refuse un terminal interactif et une valeur vide, chiffre dans `data/secrets.enc`, n'affiche que la longueur |
| `secrets forget <instance> <name>` | idem | supprime le secret, affiche `forgotten` ou `not found` ; redémarrer ensuite |
| `profile check <profile>` | fichier de profil ou id intégré | valide un profil déclaratif et affiche variables, requêtes et points (la porte que tout profil écrit par un agent doit passer) ; sans configuration |
| `profile list` | aucun | liste les profils intégrés ; sans configuration |
| `energy import` | aucun | importe un historique horaire depuis l'entrée standard (lignes JSON `{"meter", "hour" (ms, début d'heure UTC), "kwh"}`) ; seules les heures antérieures à l'enregistrement de Moli sont prises ; rejouable, possible pendant que `serve` tourne ; exige `[energy]` |
| `tuya import` | `--instance <id>` (défaut `tuya`), `--devices-file <chemin>` (défaut `data/tuya.json`) | lit l'export JSON de `tools/home-assistant/tuya-export.py` sur l'entrée standard (refuse un terminal) : clés locales dans le coffre chiffré, le reste dans le fichier d'appareils ; redémarrer ensuite |

**Les secrets passent toujours par un tube, jamais par la ligne de commande ni le
clavier** (sinon ils restent à l'écran, dans l'historique du shell ou la liste des
processus). Secrets nommés connus : `assistant/api_key` (clé du modèle de Moli),
`automations/telegram_webhook` (canal Telegram), `core/ui_sessions` (interne, ne pas
toucher). Les identifiants de pilote `core`, `assistant` et `automations` sont réservés.

Exemples :

```sh
# Clé de l'assistant : saisie sans écho, puis passée par un tube
read -rs V; printf %s "$V" | moli-os secrets set assistant api_key; unset V

# Oublier la clé d'application d'un pont Hue remplacé (secrets du pilote Hue :
# app_key, client_key, cert_sha256), puis redémarrer
moli-os secrets forget hue app_key

# Valider la configuration d'une autre instance
moli-os -c /data/moli.toml check-config

# Vérifier un profil (fichier ou id intégré) avant de l'utiliser
moli-os profile check integrations/daikin-brp069/profile.toml
moli-os profile list

# Vérifier tout le catalogue (manifestes, profils, fixtures rejouées)
moli-os catalogue check integrations

# Importer l'historique d'énergie de Home Assistant
# (usage du script : ha-energy-export.py <meter id>=<statistic id> ...)
python3 tools/home-assistant/ha-energy-export.py <compteur>=<statistique HA> > rows.jsonl
moli-os energy import < rows.jsonl

# Importer les appareils Tuya (clés locales comprises, jamais affichées) :
# l'export tourne dans le conteneur Home Assistant, l'import dans celui de moli-os
docker exec -i homeassistant python3 - < tools/home-assistant/tuya-export.py \
  | docker exec -i moli-os /moli-os tuya import
```

Le mode d'emploi exact de chaque script est dans son en-tête : `ha-energy-export.py` lit
la base de Home Assistant en lecture seule, `tuya-export.py` passe par le compte Tuya déjà
lié à Home Assistant.

---

## 6. Codes d'erreur

| Code | Sens dans Moli OS | Où |
|---|---|---|
| `200` | succès | partout |
| `202` | accepté mais **pas exécuté** : ordre retenu par la garde (`pending`), ou import d'automatismes lancé en tâche de fond | `/api/command`, `/api/automations/import` |
| `400` | requête mal formée : JSON syntaxiquement invalide, paramètre de requête manquant ou invalide (`point`, `step`…), id d'approbation non numérique, demande refusée par Moli (dernier message non utilisateur, audio absent ou trop court, demande vide, plus de 150 imports), plage d'énergie invalide | rejets axum, `/api/assistant*`, `/api/automations/draft` et `/import`, `/api/energy/series` |
| `401` | PIN incorrect | `POST /api/session` |
| `403` | refusé : hôte non autorisé (garde `Host`, texte brut) ; point en lecture seule ; décision d'approbation sans origine `ui` ; changement de pièce gardé ; image d'une pièce protégée sans session ; action d'automatisme réservée à une personne | partout |
| `404` | inconnu : appareil, point, caméra, automatisme ; brique non activée (historique désactivé, pas de `[energy]`, pas de `bench.json`) ; chemin `/api/...` inexistant | |
| `405` | méthode non prise en charge sur un chemin existant (axum) | |
| `409` | conflit : **empreinte d'automatisme différente** de la version actuelle (quelqu'un l'a modifiée depuis : relire), ou point d'une décision d'approbation différent de celui de la demande | `/api/automations/{id}/approve`, `/enabled`, `/run`, `/api/approvals/{id}` |
| `410` | demande d'approbation inconnue, déjà décidée ou expirée | `/api/approvals/{id}` |
| `413` | corps au-delà de la limite par défaut d'axum (2 Mo) (à vérifier) | `/api/assistant/listen` surtout |
| `415` | `content-type: application/json` manquant sur une route JSON | rejet axum |
| `422` | contenu invalide : valeur hors spécification d'un point (type, bornes, énumération), JSON bien formé mais de mauvaise forme (champ manquant…), automatisme invalide (nom vide, trop de nœuds ou de liens, problèmes à corriger, pas de version validée, pas de déclencheur) | `/api/command`, `/api/automations*`, rejets axum |
| `429` | trop de requêtes : PIN bloqué pour cette IP, Moli occupé (2 tours simultanés, 60 tours / 10 min), import déjà en cours | `/api/session`, `/api/assistant*`, `/api/automations/draft` et `/import` |
| `500` | erreur interne : écriture des étiquettes ou des automatismes, base d'historique ou d'énergie, `home.json` illisible | |
| `502` | amont en échec : erreur rapportée par le pilote, caméra sans image (échec ou 20 s), fournisseur du modèle de Moli | `/api/command`, `/api/approvals/{id}`, `/snapshot`, `/api/assistant*` |
| `503` | indisponible : pilote absent ou arrêté, file de commandes du pilote pleine (64), Moli non configuré (pas de `[assistant]`, pas de clé, voix désactivée), automatismes arrêtés, aucun PIN configuré | |
| `504` | le pilote n'a pas répondu dans les 5 s | `/api/command`, `/api/approvals/{id}` |

Côté MCP, ces cas ne sont pas des codes HTTP : l'appel répond `200` et l'outil renvoie un
résultat `isError: true` avec le message (ordre retenu = succès avec
`awaiting_human_approval`).

---

## 7. Points relevés en écrivant cette référence

Corrigés le 3 octobre (même jour) :
- le commentaire de module de `automations.rs` disait qu'enregistrer validait (faux) ;
- `POST /api/automations/{id}/run` sur un automatisme validé mais **éteint** répondait
  « running » sans rien lancer : refusé désormais (`422`, « switch it on first ») ;
- avec `protect_unassigned = true`, l'image d'une caméra **sans pièce** était servie aux
  agents : `Hub::protected_room` la protège maintenant comme la garde le fait pour les
  commandes ;
- les ids `check`, `draft`, `import`, `runs`, `live` sont réservés (un automatisme nommé
  « Live » reçoit `live-2`) ;
- `POST /api/automations` n'exige plus de champ `id` ;
- l'aide de `moli-os secrets set` ne demande plus d'arrêter Moli (il faut seulement
  redémarrer **après**) ;
- `AGENTS.md` renvoie à cette doc pour la liste des outils MCP.

Restent ouverts (non urgents) :
- `/api/automations/live` perd silencieusement les messages en retard (pas de
  resynchronisation) ;
- `GET /api/assistant` : `configured` n'apparaît que lorsqu'il vaut `false` ;
- l'origine `cli` repose sur l'IP de boucle locale et les sessions sont liées à l'IP :
  derrière un proxy inverse local, tous les clients deviendraient `127.0.0.1` (`cli`
  revendicable par tous, blocage PIN partagé). Sans effet sur la garde (`cli` n'y est pas
  de confiance) et sans objet avec le déploiement actuel en `--network host` ;
- lectures et coûts ouverts sans session, par conception (réseau local + garde `Host`) :
  état, journal, historique, énergie, images des pièces non protégées, et les routes qui
  consomment le modèle de Moli (`/api/assistant`, `/draft`, `/import`), bornées par les
  limites de débit. À revoir avant toute exposition hors du réseau local ;
- le commentaire de `crates/moli-api/src/lib.rs` ne liste ni `/api/system`, ni
  `/api/home`, `/api/bench`, `/api/history`, `/api/devices/{id}/snapshot` ; la plupart des
  handlers de `rest.rs` et `automations.rs` n'ont pas de commentaire de documentation.

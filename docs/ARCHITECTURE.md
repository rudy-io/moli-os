# Moli OS : architecture

> État au 3 octobre 2026 au soir, avec les décisions prises après une revue adversariale du
> code et les arbitrages de l'auteur.

Plateforme domotique **agent-native**, ultra-légère, en Rust. Thèse : Home Assistant a été
pensé pour un humain qui clique ; Moli OS est pensé pour une famille qui veut que ça marche,
et pour des agents qui configurent, créent les automatismes et **écrivent les intégrations**,
sans jamais être aux commandes en temps réel.

Étalon : un Home Assistant réel, sur la même machine, avec 59 intégrations, 171 appareils,
1 591 entités, 90 automatisations, **2,1 Go de mémoire**. Moli OS à côté, ce jour-là : 71
appareils, 446 valeurs, 18 pilotes, **12 Mo**, 0,23 % CPU, image 12 Mo (mesures à jour :
[PERFORMANCE.md](PERFORMANCE.md)).

## Règles non négociables

1. **Aucun LLM dans la boucle de contrôle.** Le moteur est déterministe ; l'IA crée,
   explique, configure, rédige.
2. **La famille commande sans friction, les agents proposent, un humain valide ce qui agit
   seul.** Un clic dans le dashboard est obéi. Le PIN ne sert qu'à valider un automatisme et
   à autoriser un agent dans une pièce protégée ou aux heures calmes. (Changé le 3 oct. : la
   version précédente demandait un code à la famille aux heures calmes.)
3. **L'identité ne dérive jamais d'un nom ni d'une adresse** : `DeviceId = <instance>:<id
   natif stable>` ; nom et pièce sont des étiquettes.
4. **Valeurs typées, unités explicites.**
5. **Une panne de pilote n'emporte jamais le cœur** (tâche supervisée, backoff, statut), et
   **ses appareils passent hors ligne** le temps de la panne.
6. **Aucun secret en clair** : coffre chiffré de Moli (clé maître tenue par un gestionnaire de
   secrets, livrée en tmpfs) ; la config ne contient que des noms ; un LLM ne voit jamais un
   secret.
7. **Toute écriture est journalisée** : commandes, étiquettes, approbations, **et** toute
   modification d'un automatisme (qui, par où, quoi, résultat).
8. **Agnostique aux agents** : tout est exposé en MCP, REST et CLI.
9. **Le binaire fait respecter les règles.** Un profil ou un pilote écrit par un agent ne
   peut écrire qu'en passant par le hub (donc par la garde), ne peut atteindre que l'hôte
   déclaré, et n'est chargé qu'une fois validé par un humain.
10. **Toute entrée est bornée** (taille des corps, des textes, des files, des fichiers) et
    **tout ce qui vient du réseau est traité comme hostile**, LAN compris.
11. **Performance** : chaque livraison est mesurée contre HA sous un budget
    ([PERFORMANCE.md](PERFORMANCE.md)) ; une brique inactive ne coûte rien.

## Vue d'ensemble

```
 famille ─► Maison (Svelte, embarqué)        agents ─► MCP /mcp      scripts ─► REST /api/*
              │  SSE /api/events                │                        │
              └───────────────────────┬─────────┴────────────────────────┘
                                      │ moli-api (garde Host, origines, sessions PIN, bornes)
 ┌────────────────────────────────────▼──────────────────────────────────────────────┐
 │ moli-runtime : Hub (état, bus, commandes avec échéance, GARDE, approbations,      │
 │                journal, étiquettes, coffre chiffré, images caméras, notices)      │
 └───▲───────────────────────────────▲───────────────────────────────▲───────────────┘
     │ trait Driver                  │ abonnements au bus             │ briques
 ┌───┴──────────────────────┐  ┌─────┴──────────┐   ┌────────────────┴──────────────────┐
 │ pilotes natifs : z2m,    │  │ moli-history   │   │ moli-energy · moli-automation ·    │
 │ hue, tuya, reolink,      │  │ (SQLite)       │   │ moli-assistant (LLM compatible     │
 │ sonos, philips, frigate, │  └────────────────┘   │ OpenAI, outils, sous la garde)      │
 │ bambu, tapo, igd         │                       └────────────────────────────────────┘
 │ + profils déclaratifs    │  moli-net : HTTPS épinglé (TOFU), TLS public, HTTP, UPnP, web()
 │   (catalogue)            │  moli-core : le vocabulaire (aucune entrée/sortie)
 └──────────────────────────┘
```

La carte vivante de ce schéma est dans le dashboard : `#/systeme` (« Comment ça marche »).

## Crates

| Crate | Rôle | Doc |
|---|---|---|
| `moli-core` | domaine pur : identifiants, points, valeurs, événements, origines, journal | [coeur](components/coeur.md) |
| `moli-runtime` | hub, garde, superviseur, journal, étiquettes, coffre, médias | [coeur](components/coeur.md) |
| `moli-net` | HTTPS avec certificat épinglé, TLS public, HTTP, UPnP, requêtes web bornées | [pilotes](components/pilotes.md) |
| `moli-z2m`, `moli-hue`, `moli-tuya`, `moli-reolink`, `moli-sonos`, `moli-philips`, `moli-frigate`, `moli-bambu`, `moli-tapo`, `moli-igd` | pilotes natifs | [pilotes](components/pilotes.md) |
| `moli-profile` + catalogue | moteur de profils déclaratifs (lecture **et écriture** déclarées) | [profils](components/profils.md) |
| `moli-history` | historique SQLite, SQL en lecture seule pour agents | [historique & énergie](components/historique-energie.md) |
| `moli-energy` | kWh et € par heure, Linky + circuits, import HA | [historique & énergie](components/historique-energie.md) |
| `moli-automation` | automatismes en graphe, moteur déterministe, validation humaine | [automatismes](components/automatismes.md) |
| `moli-helpers` | aides de la maison (mode, alarme, budget), comme les `input_*` de HA | [pilotes](components/pilotes.md) |
| `moli-assistant` | Moli : conversation, création d'automatismes, import HA, textes | [moli](components/moli.md) |
| `moli-api` | REST, SSE, MCP, sessions humaines, garde Host, bornes, dashboard embarqué | [api](components/api.md) |
| `moli-os` | binaire : CLI, config TOML, démarrage, sauvegarde, santé | [exploitation](components/exploitation.md) |
| `web/` | dashboard Maison + Atelier (Svelte 5), plan vivant | [dashboard](components/dashboard.md) |

Une crate = une responsabilité. Un pilote ne dépend que de `moli-runtime`, `moli-net` et de son
transport. Les briques s'abonnent au bus ; aucune ne parle directement à un pilote.

## Commandes : le chemin d'un ordre

1. le point existe et est inscriptible ; la valeur est validée contre son `PointSpec` (type,
   plage, **longueur**) ;
2. **garde** : selon l'origine, la pièce et l'heure → exécuté, ou retenu en demande
   d'approbation (202) qu'un humain libère ; le dashboard (`ui`) est toujours obéi ;
3. routage vers le pilote propriétaire avec une **échéance** (5 s) : un ordre que personne
   n'attend plus n'est jamais exécuté, même après une reconnexion ;
4. journal ;
5. la confirmation d'état revient comme un événement `state`.

## Origines et confiance

| Origine | Qui | Garde | Comment on la reconnaît |
|---|---|---|---|
| `ui` | la famille dans le dashboard | jamais retenue | en-tête `x-moli-origin: ui` depuis une adresse du LAN (mode `dashboard = "trusted"`, défaut) ; en mode `"pin"`, une session PIN est exigée |
| `automation` | un automatisme validé par un humain | jamais retenue | lancé par le moteur |
| `system` | Moli lui-même | jamais retenue | interne |
| `api`, `mcp`, `cli`, `assistant` | agents et scripts | retenue en pièce protégée, en heures calmes, sur appareil sans pièce | tout le reste |

**Le PIN** (session liée à l'adresse, 30 jours) sert à : approuver une demande retenue,
valider / allumer / lancer / restaurer un automatisme, importer, supprimer, renommer un
appareil déjà nommé. Jamais à commander.

Le mode `trusted` suppose un réseau domestique de confiance : un appareil compromis sur le
LAN peut commander comme la famille, mais ne peut ni valider un automatisme ni autoriser un
agent. Sur un réseau moins sûr : réseau IoT isolé, ou mode `pin`, ou sessions d'appareil (après MVP).

## Le paquet d'intégration

La forme que prend **toute** intégration, native ou déclarative, dans le catalogue interne
(`integrations/<id>/`) :

```
integrations/daikin-brp069/
├── integration.toml     # manifeste : id, nom, marques, modèles, transport (http|mqtt|…),
│                        #   découverte (mdns, ssdp, ports), identifiants/variables/options
│                        #   requis, origine (intégration HA « daikin », pydaikin, licence),
│                        #   auteur, et la liste des fixtures ([[fixture]] file, request)
├── profile.toml         # profil déclaratif (lectures [[request]], écritures [[write]])
│   — ou —  crate = "moli-daikin"   dans le manifeste, pour un pilote natif
├── fixtures/            # réponses réelles capturées sur l'appareil (anonymisées) ; pour un
│                        #   profil, chacune nomme la requête dont elle est la réponse
├── onboarding.md        # la procédure : questions à poser, appuis physiques, où trouver
│                        #   les identifiants, comment tester ; écrite par l'agent fort,
│                        #   déroulée par l'agent de Moli
└── README.md            # ce qui a été réutilisé, ce qui a été deviné, ce qui reste
```

- `moli-os catalogue check` valide chaque paquet (manifeste, profil, fixtures rejouées).
- Les paquets embarqués dans le binaire sont de confiance. Un paquet externe (fichier)
  n'est chargé qu'après `moli-os profile trust <chemin>` par un humain, qui a vu la liste
  des chemins lus et écrits.
- Un profil déclare ses écritures (`[[write]]`, rendues depuis la dernière lecture) ; elles
  sont exécutées **uniquement** via `hub.command`, donc sous la garde et au journal.
- Tout secret est lié à l'hôte déclaré ; un profil qui utilise un secret doit déclarer `host`.

C'est la forme MVP du catalogue. La forge de terrain (après MVP) produit exactement ces
paquets ; l'onboarding par brique les consomme.

## Décisions (3 octobre 2026)

| # | Décision | Choix | Pourquoi |
|---|---|---|---|
| D0 | Le dashboard et le PIN | `ui` = dashboard sur le LAN, obéi sans code ; PIN réservé à la validation et à l'autorisation ; mode `pin` disponible en config | Demander un code pour éteindre une lumière : inacceptable pour une famille. La sécurité contre un LAN hostile n'est pas l'objectif du MVP. |
| D1 | Qu'est-ce qu'une brique | module compilé **dormant** : aucune tâche, aucune minuterie, aucune mémoire tant qu'il n'est pas configuré ; cycle de vie `start(hub, config, cancel) → handle` ; `stop` = annulation + vidage des files + appareils hors ligne + désinscription | « Une brique inactive ne coûte rien » se vérifie par `0 poll` ; les prérequis de `stop` sont ceux de l'activation à chaud, implémentés dès le durcissement (échéances, drain, hors ligne). |
| D2 | Format des intégrations | **paquet** (ci-dessus) ; déclaratif étendu d'abord (écritures, puis MQTT et websocket), natif pour les transports lourds ; **toute écriture passe par le hub** ; WASM non retenu pour le MVP | La revue a montré qu'un profil écrit : l'isolation qui compte est celle de la garde, pas celle de la mémoire. WASM se réévalue si des paquets non relus doivent tourner chez des tiers. |
| D3 | Activation à chaud | après le MVP ; la config vivante (`data/bricks.json`) et `stop_driver` arrivent avec la page Administration | Choix de l'auteur : pas le système d'agents maintenant ; mais rien dans le code ne doit l'empêcher. |
| D4 | Découverte réseau | module `moli-discover` après le MVP, exposé en outils ; dès maintenant, SSDP n'accepte qu'une adresse privée du même sous-réseau, corps bornés, UDN mémorisé | Une découverte qui accepte n'importe quelle réponse est une porte. |
| D5 | Agent d'onboarding | après le MVP ; règles figées maintenant : noms d'appareils bornés et filtrés, identité native affichée à toute validation, secrets saisis par l'humain directement dans le coffre, l'agent ne choisit jamais une URL | Un agent peut rendre une phrase de validation mensongère ; `web()` vers `127.0.0.1` donnerait les droits `cli`. |
| D6 | La forge et le catalogue | **forge de terrain** : un agent crée le paquet dans la maison, contre le vrai matériel, en lisant HA/HACS/docs, capture les traces, et le propose au catalogue commun ; licences HACS au cas par cas (HA core est Apache-2.0) | Création sur place = validation sur l'appareil réel ; le catalogue évite que chaque maison soit un cas unique. |
| D7 | Au-delà d'une maison | après le MVP ; prérequis listés : bornes et débit (durcissement), HTTPS local, sessions d'appareil | Ce qu'il faut avant d'exposer Moli plus largement que le réseau de la maison. |
| D8 | Budget par brique | `tokio-metrics` `TaskMonitor` par tâche supervisée (stable, ~200 o), part de CPU et `0 poll` affichés sur la page Système ; `cache_size` SQLite explicite | Le moins cher qui prouve « inactif = zéro ». |
| D9 | Transports standards | évaluer **Matter** (`rs-matter`) comme transport du cœur après le MVP ; liste de marques recommandées ; la flexibilité (rétro-ingénierie) reste | Réduire la longue traîne au lieu de la courir, sans fermer la porte. |
| D10 | Matériel cible | Pi Zero 2 W (ou équivalent ARM 512 Mo), binaire seul sans Docker ; compiler pour ARM, puis mesurer | 30 € contre 150 € par installation. |
| D11 | Le plan de la maison | **la maison est une donnée** (`data/plan.json` : par étage, taille, murs, pièces, ouvertures, éléments, appareils, en cm ; altitude et décalage pour empiler les étages), **le rendu est un habillage** : 2D en SVG (zoom, 4 habillages) et 3D (three.js, chargée à la demande) depuis la même donnée. Un agent écrit la donnée à partir d'une photo du plan (`scripts/plan-from-image.py` extrait les murs) ; la personne ajuste au doigt. Un étage peut rester une simple image (ancien mode). | Le propriétaire veut zoomer, changer l'aspect, ajouter des choses, voir en 3D : impossible sur une image de fond. Une donnée géométrique sert aussi plus tard (présence par pièce, trajets, éclairage par zone). |

### D12 — Les lumières : luminaires, groupes, interrupteurs (6 octobre 2026)

Constat sur une installation réelle : une même lumière apparaît trois fois (le relais
derrière l'interrupteur, chaque ampoule, la pièce Hue), éteindre « le salon » a pu couper la
télé et l'enceinte, et une ampoule coupée au mur reste « allumée » (le pont Hue garde son
dernier état).

| Sujet | Règle |
|---|---|
| **Lampe** | ce qui éclaire : une ampoule connectée, ou un relais qui alimente une lumière ordinaire. |
| **Hors tension** | une lampe injoignable n'éclaire pas : jamais comptée allumée, jamais de halo ; l'écran dit « coupée à l'interrupteur » (rallumer l'interrupteur, pas Moli). |
| **Luminaire** (`[[fixture]]` dans `moli.toml`, pilote interne `lumieres`) | plusieurs ampoules et/ou le relais qui les alimente, vus et pilotés comme **une** lampe (`lumieres:<id>` : `on`, `powered`, `brightness`, `color_temp`, `color`). Ses membres disparaissent des vues de la famille (ils restent dans l'Atelier et dans la fiche du luminaire, pour les effets ampoule par ampoule). |
| **Ordres d'un luminaire** | les ampoules d'abord, le relais en dernier recours : éteindre = éteindre les ampoules joignables (le relais reste alimenté, les ampoules restent pilotables) ; le relais ne coupe que si aucune ampoule ne répond. Allumer = remettre le relais s'il est coupé (réponse dès que le relais a obéi), puis rallumer et régler les ampoules quand elles sont revenues (≤ 15 s, en tâche de fond). |
| **Groupe** (pièce ou zone Hue) | pas une lampe : la commande « toute la pièce ». Allumé si une de ses ampoules joignables l'est (le pont compte les ampoules coupées au mur). Pas de pastille sur le plan. |
| **Une pièce** | « éteindre la pièce » = ses lampes et luminaires, **jamais** la télé, l'enceinte ni les prises. |
| **Garde** | un ordre au luminaire est jugé sur les pièces de ses membres ; les ordres qu'il donne ensuite aux membres sont ceux du système (déjà jugés). |

## Sécurité (réseau de la maison + Cloudflare Access)

Garde `Host` sur toutes les routes (anti DNS-rebinding). Pas de compte : la famille est
reconnue par son réseau, ou par Cloudflare Access quand elle passe par le tunnel
(par exemple `maison.example.org`, assertion vérifiée par Moli lui-même). **Tout autre appelant est
refusé (403)**, y compris un appareil du réseau qui imite les en-têtes de Cloudflare : ils
ne comptent que venus du tunnel, donc de la machine elle-même. Les actes qui engagent
(validation, autorisation, import, plan) exigent une session PIN liée à l'appelant ; les
agents sont soumis à la garde. Bornes de taille et de débit sur toutes les entrées ; les
gros envois sont refusés avant d'être lus si personne n'est identifié. Avant une exposition
plus large (appli mobile, accès distant) : sessions d'appareil.

## Crochets réservés (à ne pas casser)

- `DriverConfig { id, kind, enabled, options }` et `build_driver` : la page Administration
  remplacera le `match` par le catalogue, et `moli.toml` par `data/bricks.json` pour la
  config vivante. Garder `moli.toml` pour la config machine.
- `Hub::register_driver` / `set_driver_status` : `unregister_driver(instance)` viendra avec
  l'activation à chaud ; ne pas supposer qu'une instance vit toujours.
- `/api/system` : la liste des briques y est écrite à la main ; à rendre générique quand les
  briques auront un trait.
- `Action` du journal : extensible (les automatismes y ont été ajoutés) ; l'astreinte lira ce
  journal.
- `DriverStatus::Backoff { error, retry_in_ms }` : c'est le signal de l'astreinte.

## Décisions d'origine revisitées

- « Pas de WASM (trop lourd) » (2 oct.) : confirmé pour le MVP, pour une autre raison (D2).
- Transports natifs en Rust + profils déclaratifs pour la longue traîne : confirmé, avec les
  écritures déclarées et le paquet comme unité.

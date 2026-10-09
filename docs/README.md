# Moli OS : la doc

Moli OS est une plateforme domotique écrite en Rust, née dans une maison habitée, qui
remplace Home Assistant. Elle est pensée pour une famille qui veut que ça marche, et pour
des agents (Moli, Claude, Codex) qui configurent, créent les automatismes et écrivent les
intégrations. Ces agents ne sont jamais aux commandes en temps réel.

## Où on en est (4 octobre 2026)

Mesures sur une installation réelle, à côté de son Home Assistant (détail dans
[PERFORMANCE.md](PERFORMANCE.md)) :

| | Moli OS | Home Assistant (même machine) |
|---|---|---|
| Mémoire (pages anonymes du conteneur) | **4 à 7 Mio** selon les clients connectés | 2,15 Gio |
| CPU au repos | **0,24 à 0,3 %** | 1,5 à 2,3 % |
| Image | **17 Mo** | 2,3 Go |
| Échelle | 77 appareils (22 pilotes) | 171 appareils (1 591 entités) |

Ce qui marche aujourd'hui :
- 24 intégrations : Zigbee (Zigbee2MQTT), Hue, Tuya (local et cloud), Reolink, Sonos (et ses
  annonces vocales), télé Philips, Frigate, Bambu (et son AMS), Klipper, Tapo, box UPnP,
  imprimante IPP, l'hôte et les ordinateurs, la présence par Wi-Fi, les aides de la maison,
  **Telegram** (votre bot, sans relais), les **téléphones** (appli Moli), et des profils
  déclaratifs (Daikin, Meross, Open-Meteo, Tempo, iopool) ;
- la garde (pièces protégées, heures calmes), l'historique et l'énergie ;
- le dashboard « Maison », avec des **filtres par type** et le **plan vivant** : la maison
  redessinée en données, en 2D zoomable à 4 habillages et en **3D** ;
- Moli, l'assistant ; les automatismes (graphe, validation humaine), avec les canaux
  maison, voix, **téléphone** et Telegram ;
- l'**appli mobile « Moli »** (iOS, TestFlight) : le dashboard dans l'appli, et le téléphone
  devient un capteur de la maison (à la maison ou non, batterie, Wi-Fi, position) et reçoit
  les notifications.

Sur le réseau local, Moli OS répond sur `http://<hôte>:8790`. Depuis l'extérieur, il peut
passer par un tunnel Cloudflare protégé par Cloudflare Access (par exemple
`https://maison.example.org`, réservé aux e-mails du foyer) ; seuls les rapports des
téléphones passent sans Access, avec leur propre jeton.

## Par où commencer

| Je veux… | Lire |
|---|---|
| installer Moli chez soi (Docker, code d'installation, premiers appareils) | [components/installation.md](components/installation.md) |
| comprendre comment c'est construit, et les décisions prises | [ARCHITECTURE.md](ARCHITECTURE.md), puis la carte vivante `#/systeme` du dashboard |
| l'appli mobile (`mobile/`) | [components/api.md](components/api.md) |
| connaître la règle de performance et les mesures | [PERFORMANCE.md](PERFORMANCE.md) |
| compiler, déployer, opérer, gérer les secrets | [components/exploitation.md](components/exploitation.md) |

## Les composants

| Doc | Contenu |
|---|---|
| [coeur.md](components/coeur.md) | vocabulaire, hub, bus, garde et approbations, sessions PIN, journal, étiquettes, coffre des secrets, superviseur |
| [pilotes.md](components/pilotes.md) | trait `Driver`, chaque pilote natif, `moli-net` |
| [profils.md](components/profils.md) | moteur de profils déclaratifs, format TOML, ajouter un appareil sans code |
| [historique-energie.md](components/historique-energie.md) | historique SQLite, SQL pour agents, énergie (kWh, €, Tempo), import HA |
| [automatismes.md](components/automatismes.md) | modèle de graphe, validation humaine, moteur, éditeur, import HA |
| [moli.md](components/moli.md) | l'assistant : conversation, outils, garde, création d'automatismes, filtres |
| [api.md](components/api.md) | REST, SSE, MCP, CLI, sécurité des routes |
| [dashboard.md](components/dashboard.md) | interface Maison et Atelier (Svelte 5), carte vivante |
| [exploitation.md](components/exploitation.md) | image Docker, clé maître, sauvegarde, mise à jour, développement |
| [installation.md](components/installation.md) | premier démarrage, code de la maison, clé maître, premiers appareils, accès hors de chez soi |
| [traduction.md](components/traduction.md) | catalogues `locales/`, clés, pluriels, ajouter une langue, contrôle |

## Les règles à ne jamais oublier

- **La famille commande sans friction** ; le PIN sert à valider un automatisme et à autoriser
  un agent, jamais à commander (décision D0).
- **Pièces protégées** (là où dort un enfant) : aucun agent n'y agit sans un humain. Les
  tests physiques se font sur une cible neutre (jamais une chambre), un humain présent, en
  remettant l'état comme avant.
- **Performance** : chaque déploiement est mesuré contre HA. Le budget est dans
  [PERFORMANCE.md](PERFORMANCE.md).
- **Aucun secret en clair** : ils vont dans le coffre chiffré de Moli, dont la clé maître
  arrive par l'environnement (de préférence un fichier tmpfs fourni par un gestionnaire de
  secrets). Ne jamais régénérer `MOLI_MASTER_KEY`.

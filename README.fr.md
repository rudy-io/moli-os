# Moli OS

Une domotique ultra-légère pensée pour les agents, écrite en Rust. [English version](README.md).

Home Assistant a été conçu pour une personne qui clique dans une interface. Moli OS est conçu
pour que des agents (Claude Code, Codex, n'importe quel modèle derrière une API) le
configurent, le pilotent, le dépannent et finissent par **écrire les pilotes**, sans jamais
être dans la boucle de contrôle en temps réel. Le binaire est entièrement déterministe.

Moli est né comme la domotique d'une famille, construite avec des agents, et il est partagé tel
quel : libre d'utilisation, de modification et de redistribution (Apache-2.0). Aucun support
n'est promis ; les signalements et les contributions sont bienvenus
([`CONTRIBUTING.md`](CONTRIBUTING.md)). L'interface parle français et anglais.

Mesuré le 4 octobre 2026, à côté d'un vrai Home Assistant sur la même machine (détail et
historique : [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)) :

| | Moli OS | Home Assistant |
|---|---|---|
| Mémoire (pages anonymes du conteneur) | 6,1 Mio (15,9 Mio RSS) | 2,1 Gio |
| Processeur au repos | 0,36 % | 1,5 % |
| Image | 17,5 Mo (`FROM scratch`) | 2,3 Go |

## Ce qu'il y a dedans

- **Le cœur** : des valeurs typées avec leurs unités, des identités stables (jamais tirées des
  noms), les noms et pièces séparés de l'identité, un bus d'événements en direct, des pilotes
  supervisés (un pilote qui plante redémarre, le cœur ne tombe jamais), le journal de chaque
  écriture, le dernier état connu restauré avec son âge, un coffre chiffré pour les secrets, et
  une garde qui retient les ordres des agents dans les pièces protégées pour qu'une personne
  les valide.
- **24 intégrations** (`integrations/`) : Zigbee2MQTT, Hue, Sonos, Tuya, Reolink, Frigate,
  télés Philips, Bambu Lab, Klipper, Tapo, imprimantes IPP, box UPnP, présence, Telegram,
  téléphones, et des profils déclaratifs (Daikin, Meross, Open-Meteo, Tempo, iopool…).
- **Le tableau de bord** (Svelte 5), embarqué dans le binaire : pièces, lumières, climat,
  caméras, énergie, plan de la maison en 2D et en 3D.
- **Moli, l'assistant** : conversation (texte ou voix) avec un modèle compatible OpenAI, qui agit
  sous la garde.
- **Les automatismes** : des graphes dessinés dans un éditeur visuel ou par Moli, exécutés par un
  moteur déterministe, jamais actifs avant qu'une personne valide la version exacte.
- **L'appli mobile** (`mobile/`, Expo) : le tableau de bord, les notifications, le téléphone comme
  capteur.
- **Les surfaces**, sur un seul port : REST, événements (SSE), **MCP** (HTTP). Liste des hôtes
  autorisés sur chaque route.

## Lancer

```sh
git clone https://github.com/rudy-io/moli-os && cd moli-os
mkdir -p data && sudo chown 1000:1000 data   # l'image tourne en 1000:1000
docker compose up -d --build
docker compose logs moli-os                  # le code d'installation, au premier démarrage
```

Ouvrir `http://<adresse>:8790`, choisir la langue, taper le code d'installation et choisir le
code de la maison. Ensuite, ajouter ses appareils dans `data/moli.toml` (un `[[driver]]` par
intégration). Guide : [`docs/components/installation.md`](docs/components/installation.md).

**Avec un agent** : ouvrir ce dépôt dans Claude Code et lui demander d'installer Moli : le skill
`installer-moli` (`.claude/skills/`) vous accompagne, vos codes et secrets toujours tapés par
vous. Les autres agents lisent [`AGENTS.md`](AGENTS.md). Une fois lancé, un agent se branche
sur `http://<adresse>:8790/mcp`.

## Développer

Aucune chaîne Rust à installer : voir `AGENTS.md`. Architecture : [`docs/`](docs/README.md).
Traduire : [`docs/components/traduction.md`](docs/components/traduction.md).

## Remerciements

Moli OS est une implémentation indépendante : il ne contient aucun code des projets ci-dessous.
Il leur doit la connaissance des protocoles, des vérifications d'interopérabilité ou des données.

- Home Assistant : la maison à côté de laquelle Moli OS a grandi ; les outils d'import lisent une
  installation existante.
- tinytuya et localtuya : le protocole local Tuya ; trames vérifiées contre leur sortie.
- python-kasa : la session KLAP de TP-Link, vérifiée contre sa sortie.
- pydaikin, meross_lan / MerossIot, ha-bambulab, androidtvremote2 : documentation des protocoles.
- Zigbee2MQTT, Frigate, Moonraker : leurs API documentées.
- Powercalc : profils de consommation mesurés des lampes (voir `NOTICE`).
- Données météo : [Open-Meteo.com](https://open-meteo.com/) (CC BY 4.0).

Les noms de produits sont des marques de leurs propriétaires. Moli OS n'est ni affilié à eux ni
approuvé par eux, ni par le projet Home Assistant.

Licence : Apache-2.0 (`LICENSE`, `NOTICE`).

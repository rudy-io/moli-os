# Performances : la règle

Moli OS n'a de sens que s'il reste **très loin devant Home Assistant**, brique après brique.
Chaque livraison est mesurée : après le déploiement, un script côté hôte, **hors de ce
dépôt**, compare Moli à HA sur la même machine, au même moment, et signale tout dépassement
de budget ou toute dégradation de plus de 20 % depuis la mesure précédente. Il ajoute une
ligne JSON par mesure à `data/perf.jsonl` ; Moli ne fait que lire ce fichier (les 20
dernières lignes, telles quelles, servies par `/api/system` et affichées sur la page
Système). Les mesures ci-dessous viennent d'une installation réelle, à côté de son Home
Assistant.

**Une alerte se corrige avant de passer à la suite**, ou s'explique noir sur blanc ici.

## Budget

### Build actuel — pile locale et familles étendues, 6 novembre 2026

Les variantes de production construites depuis le dépôt Moli seul mesurent :

| Variante x86_64 musl | Image non compressée | Binaire |
|---|---:|---:|
| Standard avec symboles | 30 268 337 octets | 30 264 240 octets |
| Compacte explicite | **24 954 289 octets** | 24 949 904 octets |

La compacte respecte 25 Mo avec 45 711 octets de marge. Le standard reste au-dessus
pour conserver les symboles. Les nouvelles fonctions et la source de la pile sont
dans Moli ; la copie des sources n'ajoute pas de service ni de dépendance Git.
Les deux binaires passent les essais protocolaires étendus. Diagnostics correspond
au binaire standard. Hash et reproduction : `tools/matter-e2e/RESULTS.md`.

Sur 29,79 s au repos avec les familles virtuelles : 17 280 Kio RSS, 3 544 Kio anonymes,
0,067 % CPU d'un cœur à la résolution des ticks. Il s'agit du processus, pas du
budget cgroup, d'un essai de charge, d'une mesure radio ou d'une comparaison HA.
Les mesures historiques ci-dessous restent rattachées à leurs versions précédentes.

### Contribution Matter expérimentale — 6 novembre 2026

Image de production construite avec le Dockerfile du dépôt, Rust 1.99.1 musl
x86_64, dashboard embarqué et contrôleur Matter épinglé : **30 113 713 octets**
non compressés ; binaire : **30 109 368 octets**. Cela **dépasse le budget de
25 Mo**. Comparaison construite séparément avec le Dockerfile inchangé de la
base `9ce5868c7832f1e97010f84ffb7e33d7295840f4`, même architecture et toolchain :
**27 737 521 octets** sans Matter (binaire **27 733 136 octets**). L'ajout de la
contribution représente donc **2 376 192 octets (+8,57 %)** pour l'image.
La base dépasse déjà 25 Mo ; la contribution accroît ce dépassement. Le budget
reste non résolu : réduction/découpage ou arbitrage explicite avant livraison stable.
SHA-256 du binaire de référence :
`a435b60f0b76b50cff5a4383ac1976abe90015f777032b716b9ebdbc3e9743ae`.
Build final de la contribution : `d6add50616518a84cdb139ba01a58e6871f7ccaf`,
avec le correctif d’attestation et les noms bridgés. Hash du binaire et reproduction :
`tools/matter-e2e/RESULTS.md`.

Le binaire extrait de cette image a passé l'interopérabilité avec une lampe
virtuelle matter.js, puis une fenêtre au repos de 29,78 s après redémarrage :
RSS maximal échantillonné 17 324 Kio, mémoire anonyme du processus 3 460 Kio,
CPU 0,000 % d'un cœur à la résolution des ticks Linux. Ce sont des mesures
**du processus**, pas du cgroup : elles ne certifient pas les budgets ci-dessous.
Aucun dashboard connecté pendant cette fenêtre ; un seul appareil virtuel,
ni BLE ni OTBR. Pas de comparaison à Home Assistant, de p95 API, de mesure radio
ou de test de charge multi-appareils. Reproduction et hash du binaire :
`tools/matter-e2e/RESULTS.md` (`E2E_MEASURE=1`).

### Variante compacte facultative et compatibilité minimale — 6 novembre 2026

Les deux variantes ci-dessous utilisent les mêmes sources/fonctionnalités, après
les corrections de compatibilité Rust 1.89 ; seule l'option de symboles diffère :

| Variante x86_64 musl | Image non compressée | Binaire |
|---|---:|---:|
| Standard, symboles conservés (défaut inchangé) | 30 113 712 octets | 30 109 520 octets |
| Compacte, table de symboles retirée | **24 827 312 octets** | 24 822 928 octets |

La variante compacte respecte 25 Mo avec une marge mesurée de 172 688 octets.
Elle conserve tous les pilotes et le mécanisme de déroulement/interception des panics,
mais les backtraces locales perdent les noms de fonctions. Les sections de code,
données constantes et métadonnées d'exception/unwind sont identiques octet pour octet.
Le target `diagnostics` exporte le binaire avec symboles : son SHA-256 correspond
exactement à celui du build standard. Il n'est pas inclus dans l'image compacte.
Le choix du profil par défaut reste un arbitrage de diagnostic/exploitation.
L'application complète passe `cargo check --locked -p moli-os` avec Rust 1.89.0,
fonctionnalités par défaut. Deux match guards trop récents ont été remplacés par
des expressions équivalentes ; le contrôle de pièce de l'assistant reste avant
l'envoi d'une commande. Les tests existants et une revue ciblée couvrent ce point.

Le binaire compact a passé le même test indépendant matter.js : commissioning avec
attestation, Descriptor, OnOff confirmé par l'accessoire, abonnements, reprise après
redémarrage et refus des états corrompus/mauvaise clé. Mesure indicative de processus
avec une lampe virtuelle sur 29,78 s : 17 340 Kio RSS, 3 476 Kio anonymes, CPU 0,000 %
à la résolution des ticks. Ce n'est pas une certification du budget cgroup ni radio.
Commandes de build/export : `docs/components/exploitation.md` ; hash et résultat :
`tools/matter-e2e/RESULTS.md`.

### Objectifs du projet

| Mesure | Budget | Pourquoi |
|---|---|---|
| Mémoire (mémoire **anonyme** du cgroup : tas et piles, aucun cache de fichiers ; **la même définition pour Moli et HA** ; le RSS de Moli est noté à côté) | ≤ 32 Mio (plafond conteneur 128) | HA : ~2,1 Gio. Rester sous 1/50. |
| CPU **au repos** (compteur du cgroup sur 30 s, aucun client connecté) | ≤ 0,5 % | HA : 1,5–2,5 %. Rien ne tourne pour rien. Une mesure faite avec un dashboard ouvert est notée « en usage » (nombre de connexions) et n'est pas jugée contre ce budget. |
| Image | ≤ 25 Mo | HA : 2,3 Go. |
| API, p95 | ≤ 25 ms (listes), ≤ 5 ms (santé) | Le dashboard doit répondre au doigt. |
| Démarrage | ≤ 2 s jusqu'à « sain » | Un redémarrage ne doit rien coûter. |

## Principes de conception qui tiennent le budget

- Rien ne se recalcule « au cas où » : ce qui sert à chaque seconde ou à chaque événement
  est mis en cache et recalculé seulement quand il change (ex. : la liste des automatismes
  actifs, voir ci-dessous).
- Une brique inactive ne coûte rien : pas de tâche, pas de minuterie, pas de mémoire.
- Les pilotes écoutent (MQTT, SSE, websockets) plutôt que d'interroger quand l'appareil le
  permet ; sinon intervalle le plus long qui reste utile.
- Le LLM n'est jamais sur un chemin chaud : seulement à la demande d'une personne ou d'un
  automatisme validé (texte).

## Historique

| Date | RSS | CPU | Image | Note |
|---|---|---|---|---|
| 2026-10-02 | 4,3 Mio | 0,00 % | 5,5 Mo | MVP, 7 appareils Zigbee |
| 2026-10-03 | 7,9 Mio | — | — | 38 appareils |
| 2026-10-03 | 12,5 Mio | **0,58 %** | 12 Mo | 71 appareils, 87 brouillons d'automatismes : **régression** |
| 2026-10-03 | 12,0 Mio | 0,25 % | 12 Mo | Corrigé : le moteur hachait (SHA-256) chaque brouillon à chaque seconde et à chaque événement pour savoir lesquels sont actifs ; la liste active est désormais en cache. HA au même instant : 2,14 Gio, 2,4 % CPU. |
| 2026-10-03 | 12,2 → 12,7 Mio | 0,51 → **0,23 %** | 12,1 Mo | Première alerte automatique (0,51 % > 0,5) : mesurée 15 s après le redémarrage, pendant la connexion des pilotes. À froid : 0,23 %, HA 1,92 % (÷8), mémoire ÷360. La mesure attend désormais 2 min 30 de fonctionnement (régime normal). |
| 2026-10-03 | 12,6 Mio | **2,09 %** (en usage) | 12,2 Mo | Alerte expliquée : un tableau de bord resté ouvert montrait une caméra **Reolink en direct** (une image par seconde, 430 Ko/s). Mesuré au cgroup : une image Reolink/s coûte **+1,45 %** (~15 ms par image), une image Frigate/s +0,04 %, un client SSE de plus ~0. Pas une régression du déploiement (le changement ne touche que le traitement des requêtes). Conséquences : le script de mesure prend le CPU au cgroup sur 30 s et note les clients connectés (une mesure « en usage » n'est plus jugée contre le budget de repos) ; le chemin d'image Reolink est à optimiser (connexion réutilisée, cache d'une seconde partagé entre clients). |
| 2026-10-04 | 12,2 Mio (5,1 cgroup) | 0,42 % | 15,7 Mo | Deux dérives attrapées par la mesure : `/api/automations` p95 28,4 ms (> 25) car la recherche des enceintes copiait tout le modèle pour chacun des 87 automatismes → lecture sans copie, capacités calculées une fois par liste (13,5 ms) ; CPU au repos 0,42 % dont ~0,13 % pour le healthcheck Docker (39 ms par appel, toutes les 30 s) → retiré, le chien de garde interroge `/api/health` depuis l'hôte. Image +3 Mo : symboles gardés (`strip = "debuginfo"`). |
| 2026-10-04 | 12,3 Mio | 0,24 % | 15,8 Mo | Fausse alerte « mémoire +39 % » : la mesure façon `docker stats` compte le cache de fichiers actif, qui variait de 4,9 à 8,5 Mio selon ce qui venait d'être lu, RSS stable à 12,3 Mio. La mémoire mesurée est désormais la mémoire anonyme du cgroup, des deux côtés. |
| 2026-10-04 | 4,0 Mio anonyme (12,6 RSS) | 0,24 % | 16,1 Mo | Corrections de la contre-relecture (inconnus refusés, budget de codes, file d'annonces, sauvegarde du plan). Rien de mesurable : HA 2 138 Mio, 1,5 % (mémoire ÷534, CPU ÷6). p95 `/api/automations` 12,5 ms. |
| 2026-10-04 | — | — | 6,5 Mo | arm64 : même `Dockerfile` en `linux/arm64` (musl, `ring`, `rusqlite` bundled) : compile, 21 min sous émulation QEMU sur un PC x86. Pas encore lancé sur un Pi. |
| 2026-10-04 | 4,2 → 4,8 Mio anonyme | 0,33 % | 16,2 Mo | Pilote Telegram natif. Alerte `/api/automations` p95 27,5 ms (> 25) **passagère** : `/api/energy`, non touché, avait doublé au même instant (hôte chargé). Remesure 3 min après : 14,1 ms et 7,6 ms, budget OK. |
| 2026-10-04 | 7,0 → 7,2 Mio anonyme (16,9 RSS) | 0,26 % (en usage) | 17,0 Mo | Pilote téléphones + appli mobile. Alerte « mémoire +43 % » mesurée avec **11 à 12 clients connectés** (réseau local et tunnel : appli mobile, tableaux de bord), bien plus que d'ordinaire ; latences `/api/automations` 28,4 ms à la 1re mesure (hôte chargé, charge 1,7), 14,8 ms à la remesure. Mémoire suivie 2 min 30 : 7,2 Mio stable (7 à 11 connexions), pas de croissance. **À revérifier au repos** (mesure de nuit) : si elle reste au-dessus de 5 Mio sans client, chercher. |
| 2026-10-04 | 6,1 Mio anonyme (15,4 RSS) | 7 % → **0,32 %** | 17,2 Mo | Ambiances, cadran de clim, conseils d'énergie. Alerte au déploiement expliquée : un onglet de vérification était resté sur l'Accueil (image de la sonnette rafraîchie, comme le 3 oct.) : 4 à 9 % tant qu'il était ouvert, 0,1 à 0,5 % une fois fermé. Remesure : budget OK, `/api/energy` p95 7,9 ms. |
| 2026-10-04 | 6,1 Mio anonyme (15,9 RSS) | 0,36 % | 17,5 Mo | Alerte `/api/automations` p95 35,1 ms (> 25), mesurée pendant le démarrage ; hors démarrage 16,4 ms, en hausse depuis 12 ms avec 10 brouillons de plus (103 automatismes). Profil local sur les vraies données : l'**empreinte** (graphe sérialisé puis SHA-256) recalculée 4 fois par automatisme et par listing était le gros du temps, puis la vérification qui copiait tout l'état de chaque appareil cité. Corrigé : `Hub::describe` (description + étiquette, sans l'état), une seule empreinte par vue, puis **empreintes gardées entre deux écritures** (oubliées sous le verrou d'écriture ; valider et lancer recalculent toujours). `/api/automations` **16,4 → 8,5 ms** (p95 9,6). |

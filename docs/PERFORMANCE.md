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

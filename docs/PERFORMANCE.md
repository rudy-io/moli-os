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

### Contribution Matter expérimentale — 5 novembre 2026

Image de production construite avec le Dockerfile du dépôt, Rust 1.99.1 musl
x86_64, dashboard embarqué et contrôleur Matter épinglé : **30 078 897 octets**
non compressés ; binaire : **30 074 832 octets**. Cela **dépasse le budget de
25 Mo**. Comparaison construite séparément avec le Dockerfile inchangé de la
base `9ce5868c7832f1e97010f84ffb7e33d7295840f4`, même architecture et toolchain :
**27 737 521 octets** sans Matter (binaire **27 733 136 octets**). L'ajout de la
contribution représente donc **2 341 376 octets (+8,44 %)** pour l'image.
La base dépasse déjà 25 Mo ; la contribution accroît ce dépassement. Le budget
reste non résolu : réduction/découpage ou arbitrage explicite avant livraison stable.
SHA-256 du binaire de référence :
`a435b60f0b76b50cff5a4383ac1976abe90015f777032b716b9ebdbc3e9743ae`.
Ces chiffres concernent les builds de taille documentés, avant l'ajout du diagnostic
d'appairage ; ce dernier n'a pas fait l'objet d'une nouvelle mesure de release.

Le binaire extrait de cette image a passé l'interopérabilité avec une lampe
virtuelle matter.js, puis une fenêtre au repos de 29,78 s après redémarrage :
RSS maximal échantillonné 17 156 Kio, mémoire anonyme du processus 3 484 Kio,
CPU 0,000 % d'un cœur à la résolution des ticks Linux. Ce sont des mesures
**du processus**, pas du cgroup : elles ne certifient pas les budgets ci-dessous.
Aucun dashboard connecté pendant cette fenêtre ; un seul appareil virtuel,
ni BLE ni OTBR. Pas de comparaison à Home Assistant, de p95 API, de mesure radio
ou de test de charge multi-appareils. Reproduction et hash du binaire :
`tools/matter-e2e/RESULTS.md` (`E2E_MEASURE=1`).

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

## 2026-11-02 — observateur Somfy cloud, essai synthétique ARM64

Comparaison locale sur le même hôte Docker Desktop ARM64, sans compilation ni client
UI sur les instances mesurées. Il ne s'agit ni d'une mesure de la maison réelle,
ni d'une comparaison simultanée avec Home Assistant, ni d'une qualification Freebox.
Les [résultats bruts](performance/somfy-cloud-2026-11-02.json) conservent les octets,
les durées, les identités des images et les conditions de mesure.

Source de référence : `d81b2be13ec6a5a3496c29617f2706f6f37a0f1c`.
Source construite : `9183b39530e81fda75c2e0b818441698854f4838`, archivée avec
`git archive` avant le build ; les modifications documentaires ultérieures ne sont
pas embarquées. Même Dockerfile et mêmes bases que la référence :
`node:26-alpine@sha256:c6ad9e9c1d80f6eb607c26b4b0a132ff7a2344fad1c78ea3baa2a7d4c8de6d94`
et `rust:1-alpine@sha256:7e9e2d068a107a2adafd19930011178d14a78326053cd4c3a9f7ad3fd049557d`.
Image construite : `sha256:e746cb39820381d5159e74761e1fcfc6c7505c3a06cc50e3fc81183b98742511`.

Les trois conteneurs utilisent deux workers et un plafond de 128 Mio. Après au moins
150 s de stabilisation, le CPU au repos est mesuré sur 30 s ; les mesures sont
séquentielles. Chaque p95 est le 48e résultat de 50 requêtes triées. Les durées HTTP
incluent la boucle locale hôte → VM Docker et le transfert de la réponse ; le
démarrage va de l'appel `docker start` à la première réponse de santé valide.
La mémoire est l'`anon` du cgroup, avec RSS des processus indiqué séparément.

Le cas catalogue contient 1 000 030 octets sérialisés, 120 définitions dans deux
collections et quatre collections non observées. Sa session fictive exige une
reconnexion : aucun jeton fournisseur ni demande de connexion n'est présent. Le
coffre est chiffré avec une clé de test. Un réseau Docker dédié publie uniquement
des ports loopback ; les hôtes Somfy fixes et le DNS configuré pointent vers
loopback, et les proxies sont vidés. Aucun compte réel n'est utilisé.

| Série | Charge | Anonyme (Mio) | RSS (Mio) | CPU repos (%) | Santé p95 (ms) | Catalogue p95 (ms) | Démarrage (s) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | Référence vide | 2.16 | 11.59 | 0.0350 | 2.61 | — | 0.144 |
| 1 | Nouvelle image, cloud omis | 2.20 | 12.01 | 0.0241 | 2.89 | — | 0.132 |
| 1 | Catalogue fictif proche de 1 Mio | 4.46 | 14.88 | 0.0337 | 5.72 | 26.14 | 0.120 |
| 2 | Référence vide | 2.23 | 11.58 | 0.0303 | 2.50 | — | 0.148 |
| 2 | Nouvelle image, cloud omis | 3.55 | 13.28 | 0.0226 | 1.41 | — | 0.103 |
| 2 | Catalogue fictif proche de 1 Mio | 5.36 | 15.89 | 0.0379 | 2.25 | 14.93 | 0.100 |

La première série a déclenché deux alertes sur la charge maximale : santé p95
**5.72 ms** (> 5) et catalogue **26.14 ms** (> 25).
La deuxième série, sans changement de code ni de recette, donne respectivement
**2.25 ms** et **14.93 ms**.
Les deux séries sont conservées ; leur variation ne permet pas d'attribuer les
écarts à une cause précise. Une mesure comparable sur l'hôte cible reste nécessaire.

Après les 50 lectures privées de 1 000 307 octets chacune,
la mémoire anonyme est de **5.48 Mio**
et le RSS de **16.08 Mio** lors du second relevé.
Les fenêtres CPU comprennent une pause de 30 s, puis la collecte Docker : leur durée
observée est d'environ 31 s, enregistrée dans les résultats bruts.

Les seuils mémoire, CPU, santé, catalogue et démarrage sont respectés dans la deuxième série. Cela ne fait pas disparaître les alertes du premier relevé ni le dépassement du budget image ci-dessous.

**Variation relative de mémoire encore à expliquer.** Sans changement de code,
la nouvelle image sans cloud passe de 2,20 à 3,55 Mio anonymes entre les deux
séries (+61 %), et le cas catalogue de 4,46 à 5,36 Mio (+20 %). La référence varie
de 2,16 à 2,23 Mio. Les plafonds absolus restent respectés, mais ces variations
interdisent de conclure à une absence de régression mémoire. Leur cause n'est pas
isolée dans ce banc Docker Desktop ; un profilage comparable sur la cible reste
ouvert. Les tests d'absence de tâche cloud ne remplacent pas cette mesure globale.


**Budget image dépassé, déclaré explicitement.** La taille Docker locale non
compressée passe de **25 711 448 à 26 409 536 octets** : **+698 088 octets (+2,72 %)**.
La nouvelle image vaut **26,410 Mo décimaux / 25,186 Mio** ; elle dépasse donc aussi
bien 25 000 000 octets que 25 Mio. La référence dépassait déjà 25 Mo décimaux. Les
options release et les symboles conservés (`strip = "debuginfo"`) restent inchangés.
Aucun nouveau paquet runtime externe n'est ajouté ; le supplément comprend le protocole, le catalogue,
l'API et l'interface cloud. Aucune conformité au budget image n'est revendiquée.
Une optimisation de taille reste nécessaire pour revendiquer ce budget ; ce
dépassement documenté n'empêche pas l'essai isolé en lecture seule.

Les essais prouvent le comportement borné de ces charges synthétiques. Ils ne
mesurent pas le coût réseau d'un cycle Somfy authentifié, un redémarrage sur la
Freebox, une coupure secteur ou l'exhaustivité des programmes du fournisseur. Le
cas cloud désactivé est complété par les tests qui vérifient l'absence de tâche,
de minuterie et de requête cloud lorsque l'option est omise.

## 2026-11-02 — lecture des programmations TaHoma dans le navigateur

La vue lisible réutilise le catalogue déjà chargé : aucun nouvel appel API, minuteur,
paquet ou changement de tâche cloud. Un modèle de lecture pur est calculé pour les
collections ouvertes. La vue est bornée à 50 définitions par collection, 64 horaires,
100 actions partagées par définition et 16 commandes par action ; le dépassement est
signalé et le JSON d’origine reste accessible. Trois actions sont rendues avant
d’ouvrir le détail du groupe.

Même hôte, dépendances et build Vite que la référence `8b0febc` : les 30 fichiers
produits passent de **3 965 826 à 3 992 035 octets**, soit **+26 209 octets (+0,66 %)**.
Le bloc JavaScript principal passe de **834 722 à 854 539 octets (+2,37 %)**.
L’avertissement Vite pour les blocs de plus de 500 ko reste présent et existait
déjà ; cette livraison ne prétend pas résoudre le découpage global de l’interface.
Les [mesures et empreintes des sources](performance/somfy-programme-ui-2026-11-02.json)
identifient exactement ce qui a été construit. Ce relevé de taille ne mesure ni
le CPU/RAM du navigateur ni les latences serveur. Les réserves et le dépassement
du budget image documentés ci-dessus restent ouverts.

## Essai Airzone — 2026-11-03

La dernière image ARM64 mesurée avant l’ajout de passerelle depuis le dashboard
(`4aec51d`) pèse 26 246 840 octets : le budget de 25 Mo est déjà dépassé.
Cette branche étend le pilote, les diagnostics et les vues Airzone ; elle ne
revendique donc pas le respect intégral du budget. L’ajout interactif utilise un
registre borné (32 passerelles, 64 Kio), des lectures à la demande et le superviseur
existant ; il n’ajoute aucune interrogation périodique lorsqu’il est inactif.
La taille de l’image finale et les mesures de l’essai sont rapportées dans la PR.
Les mesures localhost avec un dashboard ouvert ne constituent ni un test CPU au
repos, ni un comparatif avec Home Assistant.

Le raffinement UX conserve le chargement différé de la vue. Le bundle Airzone
mesuré passe de 54,57 Ko / 16,03 Ko gzip à 60,75 Ko / 17,51 Ko gzip ; aucune
bibliothèque ni requête périodique supplémentaire n’est ajoutée. Les mesures et
captures actualisées figurent dans la PR, sans assimiler ce coût à un gain de
latence ou à une qualification complète du budget.

La gestion des passerelles porte ce même chunk différé à 72,72 Ko / 20,75 Ko
gzip. La liste se rafraîchit sur les événements de cycle de vie et les
reconnexions, sans nouveau poller. La conservation des appareils indisponibles
ajoute un inventaire borné à 8 Mio sur disque, réécrit seulement lorsque les
définitions changent. La tâche de persistance existante enregistre les valeurs
dans un fichier séparé, lié à la génération de cet inventaire. Une observation
invalide est écartée individuellement ; elle ne bloque pas les autres appareils. Les observations restaurées ne sont pas
rejouées comme des mesures fraîches dans l’historique ou les intégrales d’énergie.
Le coût de l’image et les mesures après redémarrage sont consignés dans la PR.

L’assistant de découverte porte le chunk Airzone à 82,99 Ko / 23,18 Ko gzip,
sans nouvelle dépendance. La recherche est déclenchée explicitement : au plus
deux sous-réseaux privés /24, 512 cibles, 16 connexions simultanées et quatre
vérifications Airzone simultanées. Elle s’arrête après 18 secondes et une
nouvelle recherche attend au moins 30 secondes depuis le début de la précédente.
Il n’y a aucune tâche de scan quand l’assistant est inactif. Ces bornes ne
constituent pas une mesure de CPU au repos ni une garantie de découverte derrière
un réseau Docker isolé ; le formulaire manuel reste disponible.

Le basculement de pilotage protégé par code porte le chunk Airzone à
88,71 Ko / 24,43 Ko gzip (mesure du build local du 4 novembre 2026), contre
82,99 Ko / 23,18 Ko gzip avant ce changement. Aucune dépendance supplémentaire
n'est ajoutée. Le changement de mode est ponctuel et sérialisé ; l'autorisation
vérifie l'identité par une lecture, la révocation ne dépend d'aucune réponse
réseau. Le superviseur précédent est arrêté avant le remplacement.

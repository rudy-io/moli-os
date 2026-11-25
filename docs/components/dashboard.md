# Le dashboard (`web/`)

Svelte 5 (runes) + Vite, **embarqué dans le binaire** (`rust-embed`, ~210 Ko JS / 66 Ko CSS,
compressé à l'envoi). Deux visages sur le même hub, routés par le hash (`web/src/App.svelte`) :

| Visage | Route | Pour qui |
|---|---|---|
| **Maison** | `#/…` (défaut) | la famille : un membre de la famille qui n'est pas technicien doit pouvoir s'en servir sans aide |
| **Atelier** | `#/atelier` | bricoler : tous les appareils, toutes les valeurs, journal, approbations, bench |

Le flux SSE `/api/events` est ouvert **une seule fois** et partagé (`web/src/lib/hub.svelte.js`).

## Maison : pages (`web/src/maison/pages/`)

| Page | Route | Contenu |
|---|---|---|
| Accueil | `#/` | date, salut, heure, météo ; alertes seulement quand il y a un souci (fumée, eau, portes, sonnette, piscine, approbations) ; une carte **Alarme** si une centrale est là (mode, batterie, GSM, sirènes ; désarmer en un geste, armer avec confirmation ; masquer l'appareil la retire de l'Accueil) ; lumières favorites, et dessous **Ambiance** : le choix de la pièce (pastilles, la dernière choisie est retenue sur l'appareil) et le bandeau d'ambiances ; électricité (jauge du jour vs hier, et à côté la jauge **en direct** du Linky sur l'abonnement, `[energy.live]` : sa couleur dit combien on tire, pas la distance à l'abonnement, verte au calme, orange dès `warn` (par exemple 2 kVA), rouge au double ; mot « calme / modérée / soutenue / forte ») ; clims (cadran **tactile** : on fait glisser le doigt sur l'anneau pour la consigne, le point gris = la température de la pièce, la perle = la consigne, visibles même clim éteinte ; une consigne glissée clim éteinte part tout de suite et la clim démarre dessus ; marche/arrêt, consigne ± 0,5 °C regroupée en un seul ordre, mode, dès que le profil sait écrire) ; sonnette (en **direct** tant qu'on regarde : page affichée et un geste il y a moins de 2 min, à la maison ou partout avec `live_outside` ; sinon l'image toutes les 15 s ; + dernier passage réel, lu dans l'historique) ; télécommande ; dehors en bref **Mise à jour du 7 oct.** : une tuile de lumière peut en commander plusieurs (`favorites.lights[].also` : par exemple une pièce et son couloir) ; Électricité : les plus gros consommateurs du moment en légende ; Climatisation : coût du jour et du mois par clim (compteur de son circuit, sinon « coût non mesuré ») ; Dehors : l'humidité à la place de la couleur Tempo. |
| Moli | `#/moli` | l'assistant plein écran : la conversation à gauche, **l'écran se compose** à droite avec les cartes choisies par Moli (voir [moli.md](moli.md)) |
| Salon | `#/salon` | télécommande porcelaine (touches ou **pavé tactile** : glisser = flèches, toucher = OK ; clavier), « à l'écran », barre de son, Ambilight, lampes. La télécommande n'offre que les touches que la télé annonce (point `key` : pas de sourdine sur une Apple TV) ; les boutons d'applis viennent de la télé quand elle les liste (point `apps`, « nom=identifiant » par ligne : ses favoris parmi Netflix, YouTube, Disney+, Prime Video, CANAL+, TV), sinon la liste Android |
| Pièces | `#/pieces` | une carte par pièce : groupe Hue, lampes (curseur de luminosité ; lampe couleur : pastille qui ouvre une palette, trois blancs par température et huit couleurs + « autre couleur », et un bouton **Flamme** quand le point `effect` de la lampe propose `flame` (un appui la lance, un second l'arrête, comme le choix d'une couleur ou d'un blanc) ; en mode couleur l'ampoule de la tuile prend la teinte ; la tuile « Toute la pièce » a la même palette, appliquée à chaque lampe de la pièce qui sait la prendre) ; bouton **Ambiances** : un bandeau (Feu de cheminée, Bougies, Fin de journée, Cocooning, Fête, Océan, Forêt, Aurore boréale, Romantique, Cinéma, Lecture, Veilleuse), aussi sur la page Salon, volets (ouvrir / stop / fermer, et un curseur d'ouverture quand l'appareil a un point de sens `position`), clims, prises, médias, capteurs en pastilles ; « Protégée » sur les chambres ; « Tout éteindre » ; une prise qui mesure sa conso (point de sens `power`, en W ou kW) la dit sur sa pastille tant qu'elle est allumée (« Allumée · 42 W », aussi dans l'infobulle en mode icônes) **Depuis le 7 oct., cartes compactes** (`ui/RoomCard.svelte`) : en-tête = température de la pièce (moyenne de ses sondes et clims, jauge froid → chaud), ambiances (pastille), petit interrupteur de toutes ses lumières (jamais télé ni prises) ; lumières, télé, enceinte, prises en petites pastilles côte à côte, icônes parlantes (suspension, spot, lampe, ventilateur, prise…), ⚙ pour les réglages d'une lampe (luminosité, couleurs, ampoule par ampoule, ventilateur) ; une clim sur une ligne (le cadran au toucher) ; affichage « Aa » (noms) ou icônes seules, retenu par appareil (`pieces-look`). **Boutons et télécommandes** (#198, #211) : leur dernier appui en pastille, « double · il y a 3 min », pendant 10 min (puis toujours dans le filtre « Capteurs ») ; rien tant qu'aucun appui n'a été vu (`lastPress`, `pressWord`). **Capteurs** : chaque pastille de mesure dit au survol quel appareil la donne, « <nom> : <valeur> » (« Capteur luminosité Sud : 118 lx », « Sonde : 62 % », « Détecteur : présence », « Porte atelier : fermée ») ; une mesure notée garde son niveau après la valeur (« … : 1 200 ppm · Attention »). Un bouton se reconnaît au sens `press` de son point, posé par son pilote (Zigbee2MQTT, Hue, Dirigera, Tuya), jamais à sa clé ; une télécommande à plusieurs boutons (variateur Hue) a une seule pastille, celle du dernier bouton appuyé, qui le nomme (« Bouton 2 · simple »). Chaque geste est nommé par l'événement qui le termine : « simple » (`single`, `click`, `short_release`), « double » (`double`, `double_click`, `double_short_release`), « maintenu » (`hold`, `long`, `press`, `long_press`, `repeat`, `long_release`) ; `initial_press`, le début d'un appui Hue, se dit « appui » ; une autre valeur s'affiche telle quelle. |
| Lumières (D12) | partout | une lampe injoignable est « Coupée à l'interrupteur » (jamais comptée allumée, pas de halo ; un appui l'explique) ; une pièce Hue est « Toute la pièce », jamais une lampe ni une pastille du plan ; un **luminaire** (`lumieres:…`) remplace ses ampoules et son relais dans les vues de la famille, avec un bouton « ampoule par ampoule » (une couleur chacune) ; « Tout éteindre » une pièce = ses lampes et luminaires une à une, jamais la télé, l'enceinte ni les prises ; les ambiances visent les ampoules d'un luminaire. `home.svelte.js` : `isOn` (injoignable = éteint, groupe = une de ses lampes joignables), `isFixture`, `isGroup`, `fixtureOf`, `unpowered` |
| Plan | `#/plan` | **le plan vivant** : un onglet par étage. Étage **dessiné** (`plan/Drawing.svelte`) : la maison redessinée en SVG depuis la donnée, zoom molette / pincement / double tap / boutons, 5 habillages (Plan, Blueprint, Nuit, Aquarelle, **Réel** ; choix par appareil, défaut de la maison en modification), chaque lampe allumée projette un halo découpé par sa pièce, les pièces se teintent ; bouton **3D** (`plan/House3D.svelte` + `plan/scene3d.js`, three.js chargé à la demande) : étages empilés, murs coupés à 60 % sur l'étage regardé, meubles modélisés (`plan/furniture3d.js` : lits, canapés, chaises, voitures, trampoline, transat…), murs de biais et murets, toit de tuiles sur l'étage qui en a un (le regarder = la maison de dehors), vraies lumières, éclairage d'environnement calculé sur place et ombres douces du soleil (redessinées seulement quand la scène change), toucher une perle = l'appareil. L'habillage **Réel** dessine la maison telle qu'elle est, depuis la donnée : sol de chaque pièce (carrelage, parquet, gazon… textures dessinées dans le navigateur, `plan/textures3d.js`, rien à télécharger), peinture et murs peints ou imprimés, façade et bandeau du toit, cadres des fenêtres, couleurs propres des meubles, piscine hors-sol, escalier ouvert ; un agent les écrit à partir de photos des pièces. En modification d'un étage dessiné : ajouter / déplacer / tourner / redimensionner un élément (lit, canapé, voiture…), « poser les appareils dans leur pièce ». Étage **image** (ancien mode) : l'image du plan, une lumière allumée brille, une porte ouverte ou une présence se colore, une fumée ou une fuite clignote ; toucher = allumer/éteindre (lumières, prises), caméras et télé ouvrent leur page, le reste ouvre une fiche. « Modifier » : ajouter un étage avec son image (photo du plan acceptée), poser un appareil (le choisir dans la liste, toucher le plan), glisser pour déplacer ; enregistrer demande le code. Données : `data/plan.json`, images dans `data/plan/` (voir [api.md](api.md)). |
| Carte | `#/carte` | où sont les gens (leurs téléphones, appli Moli) et les zones des automatismes (`pages/Carte.svelte`). L'onglet n'apparaît que si un pilote de téléphones tourne ou si quelqu'un a une position connue (`lib/map-tab.js`) : une maison sans téléphone ne voit pas de carte vide, et l'onglet revient seul au premier téléphone appairé |
| Caméras | `#/cameras` | (onglet présent s'il y a une caméra, ou de quoi remplir la carte Sécurité de la page : un capteur de porte, un détecteur de mouvement, un interrupteur de la pièce « Sécurité », `doors` de `home.json` ; `lib/tabs.js`) mur d'images (rafraîchies si visibles, une image qui ne vient pas abandonnée après 10 s ; caméras sur batterie au toucher seulement), plein écran avec les passages du jour. **Le passé** (une caméra dont l'enregistreur le dit, appareil `review: true` : Frigate avec `review`, voir [pilotes.md](pilotes.md)), **à la maison seulement** (colonne `distant` de `routes.txt` ; loin de la maison, images et direct comme avant) : en haut, les **derniers événements** des 24 h de toutes ces caméras en vignettes (un appui ouvre la caméra sur ce moment ; redemandés chaque minute ou quand les caméras changent, pas quand un autre appareil arrive), et la carte Sécurité reste dessous (portes, présences, commande d'alarme). Le plein écran devient une **visionneuse** (`ui/CameraReview.svelte`, `lib/review.js`) : le direct (ou l'image si le direct échoue), la **frise d'un jour** (alertes, détections, plages enregistrées ; jour précédent / suivant ; « aujourd'hui » passe minuit, visionneuse ouverte), toucher un moment le rejoue (10 min d'enregistrement chargées d'un coup, flèches ±1 min, Maj ±10 min), un événement se revoit en vidéo ou en photo, les **zones** de l'enregistreur en surimpression (aux proportions de l'image affichée : aucune image demandée pour elles, une caméra sur batterie ne se réveille pas pour rien), la liste du jour filtrable par ce qui a été vu (personne, voiture…) ; dessous, les appuis sur la sonnette des 24 h tirés de l'historique, quand la caméra en a (l'enregistreur ne les dit pas). Une panne de l'enregistreur se dit « Frigate ne répond pas » (le détail au survol) ; un refus (invité, pièce protégée) dans les mots du serveur |
| Alarme | `#/alarme` | l'alarme tenue par Moli (pilote `alarm`, voir `integrations/alarm/README.md`), présente seulement quand elle existe : un bloc dit où elle en est (désarmée, sortie avec le compte à rebours, armée, entrée, déclenchée, la cause) et choisit le mode ; une fois un mode choisi, seul « Désarmée » se propose (l'alarme refuse un autre mode, `off` d'abord, le code demandé si `[[guard.code_each_time]]` le veut). Un mode dont un capteur voit quelque chose est grisé, avec le nom du capteur. Capteurs avec leur état et leurs modes ; journal des 7 derniers jours (armements, refus, entrées, déclenchements, capteurs muets, avec les capteurs en cause), qui défile. **Réglages** (désarmée seulement) : modes et délais, capteurs par mode, « immédiat » ; enregistrer demande le code ; un brouillon qui ne change rien ne s'enregistre pas. Les centrales physiques (#215) viendront dessous, avec leur carte |
| Énergie | `#/energie` | en ce moment (Linky en direct coloré selon la conso, repère à `warn`, prochain changement heures creuses / pleines), aujourd'hui (au rythme d'hier), mois + projection vs mois dernier + part en heures creuses, Tempo ; **conseils du moment** (`lib/energy-advice.js` : porte ouverte clim allumée, grosse conso en heures pleines et qui tire, clim alors qu'il fait plus frais dehors, consignes, pompe de piscine en heures pleines, heures creuses en cours ou proches, talon (heure la plus calme de chaque jour, médiane sur 7 jours) en €/an) ; qui consomme maintenant ; où ça part (aujourd'hui / hier / ce mois, par circuit, %) ; **journée type** (moyenne heure par heure sur 7 jours, heures creuses **apprises** de l'historique du point PTEC, talon) ; graphe 24 h / 30 j / 12 mois. Mobile : une colonne, chiffres deux par deux **Journée type** : la journée en cours par-dessus (repère sur chaque heure passée, « aujourd'hui jusqu'à X h : … kWh, d'habitude … (± %) ») ; lumières estimées « ≈ estimé » dans « qui consomme » et « où ça part ». |
| Impression 3D | `#/impression` | (onglet présent s'il y a une imprimante : `progress` et `nozzle_temperature`, comme `deviceKind` ; `lib/tabs.js`) une carte par imprimante (Klipper, Bambu) : état, vignette du travail, progression, couche, temps restant et heure de fin, buse / plateau / enceinte, têtes et filaments (couleurs ; AMS pour Bambu), lumière, **pause / reprise / annulation** (annuler demande une confirmation) ; totaux (impressions, heures, km de filament). Sur l'Accueil, une pastille tant qu'une impression tourne, est en pause ou en erreur ; dans Pièces, une pastille sur la carte de la pièce |
| Dehors | `#/dehors` | météo (soleil calculé), air/UV, piscine (pH, chlore, filtration : `outdoor.pool`, sinon l'appareil d'une pièce extérieure qui mesure pH ou chlore ; la pompe seulement par `outdoor.pool_pump`), une carte par pièce extérieure (`rooms[].outside`) avec ses lumières, portail, volets, prises et capteurs comme dans Pièces (sauf une station déjà sur « Données réelles »), jardin (`outdoor.garden_lights`, clé ancienne : les lumières qu'aucune pièce extérieure ne montre déjà), caméras extérieures (favorites et celles des pièces extérieures, une fois chacune). Avec `[outdoor]` dans `moli.toml` (#210, voir [pilotes.md](pilotes.md), `dehors`), la température, l'humidité, le vent, les rafales, la pluie du jour et l'UV viennent de `dehors:meteo` : chacun porte une pastille visible sans survol, « mesuré » (une station) ou « prévu » (« prévu · station muette » quand une station a ce sens mais s'est tue) ; un appui dit la source et l'âge (« Station · il y a 25 min »). La direction du vent s'écrit après sa vitesse (N, NNE…). Une carte « Données réelles » montre chaque station : toutes ses mesures (pas ses réglages, ce qui se règle, ni sa radio), chacune avec son âge. En bas, le crédit dit les deux : prévisions et mesures. **Partout ailleurs aussi** (#246) : l'Accueil (la grande température, avec sa pastille « mesuré » / « prévu » à côté ; l'humidité et l'UV de la carte « Dehors »), la carte météo de Moli (température, vent) et le Plan (la température du jardin) lisent `dehors:meteo` quand il a la valeur, sinon la prévision comme avant (`readOutside` de `lib/outside.js`, qui ne décide rien : c'est `dehors:meteo` qui choisit). Ces pages-là ne portent pas de pastille, sauf la grande température de l'Accueil. Sous la ligne « Ressenti · max · min » (la prévision) de la page Dehors, « Mesuré aujourd'hui : max · min » quand une station mesure la température (`measured_max` / `measured_min`, #246) ; l'Accueil garde le max/min prévu. Le ciel, le max/min du jour, le lever et le coucher, la nébulosité et l'indice de qualité de l'air restent la prévision : aucune station ne les donne. **Le soleil** (#258) : sous le lever et le coucher, sa hauteur et sa direction (« hauteur 24° · SSO ») depuis `dehors:soleil`, calculé dans la maison ; un arc léger du lever au coucher, le soleil dessus de jour, sans soleil la nuit ; et, avec `[[sun.facade]]`, une pastille par façade (« Soleil sur »), colorée quand le soleil tape dessus (`<id>_sun`), son incidence au survol. Les noms des façades viennent du point `facades` (`id=name`, une par ligne) |
| Automatismes | `#/automatismes`, `#/automatismes/<id>` | liste, idées, éditeur à canevas (voir [automatismes.md](automatismes.md)) |
| Système | `#/systeme` | **carte vivante** du système et comparaison avec HA (voir plus bas) |
| Ajouter un appareil | `#/ajouter` | (lien depuis Système) une prise du réseau par son adresse, en trois temps : l'adresse (Moli demande à l'appareil qui il est, une lecture), la clé s'il la demande (champ masqué, elle part au coffre), puis le modèle reconnu avec un nom et une pièce (une pièce protégée est signalée). Un appareil déjà présent est dit, pas ajouté. Code de la maison demandé si la session n'est pas humaine. Aujourd'hui : les profils qui déclarent `recognize` (prises et multiprises Meross) ; voir [profils.md](profils.md) et [api.md](api.md) |

## Airzone : une vue de lecture dédiée

`#/airzone` est chargé à la demande lorsque le pilote natif Airzone est configuré. La navigation Maison et un lien dans Atelier y donnent accès. Les cartes regroupent les zones par instance et système ; recherche par nom/pièce et filtre de système restent locaux au navigateur. Le pont Home Assistant reste dans ses vues existantes : le fabricant ou le nom ne suffisent pas à classer un appareil comme natif.

La fiche `#/airzone/<identifiant encodé>` distingue température mesurée, consigne, humidité, mode, disponibilité et demandes effectivement signalées. Un appui sur une température/consigne ouvre l’historique existant. Les informations secondaires sont regroupées par thèmes ; les commandes communes sont présentées séparément, uniquement lorsque leurs membres contiennent la zone. `#/airzone/installation` regroupe passerelles, systèmes et ressources facultatives.

**Cette vue est toujours consultative**, y compris pour un pilote dont les points sont inscriptibles : aucun contrôle ni chemin d’envoi d’ordre. Elle partage le flux SSE, les unités déclarées et les formats existants, sans relève supplémentaire. Les anciennes valeurs hors ligne sont signalées, les inconnues ne deviennent pas des zéros ou des degrés Celsius, et aucune demande n’est déduite d’un écart de température. Les programmations cloud ne sont pas annoncées comme disponibles localement.

Le modèle pur `maison/lib/airzone.js` conserve les périmètres et les champs inconnus. Les composants `AirzoneZoneCard` et `AirzoneReadings` sont partagés entre aperçu, fiche et diagnostic. Les tests métier et SSR utilisent exclusivement des appareils fictifs ; les captures publiées sont celles d’une maison de démonstration.

### Gestion et alertes Airzone

`#/airzone/devices` présente aussi les appareils masqués. Le nom et la pièce modifient seulement les étiquettes Moli. Masquer/restaurer réutilise le layout existant via une API générique authentifiée ; aucun appareil physique, historique ou automatisme n’est supprimé. Un lien `#/airzone/devices/<identifiant encodé>` ouvre son éditeur. Le flux `layout_changed`, puis les snapshots de reconnexion, invalident le layout des autres tableaux de bord.

`#/airzone/alerts` conserve la liste des défauts actuellement signalés, y compris les appareils masqués. Les défauts système répétés et les copies dans les commandes communes sont regroupés ; une panne de pilote ou du flux ne devient pas une cascade de défauts matériels. `errors` absent/invalide reste inconnu, et `problem=false` ne suffit pas à affirmer que tout fonctionne. Aucun seuil constructeur n’est inventé : batterie et couverture radio sont des pourcentages lorsqu’ils sont transmis ; « batterie faible » provient de l’avertissement explicite Airzone. Les timestamps SSE reflètent les changements de valeur, pas chaque relève : ils ne servent pas de détecteur de péremption. Cette vue n’émet aucune notification externe.

`#/airzone/add`, accessible aussi depuis Système avant la première intégration, propose un assistant **Rechercher → Choisir → Ajouter**. Le premier écran ne demande aucune adresse IP. Une recherche ponctuelle, déclenchée explicitement, interroge les réseaux privés que Moli peut identifier ; elle n’ajoute jamais de passerelle. Les résultats indiquent les connexions déjà enregistrées ou en pause, et conservent l’adresse et la MAC dans un volet d’identification. Une recherche vide ou incomplète propose toujours la saisie manuelle : elle ne prouve pas l’absence de matériel, notamment lorsque Moli n’a pas de visibilité sur le réseau domestique.

Le choix d’un résultat vérifie à nouveau son identité avant d’afficher une confirmation distincte. L’enregistrement recontrôle encore cette identité côté serveur et force la lecture seule. Le formulaire manuel partage ces mêmes actions ; le port, l’identifiant et l’empreinte HTTPS restent dans les réglages avancés. Les réponses tardives après arrêt, retour, passage au manuel ou sortie de page sont ignorées. La recherche est annulable ; aucune reconnexion SSE ni validation PIN annulée ne la relance automatiquement. Une réponse d’enregistrement perdue conserve l’identité vérifiée pour une reprise explicite et idempotente.

Le code Moli existant protège l’accès à la recherche et aux modifications de connexion. La demande de code décrit une recherche réseau lorsque c’est l’action demandée. Le succès confirme l’enregistrement, sans annoncer que toutes les zones ont déjà répondu. L’édition et la réactivation gardent l’identité existante. Les autres fenêtres reconnaissent le nouveau pilote grâce à `driver_registered`.

La page Installation affiche le mode réel de chaque passerelle et propose **Autoriser le pilotage** ou **Revenir en lecture seule**. Chaque changement demande une confirmation explicite. L’autorisation de pilotage exige un propriétaire et un code Moli vérifié à nouveau par le serveur ; la révocation utilise la session humaine existante. Les restrictions explicites du fichier TOML sont indiquées sur la carte et ne peuvent pas être levées depuis cette interface. L'autorisation couvre les zones et commandes compatibles de cette passerelle ; les autres intégrations restent inchangées. Le retour en lecture seule bloque les futures commandes Moli sans changer le chauffage en cours. Une passerelle en pause doit être réactivée avant d'autoriser le pilotage ; sa révocation reste possible hors ligne. Le serveur arbitre les changements concurrents : une liste devenue obsolète est rechargée avant une nouvelle confirmation.

La vue Airzone reste une vue de consultation. Le pilotage autorisé est accessible dans les cartes de commande existantes de Pièces et dans Atelier ; aucun réglage physique n'est envoyé par le bouton d'autorisation. Les ajouts, modifications de connexion et réactivations démarrent toujours en lecture seule. Une passerelle ajoutée depuis Moli conserve le périmètre historique (`on`, consigne, mode et ventilation) après autorisation. Les commandes étendues exigent une instance explicitement configurée avec `write = "extended"` ; le bouton ne change pas cette option.

La page Installation présente d’abord les passerelles et leurs actions courantes. Les adresses, les identifiants, le retrait réversible et le remplacement sont repliés dans les réglages ; les diagnostics détaillés sont dans « Informations techniques ». L’assistant conserve l’aide sur l’appairage physique et la restauration des cartes sous « Besoin d’aide ? ». La découverte des zones déjà appairées reste automatique ; l’API locale n’offre ni appairage ni désappairage physique.


Navigation : un menu à gauche par défaut (une barre en bas sur téléphone, défilable) ; pied du
menu : Thème, Système, Atelier. Un menu plus long que l'écran défile, le pied reste en vue. Bulle « Demander à Moli » sur toutes les pages sauf Moli. Une pastille glisse
d'un onglet à l'autre, une page arrive en fondu ; un onglet ajouté ou retiré se range en glissant.

**Thème** ouvre une feuille : l'apparence (Auto / Jour / Nuit), les **couleurs** (un thème,
aperçu dessiné ; voir « Les thèmes » plus bas) et les onglets du menu, chacun avec
son interrupteur. Le thème choisi est retenu par écran (`maison-palette` dans le navigateur) ;
sans choix, l'écran prend celui de la maison (`theme` de `home.json`), sinon Porcelaine. Seuls les onglets éteints sont mémorisés (`maison-onglets` dans le navigateur) :
chaque écran a son menu, et un onglet qui apparaît plus tard (une première caméra) se montre de
lui-même. Accueil ne s'éteint pas. Un onglet que la maison n'a rien pour remplir n'est pas listé.
Une page éteinte s'ouvre toujours par son adresse (`#/plan`).

La feuille règle aussi **le menu** de l'écran (`lib/menu.js`) : sa **position** (gauche, droite,
haut, bas ; sur un téléphone il reste une barre, en haut ou en bas), sa **forme** (classique :
icônes et noms ; compact : icônes seules, le nom en info-bulle et pour les lecteurs d'écran ;
flottant : une carte détachée du bord) et **l'ordre** des onglets (flèches monter / descendre,
« Revenir à l'ordre d'origine »). Accueil reste premier ; un onglet qui apparaît plus tard prend sa
place d'origine après ceux qu'on a rangés. Retenu par écran (`maison-menu` dans le navigateur) ;
sans choix, l'écran prend le `menu` de `home.json`, sinon le menu classique à gauche.

## La mise en page de la maison : `data/home.json`

Servie telle quelle par `/api/home` depuis le dossier de données (`data/home.json`, écrite à
la main ou par un agent pour chaque maison). Sans elle, le dashboard reste utilisable (pièces
telles que les appareils les nomment).

```json
{
  "title": "Maison",
  "rooms": [{ "name": "Chambre des enfants", "icon": "bed-outline", "aliases": ["Chambre 2"] }],
  "favorites": {
    "lights":  [{ "id": "hue:…", "name": "Salon", "icon": "sofa-outline" }],
    "climate": [{ "id": "clim-salon:…", "name": "Salon" }],
    "switches": [{ "point": "tuya:…/switch_1", "name": "Appareil photo", "icon": "camera" }],
    "cameras": [{ "id": "reolink:…", "name": "Sonnette", "refresh": 10 }, { "id": "reolink:…", "name": "Allée", "refresh": 60, "battery": true }]
  },
  "salon":   { "tv": "tele:…", "speaker": "sonos:…", "room_light": "hue:…", "lamps": ["hue:…"] },
  "doors":   [{ "id": "tuya:…", "name": "Entrée" }],
  "outdoor": { "weather": "meteo:…", "air": "air:…", "tempo": "tempo:…", "pool": "piscine:…", "pool_pump": "tuya:…" },
  "safety":  ["z2m:…"],
  "hidden":  ["tuya:…", "compteur:…"],
  "quiet":   ["tuya:…"],
  "outlets": { "tuya:…": { "switch_1": "Prise 1", "switch_2": "Prise 2", "switch_3": "Prise 3" } },
  "scenes":  [{ "id": "soiree", "name": "Soirée", "icon": "fire", "colors": ["#ff7a00", "#ffb060"] }]
}
```

- `favorites.switches` (facultatif) : les interrupteurs dont la maison se sert le plus (une prise,
  une sortie de multiprise), en haut de l'Accueil, juste sous le ciel : un appui allume ou éteint
  le point (`point` = `<appareil>/<clé>`, valeur vrai/faux), la pastille est colorée quand il est
  allumé, grisée quand l'appareil ne répond pas. `icon` : un nom du jeu d'icônes (`power` par défaut).
  La garde s'applique comme partout (pièce protégée, heures calmes).
- `live_outside` (facultatif, `true`) : le direct des caméras aussi **hors de la maison** (carte « Devant la porte » de l'accueil, visionneuse des caméras). Seulement si la maison a ouvert le port WebRTC de ses caméras vers Internet (Frigate : redirection du port 8555 TCP et UDP, et l'adresse publique dans `go2rtc.webrtc.candidates`) : sans lui, le direct échouerait après 15 s et retomberait sur les images. Le port ne sert que les flux négociés par Moli (même contrôle que les images : pas d'invité, pièce protégée derrière le code). Le passé reste à la maison.
- `scenes` : les scènes de la maison (`[[scene]]` dans `moli.toml`, voir [pilotes.md](pilotes.md))
  montrées sur l'Accueil, sous les lumières, dans cet ordre : nom, icône et couleurs de la pastille.
  Sans cette liste, toutes les scènes déclarées s'affichent sous leur identifiant. Un appui met la
  scène ; celle en place est entourée. Avec un `[rhythm]`, un bouton « Rythme de la journée » dit si
  les lumières suivent l'heure.

- `rooms[].aliases` : les noms que HA, Hue ou Tuya donnent à la même pièce (le dashboard et
  Moli les ramènent au nom canonique).
- `rooms[].floor` (facultatif) : l'étage de la pièce, dans les mots de la famille
  (`"en bas"`, `"à l'étage"`). L'inventaire de Moli l'écrit à côté de la pièce
  (« Salon (étage : en bas) ») : « allume en bas » allume chaque pièce de cet étage.
- `rooms[].outside` (facultatif) : la pièce est dehors (`true`) ou dedans (`false`). Sans lui,
  `/api/home` le déduit, dans cet ordre : le Plan (une pièce qu'il dessine seulement dehors :
  jardin, terrasse, ponton, piscine, allée), puis le nom ou un alias de la pièce (un mot entier
  parmi ceux de `moli_core::outside::WORDS` : « Extérieur », « Jardin », « Terrasse », « Allée »,
  « Piscine », « Spa », « Garden », « Porch »…). `/api/home` renvoie chaque pièce avec son
  `outside`, et ajoute en fin de `rooms` les pièces extérieures que seuls les appareils ou le Plan
  nomment, avec `"added": true` (elles ne prennent pas de place dans l'ordre des pièces).
  Ce qui est rangé dans une pièce dehors s'affiche sur la page Dehors, sans autre liste.
  Le nom ne peut pas tout dire : un mot de la liste suffit, même avec un autre à côté. Le
  marqueur tranche :

  ```json
  "rooms": [
    { "name": "Jardin d'hiver", "outside": false },
    { "name": "Cour intérieure", "outside": false },
    { "name": "Pool house", "outside": false },
    { "name": "Local piscine", "outside": false },
    { "name": "Préau", "outside": true }
  ]
  ```
- `outdoor.garden_lights` (clé ancienne, toujours lue) : la liste des lumières de la carte
  « Jardin » de la page Dehors. Une pièce extérieure suffit maintenant : rangez la lumière dans
  une pièce dehors (« Jardin », « Allée »… ou `outside: true`), elle s'affiche dans la carte de
  sa pièce, puis retirez son entrée. La carte « Jardin » ne garde que les lumières qu'aucune
  pièce extérieure ne montre déjà, et disparaît quand la liste est vide.
- `charts` (facultatif) : des cartes d'historique sur l'accueil, `[{ "title": "Historique 10 minutes", "minutes": 10, "points": ["p1:…/momentary_active_import", "prise:…/power"] }]` : un graphique par unité, une légende qui masque ou montre chaque courbe et dit sa valeur du moment (nombres et heures dans la langue de la maison) ; relu toutes les 10 à 60 s selon la fenêtre, jamais quand la page est cachée. 8 points au plus par carte, `minutes` de 1 à 10 080 (une semaine) ; un point mal écrit (sans `/`) ou que la maison n'a pas est écarté, et la carte le dit.
- `infra.box` (facultatif) : la carte de la **box internet** sur la page Infra (`ui/infra/BoxCard.svelte`,
  `lib/box.js`), faite des points que la maison lit déjà (pilote `freebox`, une box vue par Home
  Assistant, tout autre pilote) et qu'elle **désigne** ici : rien n'est deviné d'après les noms, un
  point absent n'apparaît pas. Lecture seule.

  ```json
  "infra": { "box": {
    "name": "Freebox",
    "down": "freebox:box/rate_down", "up": "freebox:box/rate_up",
    "free": "freebox:box/free_space", "degraded": "freebox:box/raid_degraded", "wifi": "freebox:box/wifi",
    "temperatures": [{ "label": "Disque", "point": "freebox:box/temp_hdd0", "warn": 50, "alert": 55 }],
    "fans": [{ "label": "Ventilateur", "point": "freebox:box/fan0_speed", "warn": 1800 }]
  } }
  ```

  Les températures en jauges (orange dès `warn`, rouge dès `alert`), les ventilateurs (orange dès
  `warn`), l'occupation du disque (`free` : % libre), la grappe (`degraded` : `true` si dégradée), le
  Wi-Fi ; les débits `down` et `up` en Mbit/s dans l'en-tête, convertis depuis l'unité que **le point
  déclare** (`rate_unit` seulement pour un point qui n'en déclare pas ; une unité qui n'est pas un
  débit n'est pas convertie : « — »), et leur courbe des dernières 24 h, dans la même unité. Entrent
  dans le bandeau « à voir » : une température au-delà de `alert`, un ventilateur au-delà de `warn`,
  la grappe dégradée, un disque plein à 90 % ou plus. L'état de la ligne (l'appareil qui donne
  `external_ip` : connectée, depuis quand, débits de la ligne, adresse publique) entre dans cette
  carte, qui remplace alors la carte « box » d'avant : une seule carte par box.
- `themes` et `theme` : les thèmes de la maison et celui qu'un nouvel écran prend (voir
  « Les thèmes » ci-dessous).
- `menu` (facultatif) : le menu qu'un écran prend tant qu'il n'a rien choisi,
  `{ "position": "bottom", "shape": "floating", "order": ["salon", "plan"] }` ; `position` parmi
  `left`, `right`, `top`, `bottom`, `shape` parmi `classic`, `compact`, `floating`, `order` les
  adresses des onglets (sans Accueil). Un mot inconnu est ignoré.
- `hidden` : masqué du dashboard **et** de l'inventaire de la conversation (pas de celui
  qui sert à créer les automatismes). Par exemple un compteur EM06 (sinon Moli montre une fiche
  « em06p » brute).
- `quiet` : rangé dans sa pièce mais **jamais un bouton** (lave-vaisselle, machine à laver,
  recharge de la voiture : un appui de travers coupe un cycle). La carte de la pièce dit ce qu'il
  consomme (« Lave-vaisselle · 1 850 W », dès 3 W) ; débranché, il ne dit rien (sauf dans une
  vue filtrée). Moli et les automatismes peuvent toujours le commander.
- `outlets` : une multiprise a sa propre carte dans sa pièce, dessinée : une prise par sortie,
  allumée (ambre, son voyant) ou éteinte, qu'un appui bascule ; une sortie nommée « USB » est
  dessinée en ports USB. L'interrupteur de la carte allume tout, ou éteint tout si tout est
  allumé, sortie par sortie (jamais le canal « toutes les sorties » de l'appareil : celui d'une
  Meross se lit allumé quand des sorties sont éteintes). **Sans réglage**, un appareil rangé
  parmi les prises qui a au moins deux points marche/arrêt inscriptibles et numérotés (`on_1`,
  `switch_2`, `state_l3`…) et déjà lus est une multiprise, ses sorties dans l'ordre de leur
  numéro, sous le nom de leur point. `outlets` les nomme et choisit lesquelles montrer, dans
  l'ordre donné (`{ "meross-multi-1:…": { "on_1": "Écran", "on_5": "USB" } }`) ; `{}` en fait
  un simple bouton. Seuls les points inscriptibles comptent. Le crayon de la carte nomme les
  prises depuis le tableau de bord : chaque nom est gardé dans l'étiquette de l'appareil
  (`labels.toml`, `points`, par `PUT /api/labels/{id}/points/{clé}`) et passe avant ceux de
  `outlets` et du pilote ; un nom effacé redonne celui d'origine.

## Les thèmes (`web/src/maison/lib/themes.js`)

Moli en apporte quatre : **Porcelaine** (la base, `maison.css`), **Sauge**, **Océan** (un léger
dégradé en fond) et **Terracotta** (police à empattements). La maison ajoute les siens dans
`home.json` ; un thème du même `id` qu'un thème de Moli le remplace. Un thème ne dit que ce qu'il
change, jour et nuit séparément ; le reste vient de Porcelaine ou de Nuit.

```json
{
  "theme": "chez-nous",
  "themes": [{
    "id": "chez-nous",
    "name": { "fr": "Chez nous", "en": "Our home" },
    "font": "rounded",
    "corners": "pill",
    "background": { "image": "/api/plan/images/3f9c0a7e5b21d864.jpg", "dim": 0.35 },
    "day":   { "bg": "#f6e6d8", "surface": "#fffaf6", "ink": "#2a1a12", "warm": "#e2763a", "cool": "#3f6fb5" },
    "night": { "bg": "#170f0b", "surface": "#241913", "ink": "#f6ebe2", "warm": "#f08a4e",
               "background": { "gradient": ["#170f0b", "#2a1810"] } }
  }]
}
```

- `id` : minuscules, chiffres, tirets (40 au plus). `name` : un texte, ou un par langue.
- Couleurs (`day`, `night`) : les jetons de `maison.css`, en `#rrggbb` (ou `#rgb`, `#rrggbbaa`)
  et rien d'autre : `bg`, `surface`, `surface-2`, `surface-3`, `ink`, `ink-2`, `ink-3`, `line`,
  `warm` (allumé), `cool` (climat), `good`, `alert`, leurs `-soft`, `warm-ink`, `warm-deep`,
  `warm-glow`, `sun`, et `on-warm` / `on-cool` / `on-good` / `on-alert` (le texte posé sur un
  bouton de cette couleur). Ce qui n'est pas dit se **déduit** : `surface-2`, `surface-3` et `line`
  de `surface` et `ink`, les `-soft` de leur accent, le texte sur un accent (blanc s'il se lit,
  sinon l'encre sombre).
- **Lisibilité** : chaque texte est mesuré contre son fond (contraste WCAG : 4,5 pour le texte,
  4 pour `ink-3`, 3 pour le texte d'un bouton). Une couleur trop pâle est remplacée par celle de
  Porcelaine ou de Nuit qui se lit ; la feuille Thème dit ce qui a été ignoré ou ajusté.
- `font` : `figtree` (dans le paquet), `system`, `rounded`, `serif`, `mono` (polices de
  l'appareil, rien à télécharger). `corners` : `square`, `soft`, `round` (défaut), `pill`.
- `background` (pour les deux, ou dans `day` / `night`) : `image` (une image gardée par Moli,
  celle d'un étage du plan : `/api/plan/images/<nom>` ; jamais une adresse ailleurs, que chaque
  écran irait chercher chez un tiers), `gradient` (2 à 6
  couleurs), `dim` (un voile de la couleur `bg`, de 0 à 0,9, pour que le texte reste lisible sur
  une photo). Le fond reste immobile quand la page défile ; les cartes gardent leur `surface`.
- Les couleurs voulues ne suivent pas le thème : vidéo des caméras, couleurs des lampes, ciel,
  tracés de la carte, habillages du Plan, logo et orbe de Moli.

## Conception visuelle

- Thème **porcelaine** le jour, **nuit** après le coucher du soleil (point `daylight` de la
  météo) ; choix Auto / Jour / Nuit dans la feuille Thème (mémorisé dans le navigateur).
- Ambre = allumé, bleu = climat/froid, corail = attention. Grandes cibles tactiles, peu de mots,
  aucun jargon. Police Figtree Variable, icônes Material Design (sélection dans
  `lib/icons.js` : seules celles-ci sont dans le paquet).
- Jetons CSS sous `.maison` dans `maison.css` (l'Atelier garde son propre style). Une couleur de
  l'interface passe par un jeton, jamais en dur : un thème la change (texte sur un bouton plein :
  `var(--on-warm)`, pas `#fff`).
- Règle de style : **pas de bordure gauche colorée** sur les cartes.
- Une lampe injoignable n'est jamais affichée allumée (sa dernière valeur est une supposition).
- Les ordres retenus par la garde ouvrent la feuille « C'est moi » (code PIN), qui sert aussi
  à valider, lancer ou supprimer un automatisme.

## Bibliothèques partagées (`web/src/maison/lib/`)

- `capabilities.js` : droits d'écriture par point et disponibilité, choix réellement
  proposés, plages numériques et arrondi sur le pas annoncé. Les limites du pilote
  priment ; les anciens contrats sans bornes gardent les replis des contrôles de
  volume (0–100) et de consigne (16–30, pas 0,5, seulement en °C ou sans unité
  déclarée). Le volume d'annonce conserve son
  pas de 5 en l'absence d'un pas explicite. Une borne invalide n'est jamais remplacée
  par un repli ; un point absent, en lecture seule ou hors ligne ne devient pas
  commandable pour autant. L'origine des pas est le minimum déclaré, ou zéro à
  défaut : un minimum de présentation ne la décale pas. Les curseurs affichent
  les premier et dernier crans dans les limites conservées du contrat ; le
  validateur du cœur contrôle ces limites mais n'impose pas de grille de pas.
  Les volets sont reconnus par le sens `position` (0–100 %, 100 = ouvert), ou
  leur ordre ouvrir/fermer, sans devenir des interrupteurs. Le curseur exige
  un point `position` inscriptible et une mesure valide ; les limites annoncées
  restent respectées. Le repli Tuya `percent_control` est seulement informatif
  et ne remplace jamais un sens `position` déclaré. Les ordres conservent la
  casse du pilote (`OPEN` ou `open`) ; une cible ou une casse ambiguë ne produit
  pas de commande. Le mouvement reste affiché, même sans position mesurée.
- `CoverTile.svelte` et `cover-controls.js` : les actions annoncées restent visibles
  avec icône et libellé, désactivées en lecture seule, hors ligne ou pendant une
  déconnexion du flux Moli. La puce « Volets » seule, dans Pièces, ou l'adresse
  `#/pieces/volets` (favori, raccourci) les regroupe par pièce en grille, avec les
  relais à impulsion (portail, garage) : ce sont des ouvrants, pas du climat (#171) ;
  le détail d'accès renvoie à Système. Une position mesurée à 0 % dit « Fermé »,
  à 100 % « Ouvert » ; une lecture périmée est explicitement la dernière position.
  Dans les pièces, le mode « icônes seules » place le nom et l'état dans l'infobulle ;
  les boutons d'action et de vitesse gardent leur icône, leur nom accessible et une
  cible tactile de 44 px. Le choix de présentation ne change ni les droits ni la vitesse.
  Le choix **Normal / Lent**, proposé seulement si annoncé, décrit la prochaine
  commande : `open_slow` / `close_slow` ou le point d'action facultatif
  `percent_control_slow` (pourcentage ouvert). Ce point n'est jamais une observation :
  l'état reste lu sur le point de position habituel. Aucune vitesse persistante n'est
  déduite. Une capacité lente retirée ne déclenche jamais un ordre normal en repli.
  **Stop** conserve son ordre propre, même pendant l'envoi d'une autre commande.
  L'acceptation, la validation demandée et l'erreur ont des retours distincts ;
  l'interface attend les lectures du pilote pour confirmer le déplacement.
  Captures d'une installation **fictive** : [jour](../images/tahoma-shutter-quick-actions-light-demo.png),
  [nuit](../images/tahoma-shutter-quick-actions-demo.png),
  [mobile](../images/tahoma-shutter-quick-actions-mobile-demo.png).
- `home.svelte.js` : état de la maison (config, ordres retenus, toasts, thème), lecture des
  valeurs, `powerKey` (le point qui allume/éteint), `deviceKind`, `roomList`, `act` (envoie
  un ordre, gère le 202 de la garde), `confirmHeld` (code PIN), formats français.
- `moli.svelte.js` : la conversation (partagée bulle/page), voix (enregistrement si contexte
  sécurisé, lecture des réponses à voix haute), approbations depuis la conversation,
  suggestions selon l'heure et la maison.
- `auto.svelte.js` : catalogue des nœuds, textes des nœuds, mise en page automatique, API des
  automatismes (avec empreintes), `asHuman`.
- `energy-live.svelte.js` : résumé énergie partagé (une seule requête par minute quel que soit
  le nombre de cartes).

## La carte vivante « Comment ça marche » (`pages/Systeme.svelte`)

Construite **uniquement** à partir de `/api/system` et du flux d'événements : pilotes (état,
appareils, injoignables) → cœur (appareils, valeurs, changements par minute) → briques
(actives ou non, chiffres clés) → surfaces. Les liens d'un pilote s'allument quand ses
appareils changent. En tête : Moli contre HA (mémoire, processeur, taille, réponse, budget,
tendance des livraisons) lu dans `data/perf.jsonl`. Un nouveau pilote ou une nouvelle brique y
apparaît sans toucher à la page.

## Programmations TaHoma en lecture seule

La page TaHoma (`#/somfy`), accessible depuis Système, présente les jours, horaires et actions du catalogue
privé déjà collecté. `lib/somfy-programmes.js` projette les définitions ;
`ui/SomfyCloudProgramme.svelte` et `SomfyCloudActions.svelte` affichent les mêmes
actions pour les scénarios, journées et règles intégrées. Les noms viennent des
étiquettes du catalogue, sans rapprochement heuristique avec d'autres pilotes.
Les états déclarés restent distincts d'une activation effective. Les formats
inconnus, données historiques et limites d'affichage sont explicitement qualifiés.
Les détails longs s'ouvrent à la demande ; la définition JSON d'origine reste
accessible. Aucun contrôle d'exécution, nouvelle requête ou minuterie n'est ajouté.

## Vérifier visuellement

Pour un agent, plutôt que son navigateur intégré (chaque clic y demande une autorisation) :
`puppeteer-core` sur le Chrome installé, avec de petits scripts de capture écrits pour la
session ; `chrome --screenshot` ne rend jamais la main à cause du flux SSE.

## Limites connues

- **Voix** : le micro exige HTTPS ; en `http://<hôte>:8790` le bouton est grisé.
- Télé : applis et clavier non disponibles (JointSpace ne le permet pas ; protocole
  Android TV Remote v2 à faire).
- Clims : lecture seule tant que les écritures Daikin n'ont pas été testées physiquement.
- **Vannes thermostatiques** : une vanne **Zigbee2MQTT** (SONOFF TRVZB, TRV-ZBT) est un climat
  reconnu à ses points `occupied_heating_setpoint` et `system_mode`, sans rien déclarer. Sa carte
  (`ui/ValveCard.svelte`) montre la température mesurée (`local_temperature`), la consigne (bornes
  et pas lus dans le point) et les modes que la vanne accepte ; chaque ordre passe par le chemin
  normal du hub, donc par la garde. Une vanne d'un autre
  protocole peut nommer ses points autrement : une Danfoss Z-Wave expose `temperature` et
  `occupied_heating_setpoint` mais pas `system_mode`, elle n'est pas reconnue.
  La **table horaire** (`ui/ScheduleEditor.svelte`, calcul pur dans `lib/schedule.js`) s'ouvre sous
  la carte quand la vanne énonce `weekly_schedule_<jour>` en écriture : un jour à la fois, les
  créneaux `HH:mm/°C`, un « appliquer aussi à » pour d'autres jours. Chaque jour écrit est un ordre
  comme un autre (`act`, donc la garde). Les bornes de température sont celles de la consigne de la
  vanne ; le nombre de créneaux par jour n'est pas un point : 6 (SONOFF TRVZB), 12 si la vanne a
  des groupes de programmes (TRV-ZBT), d'après les convertisseurs de Zigbee2MQTT. Une TRVZB ne
  renvoie pas sa table (écriture seule) : l'éditeur le dit, enregistrer la remplace. Une TRV-ZBT
  garde trois groupes ; les textes sont ceux du groupe en cours d'édition, ce que l'éditeur signale
  quand ce n'est pas le groupe suivi par la vanne. Pour une pièce du chauffage de Moli, quand la
  maison a des programmes (`[heating_house]`), un sélecteur au-dessus des jours choisit le
  programme édité (par défaut celui que choisit `chauffage:maison`) : l'éditeur écrit alors
  `weekly_schedule_<clé>_<jour>`, dit quand un jour reprend celui du premier programme, et propose
  d'y revenir.
  Partout où il paraît (Pièces, Chauffage, Accueil, Moli), un thermostat qui ne fait que chauffer
  est **dessiné en radiateur** : vanne Zigbee, thermostat de pièce de Moli, et vanne d'un autre
  protocole (Matter : `target_temperature` et un mode `off`/`heat`, sans rien d'un climatiseur,
  `heatsOnly`), qui prend alors la carte de vanne au lieu du cadran, réglée par demi-degré. Une
  vanne seule (ni membres, ni `valve_correction`, ni `valve_override`) dit si elle est **ouverte**
  ou **fermée** (`running_state`, sinon son thermomètre à plus de 0,5 °C au-dessus de sa consigne) ;
  le thermostat d'une pièce dit en plus l'état de ses vannes (« Vanne fermée · radiateur 22,6° »,
  « 1 vanne ouverte sur 2 »).
  Le dessin (`ui/RadiatorTile.svelte`, calcul pur dans `lib/radiator.js`) : ses éléments se remplissent de
  chaleur et des volutes montent quand il chauffe (sonde de la pièce à plus de 0,2 °C sous la
  consigne, ou `running_state` à `heat`), à moitié quand il est à température, gris au repos ou à
  l'arrêt, bleus et traversés d'un courant d'air pendant une aération (`valve_override`) ; une
  petite molette dit une vanne réglée à la main. En mots : ce qu'il fait, « 18,2° pour 19° », puis
  le prochain changement du programme (« Programme · 17° à 22:00 ») ou la fin du réglage à la main,
  avec **Reprendre le programme**, ou un boost (« Boost +2° · jusqu’à 15:30 ») ou une consigne
  maintenue (« Consigne maintenue · programme repris à 18:00 »). Toucher la tuile ouvre la carte
  de la vanne (consigne, modes), sans son en-tête. Animations coupées avec `prefers-reduced-motion`.
  Un thermostat de pièce de Moli (point `boost` en écriture) a en plus, hors `off`, **+1 · +2**
  (dans la limite de `max_boost`) ; pendant un boost, **1 h · 2 h** (`boost_minutes`), le compte à
  rebours tiré de `boost_until` et **Annuler** (`boost` à 0). Une consigne maintenue (`hold_until`)
  dit son échéance et son compte à rebours, avec **Reprendre le programme** (`system_mode` à `auto`,
  qui annule aussi le boost). Calcul pur dans `lib/temporary.js`.
  La page **Chauffage** (`pages/Chauffage.svelte`) réunit, au-dessus de la pompe à chaleur, chaque
  thermostat qui a une table horaire (thermostats de pièce de Moli, `crates/moli-heating`, et vannes
  Zigbee2MQTT) : la carte de la vanne sans son repli de table horaire, la correction appliquée aux
  vannes (`valve_correction`), le réglage fait à la main sur une vanne (`valve_override` : « Réglée
  sur la vanne · programme repris à 14:30 » ou « Aération », avec **Reprendre le programme** ; sinon
  « Suit le programme »), **la semaine** (`ui/WeekSchedule.svelte`, calcul pur dans
  `lib/week.js`) en sept bandes de 24 h colorées par consigne sur une même échelle pour toutes les
  pièces (15 °C froid → 23 °C chaud, `color-mix` entre `--cool` et `--warm`), avec l'heure actuelle
  sur la ligne du jour, puis l'éditeur du jour choisi. Une limite entre deux créneaux se fait glisser
  (au quart d'heure, entre ses voisines) ou se déplace aux flèches ; le jour part en lâchant (aux
  flèches : quand elles se reposent), par `act`. « Copier la semaine vers » écrit les sept jours dans
  une autre pièce, aux bornes de celle-ci, jour après jour, et s'arrête au premier ordre retenu
  (pièce protégée : le code). L'onglet apparaît dès qu'un tel thermostat ou une pompe à chaleur existe.
  La **courbe** (`ui/ValveChart.svelte`, calcul pur dans `lib/valve-history.js`) trace, sur 6 h, 24 h
  ou 7 j, la température mesurée, la consigne et ce que demande la table horaire, sur une même
  échelle, d'après `/api/history`. Lecture seule. La table tracée est celle d'aujourd'hui : le hub
  n'en garde pas l'historique. Pas encore de rubans d'ouverture ni de chiffres (`running_state`).

## Enregistrement des passerelles Airzone

Si le registre Airzone ne peut pas être écrit, revenir en lecture seule ou retirer
une passerelle bloque tout de même ses commandes. La carte indique que ce changement
n’est pas enregistré et peut être perdu au redémarrage ; elle propose de réessayer
l’enregistrement. Elle ne propose ni pilotage ni réactivation tant que cette
révocation reste temporaire.

## Accès TaHoma

`#/somfy` sépare les scénarios et les programmations des réglages de connexion.
La navigation apparaît pour une intégration Overkiz configurée ; Système conserve
un lien vers cette page. L’ouverture lit uniquement le catalogue local du
propriétaire déjà collecté : elle ne déclenche pas de requête au fournisseur.
Les informations techniques et le JSON d’origine restent disponibles à la demande.
Le lien « Volets et stores » mène à `#/pieces/volets` : les commandes y dépendent
des capacités réelles du pilote, indépendamment du catalogue cloud.

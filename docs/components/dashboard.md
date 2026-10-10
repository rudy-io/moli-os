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
| Accueil | `#/` | date, salut, heure, météo ; alertes seulement quand il y a un souci (fumée, eau, portes, sonnette, piscine, approbations) ; lumières favorites, et dessous **Ambiance** : le choix de la pièce (pastilles, la dernière choisie est retenue sur l'appareil) et le bandeau d'ambiances ; électricité (jauge du jour vs hier, et à côté la jauge **en direct** du Linky sur l'abonnement, `[energy.live]` : sa couleur dit combien on tire, pas la distance à l'abonnement, verte au calme, orange dès `warn` (par exemple 2 kVA), rouge au double ; mot « calme / modérée / soutenue / forte ») ; clims (cadran **tactile** : on fait glisser le doigt sur l'anneau pour la consigne, le point gris = la température de la pièce, la perle = la consigne, visibles même clim éteinte ; une consigne glissée clim éteinte part tout de suite et la clim démarre dessus ; marche/arrêt, consigne ± 0,5 °C regroupée en un seul ordre, mode, dès que le profil sait écrire) ; sonnette (image + dernier passage réel, lu dans l'historique) ; télécommande ; dehors en bref **Mise à jour du 7 oct.** : une tuile de lumière peut en commander plusieurs (`favorites.lights[].also` : par exemple une pièce et son couloir) ; Électricité : les plus gros consommateurs du moment en légende ; Climatisation : coût du jour et du mois par clim (compteur de son circuit, sinon « coût non mesuré ») ; Dehors : l'humidité à la place de la couleur Tempo. |
| Moli | `#/moli` | l'assistant plein écran : la conversation à gauche, **l'écran se compose** à droite avec les cartes choisies par Moli (voir [moli.md](moli.md)) |
| Salon | `#/salon` | télécommande porcelaine (touches ou **pavé tactile** : glisser = flèches, toucher = OK ; clavier), « à l'écran », barre de son, Ambilight, lampes |
| Pièces | `#/pieces` | une carte par pièce : groupe Hue, lampes (curseur de luminosité ; lampe couleur : pastille qui ouvre une palette, trois blancs par température et huit couleurs + « autre couleur » ; en mode couleur l'ampoule de la tuile prend la teinte ; la tuile « Toute la pièce » a la même palette, appliquée à chaque lampe de la pièce qui sait la prendre) ; bouton **Ambiances** : un bandeau (Feu de cheminée, Bougies, Fin de journée, Cocooning, Fête, Océan, Forêt, Aurore boréale, Romantique, Cinéma, Lecture, Veilleuse), aussi sur la page Salon, volets, clims, prises, médias, capteurs en pastilles ; « Protégée » sur les chambres ; « Tout éteindre » **Depuis le 7 oct., cartes compactes** (`ui/RoomCard.svelte`) : en-tête = température de la pièce (moyenne de ses sondes et clims, jauge froid → chaud), ambiances (pastille), petit interrupteur de toutes ses lumières (jamais télé ni prises) ; lumières, télé, enceinte, prises en petites pastilles côte à côte, icônes parlantes (suspension, spot, lampe, ventilateur, prise…), ⚙ pour les réglages d'une lampe (luminosité, couleurs, ampoule par ampoule, ventilateur) ; une clim sur une ligne (le cadran au toucher) ; affichage « Aa » (noms) ou icônes seules, retenu par appareil (`pieces-look`). |
| Lumières (D12) | partout | une lampe injoignable est « Coupée à l'interrupteur » (jamais comptée allumée, pas de halo ; un appui l'explique) ; une pièce Hue est « Toute la pièce », jamais une lampe ni une pastille du plan ; un **luminaire** (`lumieres:…`) remplace ses ampoules et son relais dans les vues de la famille, avec un bouton « ampoule par ampoule » (une couleur chacune) ; « Tout éteindre » une pièce = ses lampes et luminaires une à une, jamais la télé, l'enceinte ni les prises ; les ambiances visent les ampoules d'un luminaire. `home.svelte.js` : `isOn` (injoignable = éteint, groupe = une de ses lampes joignables), `isFixture`, `isGroup`, `fixtureOf`, `unpowered` |
| Plan | `#/plan` | **le plan vivant** : un onglet par étage. Étage **dessiné** (`plan/Drawing.svelte`) : la maison redessinée en SVG depuis la donnée, zoom molette / pincement / double tap / boutons, 4 habillages (Plan, Blueprint, Nuit, Aquarelle ; choix par appareil, défaut de la maison en modification), chaque lampe allumée projette un halo découpé par sa pièce, les pièces se teintent ; bouton **3D** (`plan/House3D.svelte` + `plan/scene3d.js`, three.js chargé à la demande) : étages empilés, murs coupés à 60 % sur l'étage regardé, meubles modélisés (`plan/furniture3d.js` : lits, canapés, chaises, voitures, trampoline, transat…), murs de biais et murets, toit de tuiles sur l'étage qui en a un (le regarder = la maison de dehors), vraies lumières, toucher une perle = l'appareil. En modification d'un étage dessiné : ajouter / déplacer / tourner / redimensionner un élément (lit, canapé, voiture…), « poser les appareils dans leur pièce ». Étage **image** (ancien mode) : l'image du plan, une lumière allumée brille, une porte ouverte ou une présence se colore, une fumée ou une fuite clignote ; toucher = allumer/éteindre (lumières, prises), caméras et télé ouvrent leur page, le reste ouvre une fiche. « Modifier » : ajouter un étage avec son image (photo du plan acceptée), poser un appareil (le choisir dans la liste, toucher le plan), glisser pour déplacer ; enregistrer demande le code. Données : `data/plan.json`, images dans `data/plan/` (voir [api.md](api.md)). |
| Caméras | `#/cameras` | mur d'images (rafraîchies si visibles ; caméras sur batterie au toucher seulement), plein écran avec les passages du jour |
| Énergie | `#/energie` | en ce moment (Linky en direct coloré selon la conso, repère à `warn`, prochain changement heures creuses / pleines), aujourd'hui (au rythme d'hier), mois + projection vs mois dernier + part en heures creuses, Tempo ; **conseils du moment** (`lib/energy-advice.js` : porte ouverte clim allumée, grosse conso en heures pleines et qui tire, clim alors qu'il fait plus frais dehors, consignes, pompe de piscine en heures pleines, heures creuses en cours ou proches, talon (heure la plus calme de chaque jour, médiane sur 7 jours) en €/an) ; qui consomme maintenant ; où ça part (aujourd'hui / hier / ce mois, par circuit, %) ; **journée type** (moyenne heure par heure sur 7 jours, heures creuses **apprises** de l'historique du point PTEC, talon) ; graphe 24 h / 30 j / 12 mois. Mobile : une colonne, chiffres deux par deux **Journée type** : la journée en cours par-dessus (repère sur chaque heure passée, « aujourd'hui jusqu'à X h : … kWh, d'habitude … (± %) ») ; lumières estimées « ≈ estimé » dans « qui consomme » et « où ça part ». |
| Impression 3D | `#/impression` | une carte par imprimante (Klipper, Bambu) : état, vignette du travail, progression, couche, temps restant et heure de fin, buse / plateau / enceinte, têtes et filaments (couleurs ; AMS pour Bambu), lumière, **pause / reprise / annulation** (annuler demande une confirmation) ; totaux (impressions, heures, km de filament). Sur l'Accueil, une pastille tant qu'une impression tourne, est en pause ou en erreur ; dans Pièces, une pastille sur la carte de la pièce |
| Dehors | `#/dehors` | météo (soleil calculé), air/UV, piscine (pH, chlore, filtration), jardin, caméras extérieures |
| Automatismes | `#/automatismes`, `#/automatismes/<id>` | liste, idées, éditeur à canevas (voir [automatismes.md](automatismes.md)) |
| Système | `#/systeme` | **carte vivante** du système et comparaison avec HA (voir plus bas) |

Navigation : rail à gauche (bas de l'écran sur téléphone, défilable) ; pied de rail : thème,
Système, Atelier. Bulle « Demander à Moli » sur toutes les pages sauf Moli.

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
    "cameras": [{ "id": "reolink:…", "name": "Sonnette", "refresh": 10 }, { "id": "reolink:…", "name": "Allée", "refresh": 60, "battery": true }]
  },
  "salon":   { "tv": "tele:…", "speaker": "sonos:…", "room_light": "hue:…", "lamps": ["hue:…"] },
  "doors":   [{ "id": "tuya:…", "name": "Entrée" }],
  "outdoor": { "weather": "meteo:…", "air": "air:…", "tempo": "tempo:…", "pool": "piscine:…", "pool_pump": "tuya:…", "garden_lights": [{ "id": "tuya:…", "name": "Lumière du jardin" }] },
  "safety":  ["z2m:…"],
  "hidden":  ["tuya:…", "compteur:…"],
  "quiet":   ["tuya:…"],
  "outlets": { "tuya:…": { "switch_1": "Prise 1", "switch_2": "Prise 2", "switch_3": "Prise 3" } }
}
```

- `rooms[].aliases` : les noms que HA, Hue ou Tuya donnent à la même pièce (le dashboard et
  Moli les ramènent au nom canonique).
- `hidden` : masqué du dashboard **et** de l'inventaire de la conversation (pas de celui
  qui sert à créer les automatismes). Par exemple un compteur EM06 (sinon Moli montre une fiche
  « em06p » brute).
- `quiet` : rangé dans sa pièce mais **jamais un bouton** (lave-vaisselle, machine à laver,
  recharge de la voiture : un appui de travers coupe un cycle). La carte de la pièce dit ce qu'il
  consomme (« Lave-vaisselle · 1 850 W », dès 3 W) ; débranché, il ne dit rien (sauf dans une
  vue filtrée). Moli et les automatismes peuvent toujours le commander.
- `outlets` : une multiprise dont chaque prise a son bouton, dans l'ordre donné, plus un bouton
  pour toute la multiprise (« 2 sur 3 » ; un appui allume tout, ou éteint tout si tout est
  allumé). Seuls les points inscriptibles comptent.

## Conception visuelle

- Thème **porcelaine** le jour, **nuit** après le coucher du soleil (point `daylight` de la
  météo) ; bascule Auto / Jour / Nuit (mémorisée dans le navigateur).
- Ambre = allumé, bleu = climat/froid, corail = attention. Grandes cibles tactiles, peu de mots,
  aucun jargon. Police Figtree Variable, icônes Material Design (sélection dans
  `lib/icons.js` : seules celles-ci sont dans le paquet).
- Jetons CSS sous `.maison` dans `maison.css` (l'Atelier garde son propre style).
- Règle de style : **pas de bordure gauche colorée** sur les cartes.
- Une lampe injoignable n'est jamais affichée allumée (sa dernière valeur est une supposition).
- Les ordres retenus par la garde ouvrent la feuille « C'est moi » (code PIN), qui sert aussi
  à valider, lancer ou supprimer un automatisme.

## Bibliothèques partagées (`web/src/maison/lib/`)

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

## Vérifier visuellement

Pour un agent, plutôt que son navigateur intégré (chaque clic y demande une autorisation) :
`puppeteer-core` sur le Chrome installé, avec de petits scripts de capture écrits pour la
session ; `chrome --screenshot` ne rend jamais la main à cause du flux SSE.

## Limites connues

- **Voix** : le micro exige HTTPS ; en `http://<hôte>:8790` le bouton est grisé.
- Télé : applis et clavier non disponibles (JointSpace ne le permet pas ; protocole
  Android TV Remote v2 à faire).
- Clims : lecture seule tant que les écritures Daikin n'ont pas été testées physiquement.

# Historique et énergie

## Historique (`crates/moli-history`)

- SQLite `data/history.db`, enregistrement **au changement** (le hub n'émet que les
  changements), 30 jours par défaut (`[server] history_days` ; `36500` pour tout garder ; la page Système affiche la durée réellement réglée), la dernière valeur de chaque point jamais
  purgée (une valeur stable a toujours une valeur « avant la fenêtre »).
- Lecture : `series(point, from, to, max_points)` → valeurs brutes, ou seaux (moyenne, min,
  max, nombre) quand il y en a trop.
- Surfaces : `/api/history` (courbes du dashboard), outil MCP `history` (statistiques +
  série compacte) et **`history_sql`** pour les agents : une seule requête `SELECT`, `ATTACH`,
  `PRAGMA`, `BEGIN` interdits, 2 s et 200 lignes maximum, 64 Ko par valeur, sur la vue
  `history(point, ts, kind, num, txt)`.
- Utilisé par : les courbes, les passages du jour (caméras), « dernier passage » à la porte,
  Moli (questions sur le passé), le nœud d'automatisme… (les automatismes lisent l'état
  courant, pas l'historique).
- Ordre de grandeur : ~2,5 Mo pour environ 70 appareils sur 30 jours.

## Énergie (`crates/moli-energy`)

Les compteurs cumulés deviennent des **kWh et des euros heure par heure**, gardés à vie dans
`data/energy.db`.

- Configuration `[energy]` de `moli.toml` : tarif (période courante lue sur un point, prix
  par période : par exemple HC 0,14 €, HP 0,17 €), et `[[energy.meter]]` avec un rôle :
  - `grid` : ce que le fournisseur facture (index Linky HC / HP, au kWh près) ;
  - `total` : toute la maison mesurée finement (par exemple un compteur EM06 sur l'arrivée
    générale, au Wh) ;
  - `circuit` : un circuit du tableau (par exemple « Chauffe-eau »).
  - `appliance` : un **appareil** mesuré à sa prise (par exemple la télé) : affiché
    **dans** son circuit (`within = "<id du circuit>"`) ou dans « le reste de la maison »
    (sans `within`), jamais ajouté au total ni retranché du reste (pas de double compte).
  - `integrate = true` : `point` est une **puissance** (W) ; Moli en fait l'énergie
    (puissance tenue × temps, au plus 15 min d'un coup, un tic toutes les 5 min, rien de
    compté hors ligne ni pendant un arrêt de Moli). Pour les prises Tuya, dont `add_ele`
    repart de zéro à chaque envoi et n'est pas un compteur fiable.
  - `estimate = true` (appareil seulement, sans `integrate` ni `power`) : une **lumière connectée** qui ne mesure rien ; `point` = un point de la lampe ou d'un luminaire `lumieres:…`. Moli estime sa puissance d'après le profil mesuré de son modèle (powercalc, licence MIT, condensé par `scripts/light-profiles.py` dans `crates/moli-energy/src/estimate.rs` : LCA006, LTW001, LWA017, L530) : luminosité, blanc ou couleur, veille éteinte, **0 quand elle est coupée au mur** ; un luminaire = la somme de ses ampoules. Comptée dans le temps comme `integrate`, jamais ajoutée aux totaux, `estimated: true` dans l'API, « ≈ estimé » à l'écran. Modèle inconnu : pas d'estimation (jamais de chiffre inventé).
  - Pas repris de HA : ses capteurs « powercalc » (estimations, pas des mesures).
  - `feeds = ["<device id>"]` : les appareils qu'un compteur alimente (par exemple le
    circuit d'une clim → cette clim) : leur carte montre sa puissance (« circuit partagé »
    s'il alimente autre chose). Les adaptateurs Daikin BRP069 vérifiés ne mesurent **pas**
    leur conso (`get_day_power_ex` / `get_year_power_ex` à 0 même clim en marche,
    `cmpfreq=999`) : seul le tableau la donne.
  - `[energy.live]` (optionnel) : `power` = le point de puissance instantanée du compteur du
    fournisseur (Linky PAPP, en VA), `max` = l'abonnement dans la même unité (par
    exemple 9 000 pour un abonnement de 9 kVA), `warn` = à partir d'où la conso est
    « soutenue » (par exemple 2 000 ; défaut un quart de `max`) : la jauge vire à l'orange,
    puis au rouge au double. Sert la jauge « en direct » et la ligne du prompt de Moli.
  - `[[energy.contract]]` (pour une maison dont le compteur ne dit pas la période, sans
    `[energy.tariff]`) : des **contrats datés**, chacun en vigueur à partir de son jour
    (`from = "2026-01-01"`) : `offpeak = ["01:38-07:08", "14:38-17:08"]` (heures creuses de
    chaque jour, une fin avant le début passe minuit ; absent = tarif unique `Base`),
    `prices = { HP = 0.2210, HC = 0.1747 }` (ou `{ Base = … }`), `monthly_fee`. Les prix
    sont appliqués **au moment du calcul** (`tariff.rs`), heure par heure, au contrat de son
    jour, à la minute près (01:00–02:00 = 22 min creuses, 38 pleines) : un contrat ajouté plus
    tard recalcule aussi le passé ; un compteur avec son propre `price` le garde. Les totaux
    (aujourd'hui, mois…) et chaque seau des séries portent `by_period` (`HC`, `HP` : kWh et €,
    pour la maison : compteur général, sinon facturés, sinon circuits) ; le résumé dit la
    période, son prix, l'abonnement et les heures creuses du jour (`offpeak`), que la page
    Énergie prend au lieu de les apprendre du compteur. Chaque compteur a aussi son propre
    découpage (`meter_periods`) : les kWh creux et pleins d'un radiateur, pas ceux de la maison.
  - **Modifiables depuis le tableau de bord** (roue crantée de la carte « En ce moment » de la
    page Énergie, code de la maison demandé, `PUT /api/energy/contracts`) : la liste entière
    est vérifiée (dates distinctes, plages sans chevauchement), écrite dans
    `data/contracts.json` (à côté de `energy.db`) et appliquée tout de suite. Dès que ce
    fichier existe, il **remplace** les `[[energy.contract]]` de `moli.toml` (un fichier
    illisible ou incohérent est ignoré, avec un avertissement, et `moli.toml` reprend la
    main). Impossible avec `[energy.tariff]` (la période vient alors du compteur).
  - `monthly_fee` (optionnel) : l'abonnement par mois TTC (par exemple 20 €) : la facture
    estimée = kWh du mois au rythme actuel + abonnement, comme le « Projection facture fin de
    mois » de HA. `budget` (optionnel) : le point qui tient le budget mensuel (par exemple
    une aide `maison:budget_elec_mensuel/value`).
- **Appareil « Énergie »** (`energie:maison`, `points.rs`) : Moli le fait tourner tout seul
  dès que `[energy]` existe (l'id de pilote `energie` est réservé). Points en lecture seule,
  recalculés toutes les 5 min : `today_kwh/_cost`, `yesterday_kwh/_cost`, `month_kwh/_cost`
  (kWh seulement), `projection_kwh`, `bill_projection` (abonnement compris, absent le premier
  jour du mois), `over_budget` (si `budget` : facture estimée > budget, jamais avant 5 jours
  écoulés, comme HA qui attendait le 6). Sémantique `other`, jamais `energy` : ce sont des
  résumés, pas des compteurs à additionner. Sert les automatismes (par exemple une alerte de
  budget mensuel) et les agents.
- Robustesse : saut impossible (> 36 kW) ou baisse = suspect ; remise à zéro comptée une fois
  confirmée ; nouveau socle après 3 relevés cohérents ; chaque heure au prix de sa période
  (l'index HC n'est étalé que sur du temps HC).
- Chiffre principal = la plus complète des deux mesures (`grid` et `total`, par exemple Linky
  et EM06).
- **Historique repris de HA** (statistiques long terme, lecture seule) :
  `tools/home-assistant/ha-energy-export.py … | docker exec -i moli-os /moli-os energy
  import`, relançable sans doublon.
- Surfaces : `/api/energy` (en direct, aujourd'hui, hier, mois + projection, mois dernier,
  part non mesurée), `/api/energy/series?step=hour|day|month`, outil MCP `energy`, page
  Énergie, carte Accueil ; la ligne « Électricité en direct » du prompt de Moli.
- **Brique future : production** (panneaux solaires, injection, batterie).

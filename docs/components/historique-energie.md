# Historique et énergie

## Historique (`crates/moli-history`)

- SQLite `data/history.db`, enregistrement **au changement** (le hub n'émet que les
  changements), 30 jours (`retention_days`), la dernière valeur de chaque point jamais
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
- **Production solaire** : rôles `solar` (index de production à vie d'un onduleur, sans prix) et
  `export` (index de ce que la maison injecte, par exemple `energy_returned` d'une pince Shelly
  Pro 3EM ; `price` = ce que la revente rapporte). Chaque période donne alors `production`,
  `export`, `self_consumed` (production − injection, jamais négatif ni au-delà de la
  production), `consumption` (acheté + autoconsommé : ce que la maison a vraiment utilisé),
  `self_consumption_rate` (autoconsommation) et `self_sufficiency` (autoproduction). `total`
  reste ce qui a été **acheté** (la facture). Avec des panneaux, « le reste de la maison »
  (pince − circuits) compte aussi l'autoconsommé, sans coût (les circuits ont tourné en
  partie au solaire). `/api/energy` ajoute `flows` : `production_w` (puissance des compteurs
  `solar`), `grid_w` (puissance signée du compteur `total` : > 0 achat, < 0 injection),
  `consumption_w`. L'appareil `energie:maison` gagne `today_production_kwh`,
  `today_export_kwh`, `today_self_consumed_kwh`, `today_self_consumption_rate`,
  `today_self_sufficiency`, `month_production_kwh`, `month_self_consumption_rate` (%).
  Pas encore : batterie.
- **Charges pilotées** (`[[energy.load]]`, `crates/moli-energy/src/load.rs`) : un chauffe-eau
  (ou toute charge qui stocke) sur un relais, mis en marche sur le surplus solaire et complété
  en heures creuses. Logique **déterministe** (jamais de modèle dans la boucle), testée, un tour
  toutes les 15 s ; ordres au relais par le hub en origine `system` (garde, journal
  « charge « Nom » »). Chaque charge est un appareil `charges:<id>` : `mode` ✎ (`auto`,
  `heures_creuses`, `marche`, `arret`, gardé entre deux démarrages), `reason` (ce que fait
  Moli : `surplus_solaire`, `achat_reseau`, `complement_heures_creuses`, `attente_surplus`,
  `compteur_absent`, `duree_minimale`…), `ran_minutes` (temps de marche du cycle).

```toml
[[energy.load]]
id = "chauffe_eau"                       # jamais renommé
name = "Chauffe-eau"
switch = "relais:aabbccddeeff_switch_0/on"
grid = "pince:aabbccddeeff/power"        # puissance signée à l'arrivée (W)
power_w = 2400                           # ce que la charge tire en chauffe
offpeak = ["22:00-06:00"]                # sinon : la période du tarif (« HC… »)
daily_minutes = 120                      # au moins, par cycle, complété en heures creuses
# min_on_minutes = 10 ; min_off_minutes = 10
```

  En `auto` : marche quand la maison injecte au moins 90 % de `power_w` depuis 3 min, arrêt
  quand elle achète plus d'un quart de `power_w` depuis 3 min (jamais avant les durées
  minimales) ; un cycle va de la fin d'une plage creuse à la fin de la suivante ; en heures
  creuses, tant que le cycle n'a pas atteint `daily_minutes`, la charge tourne. Sans lecture
  de la pince : comportement « heures creuses ». Un relais bougé à la main recommence la
  durée minimale. Une charge dont le relais commande la bobine d'un contacteur (sur « Auto »)
  convient telle quelle.

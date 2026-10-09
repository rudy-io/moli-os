# Automatismes (`crates/moli-automation`)

Objectif : des automatisations **plus simples que Home Assistant, plus rapides, une
expérience agréable, l'IA au cœur**, avec un éditeur visuel « à la n8n de la maison ».
Livré le 3 oct., durci après une revue adversariale (1 critique, 5 élevés corrigés).

## Règles non négociables

1. **Un humain valide, la maison exécute.** Une automatisation ne tourne que si un humain
   (session PIN du dashboard) a validé *la version qu'il avait sous les yeux* : valider,
   allumer et lancer envoient l'**empreinte** (SHA-256 complet du graphe + mode) de ce qui est
   affiché ; si quelqu'un l'a changée entre-temps → **409**, il faut relire.
2. **Enregistrer n'approuve jamais et n'allume jamais.** Seule la validation le fait. Créé
   ou modifié par Moli, un agent MCP, l'API sans PIN ou un import : brouillon. Un agent ne
   peut **qu'éteindre**. Un changement de logique d'un automatisme validé le fait cesser de
   tourner (et annule ce que l'ancienne version avait lancé) jusqu'à nouvelle validation ;
   « Revenir à la version validée » restaure la dernière version approuvée.
3. Une automatisation validée agit comme `Origin::Automation` : la garde la laisse passer
   (c'est l'intention d'un humain). L'éditeur prévient avant validation si elle touche une
   pièce protégée.
4. **L'IA n'est jamais dans la boucle de contrôle.** Elle *crée*, *explique*, *corrige* et
   *écrit des messages* (nœud « Moli écrit »), mais aucune décision d'allumer/éteindre
   n'est prise par un LLM à l'exécution. Le moteur est déterministe.
5. Lancer pour de vrai : humain seulement, sur la version validée. Les agents ont l'essai à
   blanc (lit les conditions, n'agit jamais, n'attend jamais, n'appelle pas le LLM).
6. Pas de boucle folle : plus de 20 exécutions par minute → l'automatisation se coupe et le
   dit. Files d'attente plafonnées à 10. Supprimer : humain seulement.
7. Bornes : 60 nœuds, 120 liens (doublons retirés), textes 2 000 caractères, 400
   automatismes ; la phrase française a un budget de calcul (pas d'explosion).

## Performance

La liste des automatismes **actifs** est en cache, recalculée seulement quand un
automatisme change : le moteur ne hache rien à chaque seconde ni à chaque événement (c'était
la régression attrapée par la mesure : CPU 0,58 % → 0,23 %).

## Modèle

Une automatisation = un **graphe** de nœuds reliés (comme n8n), stocké dans
`data/automations.json` (écriture atomique). Ports : `out` ; `yes`/`no` (Si) ;
`ok`/`timeout` (Attendre que).

| Famille | Nœud (`type`) | Paramètres |
|---|---|---|
| Déclencheurs | `when_state` | `point`, `to?`, `from?`, `for_s` (maintenu N s) |
| | `when_threshold` | `point`, `above?`, `below?`, `for_s` (franchissement) |
| | `at_time` | `at` « HH:MM », `days` (1 = lundi … 7) |
| | `at_sun` | `event` rise/set, `offset_min`, `days` |
| | `every` | `minutes` |
| | `on_start`, `manual` | — |
| Logique | `if` | `rules[]` (état / plage horaire / jour-nuit), `all` (ET) ou OU |
| Actions | `set` | `point`, `value` |
| | `toggle` | `point` |
| | `wait` | `seconds` |
| | `wait_for` | `rule`, `timeout_s` → `ok` / `timeout` |
| | `notify` | `title?`, `message`, `channels` (`maison`, `telegram`) |
| | `write` | `prompt` → texte `{{texte}}` pour la suite (IA, jamais de contrôle) |

Messages : `{{<appareil>/<clé>}}` = valeur actuelle (avec unité), `{{texte}}` = dernier texte
de Moli, `{{heure}}`.

Modes (comme HA) : `restart` (défaut : minuteries « mouvement → lumière 5 min »), `single`,
`queued`.

## Journal et coupure

Chaque modification d'un automatisme entre au journal du hub (`Action::Automation` :
créé, modifié, validé et allumé, coupé, revenu à la version validée, supprimé), avec qui
l'a faite (une personne, Moli, un agent, l'import) et l'empreinte de la version. Couper
(par `enabled` ou par un enregistrement avec `enabled: false`) **arrête toujours** ses
exécutions en cours, files d'attente comprises. Quand un non-humain coupe un automatisme ou
lui fait perdre sa validation, la maison est prévenue (notice dans le dashboard).

## Moteur

- Abonné au bus du hub : changements d'état (le hub n'émet qu'en cas de changement),
  valeur précédente gardée pour `from` et les franchissements de seuil. `for_s` = minuterie
  annulée si la valeur repart (sans `to` : chaque nouveau changement relance la minuterie).
- Horloge : un tic par seconde en heure locale (jiff, fuseau de la config) ; chaque
  déclencheur horaire retient la dernière minute où il a tiré (jamais deux fois, y compris
  après un redémarrage dans la même minute). L'heure sautée au passage à l'heure d'été ne
  tire pas (comme HA).
- Soleil : calcul astronomique local (NOAA, ±1 min, vérifié contre un almanach) depuis
  `[automations] latitude/longitude` ; à défaut, le point `daylight` de la météo.
- Exécution : parcours du graphe depuis le déclencheur, **branches dans l'ordre des liens**
  (qui fait partie de ce qu'on valide ; la position à l'écran n'y est pour rien), 100 étapes
  maximum. Chaque run a un jeton enfant de celui de l'automatisme : l'éteindre annule tout,
  files d'attente comprises ; un run en file revérifie qu'il est toujours la version validée.
- Chaque étape est journalisée (`data/automation-runs.jsonl`, 500 gardés, détails tronqués) et
  diffusée en direct (`/api/automations/live`) : l'éditeur colore les nœuds.
- Notifications : `maison` = événement `notice` du hub (toast dans le dashboard) ;
  `voix` = dit à voix haute par chaque enceinte qui sait annoncer (point `announce` du
  Sonos, commande d'automatisme au journal) ;
  `telegram` = une commande (titre, ou nom de l'automatisme, puis le message) sur chaque
  point `notify` inscriptible, c'est-à-dire l'appareil du pilote Telegram
  (`integrations/telegram/`), journalisée au nom de l'automatisme ; sans aucun point
  `notify`, le message part dans la maison.
- Persistance : `data/automations.json` écrit à part, `fsync`, puis renommé ; version
  précédente en `.bak` ; un fichier abîmé est mis de côté et Moli repart sur le `.bak`.

## Surfaces

- REST (détail dans [api.md](api.md)) : `GET/POST /api/automations`,
  `GET/PUT/DELETE /api/automations/{id}`, `POST …/{id}/approve {fingerprint}` (humain),
  `…/{id}/enabled {on, fingerprint}` (allumer = valider ; éteindre : tout le monde),
  `…/{id}/restore` (humain), `…/{id}/run {fingerprint}` (humain), `…/{id}/test` (à blanc),
  `POST /api/automations/check` (validation + phrase en français), `…/draft` (Moli),
  `…/import` (HA, humain), `GET …/runs`, `GET …/live` (SSE).
- MCP : `list_automations`, `get_automation`, `save_automation_draft` (brouillon non
  validé, documentation du format dans la description de l'outil), `test_automation`.
- Moli (conversation) : « préviens-moi quand… » → brouillon + carte « Valider ».

## Éditeur (onglet « Automatismes »)

- **Liste** : barre « Décris-le à Moli », idées prêtes en un clic, cartes (phrase en
  français, interrupteur, état Actif / À valider / Coupé / À corriger, dernière exécution),
  section repliable « Importés de Home Assistant », fil « Ce qui s'est passé ».
- **Éditeur** (`web/src/maison/auto/`) : canevas façon n8n (glisser, relier depuis un port ;
  lâcher dans le vide = ajouter une étape là ; zoom à la molette ou au pincement ; rangement
  automatique ; Suppr), panneau de réglage (appareil choisi par nom et pièce avec sa valeur
  actuelle, valeur adaptée au type, jours, durées, conditions), phrase écrite par le serveur
  en direct, « noms donnés dans Moli » (nom d'origine et identifiant de chaque appareil
  renommé, à vérifier avant de valider), problèmes signalés sur les nœuds, « Modifier avec Moli », essai à blanc,
  exécutions avec surlignage, nœuds qui s'allument en direct, validation par code PIN.

## Moli, le créateur

Voir [moli.md](moli.md) : une phrase → un graphe (JSON à schéma strict, vérifié, un tour de
correction), modèle `builder_model` (gpt-4.1), 5 à 9 s ; inventaire complet ; outil
`automation` dans la conversation.

## Import de Home Assistant

`tools/home-assistant/ha-automations-export.py` → `POST /api/automations/import`. Essai sur
un HA réel : 86 des 90 automatisations traduites en brouillons (61 sans erreur), dans leur
section repliable, **à relire** (quelques minuteries avaient une attente en double : consigne
corrigée depuis). Les présences par zone deviennent « à lancer à la main » faute d'appli
mobile ; les annonces Sonos deviennent « prévenir la maison ».

Garde-fous de l'import (détail dans [moli.md](moli.md)) :

- lancer un import exige une **session humaine** (PIN) du client qui appelle, sinon `403` ;
- chaque traduction passe par le **même sas que la conversation** (`admit()`, en laissant
  10 tours de la fenêtre à la famille) ; doublons écartés par nom enregistré et par SHA-256
  du YAML ; le plafond de 400 (existants + à traduire) est vérifié avant toute dépense ;
- **lien avec HA** : l'export garde l'identifiant HA (`ha_id`) ; la note du brouillon dit de
  quelle automatisation HA il vient et qu'il faut **désactiver l'original dans HA en le
  validant**, sinon les deux agissent. L'éditeur affiche ce début de note en avertissement
  (style `.warn`, comme la pièce protégée) tant que l'import n'a jamais été validé, et le
  rappelle après validation ; la section « Importés de Home Assistant » de la liste le dit
  aussi, et l'interrupteur d'une carte importée le rappelle en validant.

## Plus tard

Annonces vocales Sonos (TTS + `audioClip`), présence
(appli mobile), suggestions d'automatismes tirées de l'historique, Telegram testé en réel.

# Moli, l'assistant (`crates/moli-assistant`)

Une conversation (écrite, ou parlée en HTTPS) avec un modèle **compatible OpenAI** (OpenAI,
Mistral, un Ollama local en `http://`…). Moli lit l'inventaire de la maison, agit sous la
garde, **choisit ce que l'écran montre**, crée des automatismes en brouillon et rédige les
messages des automatismes. Il ne voit jamais d'image de caméra ni de secret, ni où est quelqu'un
au mètre près : la position d'un téléphone (`latitude`, `longitude`, `accuracy`) et son Wi-Fi
(`wifi`) ne sont ni dans l'inventaire, ni dans `get_device`, ni dans `history`, ni dans la
consigne d'un nœud « Moli écrit » (`house::private`) ; il lit `home`, `zone` et la batterie.

## Configuration

```toml
[assistant]
model = "gpt-4.1-mini"                 # la conversation
builder_model = "gpt-4.1"              # dessiner des automatismes (rare, donc un modèle plus fort)
transcribe_model = "gpt-4o-mini-transcribe"   # voix → texte ; "" = pas de voix
# base_url = "https://api.openai.com/v1"      # http://… pour un modèle local (pas de clé exigée)
```

L'heure que Moli connaît est celle de la maison : `[server] timezone`.

Clé : `moli-os secrets set assistant api_key` (une clé déjà donnée à l'intégration
`openai_conversation` de HA peut être reprise, toujours par tube). Elle ne part qu'en
https : un `base_url` en `http://` ne la reçoit jamais, même rangée dans le coffre. Les modèles « à
raisonnement » (gpt-5…, o…) sont reconnus (`max_completion_tokens`, `reasoning_effort: low`, pas de température).

### Pourquoi ces modèles (mesuré le 3 oct.)

| Modèle | Verdict |
|---|---|
| gpt-4o-mini | ❌ inventait des refus (« cette pièce est protégée ») **sans appeler `set`** |
| **gpt-4.1-mini** | ✅ conversation : 2 à 5 s, bons appels d'outils, ~0,5 centime la question |
| gpt-5-mini | plus précis mais 5 à 7 s et sec : trop lent pour la famille |
| **gpt-4.1** | ✅ conception d'automatismes : 5 à 9 s ; gpt-4.1-mini inversait le sens des capteurs de porte et oubliait des liens |

### Fournisseurs et niveaux de confidentialité

Les données que l'assistant manipule ont quatre niveaux de sensibilité (plus le nombre est petit, plus la
donnée est sensible) : **0** ne sort jamais (secrets, caméras, position), **1** reste chez soi (noms, valeurs en
direct, présence, historiques), **2** peut aller chez un cloud une fois pseudonymisée (la structure de la
maison), **3** est libre (code, documentation). Un fournisseur déclare `tier` = le niveau le plus sensible qu'il
peut recevoir ; il reçoit ce niveau et les moins sensibles, jamais le 0.

`[[assistant.provider]]` (`id`, `base_url`, `model`, `tier`) et `[assistant.routes]` (`chat` obligatoire ; `voice`
et `builder` suivent `chat` pour l'instant ; `audit` pour les tâches lourdes) remplacent `base_url` et `model` de
`[assistant]` : ces champs, `builder_model` et `search_model` sont ignorés en mode fournisseurs, et la recherche
web (qui passe par la recherche d'un fournisseur cloud) est coupée. Si le fournisseur de `chat` est chez soi,
la voix cloud l'est aussi (`transcribe_model` et `speech_model` vidés : Ollama ne fait ni l'un ni l'autre) et les
voix locales (Whisper, Piper) servent ; un fournisseur cloud garde la sienne. Un fournisseur nommé de niveau 1
hors de chez soi est accepté (c'est un choix déclaré), mais une ligne le dit au démarrage dans le journal. Sans fournisseur déclaré, rien ne change :
un fournisseur implicite `default` de niveau 1. La clé d'un fournisseur nommé est le secret `api_key.<id>`
(`moli-os secrets set assistant api_key.<id>`), jamais celui d'un autre : une clé donnée à un hôte ne part pas
vers un autre. `moli-os check-config` valide le tout ; `GET /api/assistant` expose `privacy` (mode, fournisseurs,
niveaux, routes ; `local` = adresse privée, boucle locale ou nom de la maison, pas seulement « sans TLS »).

Pour l'instant, la conversation ne peut aller que vers un fournisseur de niveau 1 : les niveaux 2 et 3 arrivent
avec le filtre de contexte. Un ancien `[assistant]` avec une clé et une adresse hors de chez soi écrit un
avertissement au démarrage : les noms et les valeurs en direct de la maison y partent.

### Le modèle maison (contrat `moli-v4`)

Optionnel et inactif par défaut. Un petit modèle local (Qwen3-1.7B affiné, GGUF Q4_K_M de 1,1 Go,
servi par Ollama sur le CPU) **reconnaît** la demande et appelle un outil ; Moli exécute sous la garde
et formule la réponse. Il ne voit aucune valeur de la maison : un prompt statique (pièces, appareils
numérotés `d1…dN` et leurs points) et les 25 outils du contrat (`tools/model/contract/tools.json`).

- **Le contrat est versionné.** `tools/model/contract/snapshots.txt` garde l'empreinte de
  `moli-os model tools` et `moli-os model prompt` pour chaque version : un test échoue si l'un des
  deux change sans nouvelle version. Un fournisseur qui déclare une autre version est refusé :
  l'assistant se dit hors service et dit pourquoi (statut `refused`), sans repli vers une
  conversation générale, et `moli-os check-config` échoue.
- **Les poids ne sont ni dans le dépôt ni dans l'image.** Moli ne télécharge rien. Le modèle est
  publié sur Hugging Face, avec l'empreinte de son fichier :

  ```sh
  hf download MaTa31/moli-home-1.7b-v4 --local-dir moli-home-1.7b-v4
  cd moli-home-1.7b-v4
  sha256sum -c SHA256SUMS      # moli-home-1.7b-v4-q4_k_m.gguf: OK
  ollama create moli-home-v4 -f Modelfile
  ```

  SHA-256 de `moli-home-1.7b-v4-q4_k_m.gguf` : `1e11026d6729ad589c4c3f076cd68534b7a9563efc0a9c967d2ae4136cf0e3c9`.
  **Garder le `Modelfile` fourni** : il fige le gabarit de conversation de l'entraînement (outils avant
  le prompt de la maison). Avec le gabarit par défaut de Qwen3 (`ollama pull` ou un `Modelfile` réduit
  à `FROM`), Ollama réécrit les outils dans un autre format et la précision chute nettement.
- **Dans `moli.toml`** : un fournisseur de niveau 1 qui déclare le contrat.

  ```toml
  [[assistant.provider]]
  id = "maison"
  base_url = "http://127.0.0.1:11434/v1"
  model = "moli-home-v4"
  tier = 1
  contract = "moli-v4"

  [assistant.routes]
  chat = "maison"
  ```

  Moli fait lire le prompt de la maison au modèle au démarrage (une trentaine de secondes sur un CPU
  modeste), puis la réponse vient en quelques secondes. Après un changement d'appareils ou de noms,
  le premier tour relit le prompt.
- **Garde-fous de Moli, hors du modèle** : les réglages d'un appareil (délais, calibrage…) ne sont ni
  montrés au modèle ni commandés depuis la conversation ; un ordre « pour plus tard » n'est exécuté que
  par `schedule` ou `remind` ; un appareil d'une autre pièce que celle que la personne a nommée n'est pas
  commandé ; ouvrir une serrure, une porte de garage ou un portail attend toujours la validation d'une
  personne (garde du hub), quelle que soit la pièce ; fermer passe (règle détaillée dans `SECURITY.md`).
- **Hors du cadre de la maison** (une blague, une explication), l'outil `delegate` répond « pas encore
  disponible » tant qu'aucun agent Hermes n'est configuré ; avec lui, la question lui est confiée,
  une seule par tour (voir « Confier une demande longue », plus bas).
- **Configuration proposée, puis confirmée par une personne.** `rename_device`, `move_device`,
  `set_device_enabled`, `rename_room`, `add_person` et `set_home_location` ne changent rien : Moli
  vérifie la demande, garde une **proposition** (10 minutes, en mémoire) et l'écrit sous sa réponse
  (Confirmer / Annuler). Seule une session humaine (`Origin::Ui`, le code de la maison) la confirme,
  par `POST /api/assistant/proposals/{id}` ; un agent, un script ou le tableau de bord sans code reçoit
  403, et le tableau de bord demande alors le code. La garde s'applique à la confirmation : une pièce
  protégée n'est pas renommée, un appareil n'en sort pas sans une personne.
  - Les noms et les pièces vont dans `home.json` (écriture atomique, ancien nom d'une pièce gardé
    en alias) ; un nom vide, trop long, identique ou avec un caractère de contrôle n'est pas proposé.
  - `add_person` ajoute un membre avec un nom, rien d'autre (ni e-mail, ni pièce, ni téléphone), par
    le même foyer que l'écran Personnes.
  - `set_home_location` cherche la ville par le géocodage d'**Open-Meteo** (le nom de la ville, et
    seulement lui, part chez ce service ; au plus 12 s de recherches par demande, 4 s chacune),
    arrondit la position à 0,01° et la garde dans `home.json` (`location`). La météo du tableau de
    bord et le soleil des automatismes la suivent sans redémarrage, même si Moli a démarré sans
    position ; au démarrage, elle l'emporte sur celle de `moli.toml`. `GET /api/home` ne la renvoie pas.
  - `undo` reprend ce qui vient d'être programmé, sinon propose l'inverse du dernier changement
    confirmé (à confirmer aussi) ; « annule » juste après une proposition refuse la plus récente
    **de celui qui parle** (le même client du tableau de bord), jamais celle d'un autre.
  - **Un satellite vocal ne propose rien** : il n'a pas d'écran où confirmer. Moli invite à faire la
    demande depuis le tableau de bord.
- **« Pas ça » : corriger une réponse, et que Moli s'en souvienne.** Sous chaque réponse du modèle
  maison, le tableau de bord offre « Pas ça » : « Défaire » remet la valeur d'avant des ordres du tour
  (dans l'ordre inverse, ce qui n'a pas répondu peut être réessayé), **seulement là où la valeur est
  encore celle que Moli a écrite** : ce qui a changé depuis (à la main, par quelqu'un, par un
  automatisme) est laissé tel quel, et la réponse le dit. Puis la personne choisit pièce →
  appareil → action (ou : lire, l'heure, la météo, ne rien faire), parmi ce que Moli lui propose
  (`choices`) ; le choix renvoyé est contrôlé contre cette liste. « Retenir pour la prochaine fois »
  garde une **règle** (`assistant-rules.json`, 500 au plus, numéros aléatoires qui ne reviennent
  jamais, même après un redémarrage) qui répond **avant** le modèle quand la même phrase revient.
  - Une règle passe par le même chemin que le modèle (`house_call`) : même garde (pièces protégées,
    heures calmes, validations), mêmes refus (réglages, « plus tard », autre pièce, ouverture).
  - Seule une session humaine (`Origin::Ui`, le code) corrige, retient, désactive ou supprime une
    règle ; un agent, un script ou le tableau de bord sans code reçoit `403`.
  - Prudence : une phrase qui dépend de ce qui précède (« éteins-la », « monte un peu », « fais
    l'inverse ») est corrigée mais jamais retenue ; un nombre ou un mot court doit revenir à
    l'identique ; une phrase sans pièce ni appareil n'est pas retenue pour un appareil.
  - Chaque correction est notée dans `assistant-corrections.jsonl` (fichiers en 0600 ; à 1 Mo, il devient
    `assistant-corrections.1.jsonl` et un nouveau commence : les deux derniers mégaoctets sont gardés),
    **dans la maison** : rien n'est envoyé, l'export se fait à la main (pour réentraîner son modèle).
    Voir `docs/PRIVACY.md`.
  - Un tour est **à celui qui l'a posé** (`Turn::asker` : la personne si la maison la connaît, sinon
    l'appareil) : lui seul le défait, le corrige ou en voit les choix (`410` pour quelqu'un d'autre). Les 50
    derniers tours **de chacun** restent corrigeables (en mémoire, 500 en tout) : un agent bavard ne chasse que
    les siens. Un tour arrêté ne l'est pas.
  - « Ce que Moli a appris » dit qui a appris quoi : la liste se lit avec le code, comme elle se change.
  - Système → « Ce que Moli a appris » liste les règles, les désactive ou les supprime.
  - Après une confirmation, Moli fait relire le prompt de la maison au modèle, en arrière-plan.

## Une question, côté serveur (`turn`)

1. Prompt système en français : style (2 phrases, 40 mots, pas de jargon, pas de relance),
   règles d'action, garde-fous, date, **électricité en direct par circuit**, pièces,
   **inventaire** (pièce → appareils : `id · nom · clé✎=valeur unité [choix]` ; pièces
   protégées marquées ; un appareil injoignable n'a pas de valeurs : « état inconnu »).
2. Boucle d'outils (6 tours maxi, le dernier sans outil) :
   - `show(cards)` : ce que l'écran affiche (`device`, `room`, `energy`, `weather`,
     `remote`, `cameras`, `history`) ; cartes inventées ou masquées refusées.
   - `set(point, value)` : agit en `Origin::Assistant` → la garde s'applique comme à tout
     agent ; un ordre retenu revient à l'écran avec un bouton « Autoriser » (code PIN si
     besoin).
   - `get_device`, `energy`, `history` (le passé se vérifie : « personne n'a sonné » sans
     l'avoir lu est interdit ; un mouvement n'est jamais un coup de sonnette).
   - `automation(request)` : crée un **brouillon** d'automatisme et montre une carte
     « Valider ».
3. Filets de sécurité, tous testés (`lib.rs`) : cartes évidentes ajoutées si le modèle n'en
   montre aucune (conso, météo, caméras) ; **appel d'outil écrit en clair supprimé**
   (`show({…})`, `show [ … ]`) ; réponse répétée dite une fois ; relances (« tu veux… ? ») et
   « voici… » retirés quand des cartes sont là ; découpage des phrases qui respecte les
   nombres (« 22.9 »).
4. Limites : 60 tours / 10 min et 2 en même temps (tous clients confondus), 14 messages
   d'historique, 2 000 caractères par message. Les messages des automatismes ne comptent pas
   dans cette limite (ils ont la leur : 20 lancements par minute). Dans un tour, les appels
   payants sont comptés, quoi que demande le modèle : 2 `web_search` et 1 `automation` au
   plus ; au-delà, l'outil répond « limite atteinte » et le tour continue. Le coût d'un
   brouillon (modèle `builder_model`) compte dans celui de l'échange.

## Créer un automatisme (`auto.rs`)

- Une phrase → un graphe par **sortie JSON à schéma strict** (tous les champs listés, `null`
  pour ceux qui ne servent pas), converti au format du moteur, **vérifié par le vrai
  vérificateur**, un tour de correction si erreur ou nœud jamais atteint.
- Consigne : catalogue des nœuds, sens des capteurs (`contact` false = ouverte ;
  `doorcontact_state` true = ouverte), « allume 5 min » = set → wait → set (mode restart),
  « la nuit » = règle soleil, deux exemples complets, champs obligatoires, ne jamais viser de
  soi-même une pièce protégée.
- Inventaire « constructeur » : **tous** les appareils et leurs valeurs, joignables ou non (un
  automatisme agit plus tard), et les compteurs électriques par leur sens (« la
  consommation » = le compteur général).
- Modifier un automatisme existant : le graphe actuel est donné, les positions des nœuds
  conservées sont gardées.
- Les règles `stale` sont disponibles dans `if.rules[]` et `wait_for.rule`, y compris
  lors d'une retouche : `point` et `for_s` entier de 1 à 86 400 secondes. Elles vérifient
  le silence du capteur, pas l'absence de changement de sa valeur. Dans la sortie stricte,
  `for_s` vaut `null` pour les autres règles ; les champs inutilisés sont retirés avant
  la vérification du graphe par le moteur.

## Importer les automatisations de Home Assistant (`import.rs`)

`tools/home-assistant/ha-automations-export.py` (dans le conteneur HA, lecture seule ; adresses,
webhooks et jetons masqués ; entités et appareils cités avec nom et pièce ; identifiant HA
de chaque automatisation dans `ha_id`, hors du YAML) → `POST /api/automations/import` →
traduction en tâche de fond, une à la fois, en brouillons coupés (auteur `import`). Essai
sur un HA réel (90 automatisations) : **86 traduites, 61 sans erreur**, ~3-4 $. Ce que Moli
ne sait pas encore faire (une intégration absente, une fonction de HA sans équivalent) est
adapté ou omis et dit dans la note du brouillon.

Garde-fous de l'import :

- **Geste d'humain** : sans session PIN du client qui appelle, `403` avant même de lire le
  corps. L'export se poste donc depuis l'ordinateur où le tableau de bord est ouvert, avec
  son cookie `moli_session` (mode d'emploi en tête du script) ; depuis l'hôte sans session,
  refusé.
- **Même sas que la conversation** : chaque traduction prend un tour de `admit()` (2 en même
  temps, 60 par 10 min, tous clients confondus) en laissant **10 tours de la fenêtre** à la
  famille. Faute de tour, l'import attend (5 s entre deux essais) ; s'il n'en obtient aucun
  pendant une fenêtre entière, il s'arrête et le message de fin dit combien restent
  (relancer l'import reprend là : les déjà importés sont sautés).
- **Rien n'est dépensé pour rien** : dédoublonnage par nom tel qu'il est enregistré
  (l'alias coupé à 80 caractères, comme `save` le coupe : `moli_automation::tidy_name`) et
  par **SHA-256 du YAML**, dans le lot et contre les YAML déjà traduits depuis le démarrage
  (un brouillon renommé n'est pas retraduit) ; puis `automatismes existants + à traduire ≤
  400` vérifié **avant** de lancer quoi que ce soit (sinon `400`, rien n'est traduit).
- Un seul import à la fois (`429` sinon) ; le drapeau est rendu par un garde `Drop`, même si
  la tâche panique.
- **Lien avec HA** : la note du brouillon commence par « Importé de Home Assistant
  (automation « <alias> », id <ha_id>). Quand tu valides celui-ci, désactive l'original dans
  HA, sinon les deux agiront. » (sans `ha_id` : sans « , id … »). Le tableau de bord en fait
  un avertissement à la validation (voir [automatismes.md](automatismes.md)).

Limite connue : les empreintes de YAML sont gardées en mémoire, pas sur disque ; après un
redémarrage, seul le nom protège d'un doublon. La note est effacée à la validation (comme
toute note) : le lien vers HA ne survit pas à la validation. Un champ de provenance
`{ha_id, yaml_sha256}` propriété du serveur sur `Automation` réglerait les deux (et
servirait au futur bouton « couper l'original dans HA »).

Les brouillons demandés par mots (`/api/automations/draft`) passent aussi par `admit()` ;
la demande est coupée à 2 000 caractères et la version en cours d'édition (`current`)
passe par les mêmes bornes que l'enregistrement (`tidy_name`, `tidy` : 60 nœuds, 120 liens,
textes de 2 000 caractères) avant d'entrer dans le prompt (`400` si elle les dépasse).

## Écrire pour un automatisme (`compose`, trait `Writer`)

Le nœud « Moli écrit » : un texte chaleureux de 80 mots maxi à partir de l'état de la maison
(`{{texte}}` pour l'étape suivante). Des mots, jamais une décision. Un échec arrête la branche
(aucun message vide n'est envoyé).

## Voix (conversation, 7 oct.)

Un appui sur « Parler avec Moli » (bulle ou page Moli), puis on discute : Moli écoute, répond
avec une vraie voix, se remet à écouter, se tait si on lui coupe la parole, et raccroche après
8 s de silence, au bouton, sur une phrase de fin seule (« merci », « c'est tout », « stop »,
« au revoir », « ça ira », « bonne nuit ») ou après deux incompréhensions de suite. Le micro
exige HTTPS (par exemple `https://maison.example.org` derrière Cloudflare Access) ; l'onglet
masqué raccroche.

### Serveur

- `POST /api/assistant/speak` `{ "text": "…", "voice"?: "coral" }` (1 à 300 caractères ; `voice` = une voix OpenAI pour essayer, sinon `speech_voice`) → `audio/mpeg` (voix
  OpenAI `speech_model`, au débit `speech_speed`, 1,2 par défaut, de 0,25 à 4) ou `audio/wav`
  (Piper `local_speech`), en-tête `x-moli-voice:
  cloud|local`. Le texte passe par `speakable()` (« 22.9 °C » → « 22,9 degrés », W, kWh, €,
  %, « 21h05 »). Budget à part de la conversation : 150 phrases / 10 min, 3 à la fois.
  Avec `accept: audio/pcm` (ce que fait le tableau de bord), la voix OpenAI arrive **pendant**
  qu'elle est faite : PCM 16 bits, `audio/pcm;rate=24000`, relayé morceau par morceau (coupé après
  10 s sans rien ou 4 Mo), joué au fil de l'eau par `player.playSpeech` : le premier mot environ
  une seconde plus tôt. La compression des réponses ne touche jamais `audio/*` (elle retiendrait
  le son). Piper reste entier (WAV).
- `POST /api/assistant/listen` : WAV 16 kHz mono (ce que le tableau de bord envoie) ou tout
  format audio accepté par OpenAI → `{ text, engine }`. Cloud d'abord, **Whisper local**
  (`local_listen`) si le cloud échoue ou est coupé (WAV seulement).
- `POST /api/assistant` avec `spoken: true` : consigne orale ajoutée au prompt (une phrase
  d'une quinzaine de mots, rien que la réponse, pas de symbole, nombres comme on les dit,
  « il faut valider à l'écran » si un ordre attend).
- En conversation, `show` n'est pas proposé au modèle : un aller-retour de moins (environ une
  seconde). L'écran reçoit les cartes que la question appelle (`guess_cards`) et celles des
  appareils commandés. Hors conversation, un tour s'arrête aussi dès que le modèle écrit sa
  réponse avec son appel à `show` (les cartes ne renvoient rien d'utile).
- Le journal `assistant turn` donne les outils appelés dans l'ordre (`tools=get_device,show`) :
  c'est là qu'on voit où un tour lent a passé son temps.
- `GET /api/assistant` : `listen`, `speak`, `voices: { cloud, local }`.

| Panne | Comportement |
|---|---|
| Voix OpenAI KO | Piper (`x-moli-voice: local`) |
| Piper KO aussi | voix du navigateur |
| Écoute cloud KO | Whisper local ; les deux KO → « je n'ai pas compris », réécoute ; 2× → fin |
| 429 / Moli injoignable | Moli raccroche |

### Réglages depuis le tableau de bord (page Système → « Moli, l'assistant »)

- **Clé OpenAI** : `PUT /api/assistant/key` `{api_key}` (**humain**, code demandé) → vérifiée
  par `GET /v1/models` (refusée = 400 avec la raison ; refusée d'emblée, sans rien envoyer,
  si `base_url` est en `http://`), puis rangée chiffrée dans le coffre
  (`assistant/api_key`), jamais renvoyée. Tuto pas à pas dans la carte (compte, crédit,
  limite de dépense, création de la clé). Prise en compte immédiatement.
- **Voix, ton et débit** : `GET|PUT /api/assistant/settings` `{voice?, style?, pace?}` (sans code :
  un goût, pas un pouvoir). Tons : `enjoue` (défaut), `pose`, `doux`. Débits : `normal` (1),
  `rapide` (1,2, le défaut de `speech_speed`), `tres_rapide` (1,4). Gardés dans
  `data/assistant.json` ; sans ce fichier, `speech_voice`/`speech_style` de `moli.toml`.
  ▶ à côté de chaque voix pour l'écouter.
- **Moli toujours présent** : sans `[assistant]` dans `moli.toml`, il démarre avec ses
  défauts et attend une clé (nouvelle installation).

### Navigateur (`web/src/maison/lib/voice/`)

`capture.js` (micro → AudioWorklet → trames de 20 ms à 16 kHz), `vad.js` (énergie contre un
plancher de bruit appris : début après 150 ms au-dessus de 3× le plancher, fin après 700 ms
de silence ; pendant que Moli parle, seuil ×2 et 300 ms, plancher figé pour ne pas apprendre
l'écho), `pcm.js` (rééchantillonnage, WAV), `speech.js` (phrases : la 1re part seule pour que
la voix démarre vite ; phrases de fin), `player.js` (un seul `AudioContext` débloqué au
toucher, indispensable sur iOS), `conversation.svelte.js` (états `listening → hearing →
thinking → speaking`). Le bouton micro (une seule question) enregistre aussi en WAV.
Tests : `cd web && npm test`.

### Satellites ESPHome (`crates/moli-esphome/`)

Les réponses de conversation et le carillon de fin passent tous deux par le lecteur
d'annonces du firmware, via l'événement `TTS_END`. Certains firmwares n'embarquent que
les décodeurs FLAC et MP3 : Moli fournit donc ces deux sons en **FLAC**, y compris pendant
une conversation. L'enregistrement envoyé à la transcription reste en WAV. Le pilote
demande `VoiceBrain::announcement_url` pour la réponse et publie le carillon en
`audio/flac`. Les tests relisent les événements chiffrés remis au satellite et décodent
le carillon avec un véritable décodeur FLAC.

Un journal `voice spoken` ne prouve pas que le haut-parleur a joué la phrase : il est
aussi émis lorsque le destinataire ferme le flux. La validation physique demande de
vérifier la lecture sur le boîtier, après les tests de protocole et de format.

## Côté dashboard

Bulle sur toutes les pages (réponse courte, 2 à 4 cartes) et page Moli (l'écran se compose à
droite, chaque réponse « revoyable »). Voir [dashboard.md](dashboard.md).

## Confier une demande longue à un agent de la maison (Hermes)

Une recherche fouillée, une organisation, une rédaction : Moli peut les confier à un agent
**Hermes** qui tourne à la maison, puis annoncer sa réponse quand elle arrive. Désactivé tant
que `[assistant.hermes]` n'est pas écrit.

```toml
[assistant.hermes]
url = "http://127.0.0.1:8642"      # chez soi seulement : boucle locale, adresse privée, nom local
model = "…"                        # facultatif : le modèle d'une demande ordinaire
heavy_model = "…"                  # facultatif : celui d'une demande jugée lourde
```

Le jeton de l'agent va dans le coffre, par un tube, comme la clé du modèle :
`read -rs V; printf %s "$V" | moli-os secrets set assistant hermes_token; unset V`.
Sans lui, Moli démarre sans l'agent.

Les règles, qui ne se règlent pas :

- **L'agent reste à la maison.** Une `url` qui n'est pas chez soi (même règle que les
  fournisseurs : boucle locale, adresse privée, `localhost`, nom sans point, `.local`, `.lan`)
  fait échouer `check-config` ; au démarrage, Moli démarre sans l'agent plutôt que pas du tout.
  L'adresse est lue une seule fois, par ce qui s'y connecte, et une `url` avec `#`, `?` ou
  des identifiants (`user@`) est refusée : deux lectures n'y verraient pas le même hôte.
- **Seule la demande sort** : ni la conversation, ni la mémoire, ni une image, ni l'état de la
  maison. Ce que l'agent répond est une information : rendue au modèle tronquée et marquée
  comme telle, et dès qu'elle a été lue, plus aucun outil qui agit sur la maison ou qui sort
  (`web_search`) n'est offert ni exécuté dans ce tour, même si le modèle le nomme. **Ni dans
  les tours suivants qui la ramènent** : la conversation renvoie à chaque tour les réponses de
  Moli, et une réponse écrite après avoir lu l'agent peut la porter, telle quelle ou redite.
  Moli garde l'empreinte de ces réponses (24 h, 64 au plus, en mémoire : un redémarrage
  l'oublie), et un tour dont la conversation en contient une, parmi les messages que lit le
  modèle, n'agit pas non plus. Une conversation nouvelle agit de nouveau. Son statut
  n'arrive au modèle que comme un mot d'une liste fermée (`started`, `running`, `completed`,
  `failed`, `stopped`, sinon `unknown`), et l'identifiant de sa tâche est filtré et borné
  (64 caractères). Une seule tâche confiée par tour, pour le modèle maison aussi.
- **Le modèle lourd est nommé ici**, jamais choisi par le modèle de la conversation : il peut
  seulement dire « cette demande est lourde ».
- **Une session neuve à chaque demande** : rien ne passe d'une demande à l'autre, et Moli le
  dit si on lui demande de « reprendre ».
- **Chacun sa demande** : Moli retient la dernière tâche confiée **par personne** (la personne
  quand la maison la connaît, sinon l'appareil qui demande ; chaque boîtier vocal à part). « Où en est ma demande ? »,
  « précise-lui… », « arrête-la » ne visent qu'elle ; aucun identifiant ne passe par le modèle.
- **Pas de code** : l'agent ne modifie ni modules ni configuration.

Moli suit une tâche confiée en redemandant toutes les 20 s (30 min au plus) et, quand elle se
termine, en rend la réponse à qui l'a demandée, une fois : le boîtier vocal qui a parlé la dit
(s'il a un point `say`), le tableau de bord de la personne qui a demandé l'affiche, personne
d'autre ne la voit. Un redémarrage perd ce suivi : la réponse se
demande alors. Le modèle maison (`delegate` du contrat) passe par le même chemin.

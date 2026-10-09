# Moli, l'assistant (`crates/moli-assistant`)

Une conversation (écrite, ou parlée en HTTPS) avec un modèle **compatible OpenAI** (OpenAI,
Mistral, un Ollama local en `http://`…). Moli lit l'inventaire de la maison, agit sous la
garde, **choisit ce que l'écran montre**, crée des automatismes en brouillon et rédige les
messages des automatismes. Il ne voit jamais d'image de caméra ni de secret.

## Configuration

```toml
[assistant]
model = "gpt-4.1-mini"                 # la conversation
builder_model = "gpt-4.1"              # dessiner des automatismes (rare, donc un modèle plus fort)
transcribe_model = "gpt-4o-mini-transcribe"   # voix → texte ; "" = pas de voix
# base_url = "https://api.openai.com/v1"      # http://… pour un modèle local (pas de clé exigée)
# timezone = "Europe/Paris"
```

Clé : `moli-os secrets set assistant api_key` (une clé déjà donnée à l'intégration
`openai_conversation` de HA peut être reprise, toujours par tube). Les modèles « à
raisonnement » (gpt-5…, o…) sont reconnus (`max_completion_tokens`, `reasoning_effort: low`, pas de température).

### Pourquoi ces modèles (mesuré le 3 oct.)

| Modèle | Verdict |
|---|---|
| gpt-4o-mini | ❌ inventait des refus (« cette pièce est protégée ») **sans appeler `set`** |
| **gpt-4.1-mini** | ✅ conversation : 2 à 5 s, bons appels d'outils, ~0,5 centime la question |
| gpt-5-mini | plus précis mais 5 à 7 s et sec : trop lent pour la famille |
| **gpt-4.1** | ✅ conception d'automatismes : 5 à 9 s ; gpt-4.1-mini inversait le sens des capteurs de porte et oubliait des liens |

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
   dans cette limite (ils ont la leur : 20 lancements par minute).

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
  par `GET /v1/models` (refusée = 400 avec la raison), puis rangée chiffrée dans le coffre
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
## Côté dashboard

Bulle sur toutes les pages (réponse courte, 2 à 4 cartes) et page Moli (l'écran se compose à
droite, chaque réponse « revoyable »). Voir [dashboard.md](dashboard.md).

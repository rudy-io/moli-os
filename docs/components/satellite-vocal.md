# Le satellite vocal (ESPHome)

Un appareil posé dans une pièce : on dit un mot de réveil, on parle, Moli répond par la voix
de l'appareil. C'est l'équivalent d'une enceinte connectée, mais **le cerveau est Moli** : pas
de cloud, pas de pipeline chez quelqu'un d'autre.

Le pilote `esphome` (`crates/moli-esphome`) parle l'**API native d'ESPHome** — TCP 6053,
chiffrée par Noise — **sans aucune bibliothèque ESPHome** : le transport est écrit depuis la
spécification Noise (`noise.rs`, `frame.rs`), les messages à la main (`api.rs`). L'appareil
reste un appareil ESPHome ordinaire, que l'on construit et met à jour avec ESPHome.

## Ce que Moli attend de l'appareil

Quatre composants du firmware, chacun pour une raison précise :

| Composant ESPHome | Ce que Moli en fait | Obligatoire |
|---|---|---|
| `voice_assistant` | tout le dialogue : Moli s'abonne, reçoit le son, renvoie la voix | **oui** |
| `micro_wake_word` | le mot de réveil est détecté **sur l'appareil**, sans réseau | oui en pratique |
| `media_player` | joue l'URL de la réponse ; Moli y lit le volume et la sourdine | **oui** |
| `event` (`double_press`) | un double appui met fin à la conversation | non |
| `speaker` + `microphone` | l'audio lui-même (I2S) | **oui** |

Moli ne fait **aucune** détection de mot de réveil et n'envoie aucun son par l'API : il
**fabrique** la voix et publie un fichier WAV, dont il donne l'adresse à l'appareil, qui va le
chercher en HTTP.

## Le matériel

| | Ce qu'il faut |
|---|---|
| Minimum | un **ESP32-S3** (PSRAM recommandée : le mot de réveil tourne en local), un micro I2S (INMP441…), un ampli I2S + haut-parleur (MAX98357A…) |
| Prêt à l'emploi | **Home Assistant Voice Preview Edition** (ESP32-S3, deux micros, XMOS), M5Stack Atom Echo, Tab5… tous parlent le même `voice_assistant` |
| Réseau | l'appareil et Moli sur le **même réseau** ; l'appareil doit joindre Moli en HTTP sur `:8790` |

Le Wi-Fi et le 2,4 GHz sont ceux de l'ESP32 : pas de 5 GHz seul.

## Le firmware

Un squelette complet. **Validé** par `esphome config` (ESPHome **2026.9.1**, `Configuration is
valid!`), mais **jamais flashé sur un appareil** : les broches sont des exemples, à adapter au
câblage réel.

```yaml
substitutions:
  broche_micro_donnees: GPIO15
  broche_i2s_entree_horloge: GPIO4
  broche_i2s_entree_bits: GPIO5
  broche_hp_donnees: GPIO16
  broche_i2s_sortie_horloge: GPIO6
  broche_i2s_sortie_bits: GPIO7
  broche_bouton: GPIO18   # pas GPIO0 : broche de démarrage (strapping)

esphome:
  name: moli-satellite
  friendly_name: Satellite Moli
  min_version: 2026.3.0

esp32:
  board: esp32-s3-devkitc-1
  variant: esp32s3
  framework:
    type: esp-idf

psram:
  mode: octal
  speed: 80MHz

wifi:
  ssid: !secret wifi_ssid
  password: !secret wifi_password

logger:
  level: INFO

api:
  encryption:
    key: !secret api_key

ota:
  - platform: esphome

i2s_audio:
  - id: bus_entree
    i2s_lrclk_pin: ${broche_i2s_entree_horloge}
    i2s_bclk_pin: ${broche_i2s_entree_bits}
  - id: bus_sortie
    i2s_lrclk_pin: ${broche_i2s_sortie_horloge}
    i2s_bclk_pin: ${broche_i2s_sortie_bits}

microphone:
  - platform: i2s_audio
    id: micro
    i2s_audio_id: bus_entree
    i2s_din_pin: ${broche_micro_donnees}
    adc_type: external
    pdm: false
    sample_rate: 16000
    bits_per_sample: 32bit
    channel: left        # « mono » est refusé par ESPHome

speaker:
  - platform: i2s_audio
    id: hp_sortie
    i2s_audio_id: bus_sortie
    i2s_dout_pin: ${broche_hp_donnees}
    dac_type: external
    sample_rate: 48000
    bits_per_sample: 16bit
    channel: mono

media_source:
  - platform: audio_http
    id: annonces_http
    buffer_size: 250000

media_player:
  - platform: speaker_source
    id: enceinte
    name: None
    announcement_pipeline:
      format: FLAC
      sample_rate: 48000
      num_channels: 1
      speaker: hp_sortie
      sources:
        - annonces_http

micro_wake_word:
  id: mww
  microphone: micro
  models:
    - model: hey_jarvis        # mot livré avec ESPHome ; « hey Moli » demande un modèle entraîné
      id: hey_jarvis
  on_wake_word_detected:
    - voice_assistant.start:
        wake_word: !lambda return wake_word;

voice_assistant:
  id: va
  microphone: micro
  media_player: enceinte
  micro_wake_word: mww
  use_wake_word: false
  on_client_connected:
    - micro_wake_word.start:
  on_client_disconnected:
    - voice_assistant.stop:
  on_end:
    - wait_until:
        not:
          media_player.is_announcing:
            id: enceinte
    - micro_wake_word.start:

binary_sensor:
  - platform: gpio
    id: bouton
    internal: true
    pin:
      number: ${broche_bouton}
      inverted: true
    on_multi_click:
      - timing:
          - ON for at most 1s
          - OFF for at least 0.25s
        then:
          - voice_assistant.start:
      - timing:
          - ON for at most 1s
          - OFF for at most 0.25s
          - ON for at most 1s
          - OFF for at least 0.25s
        then:
          - event.trigger:
              id: bouton_evenement
              event_type: double_press

event:
  - platform: template
    id: bouton_evenement
    name: Bouton
    event_types:
      - double_press
```

### Ce qu'il ne faut pas retirer

- **`media_source: audio_http`** et sa présence dans `announcement_pipeline.sources` : sans
  elle, l'appareil ne sait pas aller chercher le WAV de Moli. Une annonce sans source HTTP
  échoue silencieusement côté appareil.
- **`on_client_connected` → `micro_wake_word.start`** : le mot de réveil ne tourne que
  lorsqu'un client est connecté. Sans cette ligne, l'appareil est sourd.
- **`voice_assistant.media_player`** : c'est par lui que passe la voix de la réponse.

### Construire l'image

Deux chemins, le même YAML :

| Chemin | Comment |
|---|---|
| **ESPHome Device Builder** (l'add-on de Home Assistant) | créer un appareil, coller le YAML, *Install* → OTA. Le constructeur parle directement à l'appareil, **il n'a pas besoin que l'intégration ESPHome de HA gère l'appareil** |
| Ligne de commande / Docker | `docker run --rm -v ./:/config esphome/esphome run moli-satellite.yaml` |

**Rien à installer à côté.** ESPHome n'est pas un service auquel Moli se connecterait : c'est un
**firmware** qui tourne sur l'appareil. Le constructeur ne sert qu'à compiler et flasher, et
celui de Home Assistant fait très bien l'affaire — inutile d'en avoir un second. Moli, lui,
parle directement à l'appareil sur le port 6053 : il ne passe **ni par ESPHome, ni par Home
Assistant**.

La ligne Docker ci-dessus n'est donc pas une seconde installation : c'est le même outil, en
local, pour **valider un YAML hors ligne** (`esphome config`, ce qui a servi à écrire cette
page) ou pour flasher depuis un poste qui n'a pas Home Assistant.

Renseigner `secrets.yaml` (`wifi_ssid`, `wifi_password`, `api_key`) — au moins 32 octets en
base64 pour la clé, ESPHome la génère autrement.

### Deux configurations de référence, éprouvées

Plutôt que d'inventer, s'inspirer de ce qui tourne déjà :

- **Home Assistant Voice PE** :
  `https://github.com/esphome/home-assistant-voice-pe` (`home-assistant-voice.yaml`) — LED,
  bouton unique et double, sourdine matérielle, timers, deux micros ;
- **M5Stack Tab5** : `esphome/devices.esphome.io`, `M5Stack-Tab5/voice-assistant.yaml` — la
  version la plus courte qui fonctionne.

## Le mot de réveil

Il n'y a **pas de « hey Moli »** dans le firmware, et ce n'est pas un réglage de Moli : le mot est
détecté **sur l'appareil**, par un petit modèle (microWakeWord) qui tourne hors ligne. ESPHome
n'en fournit que quelques-uns, entraînés à l'avance — `okay_nabu`, `hey_mycroft`, `hey_jarvis`.
Le squelette ci-dessus prend `hey_jarvis` parce qu'il existe ; aucun de ces mots ne nomme Moli, et
cela n'a aucune importance pour lui : il reçoit la phrase prononcée **après** le mot, jamais le
mot lui-même.

`wake_words` dans `moli.toml` ne peut donc qu'**activer** un mot que le firmware contient déjà
(voir « Pièges » 5) — il ne peut pas en inventer un.

| Je veux | Comment |
|---|---|
| **Le plus simple** | garder un mot du catalogue (`hey_jarvis`, `okay_nabu`, `hey_mycroft`) ; c'est toujours Moli qui répond |
| **« hey Moli »** | entraîner un modèle microWakeWord (dépôt `kahrendt/microWakeWord`, quelques heures), puis déclarer son JSON dans `models:` — un manifeste de modèle porte un champ `trained_languages`, donc une phrase française est possible |
| **Sans mot de réveil** | un appui sur le bouton lance la conversation (voir le `binary_sensor` du squelette) |

Un mot entraîné sur une autre langue s'entend moins bien en français : pour une phrase française,
l'entraîner avec des enregistrements français.

## La configuration côté Moli

```toml
[[driver]]
id = "salon"                  # le nom que tu veux ; il préfixe les appareils
kind = "esphome"

[driver.options]
host = "192.168.1.x"                          # l'adresse de l'appareil
media_base = "http://192.168.1.10:8790"       # l'adresse de Moli VUE PAR L'APPAREIL
wake_words = ["hey_jarvis"]                   # vide : l'appareil garde les siens
mac = "aa:bb:cc:dd:ee:ff"                     # optionnel (voir « Pièges »)
logs = "warn"                                 # none, error, warn, info, debug
```

| Option | Défaut | Effet |
|---|---|---|
| `host` | — | adresse de l'appareil (obligatoire) |
| `port` | `6053` | port de l'API native |
| `media_base` | absent | **sans elle, aucune voix** : l'appareil ne saurait pas où prendre le son |
| `window_s` | `8` | secondes pendant lesquelles une phrase doit commencer après le mot de réveil |
| `wake_words` | vide | ids des mots à activer parmi ceux du firmware |
| `mac` | absent | adresse attendue : garde-fou du provisionnement et recherche d'un appareil qui a changé d'adresse |
| `logs` | `warn` | le journal de l'appareil, reversé dans celui de Moli |

**La clé chiffrée** vit dans le coffre, jamais dans `moli.toml` :

```sh
read -rs V; printf %s "$V" | moli-os secrets set salon api_key; unset V
```

La valeur est celle de `api: encryption: key:` du firmware (base64). Le pilote la convertit en
clé Noise (`noise::psk_from_base64`).

## Ce que le satellite publie dans Moli

| Point | Écriture | Valeurs |
|---|---|---|
| `state` | non | `idle`, `listening`, `thinking`, `speaking` |
| `volume` | oui | 0 à 100 % (lu sur le `media_player` de l'appareil) |
| `muted` | oui | vrai / faux |
| `wake_word` | non | le dernier mot de réveil entendu (son id dans le firmware) |
| `say` | oui | un texte (500 caractères au plus) dit à voix haute, dans la voix de Moli |

`say` est une **annonce** : elle passe par-dessus ce que l'appareil joue, qui revient ensuite.
C'est une commande comme une autre — **la garde s'applique** (pièce protégée, heures calmes).

## Une conversation, pas à pas

1. L'appareil entend son mot de réveil et envoie `VoiceAssistantRequest{start}` avec la phrase
   du mot.
2. Moli s'abonne s'il ne l'est pas déjà (`subscribe_voice_assistant`) et **accepte** :
   `VoiceAssistantResponse{port: 0}` — port 0 veut dire « le son passe par l'API », pas par UDP.
3. Moli envoie `RUN_START`, puis `STT_START`, et **écoute**. Il **découpe lui-même** le début et
   la fin de la phrase (VAD, `moli-audio`) : `STT_VAD_START` quand la voix commence,
   `STT_VAD_END` quand elle s'arrête. L'appareil ne décide jamais de la fin.
4. La phrase part chez Whisper, le texte chez le modèle, la réponse revient.
5. `INTENT_END{conversation_id, continue_conversation=1}`, `TTS_START{text}`, puis
   `TTS_END{url}` — l'URL d'un WAV publié par Moli (`/api/media/<nom aléatoire>`, valable
   5 minutes).
6. L'appareil va chercher ce fichier et le joue en **annonce**. À la fin, il envoie
   `VoiceAssistantAnnounceFinished`.
7. Moli attend alors une nouvelle demande pendant `CONTINUE_WAIT` (5 s) : la conversation reste
   ouverte **sans redire le mot de réveil**. Sinon, retour à `idle`.
8. Pour raccrocher : une phrase de fin (« merci », « c'est tout », « stop », « au revoir »…), un
   double appui, ou **une transcription vide** — une seule suffit sur un satellite (le silence
   est enregistré dans l'historique comme une fausse activation, mais il n'y a pas de second
   essai : la conversation s'arrête). Moli envoie alors
   `INTENT_END{continue_conversation=0}` et **un carillon** qu'il fabrique lui-même
   (`sound.rs`, aucun fichier sur l'appareil).

## ⚠️ Un seul cerveau : Home Assistant **ou** Moli

C'est le piège qui coûte une soirée, et il est vérifié dans le code des deux côtés.

**Côté ESPHome** (`components/voice_assistant/voice_assistant.cpp`, `client_subscription`) :
l'assistant vocal n'accepte **qu'un** client. Un second est refusé, et l'appareil écrit :

```
Multiple API Clients attempting to connect to Voice Assistant
  Current client: …
  New client: …
```

**Côté Home Assistant** (`components/esphome/assist_satellite.py`) : dès que l'appareil est une
entrée ESPHome avec les capacités d'assistant vocal, HA **s'abonne systématiquement** dans
`async_added_to_hass`. Ce n'est pas une option que l'on décoche.

**Conséquence** : si l'appareil est dans Home Assistant, **c'est HA qui tient l'assistant
vocal**, et Moli est refusé. Moli ne peut pas s'en apercevoir — `subscribe_voice_assistant` est
un message sans réponse ; il verra l'appareil, ses points, son état `idle`, et **ne parlera
jamais**.

Deux issues, à choisir :

| Je veux | Alors |
|---|---|
| **Moli comme cerveau** | ne pas laisser HA gérer l'appareil : refuser la découverte, ou supprimer/ignorer l'entrée ESPHome de cet appareil dans HA. Le **Device Builder continue de le flasher en OTA** (il parle à l'appareil directement), seules les entités disparaissent de HA |
| **HA comme cerveau** | Moli peut rester déclaré : il lira le volume, la sourdine et l'état, mais ne conduira aucune conversation |

Vérification, côté appareil : le journal ESPHome doit **cesser** d'afficher
`Multiple API Clients…` une fois HA retiré.

## Pièges

1. **`media_base` doit être l'adresse que l'appareil joint.** L'appareil va chercher le WAV
   lui-même : `127.0.0.1` ne peut pas marcher, et un nom d'hôte résolu seulement par le PC non
   plus. Sans `media_base`, une demande de conversation est refusée et l'état reste `idle`
   (journal : `voice request: no assistant or no media_base`).
2. **La clé, selon qui l'a posée.** Si HA a provisionné l'appareil, c'est *sa* clé : la reprendre
   depuis le YAML (`api: encryption: key:`) et la poser dans le coffre. Si l'appareil n'a **pas**
   de clé, Moli lui en donne une — mais seulement si `mac` correspond, et **seulement s'il n'en
   tient aucune** : un appareil qui parle en clair alors que Moli a une clé déclenche une
   **demande à un humain**, jamais un remplacement silencieux (`run`, `provision`).
3. **L'appareil redémarre après 15 minutes sans aucun client** (comportement ESPHome). Moli
   garde la connexion ouverte, répond aux pings et en ping toutes les 20 s ; 90 s de silence =
   connexion morte, reconnexion de 5 s à 60 s.
4. **Un seul `media_player`.** Le pilote retient le **dernier** `media_player` annoncé par
   l'appareil (`lib.rs`, dans `open`) : avec plusieurs (par exemple un lecteur de groupe en plus
   de l'enceinte), le volume, la sourdine et `say` peuvent viser le mauvais. En déclarer un.
5. **`wake_words` ne peut activer que ce que le firmware contient.** Aucun des mots demandés
   n'existe : Moli ne touche à rien et l'appareil garde les siens (avertissement au journal).
   Le firmware rapporte `max_active_wake_words = 1` avec `micro_wake_word` : **un seul mot**.
6. **Un double appui ne raccroche que si le firmware émet l'événement.** L'entité `event` doit
   exister et émettre `double_press` ; sinon la conversation se ferme par la voix ou le silence.
7. **Un appareil qui change d'adresse.** Avec `mac`, quand l'appareil ne répond plus, le pilote
   frappe le port 6053 sur les 253 adresses du /24 (64 à la fois, 800 ms chacune), au plus une
   fois toutes les 10 minutes, et **avertit** en donnant la nouvelle adresse : `host` reste à
   corriger à la main.
8. **Le journal de l'appareil peut entrer dans celui de Moli** (`logs = "info"`), préfixé
   `device:`. Utile pour un mot de réveil qui ne se déclenche pas ; coûteux en volume au
   niveau `debug`.

## Vérifier que ça marche

| Contrôle | Attendu |
|---|---|
| `GET /api/health` | le pilote `esphome` en `running` |
| `GET /api/devices` | un appareil `salon:<mac>`, en ligne, avec les cinq points |
| Dire le mot de réveil | `state` passe à `listening`, puis `thinking`, puis `speaking` |
| Écrire sur `say` | l'appareil dit la phrase, puis revient à `idle` |
| Journal de Moli | `esphome device connected`, puis `satellite listened` avec les niveaux, puis `satellite turn` avec les durées |

## Ce que Moli ne fait pas (encore)

- **Les minuteurs** de l'assistant (`on_timer_*` d'ESPHome) : Moli ne les envoie pas.
- **Le son par UDP** (ancien mode) : Moli impose `port: 0`, donc l'API.
- **Le multi-micro** du Voice PE (deux canaux) : Moli lit le premier canal.
- **Le streaming de la voix** : la réponse est un WAV complet, publié puis joué. Un premier mot
  plus rapide demanderait `INTENT_PROGRESS` avec `tts_start_streaming`, comme HA le fait.

## Voir aussi

- le pilote et le contrat général : [pilotes.md](pilotes.md)
- l'assistant, la voix et les réglages : [moli.md](moli.md)
- les profils déclaratifs, pour un appareil HTTP simple **sans** code : [profils.md](profils.md)

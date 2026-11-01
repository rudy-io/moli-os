# Tuya (local)

- Aucun code repris : trames 3.3 / 3.4 / 3.5 vérifiées octet par octet contre la sortie
  d'implémentations existantes (localtuya, tinytuya), règle de tinytuya pour les en-têtes de version ; l'import lit le
  compte Tuya déjà lié à HA (`tools/home-assistant/tuya-export.py`).
- Pas de fixture d'appareil : les tests (`src/protocol.rs`) utilisent des trames produites
  par ces implémentations (vecteurs d'interopérabilité), pas des captures.
- Vérifié en vrai : import depuis HA, lecture et écriture locales sur des prises Tuya.
- Sous-appareils d'une passerelle (Zigbee/BLE) **en local** : par la connexion de la
  passerelle, chaque trame portant le `cid` du sous-appareil (son `node_id`) ; états reçus et
  ordres envoyés avec ce `cid` (charges utiles vérifiées contre tinytuya, type « zigbee »).
  Il faut dans `tuya.json` : `node_id` et `parent` (l'id de la passerelle) ; l'import les
  remplit (parent : nommé par le cloud, sinon la passerelle qui partage la clé locale, sinon
  la seule passerelle de la maison) et un `parent` ou un `node_id` écrit à la main est conservé. Un `cid`
  inconnu est écrit dans le journal avec ses DPs, pour le retrouver. Sans passerelle locale,
  le cloud reste la voie (lecture seule).
- Repli vers le cloud : avec les clés du projet cloud, les sous-appareils d'une passerelle
  locale sont aussi suivis par le cloud, qui n'est écouté que lorsque Moli ne joint plus la
  passerelle en local alors qu'elle parle encore au cloud (adresse changée, réseau coupé
  entre Moli et elle, clé locale changée) : ils restent alors lus (lecture seule) au lieu de
  passer hors ligne, et leur disponibilité est celle que le cloud a vue. Dès que la
  passerelle répond de nouveau, elle reprend la main. Une passerelle débranchée coupe aussi
  le cloud : ses sous-appareils passent hors ligne, comme le cloud le dit. Coût : ces
  sous-appareils entrent dans le relevé du cloud toutes les 30 min (20 appareils par appel).
  **Vérifié sur du vrai matériel** (29 octobre 2026, passerelle `wg2` en 3.4, sondes `wsdcg`
  et interrupteurs `wxkg`, route coupée entre Moli et la passerelle, passerelle restée
  branchée) : Moli démarré sans joindre la passerelle, ses sous-appareils sont lus par le
  cloud et restent en ligne ; passerelle perdue en cours de route, le journal dit « tuya
  gateway out of reach: its sub-devices are read through the cloud » et les sondes restent
  en ligne avec des valeurs à jour ; route rouverte, la passerelle est retrouvée en local
  et reprend la main. **Limite constatée** : pendant le repli, les appuis des interrupteurs
  à scènes (`wxkg`) ne passent pas, et les automatismes Moli qu'ils déclenchent ne tournent
  pas (essayé, route coupée : aucun appui reçu). Le repli lit les états par les relevés du
  cloud : ça suffit pour une sonde, pas pour un appui, qui ne dure qu'un instant. Essai fait
  avec le service de messages connecté, mais sans règle de messages en production : une
  telle règle pourrait les faire passer, pas encore essayé. Le cloud reste en lecture
  seule : aucun ordre pendant le repli.
- Passerelle sans clé dans le cloud : l'import lui donne celle de ses sous-appareils (ils
  partagent la clé de leur passerelle) ; une passerelle (`wg*`, `wfcon`) ou un hub infrarouge
  (`wnykq`) n'est jamais lui-même un sous-appareil, même avec un `node_id`.
- **Vérifié sur du vrai matériel** (28 octobre 2026) : passerelle Tuya Zigbee `wg2` en 3.x
  local, deux sondes température/humidité `wsdcg` (codes `va_temperature`, `humidity_value`,
  `battery_percentage`) lues en local par la passerelle ; interrupteurs `wxkg` et un `kg`
  rattachés ; firmware non relevé.
- Ampoules (`dj`, essayé sur des LSC A60 RGB+CCT en 3.5, 31 octobre 2026) : les réglages des
  lumières et variateurs (catégories `dj`, `dd`, `xdd`, `fwd`, `dc`, `tgq`, `tgkg`, seulement)
  prennent les noms de toute lampe de Moli (Hue, Zigbee2MQTT), par lesquels le tableau de bord,
  les ambiances et Moli les trouvent : `brightness` en % (`bright_value_v2`, 10–1000),
  `color_temp` en mired (`temp_value_v2`, 0 = blanc chaud, 1000 = blanc froid, réparti en mired
  entre 2700 et 6500 K), `color` en `#rrggbb` (`colour_data_v2`, teinte/saturation/valeur, lue
  en texte hexadécimal `HHHHSSSSVVVV` ou en JSON, écrite en hexadécimal). Choisir une couleur
  passe l'ampoule en mode `colour`, un blanc en `white` ; en couleur, la luminosité est celle
  de la couleur (sa valeur), lue et réglée ainsi. `scene_data_v2` reste de côté.
  - Ailleurs, un `bright_value` garde son code et son sens : un module de portail qui en aurait
    un reste un portail pour la garde (`opener`, qui prend une luminosité de lampe pour une
    lampe), et demande toujours confirmation. Un `max` nul ou négatif retombe sur 1000.
  - La plage de blanc 2700–6500 K est supposée (courante pour ces ampoules, non mesurée) : une
    ampoule d'une autre plage montrerait un blanc un peu décalé, les deux bouts restant justes.
  - Les anciennes ampoules (`bright_value`, `temp_value`, sans `_v2`) suivent les mêmes règles,
    mais ne sont pas essayées.
  - Les points s'appelaient `bright_value_v2` et `temp_value_v2` : leur historique passé n'est
    pas relié aux nouveaux noms, et un automatisme qui réglait directement l'un d'eux serait à
    reprendre (allumer, éteindre et les appuis de boutons ne changent pas).
- Interrupteurs à scènes `wxkg` (29 octobre 2026) : les appuis arrivent en local en moins
  d'une seconde, `switch_mode1` vaut `single_click`, `double_click` ou `long_press`.
  Chaque appui que l'interrupteur pousse (trame `STATUS`) est un événement (`pulse_state`) :
  deux simples appuis de suite déclenchent deux fois un automatisme, même si la lumière a
  été éteinte entre-temps depuis le tableau de bord. Ce qui répond à la requête d'état de la
  connexion n'est qu'un état, même poussé de la même façon (une passerelle répond ainsi pour
  ses sous-appareils) : tout ce qui arrive dans les 5 s qui suivent la connexion, puis le
  premier message de chaque sous-appareil, même tardif (passerelle lente), au plus 30 s
  après la connexion. Une reconnexion ne rejoue donc pas le dernier appui ; un appui fait
  dans ce délai est perdu ; au-delà des 30 s, un interrupteur qui n'a jamais répondu redevient
  actif (sans ce plafond, il resterait muet pour toujours). Le cloud ne rapporte jamais un
  appui (il ne connaît que le dernier, et le nomme autrement : `click` pour `single_click`) :
  pendant le repli vers le cloud, un interrupteur à scènes ne déclenche donc rien.
  **Vérifié sur du vrai matériel** (31 octobre 2026, passerelle `wg2` en 3.4, interrupteur
  `wxkg`) : Moli redémarré après un appui qui allume, lumière éteinte entre-temps depuis le
  tableau de bord, elle reste éteinte ; un appui une dizaine de secondes après la
  reconnexion de la passerelle déclenche, et les appuis suivants aussi, chacun. Le
  plafond de 30 s pour un interrupteur qui ne répond jamais n'est vérifié que par un test.
- Ordre vers un sous-appareil, essayé sur le point `mode` d'un `wxkg` (passerelle en 3.4) :
  la passerelle accuse l'ordre, et quand la valeur demandée est déjà la bonne, elle renvoie
  l'état de ce `cid` dans les 100 ms ; une autre valeur (`remote_control`) n'est pas
  appliquée, même bouton réveillé par des appuis. Pas encore essayé sur un relais ou une
  prise Zigbee, toujours alimentés : le seul essai qui prouverait qu'un ordre est exécuté.
- Reste : la version 3.1 n'est pas gérée ; l'import dépend encore de HA ; une sonde sur pile
  ne répond pas à la requête d'état : sa valeur arrive à son prochain envoi.
- Limites des sous-appareils en local : la disponibilité est celle de la passerelle (un
  sous-appareil sur pile éteint reste « en ligne » tant que sa passerelle l'est) ; l'accusé
  d'un ordre vient de la passerelle, pas de l'appareil Zigbee : seul l'état renvoyé ensuite
  pour son `cid` prouve que l'ordre a été exécuté ; l'état de chaque sous-appareil n'est
  demandé qu'à la connexion, ensuite seul ce que la passerelle pousse arrive.

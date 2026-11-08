# Tuya (local)

- Aucun code repris : trames 3.3 / 3.4 / 3.5 vérifiées octet par octet contre la sortie
  d'implémentations existantes (localtuya, tinytuya), règle de tinytuya pour les en-têtes de version ; l'import lit le
  cloud Tuya : directement, avec les clés du projet cloud que Moli garde déjà pour le repli
  (`moli-os tuya import --cloud`), ou par le compte déjà lié à HA
  (`tools/home-assistant/tuya-export.py`).
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
- **Import sans Home Assistant** : `moli-os tuya import --cloud` lit la liste des appareils du
  projet cloud (`/v1.0/iot-01/associated-users/devices` : nom, catégorie, produit, clé locale,
  `node_id`) puis le modèle de chaque appareil (`/v2.0/cloud/thing/{id}/model` : les DP, leur
  type, leur plage, lecture ou écriture), et donne au même `merge_import` le même fichier que
  `tuya-export.py` (passerelle d'un sous-appareil devinée de la même façon). `--summary` liste
  ce qui serait rangé, sans clé ni écriture. Le modèle du cloud nomme certains DP autrement que
  l'appareil, pour le même numéro (une ampoule : `bright_value` pour `bright_value_v2`, DP 22 à
  26 ; un interrupteur à scènes : `switch_type_1` pour `switch_mode1`) : l'import rend ces noms
  d'appareil, retire `test_bit` (contrôle d'usine), et un appareil déjà dans `tuya.json` garde
  ses DP tels quels, aucun compris (une caméra ou une télécommande infrarouge importée sans DP
  par HA n'en reçoit pas du cloud : `sd_format`, sirène, codes bruts) ; seule sa clé locale est
  mise à jour. Lancé en root, l'import rend `tuya.json` et le coffre au propriétaire du dossier
  de données (moli-os tourne sous cet utilisateur). Essayé le 8 novembre 2026 sur un vrai
  compte (43 appareils : prises, ampoules, passerelle Zigbee et ses sous-appareils, hub
  infrarouge et ses télécommandes, caméras, Fingerbot) : chaque sous-appareil rattaché à la
  bonne passerelle, les 42 appareils déjà connus inchangés, le nouveau (une ampoule) ajouté avec
  ses noms d'appareil ; Moli redémarré charge les 43. Puis une prise neuve (LSC avec mesure),
  ajoutée dans l'application Smart Life et récupérée par `--cloud` : en ligne dans Moli au
  redémarrage, commandée en local (marche, arrêt), sans Home Assistant.
- Reste : la version 3.1 n'est pas gérée ; une sonde sur pile
  ne répond pas à la requête d'état : sa valeur arrive à son prochain envoi.
- Limites des sous-appareils en local : la disponibilité est celle de la passerelle (un
  sous-appareil sur pile éteint reste « en ligne » tant que sa passerelle l'est) ; l'accusé
  d'un ordre vient de la passerelle, pas de l'appareil Zigbee : seul l'état renvoyé ensuite
  pour son `cid` prouve que l'ordre a été exécuté ; l'état de chaque sous-appareil n'est
  demandé qu'à la connexion, ensuite seul ce que la passerelle pousse arrive.

## Aspirateurs robots : Rowenta et Tefal X-plorer Serie 75

- Ces robots sont des appareils Tuya : ils passent par ce pilote (`src/vacuum.rs`),
  **en local**, sans cloud une fois la clé obtenue.
- Points : `clean`, `pause`, `dock`, `mode`, `locate`, `state`, `battery`, `fan_speed`,
  `clean_time`, `clean_area`, `edge_brush`, `main_brush`, `fault`, `detail`.
- La Serie 75 répond sur les points 101 à 144. Le cloud en décrit d'autres (`power_go` sur 2, `mode` sur 3,
  `seek` sur 13) qu'elle ne renvoie pas : le produit `X-Plorer Serie 75` reçoit toujours la table de
  `src/vacuum.rs`, y compris après un import. Table reprise de `tefal_xplorer_vacuum.yaml` de
  `make-all/tuya-local` (MIT), confirmée sur un vrai robot.
- Les Serie 75 S et 95 S utilisent d'autres numéros (1 à 31, d'après `xplorer-rs`) et n'ont pas de table ici.
  Non essayés. Un robot décrit par le cloud avec les codes standards garde cette description.
- Volontairement absents : effacer la carte, remettre à zéro les compteurs, conduire à la main, nettoyer
  une zone ou une pièce (il faudrait lire la carte, un format brut non décodé ici).
- Les ordres qui font bouger le robot sont des `Control` : un agent passe par l'accord d'un humain.
- `fixtures/robot_status.json` : réponse d'un vrai robot, sans identifiant, pendant son retour à la base.
- **Vérifié sur du vrai matériel** (3 novembre 2026, Rowenta X-Plorer Serie 75, protocole 3.3, à travers
  Moli) : lecture, `clean`, `pause` dans les deux sens, `dock`, `fan_speed`, arrêt par `clean = false`, `locate` (son entendu).
- Constaté : `clean = true` pendant une pause relance une session (la surface repart à 0), `pause = false`
  reprend la session en cours. `clean_time` est en secondes mais n'avance que par minute. `clean = false` arrête
  le robot sur place, après un bref `tocharge`.
- Non vérifié : `mode = pose`, les noms des
  défauts, repris de `tuya-local` et jamais vus en panne réelle. Un défaut `code 27`, absent de cette liste, a
  été vu quand le robot s'est arrêté hors de sa base ; il a disparu une fois remis en charge, sens inconnu.
- Tableau de bord : une carte sur l'accueil, avec seulement les ordres possibles dans l'état du robot. Rendu vérifié ; boutons Démarrer, Faire sonner et aspiration cliqués sur la vraie page, ordres interceptés, et ces mêmes ordres envoyés au robot par l'API.
- Reste : carte et zones, langues, programmation, Serie 75 S et 95 S.

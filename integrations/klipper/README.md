# Klipper / Moonraker (pilote natif)

- `crates/moli-moonraker`. Remplace le profil `moonraker` (lecture seule, gardé pour qui
  n'a besoin que de l'état) : il écrit (`control` pause / reprise / annulation, `light`),
  sert la vignette de l'impression comme image de l'appareil et lit chaque tête.
- `fixtures/u1.json` : réponses **réelles** d'un Snapmaker U1 (4 octobre 2026) : liste des
  objets Klipper (sans les macros), `objects/query` des objets lus (têtes réduites aux champs
  lus), métadonnées du dernier fichier, totaux de l'historique.
- `fixtures/k2.json` : réponses **réelles** d'une Creality K2 Combo, pendant une impression
  (29 octobre 2026) : liste des objets Klipper (sans les macros), `objects/query` de
  `print_stats`, `virtual_sdcard`, `display_status`, métadonnées Moonraker du fichier (vides
  du trancheur). Nom de fichier changé, identifiants retirés.
- **Creality K2** : avancement, couches et estimation du trancheur lus dans ce que le firmware
  Creality ajoute à `virtual_sdcard` (`cur_print_data`, `layer`, `layer_count`). Le temps
  restant reste la moyenne trancheur / fichier, comme pour toute imprimante Klipper : il peut
  différer de celui de Creality Print, qui ne prend que le trancheur.
- **Essai sur une K2 Combo** (29 octobre 2026, même impression, au même moment) : avant,
  Moli disait 18 % et 12 h 57 restantes, Creality Print 41 % et 3 h 23. Après : 52 % contre 51 %,
  couche 446 contre 447 sur 1092, 2 h 59 restantes contre 2 h 44 (fin vers 00:52 contre 00:38).
- **Série K1 (K1, K1C, K1 Max) : non essayée.** Même firmware Creality sous Klipper : reconnue
  à sa sonde (`prtouch…`), elle devrait se lire de même ; une fixture d'une K1 le confirmera.
  Chaque valeur propre à Creality n'est prise que si elle tient (avancement entre 0 et 100 %,
  total de couches au-dessus de zéro, couche dans le total), sinon la lecture habituelle reste.
- **Début et impressions rapides** : pendant la séquence de départ (chauffe, calibration),
  l'avancement lu dans le fichier est en avance sur l'écran (13 % contre 0 % avant la première
  couche, essayé le 30 octobre 2026) ; sur une impression très courte l'écart se voit, sur une
  longue il s'efface. Moli relit l'imprimante toutes les 5 s : sur une impression rapide, la
  couche peut avoir une couche de retard. Afficher « préparation » à la couche 0 serait une suite.
- **Caméra de la K2** : WebRTC seulement (port 8000), aucune image fixe : `/server/webcams/list`
  est vide et le `/webcam/` de Fluidd (port 4408) répond 502. Le navigateur lit la vidéo en
  direct dans la carte de l'imprimante ; Moli ne fait que passer l'offre et la réponse
  (`/call/webrtc_local`). La caméra ne lit une adresse du navigateur que si elle est écrite comme
  go2rtc l'écrit (`… typ host ufrag … generation 0`), et un navigateur cache la sienne derrière un
  nom `.local` : Moli réécrit ces lignes avec l'adresse du navigateur qu'il voit. Sans adresse
  qu'elle sait lire, la caméra ne vérifie jamais le chemin du retour et ignore le chiffrement
  (DTLS) sans rien dire. À la maison seulement : Moli ouvert par son adresse locale (derrière un
  tunnel, il ne voit pas l'adresse locale du navigateur), et le navigateur doit joindre
  l'imprimante en UDP (avec un pare-feu entre les deux, l'autoriser ; la caméra choisit un port
  au hasard à chaque connexion, 32768–60999 constaté).
- **Essai sur une K2 Combo** (4 novembre 2026, Chrome sous Windows) : l'offre du navigateur
  telle quelle n'aboutit pas (ICE connecté, DTLS sans réponse) ; réécrite comme ci-dessus, la
  vidéo s'affiche en direct dans la carte de l'imprimante (bouton caméra, imprimante au repos),
  Moli ouvert par son adresse locale. go2rtc lit la même caméra ; Creality Print fermé ou non
  n'y change rien. Pas encore essayé : un téléphone, Firefox, Safari, une série K1.
- **Pas encore fait sur la K2** : la vignette (Moonraker n'en trouve pas dans le G-code de
  Creality Print), la lumière (`output_pin LED`,
  pas encore essayée), le CFS (`box`).
- Les commandes d'impression ont la sémantique `control` : un agent qui les envoie attend
  la validation d'un humain (garde du hub), quelle que soit la pièce.

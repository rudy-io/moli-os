# Philips Android TV (JointSpace)

- API JointSpace en HTTPS 1926, authentification digest (vérifiée sur l'exemple de la
  RFC 2617), Wake-on-LAN pour allumer.
- Origine : rien de noté. Les identifiants d'appairage viennent d'ailleurs (le pilote
  n'appaire pas) ; `pilotes.md` : « à vérifier, probablement repris de HA ».
- Pas de fixture : aucune réponse de la télé gardée dans les tests.
- ⚠️ Identité = l'adresse `host` : un changement d'IP créerait un nouvel appareil.
- **Côté Android** (`moli-androidtv`, module `android.rs`) :
  - Android TV Remote v2 (TLS 6466, certificat client) : appli au premier plan (point `app`,
    que JointSpace ne connaît pas sur une Google TV) et ouverture d'une appli (écrire `app` :
    paquet Android → `market://launch?id=…`, ou un lien). Appairage **repris de HA**
    (`androidtv_remote_cert.pem` / `_key.pem`, secrets `atv_cert` / `atv_key`) : la télé fait
    confiance à ce certificat, rien à ressaisir.
  - Google Cast (TLS 8009, rien à appairer) : `media_app`, `media_state`, `media_title`,
    `media_subtitle`, `media_image`, `media_duration`, `media_position` ; `media_control`
    (play, pause, stop).
  - Touches maintenues (point `hold`, `START_LONG` / `END_LONG` d'Android TV Remote) : une
    vidéo défile vite, comme avec la vraie télécommande.
  - ADB (`adb = true`, TCP 5555, débogage réseau de la télé) : ce que joue **toute** appli,
    lu dans les sessions média d'Android (`dumpsys media_session`). Cast ne dit rien d'une
    appli ouverte sur une Google TV. Clé de Moli créée au premier démarrage (`adb_key`),
    autorisée une fois sur l'écran de la télé.
  - Incrustations (`overlay = true`, port 5001) : l'appli TvOverlay affiche par-dessus
    l'image une notification, une photo ou une vidéo en direct (point `notify`). Vérifié le
    3 nov. (TvOverlay 1.0.4) : la sonnette en direct, en bas à droite, sur l'accueil Google TV.
  - Un ordre donné pendant que la connexion est coupée n'est jamais rejoué à la reconnexion
    (refusé tout de suite, file vidée à chaque connexion).
- Vérifié en vrai le 4 oct. (sonde en lecture) sur une Philips Google TV : elle accepte le
  certificat de HA, dit allumée et le volume ; Cast répond et donne le même programme que HA.
- Vérifié en vrai le 3 nov. sur une Philips Google TV TA1 (Android 14) : `dumpsys
  media_session` donne titre, chaîne, état et position de YouTube ; un appui long sur droite
  (ADB `input keyevent --duration`) fait défiler la vidéo avec l'aperçu. Le texte de
  `adb::tests::DUMP` est cette sortie.
- Reste : l'appairage JointSpace ; une identité stable (numéro de série) ; appairer Android
  TV Remote depuis Moli (port 6467, code à l'écran) pour une télé que HA n'a pas connue.

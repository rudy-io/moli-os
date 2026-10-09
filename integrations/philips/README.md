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
  - Un ordre donné pendant que la connexion est coupée n'est jamais rejoué à la reconnexion
    (refusé tout de suite, file vidée à chaque connexion).
- Vérifié en vrai le 4 oct. (sonde en lecture) sur une Philips Google TV : elle accepte le
  certificat de HA, dit allumée et le volume ; Cast répond et donne le même programme que HA.
- Reste : l'appairage JointSpace ; une identité stable (numéro de série) ; appairer Android
  TV Remote depuis Moli (port 6467, code à l'écran) pour une télé que HA n'a pas connue.

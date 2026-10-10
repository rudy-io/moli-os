# Reolink (Home Hub)

- Origine : rien de noté (API HTTPS locale `api.cgi`, commandes en lots JSON).
- `fixtures/home-hub-cert.b64` : le certificat public réel du Home Hub (X.509 v1, que webpki
  ne lit pas), rejoué par les tests de `crates/moli-net/src/legacy.rs`. Les réponses d'API
  des tests (`src/lib.rs`) sont écrites dans le code, pas capturées en fichier.
- Lecture : mouvement, détections IA, sonnette, images.
- Parole : `say` sur une caméra qui a un haut-parleur (la sonnette), par le canal de retour ONVIF
  du serveur RTSP de la station (`rtsp_port`, 554 par défaut) : `DESCRIBE` avec
  `Require: www.onvif.org/ver20/backchannel` → piste `sendonly` `PCMU/8000`, puis RTP µ-law 8 kHz
  entrelacé. Chaque session finit par `TEARDOWN` : un canal resté ouvert garde la sonnette en mode
  interphone (carillon muet). Le canal est demandé une fois, quand la caméra apparaît.
- Interphone (`[driver.options.interphone]`) : on sonne → accueil dit par la sonnette, écoute du
  micro (piste AAC `MPEG4-GENERIC` 16 kHz du même flux, RFC 3640, décodée par `moli-audio`)
  jusqu'au silence, puis le cerveau vocal transmet le message (Telegram, `door_speakers`) et
  la sonnette remercie. `by_name = true` : quelqu'un vu à la porte peut dire « Hey Moli, … » ;
  seules les phrases qui appellent Moli sont gardées, le reste n'est conservé nulle part. Rien
  de ce qui est dit à la porte n'atteint le modèle ni n'agit sur la maison. Options :
  `greeting`, `thanks`, `wait_s` (8), `by_name` (non), `watch_s` (30), `reply_s` (10 : après une
  phrase dite à la porte depuis la maison dans les 3 min d'une visite, écoute de la réponse).
- Reste : testé derrière un Home Hub seulement ; caméra seule ou NVR non vérifiés.

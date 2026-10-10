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
- Reste : testé derrière un Home Hub seulement ; caméra seule ou NVR non vérifiés.

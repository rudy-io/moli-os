# Reolink (Home Hub)

- Origine : rien de noté (API HTTPS locale `api.cgi`, commandes en lots JSON).
- `fixtures/home-hub-cert.b64` : le certificat public réel du Home Hub (X.509 v1, que webpki
  ne lit pas), rejoué par les tests de `crates/moli-net/src/legacy.rs`. Les réponses d'API
  des tests (`src/lib.rs`) sont écrites dans le code, pas capturées en fichier.
- Lecture seule : mouvement, détections IA, sonnette, images.
- Reste : testé derrière un Home Hub seulement ; caméra seule ou NVR non vérifiés.

# Appareil ESPHome (API native)

- API native d'ESPHome sur TCP 6053, chiffrée par `Noise_NNpsk0_25519_ChaChaPoly_SHA256`, écrite
  sans bibliothèque ESPHome : `crates/moli-esphome/src/noise.rs` (spécification Noise, primitives
  `ring`), `frame.rs` (trames, handshake), `api.rs` (messages d'après `api.proto`, ESPHome 2026.3).
- Une connexion gardée ouverte (l'appareil redémarre au bout de 15 min sans client), pings toutes
  les 20 s, reconnexion à attente croissante.
- Origine : rien de repris. Pas de fixture : le handshake et les trames sont testés contre un
  répondeur écrit avec les mêmes primitives ; la vérification réelle se fait sur l'appareil (une
  mauvaise clé y donne « Handshake MAC failure »).

# Appareil ESPHome (API native)

- API native d'ESPHome sur TCP 6053, chiffrée par `Noise_NNpsk0_25519_ChaChaPoly_SHA256`, écrite
  sans bibliothèque ESPHome : `crates/moli-esphome/src/noise.rs` (spécification Noise, primitives
  `ring`), `frame.rs` (trames, handshake), `api.rs` (messages d'après `api.proto`, ESPHome 2026.3).
- Une connexion gardée ouverte (l'appareil redémarre au bout de 15 min sans client), pings toutes
  les 20 s, reconnexion à attente croissante.
- Origine : rien de repris. Pas de fixture : le handshake et les trames sont testés contre un
  répondeur écrit avec les mêmes primitives ; la vérification réelle se fait sur l'appareil (une
  mauvaise clé y donne « Handshake MAC failure »).

## Migration

Un appareil qui n'est pas un satellite (prise, lecteur P1…) et qui était déjà installé change de
points à la mise à jour : `state`, `volume`, `muted`… laissent la place à un point par entité
(capteurs, capteurs binaires, capteurs texte, interrupteurs). Un automatisme ou une carte qui
visait les anciens points est à refaire sur les nouveaux.

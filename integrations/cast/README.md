# Enceintes Google Cast (Google Home, Nest Mini, Lenovo Smart Clock/Display)

- Protocole Google Cast en TLS local sur le port `8009` (`crates/moli-androidtv/src/cast.rs`),
  certificat auto-signé épinglé au premier contact (`cast_pin` dans le coffre).
- Lecture/pause, volume (`0` à `100 %`), sourdine, état et titre en cours.
- Annonces et réponses vocales (`say`, `announce`) : Moli prépare sa voix en flux FLAC
  (`/api/voice/<id>.flac`), ouvre le *Default Media Receiver* (`CC1AD845`) et lance la lecture.
- Peut aussi servir de haut-parleur associé à un satellite vocal ESPHome dépourvu de haut-parleur
  (option `cast_speaker` dans `moli-esphome`).

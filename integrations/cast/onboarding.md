# Onboarding : enceinte Google Cast (Google Home, Nest)

1. Repérer l'adresse IP de l'enceinte sur le réseau local (mDNS `_googlecast._tcp`, port `8009`) ;
   une réservation DHCP sur le routeur est conseillée.
2. Ajouter un bloc `[[driver]]` dans `moli.toml` avec `kind = "cast"`, `host`, et `media_base`
   (`http://<ip-de-moli>:8790` pour que l'enceinte puisse lire la voix de Moli) :
   ```toml
   [[driver]]
   id = "google_home"
   kind = "cast"

   [driver.options]
   host = "192.168.1.x"
   name = "Google Home"
   media_base = "http://192.168.1.y:8790"
   ```
3. Vérifier avec `moli-os check-config`, puis redémarrer Moli. Au premier contact,
   Moli vérifie la preuve de possession de l'identité Cast et conserve son empreinte
   dans le coffre (`cast_device_pin`). Les connexions suivantes exigent cette identité
   et une signature SHA-256 du nonce de session et du certificat TLS courant : le
   renouvellement du certificat TLS temporaire n'exige plus de réassociation.
   Si une ancienne installation ne possède que `cast_pin`, la migration automatique
   exige que cette ancienne empreinte soit encore valide. Sinon, faire confirmer
   l'identité de l'enceinte par le propriétaire et enregistrer l'empreinte SHA-256 de
   son certificat **DeviceAuth**, après vérification de sa signature, dans
   `moli-os secrets set <instance> cast_device_pin` (valeur par un tube).
   Ne jamais utiliser l'empreinte du certificat TLS temporaire pour cette valeur,
   ni supprimer automatiquement une empreinte refusée. L'association initiale est
   TOFU (confiance au premier contact), pas une validation de la chaîne Google.
4. Pour utiliser cette enceinte comme haut-parleur d'un satellite vocal ESPHome sans haut-parleur
   intégré (ex. galet rotatif Waveshare), ajouter `cast_speaker = "192.168.1.x"` dans les options du
   pilote `esphome`.

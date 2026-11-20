# Frigate

- Les sujets MQTT que publie Frigate (`frigate/<caméra>/…`) sur le même broker que Z2M ;
  images par son API HTTP interne (`latest.jpg`, port 5000, sans authentification).
- Origine : rien de noté. Pas de fixture : les tests vérifient la correspondance sujets →
  points sur des sujets écrits dans le code.
- Images et direct (WebRTC) par go2rtc, en option (`go2rtc`, `streams`) : la vidéo va de
  go2rtc au navigateur, Moli ne passe que l'offre et la réponse. Un choix explicite : l'API de
  go2rtc n'a pas de mot de passe et montre les adresses des caméras ; réseau local seulement.
- Reste : pas d'identifiants MQTT.

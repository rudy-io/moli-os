# Onboarding : Sonos

1. Demander : quel lecteur (pièce) et son adresse IP. Une instance par lecteur.
2. Aucun identifiant pour la lecture et le volume.
3. Annonces (facultatif) : `[driver.options.announce]` avec `media_base` (Moli tel que
   l'enceinte le joint, `http://<ip de Moli>:8790`), `tts` (serveur Piper `hôte:port`),
   `voice`, `volume`. Il faut une clé maître (empreinte `ws_cert` épinglée au premier contact).
4. Déclarer `kind = "sonos"`, `host = "<ip>"` ; `moli-os check-config`, redémarrer.
5. Tester en journée, volume bas : pause puis lecture ; une annonce courte si configurée.

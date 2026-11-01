# Shelly (Gen2 et suivants)

Pilote natif `crates/moli-shelly`. Une instance = un Shelly.

- **Transport** : websocket local `ws://<hôte>/rpc` (JSON-RPC 2.0). Au départ
  `Shelly.GetDeviceInfo`, `Shelly.GetConfig`, `Shelly.GetStatus` ; ensuite l'appareil pousse ses
  changements (`NotifyStatus`, `NotifyFullStatus`) ; état complet relu chaque minute en filet.
  Mesuré sur un Pro 3EM : un message toutes les 5 s ; un Pro 4PM n'envoie que ce qui change.
- **Appareils Moli** : le boîtier (`<mac>` : signal Wi-Fi, mise à jour disponible, entrées,
  température ; et le compteur d'un Pro 3EM), une sortie par appareil (`<mac>_switch_<n>`,
  pilotable), une voie de mesure par appareil (`<mac>_em1_<n>`, `<mac>_pm1_<n>`).
- **Pro 3EM** : les phases câblées sont reconnues (tension présente ou énergie comptée) ; une
  seule phase câblée = un compteur simple (pas de points par phase). Puissance signée :
  positive quand la maison soutire, négative quand elle injecte. Énergies en kWh
  (`energy` soutirée, `energy_returned` injectée).
- **Adresse qui change** : suivi par adresse MAC (`moli_net::lan::relocate`), identité
  vérifiée par `Shelly.GetDeviceInfo` avant d'adopter la nouvelle adresse.
- **Mot de passe** : authentification digest SHA-256 (utilisateur `admin`) sur le websocket,
  secret `password` de l'instance. Écrite d'après la documentation de Shelly, **pas encore
  essayée sur un appareil protégé**.
- **Pas encore** : volets (`cover`, voir la proposition externe), lumières (`light`, `rgbw`),
  capteurs sur batterie (qui dorment), Gen1 (API HTTP différente), découverte mDNS.

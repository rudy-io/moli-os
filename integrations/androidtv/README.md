# Box ou télé Android TV

- Pilote **sans protocole propre** : il ne fait que déclarer les points et traduire les ordres
  vers le cœur `moli-androidtv` (Android TV Remote v2 en TLS 6466, Google Cast en TLS 8009,
  ADB en TCP 5555, TvOverlay en HTTP 5001), décrits dans `integrations/philips/README.md`
  (« Côté Android »), où le même cœur tourne à côté de JointSpace.
- Points : `power` (lecture seule, dit par la télécommande Android), `app` (lue et écrite),
  `key` et `hold` (touches, appuyées ou maintenues), `text`, `media_*` (Cast, puis ADB),
  `media_control`, `notify` (option `overlay`).
- Ni alimentation en écriture (pas de Wake-on-LAN), ni volume réglable (touches seulement),
  ni Ambilight.
- ⚠️ Identité = l'adresse `host` : un changement d'IP créerait un nouvel appareil.
- Pas de fixture : aucune réponse de la box gardée dans les tests.
- Vérifié en vrai le 15 nov. 2026 sur une Freebox Player POP (Android TV), appairage repris de
  Home Assistant : appareil publié, `power` et `app` lus, ADB connecté (titre, appli, état et
  position de YouTube), page Salon du tableau de bord (télécommande et « à l'écran »).
- Essayés ensuite sur la même box par son propriétaire, et ils fonctionnent : touches, maintien, texte, lancement d'appli, `media_control`, TvOverlay.
- Reste : appairer Android TV Remote depuis Moli (port 6467, code à l'écran) pour une box que
  Home Assistant n'a pas connue.

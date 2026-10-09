# Sonos

- UPnP/SOAP (`AVTransport`, `RenderingControl`) sur 1400, interrogé toutes les 2 s.
- Annonces : Piper en local (Wyoming), puis clip audio par l'API WebSocket locale (1443),
  comme le faisait le script HA `announce_sonos` (la musique baisse puis revient).
- Pas de fixture : les tests lisent du SOAP et du DIDL écrits dans le code.
- Vérifié sur une Sonos Beam.
- Reste : pas de groupes Sonos, pas de découverte SSDP.

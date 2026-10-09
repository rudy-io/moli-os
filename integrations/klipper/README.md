# Klipper / Moonraker (pilote natif)

- `crates/moli-moonraker`. Remplace le profil `moonraker` (lecture seule, gardé pour qui
  n'a besoin que de l'état) : il écrit (`control` pause / reprise / annulation, `light`),
  sert la vignette de l'impression comme image de l'appareil et lit chaque tête.
- `fixtures/u1.json` : réponses **réelles** d'un Snapmaker U1 (4 octobre 2026) : liste des
  objets Klipper (sans les macros), `objects/query` des objets lus (têtes réduites aux champs
  lus), métadonnées du dernier fichier, totaux de l'historique.
- Les commandes d'impression ont la sémantique `control` : un agent qui les envoie attend
  la validation d'un humain (garde du hub), quelle que soit la pièce.

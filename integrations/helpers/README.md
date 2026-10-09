# Aides de la maison

- Le rôle des `input_*` de HA : des valeurs que la maison garde pour elle (`select`,
  `switch`, `number`, `text`). Une commande devient la nouvelle valeur, sous la garde et au
  journal ; elle survit aux redémarrages (cache d'état).
- Rien à capturer : pas d'appareil, pas de réseau.
- Exemple : `mode_maison`, `alarme_active`, `budget_elec_mensuel`, aux mêmes noms que les
  `input_*` de HA qu'elles remplacent.

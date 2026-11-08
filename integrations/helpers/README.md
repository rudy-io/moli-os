# Aides de la maison

- Le rôle des `input_*` de HA : des valeurs que la maison garde pour elle (`select`,
  `switch`, `number`, `text`). Une commande devient la nouvelle valeur, sous la garde et au
  journal ; elle survit aux redémarrages (cache d'état).
- `aggregate` : un point **calculé, en lecture seule** — la moyenne (ou `min`/`max`) d'une
  liste de points de la maison (`points`, format `instance:appareil/cle`). L'équivalent du
  capteur « virtuel » de Jeedom (ex. la température moyenne de la maison). `stale_s` exclut
  un point sans relevé depuis N secondes ; sans aucun point valide, la dernière valeur est
  gardée. `unit` et `semantic` (ex. `°C`, `temperature`) habillent le point.
- Rien à capturer : pas d'appareil, pas de réseau (les sources sont des points Moli existants).
- Exemple : `mode_maison`, `alarme_active`, `budget_elec_mensuel`, et `temp_maison`
  (moyenne des thermomètres de la maison), aux mêmes noms que les `input_*` / le virtuel de
  HA/Jeedom qu'elles remplacent.

# Maison de démonstration

- Rejoue des appareils depuis un fichier (`fixture`), **exactement** comme les vrais pilotes
  les publient (mêmes points, mêmes états) : le tableau de bord ne voit pas la différence.
  Une instance par pilote imité (`hue`, `tuya`…), avec `"imitates"` dans le fichier.
- Les fait vivre : ordres obéis tout de suite (une pièce Hue entraîne ses lampes, une lampe
  allumée consomme), températures et humidités qui bougent, clim qui tire la pièce vers sa
  consigne, compteurs qui tournent avec la puissance, mouvements et passages devant les
  caméras de temps en temps, impression 3D qui avance, jour et nuit selon l'heure.
- Rien sur le réseau. Avec `[server] demo = true`, la maison s'ouvre à tout visiteur, et
  Moli refuse alors tout pilote réel.
- La villa de démonstration du dépôt : `demo/villa/`.

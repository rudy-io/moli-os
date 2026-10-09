# La villa de démonstration

Une maison complète, **sans aucun appareil** : 113 appareils simulés (lumières Hue,
prises et volets Tuya, capteurs Zigbee, clims, Sonos, télé, caméras, imprimantes 3D,
piscine, Linky et tableau électrique), un plan 2D/3D, des automatismes et une année de
consommation. Chaque appareil reprend la forme exacte d'un vrai appareil, tel que son pilote
le publie : le tableau de bord ne voit pas la différence.

```bash
docker compose -f demo/villa/compose.yaml up --build
```

Puis <http://localhost:8790>. Le code de la démo (pour valider un automatisme) est `123456`.

- `moli.toml` : la configuration (`[server] demo = true` : tout visiteur entre, mais ne peut
  ni changer le code, ni renommer, ni redessiner le plan ; Moli y refuse tout pilote réel).
- `devices/` : les appareils, par pilote imité (`"imitates"`), et les images des caméras.
- `seed/` : le point de départ (accueil, plan, automatismes). À chaque démarrage, la démo
  repart de là et se refait une année de consommation jusqu'à aujourd'hui.

Sans Docker, depuis ce dossier : `MOLI_UI_PIN=123456 cargo run -p moli-os -- --config moli.toml serve`.

# Téléphones (appli Moli)

- Un appareil par téléphone appairé : à la maison, batterie, en charge, Wi-Fi, latitude,
  longitude, précision, distance de la maison, version de l'appli.
- Appairage depuis l'appli (sa vue du tableau de bord est déjà connectée) :
  `POST /api/mobile/register` → jeton montré une fois, gardé dans le trousseau du téléphone ;
  Moli ne garde que son SHA-256 (`data/phones.json`).
- Rapports : `POST /api/phones/<id>/report` avec le jeton, sans connexion Cloudflare
  (application Access « bypass » sur ce chemin ; Moli vérifie le jeton).
- La maison est apprise : première position précise sur le Wi-Fi de la maison.
- Appli : `mobile/` (Expo).

# Onboarding : téléphones

1. `[[driver]] kind = "phones"`, `home_wifi = ["<nom du Wi-Fi>"]` (rayon `radius`, 150 m).
2. Cloudflare : une application Access « bypass » sur `<hôte>/api/phones` (les rapports
   portent leur propre jeton ; Moli le vérifie).
3. Installer l'appli Moli, se connecter (Cloudflare Access), accepter la localisation
   « Toujours » : l'appli s'appaire seule et envoie un premier rapport.
4. Vérifier l'appareil du téléphone sur la page Système ; la maison est apprise au premier
   rapport sur le Wi-Fi de la maison.

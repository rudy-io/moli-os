# Onboarding : téléphones

1. `[[driver]] kind = "phones"`, `home_wifi = ["<nom du Wi-Fi>"]` (rayon `radius`, 150 m).
2. Cloudflare : une application Access « bypass » sur `<hôte>/api/phones` (les rapports
   portent leur propre jeton ; Moli le vérifie).
   **Sans Cloudflare** : un téléphone qui rejoint le réseau de la maison par un VPN
   (WireGuard, par exemple) arrive avec une adresse privée, celle du tunnel ou du routeur
   qui le termine. C'est un appelant `lan`, donc du foyer, sans aucun tiers, et ses
   rapports n'ont pas besoin de l'application « bypass ». Donner l'accès au VPN, c'est
   donner l'accès du foyer.
3. Installer l'appli Moli, se connecter (Cloudflare Access ou mot de passe, ou taper le code
   de la maison sur le Wi-Fi), accepter la localisation « Toujours » : l'appli s'appaire
   seule et envoie un premier rapport.
4. Vérifier l'appareil du téléphone sur la page Système ; la maison est apprise au premier
   rapport sur le Wi-Fi de la maison.

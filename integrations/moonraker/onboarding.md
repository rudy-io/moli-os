# Onboarding : Klipper / Moonraker

1. Demander : l'adresse IP de l'imprimante et le port de Moonraker (souvent 7125).
2. Aucun identifiant sur un LAN de confiance (Moonraker sans authentification).
3. Tester : `curl http://<ip>:<port>/printer/info` répond un `hostname`. Il devient
   l'identité de l'appareil : il ne doit plus changer.
4. Déclarer `kind = "profile"`, `profile = "moonraker"`, `host`, `port` ;
   `moli-os check-config`, redémarrer, vérifier `state` et les températures.

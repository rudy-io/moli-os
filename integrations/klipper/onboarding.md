# Mettre une imprimante Klipper dans Moli

1. Trouver son adresse sur le réseau (Moonraker écoute sur le port 7125 ; l'interface web
   Fluidd/Mainsail sur le port 80).
2. Ajouter dans `moli.toml` :

```toml
[[driver]]
id = "imprimante"
kind = "moonraker"
[driver.options]
host = "192.168.1.x"
# port = 7125
# name = "Snapmaker U1"   # par défaut : le modèle annoncé, ou le nom d'hôte
```

3. Redémarrer Moli. L'imprimante apparaît sous son nom d'hôte (identité stable). Rien à
   appairer : Moonraker n'a pas d'authentification sur un réseau de confiance.

# Onboarding : Linky (TIC)

1. Brancher le module TIC au compteur (bornes I1 et I2) et à la machine ; relever le port : `ls /dev/serial/by-id/`
   (préférer ce chemin-là, il ne change pas d'un port USB à l'autre).
2. Donner le port au conteneur, par exemple dans `compose.yaml` :
   `devices: ["/dev/serial/by-id/usb-…:/dev/linky"]` et `group_add: ["dialout"]` (le groupe peut porter un autre
   numéro : `getent group dialout`).
3. Déclarer l'instance, puis `moli-os check-config` et redémarrer :

   ```toml
   [[driver]]
   id = "linky"
   kind = "linky-tic"

   [driver.options]
   port = "/dev/linky"
   mode = "standard"        # ou "historique" (1 200 bauds)
   ```

4. Tester : en quelques secondes, `apparent_power` suit la consommation de la maison (allumer une bouilloire :
   +2 kVA environ) et `energy_total` correspond à l'index affiché par le compteur. Sans valeur après 30 s, le pilote
   le dit (aucune trame valide) : vérifier le port, le câble et le mode.
5. Un seul lecteur à la fois : un autre système (Home Assistant avec `linkytic`) qui lit le même module en
   parallèle reçoit des trames tronquées, et Moli aussi.

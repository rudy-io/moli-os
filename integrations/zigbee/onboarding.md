# Onboarding : Zigbee natif

Moli parle directement à une clé coordinateur Silicon Labs (EZSP), sans Zigbee2MQTT ni
broker. Clé de référence : Sonoff ZBDongle-E (115 200 bauds, contrôle de flux logiciel).

1. **Brancher la clé** sur la machine de Moli, de préférence avec une rallonge USB (loin des
   ports USB 3 et du Wi-Fi, qui brouillent le 2,4 GHz). Une clé déjà utilisée par un autre
   logiciel (Zigbee2MQTT, ZHA) doit être libérée : un seul programme à la fois sur le port.
2. **Trouver son chemin stable** : `ls -l /dev/serial/by-id/`. Garder ce chemin
   (`/dev/serial/by-id/usb-…-if00-port0`) plutôt que `/dev/ttyUSB0`, dont le numéro change
   au gré des branchements. Un adaptateur réseau se déclare `tcp://<hôte>:6638`.
3. **Passer le périphérique au conteneur** : `--device /dev/serial/by-id/<clé>:/dev/ttyUSB0`
   (`docker run`), ou dans `compose.yaml` :

   ```yaml
       devices:
         - /dev/serial/by-id/<clé>:/dev/ttyUSB0
       group_add:
         - "20"   # le groupe du port sur l'hôte : stat -c %g /dev/serial/by-id/<clé>
   ```

   Moli tourne sous l'utilisateur 1000 : sans le groupe du port, l'ouverture est refusée.
4. **Mettre à jour le firmware** : il faut EmberZNet 7.4 ou plus récent (EZSP 13+). Avec un
   firmware plus ancien, le pilote attend avec le message « le firmware de la clé parle
   EZSP … ». Flasher la clé avec le flasheur web du fabricant (pour la ZBDongle-E : celui
   de Sonoff, image « EmberZNet NCP » 7.4.x ou plus, 115 200 bauds),
   depuis un navigateur Chrome ou Edge, la clé branchée sur l'ordinateur ; puis la rebrancher
   sur la machine de Moli.
5. **Déclarer l'instance** dans `moli.toml` :

   ```toml
   [[driver]]
   id = "zigbee"
   kind = "zigbee"
   [driver.options]
   port = "/dev/ttyUSB0"   # le chemin vu dans le conteneur
   # baud = 115200         # défaut
   # flow = "software"     # défaut ; "hardware" ou "none" selon la clé
   ```

   `moli-os check-config`, redémarrer. Il faut une clé maître (`MOLI_MASTER_KEY_FILE`) : les
   clés du réseau sont rangées par le pilote dans le coffre, jamais dans un fichier ; sans
   coffre, aucun réseau n'est formé et le pilote attend en le disant.
6. **Vérifier** : l'appareil « Coordinateur Zigbee » apparaît, `firmware` (ex. `7.4.4.0`),
   `ezsp_version`, `network` (`up` une fois le réseau repris ou formé, sur le canal 15 sauf
   `channel` dans les options).

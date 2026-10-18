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
6. **Reprendre un réseau Zigbee2MQTT** (facultatif, sans réappairer) : arrêter Zigbee2MQTT,
   copier sa sauvegarde `coordinator_backup.json` (dossier de données de Zigbee2MQTT), puis
   `moli-os zigbee import coordinator_backup.json` (`--instance <id>` si l'instance ne
   s'appelle pas `zigbee`). **Moli doit être arrêté pendant l'import** : un Moli en marche
   garde son coffre et l'état du pilote en mémoire et écraserait l'import à sa prochaine
   sauvegarde ; le démarrer ensuite. Les clés vont au coffre (jamais affichées), le réseau dans
   `<data_dir>/zigbee-<instance>.json`. **Supprimer ensuite la copie** de
   `coordinator_backup.json` faite pour l'import : elle contient la clé du réseau en clair
   (celui du dossier de données de Zigbee2MQTT ne se garde que si Zigbee2MQTT reste là pour un
   retour en arrière). Au démarrage, le pilote reprend le réseau que la clé
   porte encore, ou le restaure s'il a été effacé (par le flash du firmware). **Même clé
   seulement** : Moli ne réécrit pas l'adresse d'une clé ; changer de clé, c'est réappairer.
   **Sauvegarde récente seulement** : la restauration repart du compteur de trames de la
   sauvegarde (+ 65 536) et ne s'en sert qu'une fois, dans les 24 h qui suivent l'import
   (démarrer Moli aussitôt). Ensuite, le pilote relève lui-même le compteur de la clé (dès
   que le réseau tourne, toutes les heures et à l'arrêt, sans clé dans aucun fichier) : si la
   clé est effacée plus tard, il restaure depuis ce relevé (+ 65 536, de quoi couvrir l'heure
   écoulée), s'il date de 24 h au plus. Sans relevé à jour, une nouvelle restauration demande une
   sauvegarde **plus récente que le compteur connu** (sinon le pilote attend en le disant ;
   réimporter une sauvegarde plus ancienne est refusé, même avec `--force`). Un import
   interrompu se termine en relançant la même commande. Un réseau déjà connu de l'instance
   n'est remplacé qu'avec `--force`. Jamais Zigbee2MQTT et Moli en même temps sur le même
   réseau.
7. **Revenir à Zigbee2MQTT** (au besoin) : **arrêter Moli d'abord** (et le laisser arrêté :
   jamais les deux sur la même clé), puis exporter **tout de suite** (une clé sous tension fait
   avancer son compteur de trames ; plus l'export tarde, plus la marge s'use ; au-delà de
   24 h après le dernier relevé, l'export est refusé) :
   `moli-os zigbee export coordinator_backup.json` (`--instance <id>` au besoin, `--force`
   pour remplacer un fichier existant). Le fichier est une sauvegarde Open Coordinator Backup
   comme celle de Zigbee2MQTT : paramètres du réseau et appareils connus de Moli, clés lues au
   coffre au moment de l'export, compteur de trames = dernier relevé + 65 536 (Zigbee2MQTT
   repart de ce compteur tel quel). **Il contient la clé du réseau en clair** (écrit en mode
   0600) : le placer dans le dossier de données de Zigbee2MQTT sous le nom
   `coordinator_backup.json` (ce fichier-là, Zigbee2MQTT le garde et le réécrit lui-même : ne
   pas le supprimer), et **supprimer toute autre copie** (l'export écrit ailleurs, une copie
   de transfert) dès que Zigbee2MQTT a repris le réseau. Même clé USB qu'avant (dongle
   d'origine), et `configuration.yaml` de Zigbee2MQTT (section `advanced`) doit porter le même
   réseau que la sauvegarde, sinon il en forme un nouveau : `pan_id` = la valeur hex de
   `pan_id` (`1a62` → `0x1A62`), `channel` = `channel`, `ext_pan_id` et `network_key` = des
   tableaux d'octets : chaque paire de chiffres hex de `extended_pan_id` et de
   `network_key.key`, dans l'ordre du fichier, en décimal (`0123…` → `[1, 35, …]`). Les
   appareils appairés sous Moli manquent à la base de Zigbee2MQTT (elle date d'avant la
   reprise) : les réappairer depuis Zigbee2MQTT.
8. **Vérifier** : l'appareil « Coordinateur Zigbee » apparaît, `firmware` (ex. `7.4.4.0`),
   `ezsp_version`, `network` (`up` une fois le réseau repris ou formé, sur le canal 15 sauf
   `channel` dans les options).

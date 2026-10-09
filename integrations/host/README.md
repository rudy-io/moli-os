# La machine de Moli

- Linux : `/proc/stat` (processeur, écart entre deux lectures), `/proc/meminfo`, `/proc/loadavg`,
  `/proc/net/dev` (interface la plus active hors `lo`, Docker), `/proc/uptime`, et
  `/sys/class/hwmon` (paquet du processeur : `coretemp`, `k10temp`, `cpu_thermal` d'un Pi ; NVMe).
- Toutes les 15 s, valeurs arrondies (l'historique ne garde que les changements).
- En conteneur en réseau hôte (`network_mode: host`), `/proc` et `/sys` donnent bien la machine.
- Les tests lisent des fichiers écrits par les tests (pas de fixture).

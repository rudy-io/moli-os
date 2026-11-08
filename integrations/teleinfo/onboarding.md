# Onboarding — Linky (TIC téléinfo)

## 1. Câblage du module USB-TIC

Matériel : interface USB-Cartelectronic USBTICLC (ou équivalent), prise TIC
jaune du boîtier Linky.

1. **Débranche le boîtier Linky** (ou coupez l'alimentation de la prise TIC)
   avant de brancher quoi que ce soit : la prise TIC est sous tension
   (230 V) même quand le compteur ne consomme rien.
2. Dévissez le capuchon de la **prise TIC jaune** sur le boîtier Linky.
   Vous voyez deux contacts : **I1** et **I2**.
3. Branchez les deux fils de l'interface USB-TIC :
   - I1 ↔ fil 1 (la polarité n'a **pas** d'importance — le protocole est
     différentiel),
   - I2 ↔ fil 2.
4. Raccrochez le capuchon.
5. Branchez l'interface USB au port USB de la machine qui exécute Moli
   (le RPi / le serveur Moli, pas le ThinkPad qui dort).
6. Vérifiez que le module est reconnu :

   ```sh
   lsusb | grep -i 0403
   # 0403:6015 « Cartelectronic Interface USB 1 TIC » (série : DA3EVSTA)
   ls -l /dev/ttyUSB*
   ```

7. L'interface a un **cavalier « Linux »** (parfois étiqueté « PC ») :
   il doit être positionné côté Linux/PC.

## 2. Nom de port stable (recommandé)

`/dev/ttyUSB0` change si d'autres dongles USB série arrivent. Ajoutez une rule
udev qui donne un lien stable au module (idVendor `0403`, idProduct `6015`
du Cartelectronic USBTICLC) :

```sh
# /etc/udev/rules.d/99-teleinfo.rules
SUBSYSTEM=="tty", ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6015", SYMLINK+="serial/teleinfo"
```

Puis `sudo udevadm control --reload-rules && sudo udevadm trigger`, et
`/dev/serial/teleinfo` devient le port stable à mettre dans `moli.toml`.

## 3. Droits d'accès au port

Le nœud série appartient à `root:dialout` en 0660. Soit l'utilisateur Moli
fait partie du groupe `dialout` (puis redémarrez Moli), soit le conteneur est
démarré avec le device passé :

```yaml
# compose (conteneur moli-os)
services:
  moli:
    devices:
      - /dev/serial/by-symlink/serial/teleinfo:/dev/serial/teleinfo
```

## 4. Mode du Linky (standard vs historique)

L'écran du Linky indique le mode de la canalée TIC :

- **Standard** (défaut des Linky récents) : 9600 baud, trames à tabulations →
  `baud = 9600`.
- **Historique** : 1200 baud, trames à espaces → `baud = 1200`.

Le pilote détecte le format de trame automatiquement ; seul le **débit** doit
correspondre au réglage du compteur (sinon zéro octet lisible).

## 5. Configuration Moli

```toml
[[driver]]
id = "teleinfo"
kind = "teleinfo"

[driver.options]
port = "/dev/serial/teleinfo"   # ou /dev/ttyUSB0
baud = 9600
id = "linky"
name = "Linky"

# Énergie : le module moli-énergie consomme les points du compteur.
# [energy.live]
# power = "teleinfo:linky/papp"     # TEMPO : "teleinfo:linky/sinsts"
# max = 9000                        # puissance souscrite, dans l'unité du point (VA)
#
# [[energy.meter]]
# id = "linky-hc"                  # clé de stockage : ne jamais renommer
# name = "Heures creuses"
# point = "teleinfo:linky/hchc"    # selon tarif : hchc/hchp (BASE), hchc1..3 (HC), east/easf01..10 (TEMPO)
# role = "grid"
# price = 0.14                      # € / kWh
# reset = "never"                  # index Linky (ne remonte jamais)
```

## 6. Vérification

1. `moli-os health` (ou `curl http://localhost:8790/api/health`).
2. L'appareil `teleinfo:linky` apparaît, en ligne, après la première trame
   (~10 s).
3. Le pilote passe en `waiting` si le port n'est pas accessible :
   « le port série {port} n'est pas accessible … ».
4. L'appareil passe hors ligne après 90 s sans trame (câblage, alimentation
   du Linky, débit inadapté).

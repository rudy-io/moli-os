# Une centrale d'alarme (Daewoo SA501, Tuya)

Une centrale d'alarme n'est pas un interrupteur : elle a un **mode**
(`désarmée`, `armée`, `maison`), des délais, une sirène, une batterie et un
GSM. Moli la lit et la commande **en local**, sur le réseau de la maison
(TCP 6668), sans passer par le cloud Tuya. Le tableau de bord lui donne une
carte dédiée, et la garde s'applique à ses ordres comme aux autres.

Vérifié sur une **Daewoo SA501** (catégorie Tuya `mal`, produit
`Daewoo SA501`, protocole 3.3) : lecture et écriture locales, y compris
`system_arm_type`, les délais et les sirènes.

## Ce que Moli voit

| Point | Sens | Écriture |
|---|---|---|
| `system_arm_type` | `disarmed`, `armed`, `home` | oui |
| `bat_status` | batterie, % | non |
| `dc_status` | alimentation secteur : `low`, `normal`, `high`, `off` | non |
| `gsm_status` | `Normal`, `NoSimCard`, `NoNetWork`, `GsmOff` | non |
| `arm_delay`, `alarm_delay` | délais d'armement et d'alarme, s | oui |
| `alarm_sound_duration` | durée de la sirène, min | oui |
| `ring_times`, `tel_alarm_cycle` | sonneries et appels par alerte | oui |
| `inside_siren_en`, `wireless_siren_en`, `gsm_en`, `tel_ctrl_en` | sirènes, GSM, commande téléphonique | oui |
| `arm_sms_en`, `disarm_sms_en` | SMS à l'armement et au désarmement | oui |
| `keyboard_tone_en`, `arm_delay_tone_en`, `alarm_delay_tone_en`, `arm_disarm_tone_en` | tonalités | oui |
| `inside_siren_sound` | `ON` / `OFF` | oui |

Ne sont **pas** repris dans `tuya.json` : le mot de passe de la centrale, les
numéros de téléphone, l'IMEI et les codes bruts. Ils ne servent pas à Moli et
n'ont rien à faire dans un fichier de données.

## L'ajouter

1. **La clé locale.** Elle vient de l'application Smart Life / Tuya Smart, par
   l'import cloud de Moli (`moli-os tuya import --cloud`) ou par l'export de
   Home Assistant (`tools/home-assistant/tuya-export.py`). Une centrale déjà
   liée à HA suit donc le même chemin que les autres appareils Tuya. La clé
   change si la centrale est ré-appairée : refaire l'import dans ce cas.
2. **Le fichier `tuya.json`** (sans secret). L'import le remplit ; à la main,
   l'entrée ressemble à ceci (valeurs fictives) :

```json
{
  "id": "<identifiant Tuya>",
  "name": "Centrale",
  "category": "mal",
  "product": "Daewoo SA501",
  "ip": "192.168.1.50",
  "version": "3.3",
  "dps": [
    { "id": 101, "code": "system_arm_type", "type": "Enum", "writable": true, "scale": 0,
      "range": ["disarmed", "armed", "home"] },
    { "id": 102, "code": "gsm_status", "type": "Enum", "writable": false, "scale": 0,
      "range": ["NoSimCard", "NoNetWork", "Normal", "GsmOff"] },
    { "id": 104, "code": "dc_status", "type": "Enum", "writable": false, "scale": 0,
      "range": ["low", "normal", "high", "off"] },
    { "id": 105, "code": "bat_status", "type": "Integer", "writable": false, "unit": "%",
      "scale": 0, "min": 0.0, "max": 100.0, "step": 1.0 },
    { "id": 106, "code": "arm_delay", "type": "Integer", "writable": true, "unit": "s",
      "scale": 0, "min": 0.0, "max": 300.0, "step": 1.0 },
    { "id": 107, "code": "alarm_delay", "type": "Integer", "writable": true, "unit": "s",
      "scale": 0, "min": 0.0, "max": 300.0, "step": 1.0 },
    { "id": 111, "code": "inside_siren_sound", "type": "Enum", "writable": true, "scale": 0,
      "range": ["ON", "OFF"] },
    { "id": 120, "code": "inside_siren_en", "type": "Boolean", "writable": true, "scale": 0 },
    { "id": 121, "code": "wireless_siren_en", "type": "Boolean", "writable": true, "scale": 0 }
  ]
}
```

   Les autres points (`language`, `alarm_sound_duration`, `ring_times`,
   `tel_alarm_cycle`, `gsm_en`, `tel_ctrl_en`, les SMS et les tonalités) sont
   facultatifs : la carte s'adapte à ceux qui sont présents.
3. **La clé dans le coffre**, par un tube, jamais affichée ni au clavier :

```sh
printf '%s' "$LOCAL_KEY" | podman exec -i moli-os /moli-os --config /data/moli.toml \
  secrets set tuya key-<identifiant Tuya>
```

   (`docker exec -i` à la place de `podman` sur une installation Docker ; le
   nom du secret est toujours `key-<identifiant>`.)
4. **Déclarer le pilote** dans `moli.toml` (`kind = "tuya"`, `devices_file`)
   s'il ne l'est pas déjà, puis `moli-os check-config` et redémarrer. Corriger
   au besoin `ip` et `version` dans `tuya.json` : un import les conserve.
5. **Vérifier** que la centrale répond : `system_arm_type` doit afficher
   `disarmed` (ou son état réel) dans Atelier, sans rien changer.

## Dans l'interface

- **Accueil** : une carte « Alarme » au-dessus des tuiles, avec le mode
  courant, trois boutons (Désarmée, Armée, Maison), la batterie, le secteur,
  le GSM, la sirène et les délais. Désarmer part en un geste ; armer demande
  une confirmation, car une porte ouverte peut faire sonner la sirène.
- **Caméras** : le panneau « Sécurité » (portes, présences) porte la même
  commande, en version compacte.
- **Pièces** : la centrale apparaît dans sa pièce — réglable dans l'Atelier
  ou par `PUT /api/labels/{id}`, sans redémarrer — et sous le filtre
  **Alarme**.
- **Réglages** : les délais, la durée de la sirène, les sirènes intérieure et
  sans fil, le GSM, les SMS et les tonalités se règlent dans le volet
  « Réglages » de la carte. Chaque changement suit le même chemin qu'un
  bouton : la garde peut le retenir (pièce protégée, heures calmes) et
  demander le code de la maison.

## Limites et sécurité

- **Pas d'événement d'alarme** : la SA501 n'expose pas de point « sirène en
  cours » ni de détail de zone. La carte dit le mode, pas ce qui a déclenché.
  L'historique de Moli garde les changements de mode, pas les passages.
- **Pas de cloud** : si Moli ne joint plus la centrale (autre VLAN, adresse
  changée, clé locale changée), elle passe hors ligne ; les boutons se
  désactivent au lieu de faire semblant.
- **L'appairage des zones, la SIM et les numéros d'appel** restent dans
  l'application du fabricant : Moli ne les modifie pas.
- **Tester sans armer la maison** : d'abord lire l'état, puis essayer un
  délai (`arm_delay`) sur une cible neutre, un humain présent. Ne jamais
  armer une pièce protégée où quelqu'un se trouve, ni laisser un essai de
  sirène sans surveillance. Remettre l'état initial (désarmée) après
  l'essai.
- Un ordre envoyé par un agent suit la garde : dans une pièce protégée ou aux
  heures calmes, il devient une demande qu'un humain valide.

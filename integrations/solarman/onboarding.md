# Onboarding : Solarman (micro-onduleurs Deye et TSUN, onduleurs Solis et ZCS Azzurro)

1. **De jour** (la nuit, le micro-onduleur et son enregistreur dorment) : trouver l'adresse IP de
   l'enregistreur (box, ou application Solarman) ; son numéro de série est sur son étiquette.
   Sans numéro, Moli le demande lui-même à l'enregistreur sur le réseau.
2. Une instance par enregistreur : `kind = "solarman"`, `host` (et `serial`, `mac` si connus).
   `timezone` (par défaut `Europe/Paris`) : l'heure à laquelle la production du jour repart à 0.
3. `moli-os check-config`, redémarrer Moli.
4. Vérifier, de jour : la production, la production du jour et la production totale doivent
   correspondre à l'application Solarman (ou à l'écran de l'onduleur). Rien n'est jamais écrit
   dans l'onduleur.
5. Le soir, l'appareil passe hors ligne sans alerte : c'est normal.

Pour un onduleur Solis : `model = "solis"`, et si l'enregistreur ne répond pas aux trames V5,
`transport = "modbus_tcp"` (une clé Solis : essayer d'abord en lecture). La clé ne sert qu'un
client : désactiver l'intégration Solarman de Home Assistant avant de tester.

Pour un TSUN ou un ZCS Azzurro (**jamais essayés sur du vrai matériel** : le dire à la personne,
et lui demander de comparer chaque valeur à l'application du fabricant) :

- TSUN TITAN (TSOL-MP3000, MP2250) : `model = "tsun-titan"` ; TSUN GEN4 (TSOL-MX, MX3000D) :
  `model = "tsun-gen4"`. Demander la référence exacte sur l'étiquette de l'onduleur. Un TSOL-MS
  (GEN3 PLUS) n'est pas pris en charge.
- ZCS Azzurro 1PH TL-V3 (onduleur réseau, une entrée) : `model = "zcs-tl"` ; HYD-ZSS-HP (hybride
  avec batterie) : `model = "zcs-hyd"`. Derrière un Connext, rien ne répond : il faut un kit
  Wi-Fi ou Ethernet.
- Si l'appareil reste hors ligne de jour, regarder le journal : un onduleur qui refuse la carte
  de registres y est signalé (mauvais `model`). Rapporter ce qui marche ou non dans une *issue*,
  avec le modèle et le firmware.

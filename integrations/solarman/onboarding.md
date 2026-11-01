# Onboarding : Solarman (micro-onduleurs Deye)

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

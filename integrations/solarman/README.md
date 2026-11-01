# Solarman / Deye micro-onduleurs

Pilote natif `crates/moli-solarman`. Une instance = un enregistreur Wi-Fi Solarman (celui intégré
aux micro-onduleurs Deye). **Lecture seule** : seule la fonction Modbus 0x03 est jamais envoyée.

- **Transport** : protocole V5 sur TCP 8899 (trame Modbus RTU dans l'enveloppe de l'enregistreur,
  numéro de série de l'enregistreur obligatoire), une connexion par lecture, toutes les 10 s.
  Écrit d'après la description publique du protocole (pysolarmanv5, MIT).
- **Registres** : carte de ha-solarman `deye_micro.yaml` (MIT, voir `NOTICE`), elle-même tirée du
  protocole Modbus Deye V1.18. Deux blocs : identité (0x00-0x12, chaque heure) et mesures
  (0x3B-0x74). Vérifié valeur par valeur contre Home Assistant au même instant sur un
  micro-onduleur 2 entrées (1 kW) et un 4 entrées (2 kW).
- **Points** : production (W), production du jour et totale à vie (kWh ; seule la totale a la
  sémantique `energy`), par entrée tension, courant, puissance (tension × courant), production du
  jour et totale ; tension, courant, fréquence du réseau ; température ; état (`standby`,
  `self_test`, `normal`, `alarm`, `fault`) ; codes d'alarme et de défaut bruts ; puissance
  nominale.
- **Valeurs absurdes écartées** (comme ha-solarman) : puissance au-delà de 1,2 × la puissance
  nominale, tension hors 0-300 V… ; un compteur à vie qui recule ou saute de plus de 100 kWh est
  ignoré.
- **La nuit** : l'onduleur dort, l'enregistreur avec lui. L'appareil passe hors ligne sans
  avertissement ; la production du jour reste affichée jusqu'à minuit (fuseau `timezone`), puis
  repart à 0.
- **Adresse qui change** : suivi par MAC (`moli_net::lan::relocate`), identité vérifiée par le
  numéro de série ; cherché seulement de 10 h à 16 h (le silence de nuit est normal).
- **Pas encore** : les autres familles de la carte ha-solarman (onduleurs string, hybrides,
  batteries), la limitation de puissance (écriture), le code d'alarme en clair (le registre
  d'alarme d'un micro-onduleur sain n'est pas nul : sa table reste à établir).

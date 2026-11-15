# Solarman : micro-onduleurs Deye et TSUN, onduleurs Solis et ZCS Azzurro

Pilote natif `crates/moli-solarman`. Une instance = un enregistreur Wi-Fi Solarman (celui intégré
aux micro-onduleurs Deye) ou une clé Solis. **Lecture seule** : seules les fonctions Modbus de
lecture sont envoyées, 0x03 (Deye, TSUN GEN4, ZCS) et 0x04 (Solis), et pour les TSUN TITAN les
seuls codes de lecture de leur trame propre (0xA1 à 0xA4).

| `model` | Onduleur | État |
|---|---|---|
| `deye` (par défaut) | micro-onduleurs Deye 2 et 4 entrées | vérifié contre Home Assistant |
| `solis` | Solis S6-GR1P | vérifié contre Home Assistant |
| `tsun-titan` | TSUN TITAN (TSOL-MP3000, MP2250) | **jamais vu sur un vrai onduleur par ce pilote** |
| `tsun-gen4` | TSUN GEN4 (TSOL-MX, dont MX3000D) | **jamais vu sur un vrai onduleur par ce pilote** |
| `zcs-tl` | ZCS Azzurro 1PH TL-V3 | **jamais vu sur un vrai onduleur par ce pilote** |
| `zcs-hyd` | ZCS Azzurro HYD-ZSS-HP | **jamais vu sur un vrai onduleur par ce pilote** |

Ce qui suit décrit les micro-onduleurs Deye ; les autres modèles ont leur section plus bas.

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

## Solis (S6-GR1P)

`model = "solis"` : un onduleur string Solis S6-GR1P derrière son enregistreur. Registres
d'entrée (fonction Modbus 0x04) 3004 à 3043, lus en deux blocs : puissance AC (`power`) et des
panneaux (`pv_power`), production du jour, du mois, de l'année et totale (`energy`, compteur à
vie, jamais en recul), entrées PV 1 et 2, réseau (tension, courant, fréquence), température,
`state` (`waiting`, `starting`, `generating`, `fault`) et `fault_code`. Carte de
`solis_s6-gr1p.yaml` (ha-solarman), valeurs vérifiées contre Home Assistant au même moment.

Certains enregistreurs (une clé Solis en est) parlent **Modbus TCP** sur le port 8899 au lieu
des trames V5 : `transport = "modbus_tcp"`. Ces clés ne servent qu'un client à la fois : Home
Assistant et Moli ne peuvent pas la lire ensemble.

## TSUN (TITAN et GEN4) : jamais vu sur un vrai onduleur par ce pilote

Écrits d'après la carte de registres publique de tsun-gen3-proxy (BSD-3-Clause, voir `NOTICE`),
**sans aucun essai sur du vrai matériel**. Les fixtures `tsun-titan.json` et `tsun-gen4.json` sont
des trames **construites à la main** à partir de cette carte : elles prouvent que le pilote lit la
carte comme elle est écrite, pas que la carte est juste. Seul élément réel : une réponse d'un
TITAN publiée dans les tests de tsun-gen3-proxy (registres 3000 à 3031), que le pilote décode en
valeurs plausibles (234,5 V, 218,8 W, 1250,41 kWh) ; elle a été lue par ce projet-là, pas par
ce pilote. À faire par la première personne qui en possède un : comparer chaque point à
l'application TSUN, de jour, et le dire.

- `model = "tsun-titan"` (TSOL-MP3000, MP2250) : ces onduleurs ne parlent pas Modbus RTU mais
  une **trame propre à TSUN** dans l'enveloppe V5 (code de lecture, sous-code, premier registre,
  nombre ; mots octet faible en premier ; CRC Modbus octet fort en premier ; réponse précédée de
  `7E`). Trois blocs de 32 registres : 3000 (codes `A1 01`), 3600 (`A3 03`, entrées 1 à 3) et
  3800 (`A4 04`, entrées 4 à 6). tsun-gen3-proxy décrit cet échange sur le port local 8899 (son
  « mode client ») ; **ce pilote ne l'a jamais essayé**. `transport = "modbus_tcp"` n'a pas de
  sens ici et est refusé.
- `model = "tsun-gen4"` (TSOL-MX, dont le MX3000D) : Modbus ordinaire, registres de maintien
  0x1100 (état, alarme, défaut), 0x1200 (réseau, puissance, compteurs, température) et 0x1300
  (entrées), lus en trois blocs.
- **Points** des deux : production (`power`, W, au dixième), production du jour et totale à vie
  (`energy`, kWh au centième, seule à porter la sémantique `energy`), tension, courant et
  fréquence du réseau, température, `state` (`offline`, `on_grid`, `off_grid`), puissance
  nominale, et par entrée tension, courant, puissance, production du jour et totale. GEN4 seul :
  `alarm_code` et `fault_code` bruts.
- **Nombre d'entrées** : lu une fois dans le bloc d'identité (puissance maximale de conception :
  6 entrées à partir de 2400 W, 4 à partir de 1600 W, 2 à partir de 600 W, 1 en dessous). Tant
  que ce bloc ne répond pas (trois essais), l'onduleur est montré avec 6 entrées.
- **Garde-fous**, comme Deye : puissance au-delà de 1,2 × la puissance de conception, tension
  hors 0-300 V, fréquence hors 40-70 Hz… écartées ; un compteur à vie qui recule ou saute de plus
  de 100 kWh est ignoré ; la nuit l'appareil passe hors ligne sans avertissement et la production
  du jour repart à 0 à minuit. Un onduleur qui **refuse** la carte (mauvais `model`) est signalé
  une fois dans le journal.
- **Pas fait** : les TSUN GEN3 PLUS (TSOL-MS, registres 0x3000 : une autre carte), les alarmes du
  TITAN (bloc 3300), le suivi par MAC quand l'adresse change (Deye seulement).

## ZCS Azzurro : jamais vu sur un vrai onduleur par ce pilote

Les ZCS Azzurro sont des Sofar sous une autre marque. Écrits d'après les cartes de ha-solarman
(MIT, voir `NOTICE`) vers lesquelles pointent ses anciens profils `zcs_azzurro-*.yaml`, **sans
aucun essai sur du vrai matériel** ; les fixtures `zcs-tl.json` et `zcs-hyd.json` sont des trames
**construites à la main** à partir de ces cartes. Registres de maintien (0x03). Ne marche pas
derrière un Connext : il faut un kit Wi-Fi ou Ethernet (ZSM-WIFI).

- `model = "zcs-tl"` : onduleur réseau monophasé **1PH TL-V3** (une entrée PV), carte
  `sofar_string.yaml`, registres 0x0000 à 0x001C en un bloc. Puissance AC (`power`) et de
  l'entrée (dizaines de watts), tension et courant de l'entrée, production du jour (au centième)
  et totale (`energy` : le compteur à vie est en kWh entiers, complété au dixième par celui du
  jour, comme pour Solis), réseau (tension, courant, fréquence), température interne, `state`
  (`standby`, `self_test`, `normal`, `alarm`, `fault`).
- `model = "zcs-hyd"` : hybride monophasé **HYD-ZSS-HP** (deux entrées PV, batterie), carte
  `sofar_g3.yaml`, cinq blocs entre 0x0404 et 0x069B. `power` est ici la **puissance des
  panneaux** (entrée 1 + entrée 2), ce que compte `energy` (production totale, au dixième). En
  plus : `inverter_power` (sortie AC), `grid_power` (soutirée au réseau, négative en injection),
  `load_power` (consommation), `battery` (niveau en %), `battery_power` (positive en décharge,
  négative en charge), tension, courant et température de la batterie, température interne et du
  radiateur, et les compteurs à vie `load_energy`, `energy_import`, `energy_export`,
  `battery_charge_energy`, `battery_discharge_energy`. Seule la production porte les sémantiques
  `power` et `energy` ; le niveau de batterie n'est pas une pile d'appareil (pas d'alerte « pile
  faible »). **Non vérifié** : le sens des signes (repris de la carte, où le réseau et la batterie
  sont inversés) et l'adresse des températures sur ce modèle précis.
- **Garde-fous** : valeurs hors de ce que l'onduleur peut lire écartées, compteurs à vie jamais
  en recul ni en saut de plus de 100 kWh, hors ligne sans avertissement.
- **Pas fait** : les triphasés (3PH KTL-V3, 3PH HYD), le 1PH TLM à deux entrées, le code de
  défaut en clair, tout réglage (rien n'est jamais écrit).

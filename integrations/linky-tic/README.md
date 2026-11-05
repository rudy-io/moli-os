# Linky (lecture sur place, par la sortie TIC)

- Lit la **télé-information client** (TIC) du compteur par un module USB (uTinfo, Micro-Teleinfo…) vu comme
  `/dev/ttyACM0` ou `/dev/ttyUSB0`. Rien n'est demandé au compteur, rien ne passe par Internet : le pilote écoute.
- **Temps réel** : le compteur répète une trame toutes les une à deux secondes. À la différence du pilote `enedis`
  (veille, par un service en ligne), la puissance et les index sont ceux de l'instant.
- Deux modes, à choisir comme le compteur est réglé (`mode`) :
  - `standard` (défaut, 9600 bauds) : puissance apparente `SINSTS`, énergie soutirée totale `EAST`, les dix index
    `EASF01` à `EASF10` (un par période tarifaire), le libellé `LTARF` et le numéro `NTARF` de la période en cours,
    courants `IRMS1` à `IRMS3`, tension `URMS1`, puissance maximale du jour `SMAXSN`.
  - `historique` (1 200 bauds) : `PAPP`, `HCHC` et `HCHP` (index 01 et 02), `BASE` (total et index 01), `PTEC`, `IINST`.
- Points publiés : `apparent_power` (VA), `energy_total` et `energy_index_01` à `energy_index_10` (Wh),
  `tariff`, `tariff_index`, `current_1` à `current_3` (A), `voltage_1` (V), `max_apparent_power` (VA). Un point
  qu'une trame ne porte pas reste vide.
- **Heures creuses et pleines** : les index sont ceux du contrat. Selon le fournisseur, 01 et 02 se suivent
  (creuses, pleines) ou non : relever `tariff` et `tariff_index` en changeant de période avant de déclarer les compteurs
  de `[energy]`.
- **Ligne muette** : sans groupe valide pendant `silence_seconds` (30 par défaut, 5 au minimum), l'appareil passe hors
  ligne et le port est fermé puis rouvert (le superviseur relance le pilote). Un module qui se fige, un câble à moitié
  débranché ou un mauvais `mode` sont ainsi signalés au lieu de laisser une valeur périmée.
- Un groupe dont la somme de contrôle est fausse (un bit renversé sur la ligne) est écarté : la trame suivante
  répète la valeur.
- Dans un conteneur, le port doit être donné au conteneur (`devices:` de Docker) et lisible par l'utilisateur de
  l'image (groupe `dialout` : `group_add`).
- **Pas de mesure sur un compteur réel dans ce dépôt** : les trames de `fixtures/` sont **fabriquées** d'après la
  spécification (numéro de compteur, dates et index fictifs, sommes de contrôle valides), pas capturées. La
  somme de contrôle et les deux modes sont éprouvés par les tests ; les réglages série (7 bits, parité paire, 1 bit
  d'arrêt) et la lecture du port ne l'ont pas été sur du matériel.
- Reste : registre d'état `STGE`, autres options tarifaires du mode historique (Tempo, EJP), mode historique triphasé, lecture de plusieurs
  compteurs.

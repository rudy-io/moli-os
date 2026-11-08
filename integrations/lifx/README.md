# LIFX

Pilote natif `crates/moli-lifx`. Une instance = toutes les ampoules LIFX qu'elle trouve.

- **Transport** : protocole LAN de LIFX, UDP 56700, sans compte ni secret. Écrit d'après la
  documentation publique de LIFX ; les trames sont vérifiées par les tests de `src/proto.rs`.
- **Trouver les ampoules** : `GetService` envoyé aux adresses de `hosts` et, sauf
  `discovery = false`, en diffusion sur le réseau local. Nouvelle recherche chaque minute tant
  qu'une ampoule manque (connue avant, ou adresse de `hosts` muette), sinon toutes les 5 min.
- **Identité** : le numéro de série (12 chiffres hexadécimaux, la MAC Wi-Fi moins 1), jamais
  l'adresse. Une ampoule qui change d'adresse est suivie ; une autre ampoule à la même adresse
  ne la remplace pas.
- **Ordres** : avec accusé de réception (`ack_required`) ; la valeur n'est publiée qu'après
  l'accusé, sans accusé l'ordre échoue.
- **Réseau** : un `LightGet` par ampoule et par relevé ; version, micrologiciel et groupe lus à
  la rencontre seulement, signal à chaque recherche.
- **Capacités** : lues sur l'ampoule (`GetVersion`, `GetHostFirmware`) puis cherchées dans la
  table des produits du pilote. Un modèle inconnu n'a que la marche et la luminosité
  (avertissement du pilote).

## Vérifié sur du vrai matériel

| Modèle | Produit | Micrologiciel | Vérifié | Date |
|---|---|---|---|---|
| LIFX Mini W | à relever | à relever | pas encore | |
| LIFX White | à relever | à relever | pas encore | |

- **Couleur** (`color`) et **température réglable** (`color_temp`) : **non testées sur du vrai
  matériel** (aucune ampoule couleur ni à blanc réglable dans la maison de référence) ; couvertes
  seulement par des tests sur des trames construites d'après la documentation.
- **Reste** : effets, multizone zone par zone, matrices, infrarouge, cycle HEV, thèmes.

# Compatibilité Matter / Matter compatibility

Le pilote est indépendant de la marque. Hue est un essai matériel, pas une condition
ni une liste blanche. Les capacités sont déduites de Descriptor, des serveurs, des
fonctions et des attributs/commandes annoncés par chaque endpoint.

| Famille / cluster serveur | Fonctions exposées / exposed functions |
|---|---|
| Éclairage et prises / `0006` | `on` : lecture et commandes On/Off |
| Variation / `0008` | `brightness` en %, limites physiques et commandes annoncées |
| Couleur / `0300` | `color_temp` en kelvins ; `color` RGB `#rrggbb` via XY ou hue/saturation selon les fonctions annoncées |
| Stores / `0102` | `position` de levage (100 = ouvert, 0 = fermé), `cover_control` open/close/stop selon les commandes disponibles |
| Thermostat / `0201` | température locale ; consigne absolue chauffage/refroidissement ; modes off/heat/cool/auto selon capacités |
| Serrure / `0101` | état `locked` et état brut ; commandes lock/unlock temporisées lorsque la politique connue les autorise, avec approbation de la garde |
| Température / `0402`, humidité / `0405`, présence / `0406` | Mesures en lecture seule, valeurs nulles/inconnues explicites |
| Contact/fuite / `0045` | Lecture typée selon DeviceType contact `0015` ou fuite `0043` ; type absent ou ambigu non exposé |

Pour `brightness = 0`, le pilote utilise `Off` seulement si OnOff annonce la
commande. Sinon il utilise `MoveToLevelWithOnOff` si annoncé; il ne rabat pas
silencieusement zéro sur `MinLevel`. Une valeur positive privilégie
`MoveToLevelWithOnOff`; si seul `MoveToLevel` est disponible, le pilote envoie
d'abord `On` uniquement quand OnOff l'annonce aussi. Sans chemin compatible,
le point reste non inscriptible ou la demande est refusée.

Les nouvelles familles exigent des métadonnées valides et bornées : FeatureMap,
AttributeList, AcceptedCommandList et ClusterRevision. Une fonction optionnelle ou
une commande absente n'est pas inventée. Les politiques de sécurité et limites sont
abonnées ; un changement invalide les capacités puis déclenche la redécouverte.
Le pont exige aussi Reachable=true. Les identités ne dépendent ni du nom ni de la marque.

Les températures cibles chauffage/refroidissement sont des écritures absolues avec
limites et séparation minimale vérifiées. Les limites réglées (`Min/Max…SetpointLimit`)
sont facultatives : un appareil qui ne les liste pas dans son AttributeList est borné par
ses limites absolues, comme le prévoit la spécification (cas des vannes de radiateur) ;
une limite listée mais absente ou invalide garde la consigne en lecture seule. Un
thermostat simple utilise
`target_temperature` ; un modèle double utilise les clés fixes `heat_setpoint` et
`cool_setpoint`, y compris dans le dashboard. Le mode ne redirige jamais une commande
mise en attente vers une autre consigne.

Une serrure reste en lecture seule si un PIN est requis ou si sa politique nécessaire
n'est pas connue. Les PIN ne passent pas dans les points ni les journaux. Le contrôle
est un point Semantic::Control : une demande d'agent doit être approuvée. Un code
succès d'un autre chemin ou une réponse ambiguë n'est pas accepté comme acquittement.
L'état affiché vient des lectures/abonnements, jamais d'une anticipation de commande.
Après admission dans la file, un timeout, un arrêt du contrôleur ou une réponse
inexploitable est signalé comme issue inconnue et n'est pas réessayé. Un statut
Matter explicitement refusé est définitif et le hub ne le relance pas. Vérifier
l'état observé avant de soumettre un nouvel ordre.

## Limites

Ce tableau ne garantit pas toutes les fonctions de tous les produits certifiés.
Inclinaison des stores, gestion de codes/PIN, déverrouillage avec durée, programmes
thermostat, ventilation, scènes/groupes, événements de boutons, énergie et clusters
non listés ne sont pas encore mappés. Les prises et lampes OnOff ont la même commande ;
leur catégorie Matter n'est pas conservée pour les distinguer dans l'interface.
Les variations de conformité ou d'ACL peuvent rendre un endpoint partiellement accessible.

Matter sur IP : Ethernet/Wi-Fi, ou Thread via un routeur de bordure externe et une
route IPv6/mDNS. Le commissionnement Bluetooth n'est pas livré dans cette contribution ;
Moli ne crée ni ne gère un réseau Thread. Les nouveaux types sont validés sur un
accessoire virtuel indépendant ; cela ne remplace pas les essais physiques absents.
Voir [résultats](../../tools/matter-e2e/RESULTS.md), [Hue](HARDWARE.md), [Thread](thread.md).

## English

Support is brand independent and capability driven. The table lists implemented
functions, not universal coverage of every Matter product or cluster. New families
require valid bounded feature/attribute/accepted-command/revision metadata. Missing
or unsafe policy data never grants writes. Mutable policies are monitored and cause
rediscovery; bridged devices also require Reachable. Device identity remains stable.

Lighting uses percent, kelvin and RGB with mode-aware subscriptions. Covers use
100=open; observed position is never replaced by the requested target. Thermostats
use absolute writes with advertised limits/deadband (user limits not in the
AttributeList fall back to the absolute ones, per the specification), fixed heat/cool keys on dual
models, and exact-path acknowledgements. Lock control is always timed and guarded;
PIN-required or unknown-policy locks remain read only. No optimistic state updates.
Brightness zero requires an advertised Off or MoveToLevelWithOnOff command; a
positive value uses the on/off-aware command when available, or an advertised On
followed by MoveToLevel. After queue acceptance, a timeout, controller shutdown,
or unusable response is outcome-unknown and is never retried automatically. An
explicit Matter status refusal is final and does not trigger hub follow-up.

The limitations above remain unsupported. Standard IP commissioning and ongoing
operation support Ethernet/Wi-Fi or routed Thread; Bluetooth provisioning is not shipped
in this contribution and is not hardware validated. See the independent virtual-device and real Hue evidence separately.

Primary definitions: [CHIP data model](https://github.com/project-chip/connectedhomeip/tree/v1.4.2.0/src/app/zap-templates/zcl/data-model/chip),
[device type registry](https://github.com/project-chip/connectedhomeip/blob/master/src/app/zap-templates/zcl/data-model/chip/matter-devices.xml).

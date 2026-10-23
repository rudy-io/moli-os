# Somfy TaHoma (passerelle Overkiz)

- Origine : l'API locale est documentée par Somfy (mode développeur, `openapi.yaml`) ; le
  comportement est recoupé avec le client communautaire `python-overkiz-api` et l'intégration
  `overkiz` de Home Assistant. Aucun code repris.
- Transport : HTTPS sur **8443**, `Authorization: Bearer <jeton>`, base
  `/enduser-mobile-web/1/enduserAPI`. Le certificat de la passerelle est **auto-signé**
  (autorité Overkiz) : Moli l'épingle au premier contact, avant tout jeton.
- **Pas de `POST /login` en local** : le jeton se génère dans l'appli Somfy (mode développeur),
  il ne s'expire pas en principe, mais il est perdu à la mise à jour du firmware et à un
  changement d'heure. Le pilote ne l'écrit jamais sur le disque : il est dans le coffre.
- `GET /setup/devices` donne l'état complet (tous les appareils, `states` incluses). Le pilote
  interroge toutes les 5 s — les volets mettent des secondes à bouger, inutile d'aller plus
  vite, et la passerelle n'aime pas être martelée.
- Position : `core:ClosureState`, **0 = ouvert, 100 = fermé**. Moli affiche un pourcentage
  d'ouverture : le pilote inverse dans les deux sens (`setClosure 60` pour 40 % ouvert). Les
  stores banne utilisent `core:DeploymentState` (0 = replié, 100 = déployé) : déjà une
  ouverture, pas d'inversion. `108` (« my ») et `124` (inconnu) ne sont pas des positions.
- Commandes : `POST /exec/apply` avec les commandes que l'appareil déclare (`open`, `stop`,
  `close`, `setClosure`, `setDeployment`). Un `execId` accepté vaut acquittement ; la position
  réelle arrive au relevé suivant, comme un état ordinaire.
- `fixtures/devices.json` : un `/setup/devices` réel anonymisé — un volet roulant io
  positionnable (fermé), un volet io ouvert, un volet RTS **sans état** (il ne connaît pas sa
  position : le pilote ne publie alors que les boutons), un store banne déployé à 40 %, une
  lampe io (hors périmètre). Rejoué par `crates/moli-tahoma/src/model.rs`.
- Reste : éclairages et prises Somfy io (le même `exec/apply` les commande), lames
  orientables (`setOrientation`), scènes et tableaux de bord de la passerelle, écoute des
  événements (`/events/register`) au lieu du relevé périodique.
- **Pas encore vu en vrai sur cette installation** : commandes envoyées à un volet, coupure
  et retour de la passerelle. Le relevé et le mapping sont testés sur la fixture.

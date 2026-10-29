# Z-Wave (via zwave-js-ui)

- Réutilisé : le protocole MQTT de zwave-js-ui ; les points viennent du `getNodes` du
  gateway, sans code par marque.
- `fixtures/get_nodes.json` (6 nœuds synthétiques, dont un moteur de volet CC 38),
  `value_messages.json` et `get_nodes_real.json` (capture réelle anonymisée de la
  maison, 2026-06-24) : rejoués par les tests de `crates/moli-zwave/src/catalog.rs`.
- Identité : `nodeId` Z-Wave (le nom de nœud ne sert qu'au routage des sujets).
- Écrites : `on` passe par `targetValue` (CC 37 : booléen ; CC 32/38 : nombre 0–255,
  car le `currentValue` de Basic est en lecture seule dans zwave-js). Le succès d'une
  écriture n'est pas vérifié : la passerelle n'acquitte pas les commandes.
- Sémantique du `level` : `Brightness` seulement si la classe de device du nœud est
  Multilevel Switch (generic 0x11, specific 0x01/0x02) ; sinon `Other` (un moteur de
  volet CC 38 nommé « Portail » ne doit pas être traité comme une lumière par la garde).
- Multi-endpoints : un seul canal `on` par nœud — le premier CC switch trouvé dans les
  valeurs du gateway (le ZMNHBD dual-relay n'expose que son endpoint 0 dans `getNodes`) ;
  le second relay (endpoint 1) reste injoignable en v1, en attente d'une extension.
- Reste : l'appairage se fait dans zwave-js-ui, et le flag retain du gateway est
  optionnel (bootstrap `getNodes` à chaque connexion, réessais 30 s, rafraîchissement
  périodique 300 s, sinon fallback par accumulation des topics vus après 15 s).

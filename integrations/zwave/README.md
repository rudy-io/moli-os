# Z-Wave (via zwave-js-ui)

- Réutilisé : le protocole MQTT de zwave-js-ui ; les points viennent du `getNodes` du
  gateway, sans code par marque.
- `fixtures/get_nodes.json` (5 nœuds synthétiques) et `value_messages.json` : la cible
  restant injoignable au planning (capture réelle prévue), rejouées par les tests de
  `crates/moli-zwave/src/catalog.rs`.
- Identité : `nodeId` Z-Wave (le nom de nœud ne sert qu'au routage des sujets).
- Reste : l'appairage se fait dans zwave-js-ui, et le flag retain du gateway est
  optionnel (bootstrap `getNodes` à chaque connexion sinon).

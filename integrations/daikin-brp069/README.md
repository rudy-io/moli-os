# Daikin BRP069

- Protocole documenté par pydaikin (intégration HA « daikin ») : aucun code ni donnée repris.
  `fixtures/control_info.kv` est une réponse capturée sur un vrai BRP069 (éteint, mode auto).
- `basic_info.kv` et `sensor_info.kv` : réponses courtes présentes dans les tests depuis le
  premier profil ; leur provenance exacte n'est pas notée.
- Vérifié en lecture sur de vrais adaptateurs BRP069. Les écritures (`on`, `mode`,
  `target_temperature`, `fan`) suivent ce protocole et restent à tester physiquement, un humain
  présent, sur une unité hors des pièces protégées (jamais une chambre).
- Reste : les unités à `f_dir_ud` / `f_dir_lr` (écriture refusée faute de `f_dir`) ; les
  références exactes des adaptateurs vérifiés ne sont pas relevées.

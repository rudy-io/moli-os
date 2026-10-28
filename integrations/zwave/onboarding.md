# Onboarding : Z-Wave (zwave-js-ui)

1. Demander : où tourne le broker MQTT (hôte, port 1883 par défaut), le `base_topic`
   (`zwave` par défaut) et le `mqtt_name` du client gateway (`default` par défaut).
2. Mot de passe éventuel : dans une variable d'environnement dont seul le **nom** va dans
   `password_env` (cible : `MOLI_ZWAVE_PASSWORD`) ; jamais dans `moli.toml`.
3. Appairer les appareils dans zwave-js-ui lui-même (Moli ne fait pas l'appairage Z-Wave).
4. Déclarer `kind = "zwave"` ; `moli-os check-config`, redémarrer ; tous les nœuds de
   `getNodes` apparaissent.
5. Tester une commande sur une cible neutre, humain présent, jamais dans une chambre
   protégée.

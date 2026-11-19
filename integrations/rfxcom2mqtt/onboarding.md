# Onboarding : RFXcom 433 MHz (rfxcom2mqtt)

1. Demander : où tourne le broker MQTT (hôte, port 1883 par défaut), le `mqtt.base_topic`
   de rfxcom2mqtt (`rfxcom2mqtt` par défaut) et son `homeassistant.discovery_topic`
   (`homeassistant` par défaut).
2. Vérifier `homeassistant.discovery: true` dans la configuration de rfxcom2mqtt : sans
   découverte, le pilote ne crée aucun appareil (il ignore les sondes entendues dans le
   voisinage).
3. Mot de passe éventuel : dans une variable d'environnement dont seul le **nom** va dans
   `password_env` (cible : `MOLI_RFXCOM_PASSWORD`) ; jamais dans `moli.toml`.
4. Nommer les appareils dans rfxcom2mqtt (`devices:` de sa configuration) aide à les
   reconnaître ; une prise Chacon n'apparaît qu'une fois que rfxcom2mqtt l'a entendue
   (télécommande) ou commandée.
5. Déclarer `kind = "rfxcom2mqtt"` ; `moli-os check-config`, redémarrer ; les appareils annoncés
   par rfxcom2mqtt apparaissent.
6. Tester une commande sur une cible neutre, humain présent, jamais dans une chambre
   protégée : le 433 MHz ne confirme rien, seul un humain voit si la prise a obéi.

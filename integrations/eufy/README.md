# Eufy Security

- Origine : protocole porté d'[eufy-security-client](https://github.com/bropat/eufy-security-client)
  (MIT, voir `NOTICE` et `licenses/eufy-security-client-MIT.txt`) : connexion `login_sec`, code
  par e-mail et appareil de confiance, captcha, listes des stations et des appareils.
- Aucune capture : les vecteurs des tests (`crates/moli-eufy/src/crypto.rs`) sont produits par le
  `crypto` de Node, comme la bibliothèque l'emploie ; les réponses d'API sont écrites dans le code.
- Lecture : nom, modèle, batterie, mode de sécurité (par le cloud, toutes les 10 min) ; détections
  et changements de mode en direct par les notifications push (Firebase, checkin Android, GCM, MCS).
- Reste : P2P local avec les stations (UDP 32108 : mode de sécurité en écriture sous la garde,
  images), nouveau serveur « v6 / mega » pour les comptes migrés.

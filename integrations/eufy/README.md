# Eufy Security

- Origine : protocole porté d'[eufy-security-client](https://github.com/bropat/eufy-security-client)
  (MIT, voir `NOTICE` et `licenses/eufy-security-client-MIT.txt`) : connexion `login_sec`, code
  par e-mail et appareil de confiance, captcha, listes des stations et des appareils.
- Aucune capture : les vecteurs des tests (`crates/moli-eufy/src/crypto.rs`) sont produits par le
  `crypto` de Node, comme la bibliothèque l'emploie ; les réponses d'API sont écrites dans le code.
- Lecture : nom, modèle, batterie, mode de sécurité (par le cloud, toutes les 10 min) ; détections
  et changements de mode en direct par les notifications push (Firebase, checkin Android, GCM, MCS) ;
  photo du dernier événement (`pic_url`, déchiffrée si besoin).
- Mode de sécurité en écriture, en P2P local (UDP 32108, chiffrement de niveau 1).
- Reste : nouveau serveur « v6 / mega » pour les comptes migrés.

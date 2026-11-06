# Onboarding : tado°

1. Il faut une clé maître (`MOLI_MASTER_KEY_FILE` ou `data/master.key`) : le pilote garde et renouvelle lui-même le `refresh_token` dans le coffre chiffré.
2. Déclarer `[[driver]]` avec `id = "tado"`, `kind = "tado"` dans `data/moli.toml` ; `moli-os check-config`, puis redémarrer.
3. Autorisation OAuth2 (flux Device Code officiel tado°) : le pilote passe en `waiting` et affiche l'adresse `https://login.tado.com/oauth2/device?user_code=…`. La personne ouvre ce lien et se connecte à son compte tado°.
4. Le pilote récupère automatiquement le jeton, charge les zones de la maison et publie chaque thermostat dans sa pièce.
5. Tester : vérifier la température, l'humidité et la consigne ; ajuster la consigne de 0,5 °C sur une pièce neutre (jamais une chambre), puis la remettre en mode `auto`.

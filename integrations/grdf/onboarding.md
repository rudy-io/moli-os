# Onboarding : GRDF

1. La personne vérifie qu'elle se connecte à <https://monespace.grdf.fr> **avec une adresse e-mail et
   un mot de passe** (pas par un compte tiers ni avec un code), et y relève le numéro à 14 chiffres de
   son compteur (PCE), aussi écrit sur la facture de gaz.
2. Secrets, rangés par un tube : `moli-os secrets set <instance> email`, puis
   `moli-os secrets set <instance> password`.
3. Déclarer `kind = "grdf"` avec `pce = "<numéro>"` ; `moli-os check-config`.
4. Pour la page Énergie, déclarer **dans le même redémarrage** un compteur dans `[energy]` :
   `point = "<instance>:gazpar/hours"`, `role = "gas"`, `history = true`, `price` (en € par kWh) en
   option. La première relève remonte un an : sans ce compteur, cette année n'est pas classée.
5. Redémarrer, puis tester : l'appareil montre le dernier jour reçu, son énergie, son volume et l'index,
   à comparer à l'espace client.
6. En cas de mot de passe refusé : vérifier la connexion dans un navigateur, ranger le bon mot de passe
   (étape 2), redémarrer. Ne pas relancer en boucle : GRDF verrouille un compte qui échoue.

# Onboarding : voiture Tesla

1. Prévenir la personne : Moli pilotera la voiture (charge, portes, vigie) via l'API
   officielle de Tesla, avec son compte ; Moli apparaîtra dans la liste des clients du
   compte, comme l'appli officielle.
2. Déclarer `kind = "tesla"` dans `moli.toml` (les hôtes par défaut conviennent en
   Europe ; `api.tesla.cn` en Chine) ; `moli-os check-config`, redémarrer. Le pilote
   attend la connexion (« Connecte le compte Tesla… ») et affiche l'adresse à ouvrir.
3. La personne ouvre cette adresse sur un ordinateur, se connecte à son compte Tesla,
   et colle l'adresse de la dernière page (celle qui contient `code=`) dans le
   formulaire Tesla du tableau de bord (réservé au propriétaire, code de la maison
   requis). L'adresse porte un `state` qui la lie à la page affichée : un code
   d'une page plus ancienne est refusé. Le code expire en quelques minutes : coller
   sans attendre. Le mot de passe ne quitte jamais la page de Tesla, et le code
   collé ne passe jamais par le journal. Pour déconnecter le compte ensuite : même
   page, action « Déconnexion » — le jeton est effacé du coffre et une page de
   connexion fraîche s'affiche.
4. Vérifier les secrets au besoin : une application personnelle (developer.tesla.com)
   se passe avec `moli-os secrets set tesla client_id …` puis `client_secret` (valeur
   par un tube).
5. Tester : la voiture apparaît dans la maison (appareil `tesla:<id>`, points
   batterie/autonomie/portes/vigie/températures) ; « verrouille la voiture » et
   « démarre la charge » passent par la commande `command`, gardée et journalisée
   comme les autres.

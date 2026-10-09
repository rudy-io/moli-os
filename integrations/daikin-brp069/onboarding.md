# Onboarding : Daikin BRP069

1. Demander : quelle unité (quelle pièce), son adresse IP (appli Daikin ou box). Une instance
   par unité ; son id ne se renomme jamais.
2. Aucun identifiant, aucun appui : l'adaptateur répond en HTTP local sans authentification.
3. Tester sans rien changer : `curl http://<ip>/common/basic_info` répond `ret=OK,…,mac=…`.
4. Déclarer `kind = "profile"`, `profile = "daikin-brp069"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer. L'identité est la MAC de l'adaptateur.
5. Écriture : seulement avec un humain présent, jamais dans une chambre. Changer la consigne
   de 1 °C, la remettre, vérifier l'entrée `command` dans `/api/journal`.

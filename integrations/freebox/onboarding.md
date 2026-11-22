# Onboarding : Freebox

1. Demander quelle Freebox (Server v7 « Delta », Revolution, Pop, Ultra) et si la personne peut aller devant
   l'écran du Freebox Server au moment de démarrer le pilote.
2. Aucun identifiant à saisir. Déclarer `kind = "freebox"` (option `host` seulement si la box ne répond pas à
   `mafreebox.freebox.fr`) ; `moli-os check-config`, redémarrer.
3. Au premier démarrage, le statut du pilote demande d'autoriser Moli sur l'écran de la Freebox (flèche de droite
   pour « Oui », puis OK). La clé reçue va au coffre des secrets ; elle ne repasse jamais sur le réseau.
4. Tester : `temp_cpum`, `rate_down`, `connected` ont une valeur. Lecture seule.
5. Retirer l'accès : Freebox OS → Paramètres → Gestion des accès → Applications → « Moli ».

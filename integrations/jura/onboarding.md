# Onboarding : machine à café Jura (Wi-Fi Connect, application J.O.E.)

1. Demander : la machine est-elle connectée au Wi-Fi avec l'application J.O.E. (module Wi-Fi
   Connect, ou Wi-Fi intégré) ? Son modèle (affiché dans J.O.E., ou le numéro d'article sous la
   machine) ? A-t-on mis un **code de sécurité** dans J.O.E. (réglages du Wi-Fi Connect) ? Le
   code lui-même, jamais : la personne le range.
2. Trouver l'adresse de la machine (bail fixe dans la box conseillé) : le port TCP 51515 est
   ouvert sur une puce Espressif.
3. Déclarer `kind = "jura"` (`id = "jura"`), `host = "<adresse>"`, et pour une E8 (EC)
   `model = "EF1092"` (les noms des boissons et les réglages ; sans lui, les codes communs à
   toute la famille). `moli-os check-config`, redémarrer.
4. S'il y a un code : `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os
   secrets set jura pin`, tapé par la personne.
5. Appairage : machine allumée sur son écran d'accueil (aucun menu ouvert), application J.O.E.
   fermée (la machine ne sert qu'une connexion à la fois). Le dashboard dit « appuyez sur OK
   sur la machine » : la personne appuie sur OK sur la machine. Le jeton va au coffre (`token`).
6. Vérifier : la machine apparaît avec son état, ses alertes, l'entretien et ses compteurs ;
   la ranger dans sa pièce.
7. Ordres (code de la maison demandé ; l'ordre d'un agent attend un humain) : une boisson,
   préparée avec la recette réglée sur la machine — **une tasse sous la buse d'abord** ; un
   entretien, que la machine mène ensuite sur son écran (pastille, bac, confirmation) ; annuler.
   Premier essai : de l'eau chaude dans une tasse, avec la personne devant la machine.

# Onboarding : multiprise Meross (MSS425, MSS620)

1. Demander : l'adresse IP de la multiprise (réservation DHCP conseillée), et la clé du compte Meross qui l'a installée (elle ne voyage jamais : elle signe les
   messages ; la documentation de meross_lan explique comment la lire).
2. L'humain range la clé lui-même, par un tube : `moli-os secrets set <instance> key`.
   Un appareil installé par Matter n'a pas de clé : s'il répond à une requête signée avec une
   clé vide, déclarer `empty_secret = true` dans ses options au lieu de ranger une clé.
3. Déclarer `kind = "profile"`, `profile = "meross-multiprise"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer.
4. Tester en lecture : `on` (toutes les sorties) et `on_1`… suivent l'appli Meross ; les
   sorties que l'appareil n'a pas restent sans valeur. Repérer avec la personne quelle sortie
   porte quel numéro, et nommer les points.
5. Écrire seulement avec la personne, sur une sortie qu'on peut couper sans risque (jamais
   `on`, qui coupe tout, sans son accord), et remettre l'état initial. Ce profil n'a jamais été
   essayé sur un vrai appareil : noter ce qui se passe (modèle, firmware) dans le README.

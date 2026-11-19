# Onboarding : prise Meross MSS310

1. Demander : l'adresse IP de la prise (réservation DHCP conseillée), et la clé du compte Meross qui l'a installée (elle ne voyage jamais : elle signe les
   messages ; la documentation de meross_lan explique comment la lire).
2. L'humain range la clé lui-même, par un tube : `moli-os secrets set <instance> key`.
   Un appareil installé par Matter n'a pas de clé : s'il répond à une requête signée avec une
   clé vide, déclarer `empty_secret = true` dans ses options au lieu de ranger une clé.
3. Déclarer `kind = "profile"`, `profile = "meross-mss310"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer.
4. Tester en lecture : sans clé, statut `waiting` avec la commande à lancer ; avec la clé,
   `on`, `power`, `voltage`, `current` et `energy_today`. Comparer `power` et `voltage` à
   l'appli Meross : une tension dix fois trop grande ou trop petite veut dire que ce firmware
   ne parle pas en dixièmes de volt (à signaler, le profil est à corriger).
5. Écrire `on` seulement avec la personne, sur un appareil qu'on peut couper sans risque, et
   remettre l'état initial. Ce profil n'a jamais été essayé sur une vraie prise : noter ce qui
   se passe (modèle, firmware) dans le README du paquet.
6. Pour le suivi d'énergie : `power` et `energy_today` peuvent alimenter `[energy]`.

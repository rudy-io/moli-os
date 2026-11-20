# Onboarding : prise Meross MSS110 / MSS210

1. Demander : l'adresse IP de la prise (réservation DHCP conseillée), et la clé du compte Meross qui l'a installée (elle ne voyage jamais : elle signe les
   messages ; la documentation de meross_lan explique comment la lire).
2. L'humain range la clé lui-même, par un tube : `moli-os secrets set <instance> key`.
   Un appareil installé par Matter n'a pas de clé : s'il répond à une requête signée avec une
   clé vide, déclarer `empty_secret = true` dans ses options au lieu de ranger une clé.
3. Déclarer `kind = "profile"`, `profile = "meross-mss110"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer.
4. Tester en lecture : sans clé, statut `waiting` avec la commande à lancer ; avec la clé, la
   prise apparaît avec `on`. Une clé fausse : le pilote échoue (« wrong key or namespace? »).
5. Écrire `on` seulement avec la personne, sur un appareil qu'on peut couper sans risque, et
   remettre l'état initial. Un ordre refusé par « connection closed before message completed »
   vient d'une prise qui veut des ordres chiffrés que le moteur n'a pas reconnue comme telle :
   noter le modèle et le firmware dans le README du paquet.

# Onboarding : Meross EM06

1. Demander : l'adresse IP du compteur, et la clé du compte Meross qui l'a installé (la clé
   ne voyage jamais : elle signe les messages).
2. L'humain range la clé lui-même, par un tube : `moli-os secrets set <instance> key`.
3. Déclarer `kind = "profile"`, `profile = "meross-em06"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer.
4. Tester : sans clé, statut `waiting` avec la commande à lancer ; avec la clé, les six
   circuits (`power_1` à `power_6`) ; un circuit absent reste absent, jamais 0. Lecture seule.

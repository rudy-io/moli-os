# Onboarding : IzyPower (micro-onduleurs)

1. Demander : l'e-mail et le mot de passe du compte de l'application IzyPower (celui où la
   centrale apparaît), et, s'il y a plusieurs centrales sur le compte, laquelle suivre.
2. L'humain range les identifiants par un tube, jamais au clavier dans un fichier :
   `moli-os secrets set <instance> username` puis `moli-os secrets set <instance> password`.
3. Déclarer `kind = "izypower"` (option `station = <numéro>` seulement s'il y a plusieurs
   centrales) ; `moli-os check-config`, redémarrer.
4. Vérifier : la puissance de la centrale et de chaque micro-onduleur, en journée, doit suivre
   celle de l'application IzyPower (le cloud est mis à jour toutes les 3 minutes environ). La
   nuit, la puissance retombe à 0 et les productions restent : c'est normal.

# Onboarding : Voltalis

1. Secrets : le compte Voltalis, rangé par un tube :
   `moli-os secrets set <instance> username` puis `moli-os secrets set <instance> password`.
2. Déclarer `kind = "voltalis"` (`poll_seconds` en option, 300 par défaut, 60 au minimum) ;
   `moli-os check-config`, redémarrer.
3. Tester : chaque radiateur apparaît avec son état et sa consigne, identiques à l'application.
   Puis, sur un radiateur d'une pièce sans enfant, éteindre, rallumer et regarder l'application.
   Si un autre système pilote déjà les radiateurs, il reprendra la main : un seul maître à la fois.

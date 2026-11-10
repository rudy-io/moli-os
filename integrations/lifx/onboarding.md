# Onboarding : LIFX

1. Demander : les ampoules sont-elles sur le même réseau que Moli (la diffusion suffit) ou
   ailleurs (donner leurs adresses dans `hosts`) ? Une adresse fixe aide mais n'est pas exigée.
2. Aucun secret : LIFX n'en a pas sur le réseau local.
3. Déclarer `kind = "lifx"` (options facultatives : `hosts`, `discovery`, `poll_secs`,
   `transition_ms`) ; `moli-os check-config`, redémarrer.
4. Vérifier : chaque ampoule arrive avec son nom et son groupe LIFX en suggestion de pièce.
5. Tester, humain présent, hors chambre et hors heures calmes : éteindre puis rallumer une
   ampoule, changer sa luminosité, remettre l'état initial.

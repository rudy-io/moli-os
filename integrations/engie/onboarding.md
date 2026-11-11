# Onboarding : ENGIE Belgique

1. Prévenir la personne : interface non officielle (celle de l'appli ENGIE Smart) ; se connecter
   avec le même compte dans l'appli ou sur engie.be déconnecte Moli (un utilisateur dédié l'évite).
2. Déclarer `kind = "engie"` (options facultatives : `language` de la page de connexion, `fr` par
   défaut ; `timezone`, `Europe/Brussels` par défaut) ; `moli-os check-config`, redémarrer.
3. La personne connecte le compte depuis le tableau de bord (Système, carte ENGIE) : page ENGIE,
   connexion avec le code SMS ou e-mail, adresse `be.engie.smart://…` collée (en-tête `Location`
   de la dernière réponse 302 de `/authorize/resume`, dans l'onglet Réseau des outils du navigateur
   ouvert avant la connexion). Jamais son mot de
   passe ailleurs que chez ENGIE.
4. Tester : l'appareil « Contrat ENGIE » a ses prix et son pic du mois ; un véhicule en recharge
   intelligente apparaît avec sa batterie ; changer `departure` ou `target_level` se voit dans
   l'appli ENGIE.

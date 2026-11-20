# Onboarding : Govee

Prévenir d'abord : ce pilote n'a encore été essayé sur aucune vraie lumière (voir `README.md`).
Le premier essai est une vérification, pas une formalité : noter le modèle et ce qui marche.

1. **Activer l'API locale dans l'appli Govee, lumière par lumière** : ouvrir l'appareil, ses
   réglages, activer « LAN Control » (« Contrôle LAN »). **Tous les modèles ne l'ont pas** : si
   l'option n'existe pas pour une lumière, ce pilote ne peut pas la piloter.
2. Demander : les lumières sont-elles sur le même réseau que Moli (la multidiffusion suffit) ou
   ailleurs, ou sur un Wi-Fi qui filtre la multidiffusion (donner leurs adresses dans `hosts`) ?
   Une adresse fixe aide mais n'est pas exigée.
3. Vérifier que rien d'autre n'écoute le port UDP 4002 sur la machine de Moli (une intégration
   Govee locale de Home Assistant sur la même machine, par exemple) : les lumières ne répondent
   qu'à ce port. Une seule instance `govee`. Moli en conteneur : réseau de l'hôte.
4. Aucun secret : l'API locale n'en a pas.
5. Déclarer `kind = "govee"` (options facultatives : `hosts`, `discovery`, `poll_secs`) ;
   `moli-os check-config`, redémarrer.
6. Vérifier : chaque lumière arrive, nommée « Govee <modèle> (<fin de l'identifiant>) ». L'API
   locale ne donne ni nom ni pièce : les choisir dans Moli. Statut « aucune lumière Govee n'a
   répondu » : revoir les étapes 1 à 3.
7. Tester, humain présent, hors chambre et hors heures calmes : éteindre puis rallumer une
   lumière, changer sa luminosité, sa couleur, son blanc ; vérifier que le tableau de bord
   montre ce que la lumière a fait ; remettre l'état initial. Reporter le modèle, le
   micrologiciel et le résultat dans le tableau « Vérifié sur du vrai matériel » du `README.md`.
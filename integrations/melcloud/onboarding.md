# Onboarding : Mitsubishi Electric MELCloud

1. Vérifier que le compte utilise le MELCloud classique (`app.melcloud.com`), pas MELCloud Home.
2. Secret : `token` = la clé de contexte du compte (`LoginData.ContextKey` de la connexion, ou le
   champ `token` de l'intégration MELCloud de Home Assistant). L'humain la range par un tube :
   `moli-os secrets set <instance> token` ; le mot de passe n'est jamais stocké.
3. Déclarer `kind = "melcloud"` (`poll_seconds` en option, 300 par défaut) ;
   `moli-os check-config`, redémarrer.
4. Tester : chaque unité apparaît avec sa température et sa consigne, identiques à l'application.
   Puis changer la consigne d'une unité et revenir en arrière : l'application doit suivre en une à deux minutes.

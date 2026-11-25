# Onboarding : Pushover

1. La personne installe l'appli Pushover sur son téléphone et se connecte sur <https://pushover.net> :
   la **clé utilisateur** (*Your User Key*) est en haut de la page d'accueil. Pour prévenir plusieurs
   personnes, une **clé de groupe** convient aussi.
2. Elle crée une application pour Moli (<https://pushover.net/apps/build>, nom « Moli », une icône si
   elle veut) et relève son **jeton d'API** (*API Token/Key*).
3. Secrets, rangés par un tube, jamais collés dans une conversation :
   `moli-os secrets set <instance> token`, puis `moli-os secrets set <instance> user`.
4. Déclarer `kind = "pushover"`. Options facultatives : `devices` (noms des appareils Pushover, tous
   ceux du compte si vide), `priority` (-2 à 1, 0 par défaut), `sound` (un son Pushover),
   `per_minute` et `per_hour` (plafond, 5 et 30 par défaut). `moli-os check-config`.
5. Redémarrer, puis tester : une commande `push` avec « Essai », puis un automatisme avec le canal
   `telephone`. L'appareil montre les messages restants du mois.
6. Avec l'appli Moli **et** Pushover, le canal `telephone` envoie aux deux : n'en configurer qu'un des
   deux, comme on le fait déjà avec plusieurs téléphones.
7. En cas de clés refusées : les vérifier sur pushover.net, les ranger à nouveau (étape 3), redémarrer.
   Ne pas relancer en boucle : Pushover bloque un moment une adresse qui insiste.

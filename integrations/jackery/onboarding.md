# Onboarding : batterie Jackery

1. Il faut un broker MQTT dans la maison (Mosquitto, sur la machine de Moli ou ailleurs), avec
   un utilisateur et un mot de passe pour la batterie.
2. Dans l'appli Jackery : réglages de la batterie → MQTT local. Y saisir l'adresse du broker,
   son port, l'utilisateur et le mot de passe. L'appli affiche alors un **jeton** et le préfixe
   des sujets (`hb` par défaut). Noter le numéro de série de la batterie (étiquette, appli).
3. L'humain range le jeton lui-même, par un tube : `moli-os secrets set <instance> token`.
   Si le broker demande un mot de passe à Moli : `moli-os secrets set <instance> password`.
4. Déclarer `kind = "jackery"`, `serial = "<numéro de série>"`, et selon le broker `host`,
   `port`, `username`, `topic_prefix` ; `moli-os check-config`, redémarrer.
5. Tester en lecture : le niveau et la réserve doivent être ceux de l'appli Jackery. Sans
   jeton, statut `waiting` avec la commande à lancer.
6. La réserve : ne l'essayer qu'avec l'accord de la personne, en la remontant de quelques
   points (jamais vers le plancher), et vérifier dans l'appli qu'elle a suivi ; remettre la
   valeur d'avant.
7. Prévenir : au repos la batterie se déconnecte par moments, c'est son comportement. Pour
   la mesure du réseau, déclarer aussi son compteur (profil `jackery-meter`).

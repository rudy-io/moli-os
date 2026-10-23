# Onboarding : Somfy TaHoma (API locale)

1. Activer le mode développeur de la passerelle : dans l'appli **Somfy**, *Configuration →
   Accéder aux paramètres → toucher le code PIN de la passerelle 7 fois → Mode développeur*.
   La passerelle s'annonce alors en mDNS (`_kizboxdev._tcp`) et son API locale répond sur
   **8443**.
2. Dans ce même écran, **générer un jeton** (libellé libre, portée « devmode »). C'est lui qui
   autorise Moli ; il ne se devine pas et ne s'échange pas.
3. Ranger le jeton dans le coffre, sans jamais l'afficher :
   `read -rs T; printf %s "$T" | moli-os secrets set <instance> token`
   (sur une installation conteneurisée : `podman exec -i moli-os /moli-os secrets set <instance> token`).
   Il faut une clé maître : c'est elle qui chiffre le coffre, et l'empreinte du certificat y
   sera rangée aussi.
4. Déclarer `kind = "tahoma"`, `host = "<ip de la passerelle>"` ; `moli-os check-config`, puis
   redémarrer (le pilote lit les secrets au démarrage).
5. Au premier démarrage, le certificat de la passerelle est épinglé ; s'il change ensuite, le
   statut passe en `waiting` et dit quoi faire. Pour repartir de zéro :
   `moli-os secrets forget <instance> cert_sha256`, puis redémarrer.
6. Vérifier : les volets et stores apparaissent avec leur position ; l'ouverture d'un volet
   neutre (jamais une chambre, un humain présent) doit bouger le volet et la position suivre
   dans Moli. Remettre l'état initial.

Le jeton est perdu si le firmware de la passerelle est mis à jour ou si son horloge change
d'heure : il suffit d'en générer un nouveau (étape 2) et de refaire l'étape 3.

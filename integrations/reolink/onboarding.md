# Onboarding : Reolink

1. Demander : l'adresse IP de la station (Home Hub, NVR ou caméra seule), et les identifiants
   de la station (utilisateur, mot de passe).
2. Il faut une clé maître : le pilote épingle le certificat (`cert`) avant d'envoyer les
   identifiants.
3. L'humain range les identifiants lui-même, par un tube : `moli-os secrets set reolink
   username`, puis `password`.
4. Déclarer `kind = "reolink"`, `host = "<ip>"` ; `moli-os check-config`, redémarrer.
5. Tester : un passage devant une caméra extérieure fait passer `motion` à vrai ; une image
   s'affiche (`/api/devices/<id>/snapshot`). Lecture seule : rien à commander.

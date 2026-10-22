# Onboarding : Shelly 2 / 2.5 (porte de garage)

1. Demander : quel appareil (quelle pièce), son adresse IP (appli Shelly ou box). Une instance par appareil ; son id ne se renomme jamais.
2. Tester sans rien changer : `curl http://<ip>/shelly` répond le type et `"auth":false`. Si `auth` vaut `true`, l'appareil demande un mot de passe : non pris en charge, le retirer dans son interface web.
3. Déclarer `kind = "profile"`, `profile = "shelly-2-garage"`, `host = "<ip>"` ; `moli-os check-config`, redémarrer. L'identité est la MAC de l'appareil : si la box lui donne une autre adresse, Moli le retrouve seul.
4. Écriture : seulement avec un humain présent, jamais dans une chambre.

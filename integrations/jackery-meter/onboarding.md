# Onboarding : Jackery Smart Meter

1. Demander : l'adresse IP du compteur (dans la liste des appareils de la box ; lui donner
   une adresse fixe). Il ne répond pas au ping : vérifier avec
   `curl http://<ip>/api/measurement`, qui doit rendre du JSON avec `power` et `engy`.
2. Déclarer `kind = "profile"`, `profile = "jackery-meter"`, `host = "<ip>"` ;
   `moli-os check-config`, redémarrer. Aucun secret.
3. Tester : `power` suit la consommation de la maison (allumer une bouilloire : + 2 000 W
   environ) ; au soleil, quand la maison envoie du courant au réseau, `power` devient
   négatif. S'il reste positif en injection, la pince est montée à l'envers.
4. Dans `[energy]` : `power` pour la puissance du compteur `total`, `energy` pour son
   compteur, et `energy_returned` pour un compteur de rôle `export`. Lecture seule.

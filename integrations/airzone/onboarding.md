# Installation / Onboarding

1. FR : Confirmer une passerelle avec API locale Webserver, son adresse et son
   port (3000 par défaut). Un Aidoo cloud seul n'est pas couvert. EN: Confirm
   a Webserver with the local API, address and port (default 3000). Cloud-only
   Aidoo is outside this driver.
2. FR : Configurer une instance `airzone`, idéalement avec la MAC obtenue dans
   l'interface du contrôleur. Exécuter `moli-os check-config`. EN: Configure one
   `airzone` instance per controller, preferably pin its MAC from the controller
   interface, and run `moli-os check-config`.
3. FR : Démarrer en lecture, comparer chaque système/zone, les températures,
   unités, bornes, incréments et modes avec les thermostats. Attribuer les pièces
   dans Moli et protéger les pièces concernées. EN: Start read-only validation;
   compare each system/zone, temperature, unit, bound, increment and mode with
   the thermostats. Assign rooms in Moli and protect the relevant rooms.
4. FR : Vérifier les membres de chaque contrôle commun avant toute commande ;
   une commande de mode/ventilation peut affecter plusieurs pièces. Si plusieurs
   maîtres sont ambigus, le pilote masque ces écritures. EN: Inspect each shared
   control's members before writing: mode/fan commands may affect multiple rooms.
   Ambiguous multi-master writes remain unavailable.
5. FR : Humain présent, tester une zone neutre autorisée, conserver puis remettre
   la valeur initiale. Vérifier le retour du thermostat après acceptation. Aucun
   test en pièce protégée. EN: With a human present, test an authorized neutral
   zone, save and restore its original setting, and check thermostat feedback
   after acceptance. Never test in a protected room.
6. FR : Vérifier séparément coupure réseau, redémarrage, horaires constructeurs
   et compatibilité du transport HTTP sur ce firmware. EN: Validate network loss,
   restart, manufacturer schedules and HTTP transport compatibility separately
   on the intended firmware. Loopback tests are not hardware validation.

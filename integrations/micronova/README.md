# Micronova (poêles à granulés, Easy Connect)

- Aucun code repris : le protocole du cloud Agua IoT de Micronova (celui des applications
  Easy Connect) est décrit par py-agua-iot ; Moli le parle lui-même (`crates/moli-micronova`).
- La carte : chaque valeur est un registre (position, masque, formule, mots) ; valeur = brut
  ET masque, puis la formule où `#` est la valeur. Les formules sont calculées, jamais
  exécutées (`+ - * /`, parenthèses, `int()`).
- Fixture : la carte et une lecture d'un Camelia Metal Air réel (poêle éteint), sans
  identifiant ni numéro de série.
- Vérifié en vrai : connexion (code 354924, `remote.mcz.it`), carte de 89 registres, lecture
  de l'état, des alarmes, des températures (air, fumées), de la consigne, de la puissance, des
  ventilateurs et de l'éco-stop.
- Reste : lecture seule (consigne, puissance, marche et arrêt à venir, essayés avec quelqu'un
  devant) ; le programmateur du poêle (6 plages) n'est pas lu.

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
- Lus aussi : les 6 programmes du poêle (`chrono_p<n>_…` : actif, début et fin au pas de
  10 min, jours) et son horloge, en décimal codé binaire (la carte ne le dit pas : sur la
  fixture, `calendar_day_set` = 0x24 le 24, `clock_minute_set` = 0x58 à 58 min).
- Réglés depuis Moli : consigne, puissance, ventilation, éco-stop et son délai, programmes.
  L'écriture est celle des applications (py-agua-iot) : `POST /deviceRequestWriting`
  (`Protocol` `RWMSmaster`, `Items`, `Masks`, `Values` calculées par `formula_inverse`), puis
  la tâche suivie jusqu'à `completed`. Essayée d'abord par une sonde sur le poêle réel, un
  réglage à la fois, remis comme avant.
- Marche et arrêt aussi (`status_managed_get`, un bouton : ON 85, OFF 170, relu 32 au repos),
  essayés d'abord par une sonde avec quelqu'un devant le poêle (de la veille à éteint en 12 s,
  puis rallumé) ; seulement depuis éteint, jamais pendant l'extinction ni en alarme ; l'ordre
  d'un agent attend un humain (point de machine).
- Alarme : `alarms_get` (position 51) a sur la carte du Camelia Metal Air le masque `0x801e`,
  qui efface trois de ses propres codes (1 = A09, 32 = A03, 64 = A05) ; les cartes MCZ de
  l'intégration Home Assistant `aguaiot` disent `0xffff` pour les mêmes mots. Moli lit donc le
  registre avec les bits de ses mots. Vu sur un vrai poêle : état « MEM. ALARM » avec une alarme
  lue « OK ». Réarmement à distance : rien de connu pour cette carte (l'application ne propose
  pas Arrêt en alarme d'après `status_managed_off_enable` des cartes MCZ ; py-agua-iot n'en a
  pas) ; essayé sur le vrai poêle en « MEM. ALARM » : un OFF est pris (bouton à 170 puis au
  repos) et l'alarme reste. Le réarmement se fait sur le poêle ; Moli refuse marche et arrêt
  en alarme.

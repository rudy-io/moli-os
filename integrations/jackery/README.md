# Batterie Jackery (HomePower, SolarVault)

Pilote natif `crates/moli-jackery`. Une instance = une batterie. Lecture seule, sauf la
**réserve**.

- **Transport** : la batterie est un client MQTT. Dans l'appli Jackery, on lui donne un broker
  de la maison ; Moli se connecte au même broker, lui demande son état sur
  `<préfixe>/device/<numéro de série>/action` et lit ses réponses sur `…/status` et `…/event`.
  Forme des messages et noms des champs lus dans l'intégration Home Assistant officielle de
  Jackery (MIT, voir `NOTICE`), aucun code copié.
- **Points** : niveau (%), charge, décharge, production solaire, réseau (signé : positif quand
  la maison achète, négatif quand elle injecte ; absent si la batterie ne voit plus son
  compteur), consommation de la maison (W), état (`normal`, `attente`, `alarme`, `defaut`,
  `veille`, `charge_faible`), réserve, limite de charge (%), puissance de sortie maximale (W),
  température, signal Wi-Fi (dBm), nombre de batteries.
- **Une seule écriture : la réserve**, le niveau sous lequel la batterie cesse d'alimenter la
  maison, entre 5 et 49 % (les bornes de la batterie ; le hub refuse le reste). Un ordre pour
  la valeur que la batterie annonce déjà ne part pas. Aucune commande de charge : Moli ne peut
  pas forcer une charge depuis le réseau.
- **Vérifié** sur une vraie installation (2 batteries, micrologiciel 1.6.0, Smart Meter 1P) le
  15 novembre 2026 : les trois fixtures en viennent, numéros de série et adresses remplacés.
  La lecture a été comparée à Home Assistant. **L'écriture de la réserve par ce pilote n'a pas
  été essayée** : le message est celui que l'intégration officielle envoie tous les jours sur
  cette installation.
- **Ce qu'un hiver avec elle a appris** :
  - au repos, la batterie se coupe du broker pendant des minutes puis revient seule, quels que
    soient le rythme des questions et la qualité du réseau. Ses dernières valeurs restent,
    l'appareil passe hors ligne après 100 s de silence. Pour la mesure du réseau, lire le
    compteur en direct (profil `jackery-meter`), pas la batterie ;
  - la réserve arrête la fourniture à la maison, pas la consommation propre de la batterie :
    elle descend encore de quelques points par nuit. Garder une marge (15 % plutôt que 5) ;
  - une réserve réécrite à chaque relevé ne sert à rien : par paliers de quelques points ;
  - elle ne répond pas au ping, même connectée.
- **Pas encore** : les prises et compteurs rattachés (`cmd` 110), la limite de charge et la
  puissance de sortie en écriture, la veille automatique, le redémarrage, le détail par
  batterie et par entrée solaire.

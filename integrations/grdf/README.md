# GRDF (compteur gaz Gazpar)

- GRDF n'offre pas d'API publique aux particuliers. Moli se connecte à l'espace client
  (`monespace.grdf.fr`) comme sa page le fait, avec l'adresse e-mail et le mot de passe du client.
  Lecture seule, dépendante d'Internet, et à la merci d'un changement côté GRDF.
- La connexion passe par Okta (`connexion.grdf.fr`) : page d'accueil, jeton d'état, adresse e-mail,
  mot de passe, puis adresse de succès suivie jusqu'aux cookies de session. Rien ne sort de `grdf.fr` :
  une redirection ailleurs arrête la connexion. Les cookies ne vont qu'aux hôtes qui les ont posés
  (ou au domaine qu'ils nomment).
- Données « informatives » : une valeur par journée gazière (de 6 h à 6 h le lendemain), avec un jour
  de retard ou plus. Le compteur ne transmet rien de plus fin : pour la page Énergie, l'énergie d'une
  journée est **répartie également sur ses heures**. Les totaux par jour, semaine et mois sont justes ;
  la forme dans la journée ne l'est pas.
- L'énergie (kWh) est celle de GRDF : le volume (m³) multiplié par le coefficient de conversion du jour.
- Deux connexions par jour au plus, à des heures propres au compteur : le matin entre 8 h et 11 h,
  puis vers 16 h si la veille manque encore. Un redémarrage n'en ajoute aucune. Chaque requête couvre
  7 jours (ou remonte jusqu'au dernier jour reçu si Moli est resté arrêté plus longtemps), un trou se
  répare donc tout seul. La toute première remonte un an, pour la page Énergie : publiée trois jours
  par trois jours, 200 ms d'écart, puis envoyée une seconde fois 10 minutes plus tard (de mémoire,
  sans nouvelle requête), pour que l'enregistreur d'énergie classe ce qu'il aurait manqué ; ce qui
  est déjà classé reste, rien ne compte deux fois.
- Un mot de passe refusé (401, 403) ou une étape de plus demandée (code, double authentification)
  arrête le pilote jusqu'au redémarrage : insister ferait verrouiller le compte par GRDF. Un 400 dit
  une requête que GRDF ne comprend plus (connexion changée) : un statut, pas un refus.
- Une page renvoyée au lieu des données, une limite de requêtes (429) ou un autre statut : un
  avertissement, et un nouvel essai au créneau suivant.
- `fixtures/` : la forme des vraies réponses (relevés informatifs, étape « identify » d'Okta), avec un
  compteur, des dates et des valeurs fictifs.
- Vérifié sur un faux serveur (connexion, mot de passe refusé, requête incomprise, étape de plus,
  redirection hors de GRDF, session perdue) et sur un compte réel (novembre 2026) : connexion,
  un an de relevés, puis les relèves du jour ; journées de 6 h à 6 h, coefficient lu.
- Reste : les données « publiées » (relevés de facturation), la température, plusieurs compteurs sur
  un même compte.

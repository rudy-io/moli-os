# Prises TP-Link Kasa (protocole local historique)

Les prises Kasa d'origine (HS100, HS110 avec mesure…) en local, sans compte : JSON sur TCP 9999,
chiffré par un XOR à clé glissante. Relais et voyant commandables ; puissance, tension, courant et
énergie totale pour une prise qui mesure (relus toutes les 10 s par défaut).

Une prise dont le firmware ne parle plus que KLAP (port 80) se déclare avec le pilote `tapo`.

Fixtures : réponses d'une HS110 (matériel 1.0, firmware 1.2.6) anonymisées, et une réponse au
format du matériel 2 (milli-unités) écrite d'après sa description : ce matériel n'a pas été
essayé.

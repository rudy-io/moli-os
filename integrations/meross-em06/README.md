# Meross EM06 / EM06P

- Réutilisé : le protocole local signé décrit par meross_lan et merossiot, aucun code copié.
- `fixtures/electricity.json` : capturée sur un vrai compteur, raccourcie aux canaux 1 et 2.
  Les canaux 3 à 6 n'ont jamais été vus dans une fixture ; `Appliance.System.All` (identité
  `uuid`) non plus.
- Vérifié sur un vrai EM06P ; ses points alimentent la section `[energy]`.
- Reste : la façon d'obtenir la clé du compte Meross n'est pas documentée dans le dépôt.

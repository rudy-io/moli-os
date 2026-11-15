# Meross MSS110 / MSS210

Prise connectée à un relais, sans mesure, par son API locale signée (HTTP, sans cloud à
l'exécution) : `on` se lit (`Appliance.System.All`) et s'écrit (`Appliance.Control.ToggleX`,
message `SET`, fait seulement si la prise répond `SETACK`), puis l'état est relu.

- Réutilisé : le protocole décrit par meross_lan et merossiot (licence MIT), aucun code copié.
- **Jamais essayé sur une vraie prise.** `fixtures/all.json` n'est pas une capture : elle est
  construite d'après ces descriptions, avec des identifiants fictifs.
- Reste : un essai réel ; les très anciens matériels sans `ToggleX` (ils parlent
  `Appliance.Control.Toggle`) ne sont pas gérés : `on` y reste sans valeur et l'ordre est refusé.

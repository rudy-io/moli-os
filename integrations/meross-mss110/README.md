# Meross MSS110 / MSS210

Prise connectée à un relais, sans mesure, par son API locale signée (HTTP, sans cloud à
l'exécution) : `on` se lit (`Appliance.System.All`) et s'écrit (`Appliance.Control.ToggleX`,
message `SET`, fait seulement si la prise répond `SETACK`), puis l'état est relu.

- Réutilisé : le protocole décrit par meross_lan et merossiot (licence MIT), aucun code copié.
- **Essayé sur une vraie MSS110** (matériel 7.0.0, firmware 7.3.46).
  - **Lecture** : elle marche en clair.
  - **Ordre** : cette prise annonce `Appliance.Encrypt.ECDHE`. Elle coupe la connexion, sans
    réponse, sur un `SET` en clair (« connection closed before message completed »), et répond
    `SETACK` au même ordre chiffré. Le moteur chiffre donc ses ordres (voir
    `docs/components/profils.md`, « Écrire par le transport `meross` »).
  - `fixtures/all.json` n'est pas une capture : elle est construite d'après les descriptions
    de la communauté, avec des identifiants fictifs.
- Reste : les très anciens matériels sans `ToggleX` (ils parlent
  `Appliance.Control.Toggle`) ne sont pas gérés : `on` y reste sans valeur et l'ordre est refusé.

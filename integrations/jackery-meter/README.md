# Jackery Smart Meter

- Le compteur (la pince livrée avec les batteries HomePower / SolarVault) répond tout seul
  sur le réseau de la maison : `GET /api/measurement`, sans clé, sans la batterie et sans le
  cloud. Rien de réutilisé : les champs ont été relevés sur l'appareil.
- Pourquoi le lire en direct : la batterie relaie aussi sa mesure, mais elle se coupe du
  réseau par moments quand elle est au repos, et la maison perd alors la mesure du réseau
  avec elle. Le compteur, lui, continue de répondre.
- Vérifié sur un vrai Smart Meter 1P (monophasé, micrologiciel V1.01.111) le 15 novembre
  2026, en lecture : achat et injection observés, `power` négatif en injection, à quelques
  dizaines de watts de la mesure relayée par la batterie. `fixtures/measurement.json` en
  vient, numéro de série remplacé.
- Deviné : `fact` est le facteur de puissance en millièmes ; `rep` (sans doute la puissance
  réactive), `type`, `sc` et `ts` ne sont pas publiés.
- Pas encore : le Smart Meter 3P (triphasé) n'a pas été vu, ses champs par phase sont
  inconnus. La batterie elle-même (niveau, réserve, limite de décharge) n'est pas pilotée.
- Le compteur ne répond pas au ping : ne pas en conclure qu'il est absent.

# Meross MSS310

Prise connectée avec mesure, par son API locale signée (HTTP, sans cloud à l'exécution) :
`on` (lu et écrit, `SET` `Appliance.Control.ToggleX`, fait seulement sur `SETACK`, puis relu),
`power` (W), `voltage` (V), `current` (A), `energy_today` (Wh). Sur le fil : mW, dV, mA, Wh.

- Réutilisé : le protocole et les unités décrits par meross_lan et merossiot (licence MIT),
  aucun code copié.
- **Jamais essayé sur une vraie prise.** Les trois fixtures ne sont pas des captures : elles
  sont construites d'après ces descriptions, avec des identifiants fictifs.
- `energy_today` est la dernière journée que la prise a comptée (`Appliance.Control.ConsumptionX`,
  entrée la plus récente). La prise ne crée l'entrée du jour qu'à son premier Wh : entre minuit
  et ce moment, la valeur est encore le total de la veille.
- Reste : un essai réel (dont l'unité de la tension selon le firmware).

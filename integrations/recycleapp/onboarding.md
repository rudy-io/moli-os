# Onboarding : collectes de déchets (RecycleApp)

1. Demander l'adresse : code postal, rue (comme elle s'écrit sur recycleapp.be, en néerlandais ou
   en français), numéro.
2. Déclarer `kind = "recycleapp"` avec `zipcode`, `street`, `house_number` (et `language`, `fr`
   par défaut) ; `moli-os check-config`, redémarrer.
3. Tester : l'appareil « Collectes de déchets » a une date par type de collecte de la commune,
   les mêmes que sur recycleapp.be. Si le pilote attend : son message dit si c'est le code postal
   ou la rue qui n'est pas trouvé.

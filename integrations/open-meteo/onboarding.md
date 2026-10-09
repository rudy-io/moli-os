# Onboarding : Open-Meteo

1. Demander la commune (ou la position) de la maison ; arrondir à ~1 km (`lat`, `lon` avec
   deux décimales suffisent pour la météo).
2. Aucun identifiant, aucun appareil : API web publique.
3. Déclarer `kind = "profile"`, `profile = "open-meteo"`,
   `vars = { lat = "…", lon = "…" }` ; `moli-os check-config`, redémarrer.
4. Tester : `temperature`, `sunrise` et `sunset` ont une valeur dans le tableau de bord.

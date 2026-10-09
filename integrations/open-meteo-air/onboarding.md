# Onboarding : qualité de l'air (Open-Meteo)

1. Même position que la météo (`lat`, `lon`, arrondis à ~1 km).
2. Aucun identifiant : API web publique.
3. Déclarer `kind = "profile"`, `profile = "open-meteo-air"`,
   `vars = { lat = "…", lon = "…" }` ; `moli-os check-config`, redémarrer.
4. Tester : `aqi` et `uv_index` ont une valeur (relus toutes les 30 min).

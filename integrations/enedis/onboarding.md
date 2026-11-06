# Onboarding : Enedis

1. La personne donne son consentement sur <https://conso.boris.sh> (avec son compte Enedis) et
   récupère son jeton et le numéro à 14 chiffres de son compteur (PRM). Dans son espace client Enedis,
   activer la collecte de la courbe de charge.
2. Secret, rangé par un tube : `moli-os secrets set <instance> token`.
3. Déclarer `kind = "enedis"` avec `prm = "<numéro>"` ; `moli-os check-config`, redémarrer.
4. Tester : l'appareil montre le dernier jour reçu et sa consommation, à comparer à l'espace client.
5. Pour la page Énergie, un compteur dans `[energy]` :
   `point = "<instance>:linky/hours"`, `role = "grid"`, `history = true`, `price` en option.
6. Pour reprendre l'historique de Home Assistant, garder le même `id` de compteur lors de l'import.
   Ne pas lancer de resynchronisation massive : le service bloque sans prévenir les abus.

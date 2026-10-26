# Onboarding : imprimante réseau

1. Demander quelle imprimante, et qu'elle soit allumée sur le Wi-Fi (adresse dans la box, ou
   l'annonce `_ipp._tcp`).
2. Aucun identifiant. Vérifier qu'elle parle IPP : `Get-Printer-Attributes` sur
   `http://<adresse>:631/ipp/print` (sinon essayer `/ipp`, et `ipptool -V 1.1` : une ancienne
   imprimante refuse l'IPP 2.0, Moli s'y adapte seul). Ses consommables : `snmpwalk -v2c -c public
   <adresse> 1.3.6.1.2.1.43.11` (Moli les lit en SNMP si l'IPP ne les donne pas).
3. Déclarer `kind = "ipp"` avec `host` ; `moli-os check-config`, redémarrer.
4. Tester en lecture : `status`, les `ink_N`, `jobs`. Puis « Faire clignoter » (sans papier
   gâché), puis une page depuis le tableau de bord (Impression → Imprimer…).
5. Allumage à distance : seulement si l'imprimante a « Mise sous tension auto » (Canon : Auto
   power on) ; sinon elle s'éteint vraiment et Moli la voit hors ligne, c'est normal.

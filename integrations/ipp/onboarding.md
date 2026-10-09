# Onboarding : imprimante réseau

1. Demander quelle imprimante, et qu'elle soit allumée sur le Wi-Fi (adresse dans la box, ou
   l'annonce `_ipp._tcp`).
2. Aucun identifiant. Vérifier qu'elle parle IPP : `Get-Printer-Attributes` sur
   `http://<adresse>:631/ipp/print` (sinon essayer `/ipp`).
3. Déclarer `kind = "ipp"` avec `host` ; `moli-os check-config`, redémarrer.
4. Tester en lecture : `status`, les `ink_N`, `jobs`. Puis « Faire clignoter » (sans papier
   gâché), puis une page depuis le tableau de bord (Impression → Imprimer…).
5. Allumage à distance : seulement si l'imprimante a « Mise sous tension auto » (Canon : Auto
   power on) ; sinon elle s'éteint vraiment et Moli la voit hors ligne, c'est normal.

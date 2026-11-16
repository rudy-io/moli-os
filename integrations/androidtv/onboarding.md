# Onboarding : box ou télé Android TV

1. Demander : l'adresse IP de la box (fixe : elle sert d'identité).
2. Appairage Android TV Remote : le pilote ne sait pas appairer. Reprendre celui de Home
   Assistant, s'il connaît la box (`.storage/androidtv_remote_cert.pem` et `_key.pem`). L'humain
   les range par un tube, sans les afficher :
   `cat …/androidtv_remote_cert.pem | docker exec -i moli-os /moli-os secrets set <instance> atv_cert`,
   idem `androidtv_remote_key.pem` → `atv_key`. Il faut une clé maître. Sans eux, le pilote
   démarre quand même : Cast seul (ce qui joue, lecture/pause), ni touches ni applis.
3. Déclarer `kind = "androidtv"` et `host` ; `moli-os check-config`, redémarrer.
4. Vérifier : l'appareil apparaît avec `power` (allumée) et `app` (l'appli au premier plan).
5. Ce que joue n'importe quelle appli (YouTube, Netflix… ouvertes sur la box, que Cast ne voit
   pas) : sur la box, options pour les développeurs (Paramètres › Système › À propos ›
   appuyer 7 fois sur « Build »), activer le **débogage réseau** (ou « débogage ADB »). Ajouter
   `adb = true`, redémarrer : la box demande sur son écran d'autoriser « moli-os » (humain
   devant la télé : cocher « Toujours autoriser »).
6. Notifications par-dessus l'image : installer **TvOverlay** depuis le Play Store de la box
   puis suivre l'étape 7 de `integrations/philips/onboarding.md` (mêmes commandes), ajouter
   `overlay = true`, redémarrer : le point `notify` apparaît.
7. Page Salon du tableau de bord : étiqueter l'appareil dans « Salon » (`PUT /api/labels/{id}`
   avec `{"room": "Salon"}`) **et** renseigner `"salon": { "tv": "<id de l'appareil>" }` dans
   `data/home.json` : la page ne choisit pas sa télé d'après la pièce.
8. Aucun ordre n'allume ni n'éteint la box (`power` est en lecture seule). Essayer une touche,
   humain présent, sur un écran qui ne gêne personne.

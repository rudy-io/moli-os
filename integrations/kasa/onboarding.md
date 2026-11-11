# Onboarding : prise TP-Link Kasa

1. Demander l'adresse de la prise (réservation DHCP conseillée) ; vérifier que le port 9999
   répond (sinon c'est un firmware KLAP : pilote `tapo`).
2. Déclarer `kind = "kasa"` avec `host` ; `moli-os check-config`, redémarrer.
3. Tester en lecture : la prise apparaît avec son nom, `power` suit l'appareil branché.
4. Écrire `state` seulement avec la personne, sur un appareil qu'on peut couper sans risque.
   Home Assistant peut rester branché en même temps : la prise répond à chacun.

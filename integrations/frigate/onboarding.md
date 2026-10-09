# Onboarding : Frigate

1. Demander : Frigate publie-t-il sur MQTT, et sur quel broker (même que Zigbee2MQTT ?) ;
   l'adresse de son API interne (défaut `http://127.0.0.1:5000`).
2. Aucun identifiant : le broker doit accepter Moli sans mot de passe.
3. Déclarer `kind = "frigate"` (options par défaut : `127.0.0.1:1883`, sujet `frigate`) ;
   `moli-os check-config`, redémarrer.
4. Tester : chaque caméra apparaît à son premier `motion` ; une image s'affiche. Ne couper
   ni la détection ni l'enregistrement sans l'accord d'un humain.

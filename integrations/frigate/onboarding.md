# Onboarding : Frigate

1. Demander : Frigate publie-t-il sur MQTT, et sur quel broker (même que Zigbee2MQTT ?) ;
   l'adresse de son API interne (défaut `http://127.0.0.1:5000`).
2. Aucun identifiant : le broker doit accepter Moli sans mot de passe. Avec des ACL, Moli lit
   `frigate/available`, `frigate/stats`, `frigate/reviews` (les alertes), `frigate/+/+`,
   `frigate/+/+/state`, `frigate/+/status/detect`, et n'écrit que `frigate/+/<detect|recordings|snapshots>/set`.
3. Déclarer `kind = "frigate"` (options par défaut : `127.0.0.1:1883`, sujet `frigate`) ;
   `moli-os check-config`, redémarrer.
4. Le direct (facultatif) : seulement si la maison le choisit, avec un go2rtc joignable
   **sur le réseau local** (celui de Frigate, port 1984, ou un autre lisant les mêmes caméras).
   Prévenir : son API n'a pas de mot de passe et montre les adresses des caméras, identifiants
   compris ; elle ne doit jamais être exposée hors de la maison. Puis `go2rtc =
   "http://<hôte>:1984"` (sans identifiants) et `streams` si les noms des flux diffèrent des
   caméras. Loin de la maison, la page montre les images.
5. Le passé (facultatif) : créer dans Frigate un compte au rôle *viewer* (Réglages → Utilisateurs),
   déclarer `review = { host = "<hôte>" }` (port 8971, utilisateur `moli` par défaut) et ranger son
   mot de passe : `moli-os secrets set <instance> password`. Le certificat de Frigate est épinglé
   au premier contact ; pour ne rien confier au premier contact, écrire son empreinte SHA-256 dans
   `review.cert_sha256`. Avec `review`, images et direct passent par ce port : `go2rtc` n'est plus
   utile. S'il change un jour (Frigate réinstallé), le pilote s'arrête et dit quoi faire.
6. Tester : chaque caméra apparaît à son premier `motion` ; une image s'affiche ; avec go2rtc,
   le plein écran joue le direct depuis la maison. Ne couper ni la détection ni
   l'enregistrement sans l'accord d'un humain.

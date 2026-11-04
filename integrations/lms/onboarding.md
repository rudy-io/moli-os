# Onboarding : piCorePlayer / Lyrion Music Server (LMS)

1. Repérer l'adresse IP du serveur Lyrion Music Server (souvent le piCorePlayer lui-même,
   port `9000`).
2. Ajouter un bloc `[[driver]]` dans `moli.toml` avec `kind = "lms"` et `host` :
   ```toml
   [[driver]]
   id = "picoreplayer"
   kind = "lms"

   [driver.options]
   host = "192.168.1.x"
   # port = 9000
   # player = "aa:bb:cc:dd:ee:ff"   # optionnel : MAC ou nom du lecteur dans LMS (défaut : le lecteur SqueezeLite)
   name = "piCorePlayer"
   room = "Salon"
   ```
3. Vérifier avec `moli-os check-config`, puis redémarrer Moli.
4. Le lecteur apparaît dans l'encart Multimédia (Accueil et Salon) et répond aux demandes
   vocales (« joue du jazz sur piCorePlayer »).

# Onboarding : voitures (Torque Pro)

1. Un jeton par véhicule, tiré au hasard (20 caractères ou plus), gardé dans le gestionnaire
   de secrets de la maison ; dans `moli.toml`, seulement son SHA-256 :

   ```toml
   [[driver]]
   id = "voitures"
   kind = "vehicles"

   [[driver.options.vehicle]]
   id = "voiture"                  # identifiant stable, jamais renommé
   name = "Voiture"
   token_sha256 = "<sha256 du jeton, en hexadécimal>"
   # mil_key = "k101"              # PID perso 0101 de Torque (voyant + nombre de codes)

   [[driver.options.vehicle.maintenance]]
   id = "vidange"
   name = "Vidange"
   every_km = 15000
   every_months = 12
   ```
2. Cloudflare : une application Access « bypass » sur `<hôte>/api/torque` (l'adresse porte
   le jeton ; Moli le vérifie). Sans Cloudflare : Torque n'envoie qu'une fois sur le Wi-Fi de
   la maison (appelant du réseau local).
3. Torque Pro → Paramètres → Journalisation et envoi : cocher « Upload to web server »,
   adresse `https://<hôte>/api/torque/<jeton>`, « Test settings » doit répondre sans erreur ;
   choisir les PID à journaliser (vitesse, régime, liquide de refroidissement, PID 01 31
   « distance depuis l'effacement des codes » pour le compteur, PID 01 21 pour le voyant),
   activer « Enable logging » et la journalisation GPS.
4. Donner le kilométrage une fois (point `odometer_set`), puis noter le dernier entretien
   (`maintenance_done`) : les échéances partent de là.

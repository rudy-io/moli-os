# Voitures (boîtier OBD-II + Torque Pro)

- Un appareil par véhicule : en route, compteur (km), trajet en cours, voyant moteur, codes
  défaut, position GPS, vitesse, régime, températures (liquide, huile, admission, dehors),
  charge, papillon, carburant, tensions, et tout autre PID que Torque envoie (`pid_<clé>`,
  nommé comme dans Torque).
- Envoi : Torque Pro, « Upload to web server », adresse `https://<hôte>/api/torque/<jeton>`
  (GET ou POST, réponse `OK!`). Moli ne garde que le SHA-256 du jeton (`token_sha256`).
- Compteur : rares sont les voitures qui donnent leur kilométrage en OBD-II. Moli compte la
  distance (compteur « distance depuis l'effacement des codes » du calculateur si Torque le
  journalise, sinon vitesse × temps) à partir d'un relevé donné par une personne
  (`odometer_set`, ou les km d'un entretien noté).
- Voyant moteur : distance parcourue voyant allumé (PID 01 21), ou PID perso `0101`
  (`mil_key`, équation `A`), ou codes envoyés par une appli compagnon (`dtc=P0401,…`).
  **Torque n'envoie pas les codes défaut lui-même.**
- Carnet d'entretien : `[[driver.options.vehicle.maintenance]]` (`every_km`, `every_months`) ;
  `maintenance_done` = « vidange », « vidange 123456 » ou JSON (`item`, `km`, `date`, `note`).
  Points `<entretien>_km_left`, `_days_left`, `_due` ; `alert` + `detail` montrent le
  véhicule sur la page Parc. `GET /api/vehicles` : carnet complet.
- Vie privée : les positions restent dans l'historique de la maison, pour sa durée de
  conservation.
- Vérifié : tests du pilote (envois simulés). Essai réel : voir `onboarding.md`.

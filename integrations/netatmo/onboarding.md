# Onboarding : caméras Netatmo

1. Sur <https://dev.netatmo.com/apps/>, créer une application personnelle (ou ouvrir celle existante), puis générer un jeton OAuth avec les portées `read_camera`, `write_camera`, `access_camera`, `read_presence`, `write_presence`, `access_presence`, `read_camerapro`, `write_camerapro`, `access_camerapro`, `read_doorbell`, `access_doorbell`.
2. Déclarer `[[driver]]` avec `id = "netatmo"`, `kind = "netatmo"` dans `data/moli.toml`.
3. Enregistrer les 3 secrets dans le coffre chiffré par un tube (jamais en clair) :
   - `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set netatmo client_id; unset V`
   - `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set netatmo client_secret; unset V`
   - `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set netatmo refresh_token; unset V`
4. Redémarrer Moli (`docker compose restart moli-os`). Le pilote renouvelle et garde automatiquement le `refresh_token`, découvre les caméras sur le réseau local pour les vignettes en direct (`snapshot_720.jpg`), et publie les détections (`person`, `vehicle`, `animal`, `motion`) ainsi que la surveillance et le projecteur (`floodlight`).

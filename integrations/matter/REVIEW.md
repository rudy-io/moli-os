# Remise aux mainteneurs — 6 novembre 2026

Contribution prête pour revue ; les validations ci-dessous ne constituent pas une
certification ni une déclaration de stabilité de tous les chemins matériels.
Code vérifié : `f6d2e0b7ee909a94ffdce2b87d8399def42b3cde` (les ajouts suivants sont documentaires).

## Ordre de lecture proposé

1. [Coffre durable et lease](../../crates/moli-runtime/src/secrets.rs) : transactions
   partagées avec les écrivains asynchrones, fsync/rename, exclusion entre contrôleurs
   et relecture du snapshot à la prise de lease. [Adaptateur Matter](../../crates/moli-matter/src/store.rs).
2. [Pilote](../../crates/moli-matter/src/lib.rs) : identité stable, Descriptor/Reachable,
   abonnements, redécouverte et annulation/échéance du demandeur. Les commandes passent
   par le hub et la garde existants ; aucune nouvelle route HTTP.
3. [Appairage](../../crates/moli-matter/src/pairing.rs) et [Thread/BLE](thread.md) :
   attestation de production explicite, secrets via le coffre, code consommé avant
   toute action réseau ; BLE et dataset Thread uniquement sur option explicite.
4. [Dockerfile](../../Dockerfile) et [exploitation](../../docs/components/exploitation.md) :
   profil standard avec symboles inchangé, option compacte et artefact diagnostics.

La pile est un fork Git épinglé, pas une version publiée upstream :
[diff complet des quatre correctifs Matter](https://github.com/Roddygithub/matter-rust/compare/c59b61fdd7cf0b9cfcd3b1257cb87f336e59a48d...d80b1a3fd2204119ddf47406f4bb59916ba7576b).
Ils portent sur les échéances/annulations et retransmissions MRP, le rollback d'une
fabric non sauvegardée et l'encodage VID/PID commonName prévu par la norme.
Le choix d'adoption ou de remontée de ces correctifs appartient aux mainteneurs.

## Preuves et reproduction

| Contrôle | Résultat et périmètre |
|---|---|
| `sh scripts/check.sh` (image de développement musl) | 1 308 tests, fmt, clippy, catalogue, anti-fuite passent ; six avertissements de profils préexistants |
| Minimum Rust | `cargo check --locked -p moli-os` et `-p moli-matter` passent avec Rust 1.89.0, fonctionnalités par défaut |
| Option BLE compilée | 10 tests `moli-matter --features ble` passent ; aucune validation radio |
| Sécurité des dépendances | `cargo-audit` 0.22.2, base RustSec rafraîchie, 408 dépendances, exit 0 ; exceptions TLS existantes conservées et contrôlées par les 15 tests du garde d'audit |
| Avertissement d'audit restant | `rustls-pemfile` 2.2.0 non maintenu, RUSTSEC-2025-0134 ; aucun nouvel ignore ajouté |
| Interopérabilité indépendante | Binaires standard et compact : attestation, découverte, OnOff, abonnements, reprise sans code, refus d'états corrompus et de mauvaise clé ; [résultats et hash](../../tools/matter-e2e/RESULTS.md) |
| Matériel Matter IP | Pont Hue v2 : commande via Moli/Matter, observation indépendante, restauration et reprise après redémarrage ; [rapport anonymisé](HARDWARE.md) |
| Budget d'image | Compacte 24 827 312 octets ; standard 30 113 712 octets ; [mesures et limites](../../docs/PERFORMANCE.md) |

L'audit suit la configuration existante [.cargo/audit.toml](../../.cargo/audit.toml) :
les exceptions TLS sont explicitement limitées à une ancienne dépendance inutilisée
pour la vérification, et `scripts/audit-guard.py` vérifie que ce périmètre ne s'étend pas.

## Décisions de revue et limites

- Accepter le minimum Rust 1.89 et décider de la stratégie de dépendance à la pile épinglée.
- Choisir le profil de livraison : les symboles restent par défaut ; la variante compacte
  tient sous 25 Mo en échange des noms de fonctions dans les backtraces locales.
- La validation radio Thread/BLE, les appareils sur batterie et les coupures électriques
  restent non démontrés. Le matériel nécessaire n'est pas disponible chez le contributeur :
  ces essais ne sont pas une action attendue de sa part avant la revue. Les garder comme
  critères de validation par un laboratoire équipé avant toute promesse de support stable.
- Les capteurs sont couverts par des fixtures synthétiques, pas par des captures matérielles.
  Aucune comparaison HA, charge multi-appareils ou certification des budgets cgroup/p95
  n'est revendiquée. Le mode BLE reste optionnel, hors de l'image scratch standard.

Les rapports publiés ne contiennent ni codes, ni clés, ni identifiants de la maison.

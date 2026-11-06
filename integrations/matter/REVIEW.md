# Remise aux mainteneurs — Matter générique, 6 novembre 2026

La pile corrigée est livrée dans `vendor/matter-rust` avec sa licence Apache-2.0.
Les dépendances Matter utilisent des chemins internes : aucun fork personnel,
checkout voisin ou source Git n'est nécessaire au build. Les dépendances ordinaires
crates.io restent verrouillées. [Provenance et adaptations locales](../../vendor/matter-rust/MOLI-PROVENANCE.md).

## Parcours de revue

1. [Coffre durable](../../crates/moli-runtime/src/secrets.rs) et [adaptateur](../../crates/moli-matter/src/store.rs) : fsync/rename, verrou de transaction partagé, lease exclusive et relecture au changement de propriétaire.
2. [Pilote](../../crates/moli-matter/src/lib.rs) et [familles](../../crates/moli-matter/src/features.rs) : découverte bornée, capacités annoncées, Reachable, politiques dynamiques, commandes annulables et acquittements stricts.
3. [Appairage](../../crates/moli-matter/src/pairing.rs) : attestation explicite, coffre, code à usage unique consommé avant réseau. [Thread/BLE](thread.md) reste optionnel.
4. [Contrôleur livré](../../vendor/matter-rust/crates/matter-controller/src/node.rs) : échéances absolues pour invoke, invoke temporisé et write ; abandon des envois et retransmissions après annulation. Les parseurs stricts vérifient les chemins et refusent les doublons ambigus.
5. [Dashboard thermostat](../../web/src/maison/ui/ClimateCard.svelte) : cibles chauffage/refroidissement distinctes ; une commande différée garde sa clé même si le mode change.
6. [Images](../../docs/components/exploitation.md) : standard avec symboles par défaut, compacte explicite et export de l'artefact diagnostics correspondant.

La compatibilité est indépendante de la marque et décrite **par fonctions**, pas
comme une garantie de toutes les fonctions de tous les appareils certifiés :
[matrice complète](COMPATIBILITY.md). Elle couvre OnOff, variation, couleur,
stores de levage, thermostat, contrôle de serrure autorisé et les capteurs indiqués.
Les fonctions optionnelles absentes ou les politiques inconnues ne donnent pas de droits d'écriture.

## Validation logicielle

- `sh scripts/check.sh` passe : **1 323 tests Moli** et **916 tests ciblés de la pile livrée** sans fonctionnalités par défaut, fmt, clippy, catalogue, cinq régressions de l'exception anti-fuite et garde anti-fuite. Six avertissements de profils préexistants.
- Le contrôle standard rejoue maintenant les tests des trois bibliothèques localement modifiées : controller, interaction et commissioning. Les bibliothèques BLE système ne sont pas requises pour ce contrôle.
- Ensemble indépendant `vendor/matter-rust`: **1 724 tests unitaires passent**, un ignoré ; les tests nécessitant du matériel restent optionnels/ignorés.
- **24 tests** du pilote avec l'option BLE compilée passent ; aucun essai radio n'est revendiqué.
- **191 tests frontend**, build et contrôle FR/EN (3 623 clés) passent.
- `cargo check --locked -p moli-os` passe sous **Rust 1.89.0**, avec uniquement la source Moli et le cache registry, sans cache Git.
- `cargo-audit` 0.22.2 passe après actualisation RustSec ; les cinq exceptions TLS préexistantes restent contrôlées par le garde d'audit et ses 15 tests. Avertissement connu : `rustls-pemfile` 2.2.0 non maintenu ; aucun nouvel ignore.
- Les binaires de production **standard et compact** passent l'interopérabilité indépendante étendue : toutes les familles ci-dessus, validation des consignes, contrôle de serrure approuvé, refus sans transmission, changement de politique PIN, reprise, état corrompu et mauvaise clé. [Résultats, reproduction et hash](../../tools/matter-e2e/RESULTS.md).

## Taille et arbitrages

Image compacte mesurée : **24 954 289 octets**, sous 25 Mo (marge 45 711 octets).
Image standard : **30 268 337 octets**. Le choix par défaut conserve les symboles ;
la variante compacte retire les noms des backtraces locales, sans retirer de pilote.
L'artefact diagnostics est identique au binaire standard. [Mesures et limites](../../docs/PERFORMANCE.md).
Le minimum Rust 1.89 est nécessaire aux verrous de fichiers et validé ; les deux
adaptations de syntaxe de l'application préservent le contrôle de pièce avant envoi.

## Périmètre matériel

L'essai physique historique porte sur un pont Hue v2 et OnOff, avec observation
indépendante et restauration : [rapport](HARDWARE.md). Les nouvelles familles sont
validées sur des serveurs virtuels indépendants ; cela ne remplace pas une validation
sur chaque produit. Le matériel pour Thread/BLE, autres familles physiques, batteries
et coupures électriques n'est pas disponible chez le contributeur. Ces essais restent
à réaliser dans un environnement équipé avant une promesse de support stable.
Aucun nouveau code, appairage ou essai physique n'est attendu de l'utilisateur.
Aucune comparaison HA, charge multi-appareils ou certification cgroup/p95 n'est revendiquée.
Les rapports ne contiennent aucune donnée de sa maison.

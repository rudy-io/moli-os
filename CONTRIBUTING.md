# Contribuer à Moli OS

*In English: issues and pull requests are welcome, in French or English. Run the checks below
before opening a pull request, never put personal data in the repository, and give every new
text in both `locales/fr` and `locales/en`.*

Moli est partagé tel quel, sans promesse de support, mais les contributions sont bienvenues : une
nouvelle intégration, une correction, une traduction, une doc plus claire. Écrites à la main ou
avec un agent, elles passent les mêmes contrôles.

## Avant de commencer

1. Lire [`AGENTS.md`](AGENTS.md) (valable pour les humains aussi), [`docs/README.md`](docs/README.md)
   et les décisions de [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) : on ne les rediscute pas sans
   raison nouvelle.
2. Pour un gros changement, ouvrir d'abord une *issue* : on se met d'accord sur le quoi avant le
   comment.

## Les contrôles (obligatoires)

```sh
sh scripts/check.sh                 # dans l'image docker/dev.Dockerfile ; sous Windows : .\scripts\dev.ps1 sh scripts/check.sh
cd web && npm ci && npm run build   # le tableau de bord
node scripts/i18n-check.mjs         # aucune clé manquante, aucun texte en dur
```

`check.sh` enchaîne le formatage, clippy en mode pédant (`-D warnings`), tous les tests, la
vérification du catalogue d'intégrations et le contrôle anti-fuite. Les mêmes contrôles tournent
sur chaque pull request.

## Jamais de donnée personnelle

Pas de prénom, d'adresse du réseau, d'adresse MAC, de numéro de série, d'identifiant d'appareil ni
de clé, nulle part : ni dans le code, ni dans les tests, ni dans les captures d'appareils, ni dans
les messages de commit. Des valeurs fictives : `aa:bb:cc:dd:ee:ff` ou `02:00:00:00:00:01` pour une
MAC, `0x00124b0000000001` pour un appareil Zigbee, `192.168.1.x`, `user@example.com`.
`scripts/leak-check.sh` le vérifie (avec, si vous en avez une, votre propre liste noire hors du
dépôt : voir l'en-tête du script).

## Ajouter une intégration

Une intégration est un dossier `integrations/<id>/` :

- `integration.toml` : ce qu'elle couvre (marques, modèles), comment on la trouve sur le réseau,
  ce dont elle a besoin, d'où vient la connaissance du protocole (`[origin]` : jamais de code
  repris d'un projet sous une licence incompatible avec Apache-2.0) ;
- `README.md` (état, ce qui a été vérifié sur du vrai matériel : modèle, firmware, date) et
  `onboarding.md` (ce qu'un agent demande et fait pour la mettre en route) ;
- `fixtures/` : de vraies réponses de l'appareil, **rendues anonymes**, rejouées par
  `moli-os catalogue check`.

Le plus souvent, un **profil déclaratif** suffit, sans une ligne de Rust :
[`docs/components/profils.md`](docs/components/profils.md). Sinon, un pilote natif :
[`docs/components/pilotes.md`](docs/components/pilotes.md).

## Le texte

Tout texte qu'une personne lit passe par un catalogue, en français et en anglais :
[`docs/components/traduction.md`](docs/components/traduction.md). Une nouvelle langue est la
bienvenue (copier `locales/fr/`, traduire, lancer le contrôle).

## La maison des autres

Une commande sur un appareil, c'est un effet réel dans une maison habitée. Jamais d'essai dans une
pièce protégée (on y dort), une cible neutre, l'état remis comme avant. Et la performance est un
budget ([`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)) : une régression se corrige avant la fusion.

## Licence

En contribuant, vous acceptez que votre contribution soit publiée sous la licence Apache-2.0 du
projet (`LICENSE`).

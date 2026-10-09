# Moli OS — guide pour agents

Ce fichier est lu par n'importe quel agent (Claude Code, Codex…). Rien ici ne suppose un agent particulier.

## Avant tout

1. Lire `docs/README.md` (index, état, règles), puis `docs/ARCHITECTURE.md` (les décisions prises, à ne pas rediscuter sans raison nouvelle) et la doc du composant touché (`docs/components/`).
2. **La maison de l'utilisateur** : si un dossier `../moli-maison/` existe à côté de ce dépôt, lire son `AGENTS.md`. Il décrit l'installation de cette maison (machine, déploiement, règles de la famille). Rien de ce dossier n'entre jamais dans ce dépôt.
3. `git pull --ff-only` avant de commencer.
4. **Installer Moli chez quelqu'un** (pas modifier son code) : suivre `.claude/skills/installer-moli/SKILL.md`, lisible par tout agent.

## Commandes

Tout tourne en conteneur, aucune toolchain locale requise :

```
.\scripts\dev.ps1 sh scripts/check.sh   # Windows : fmt + clippy pedantic -D warnings + tests + catalogue + fuites, obligatoire avant chaque commit
sh scripts/check.sh                     # Linux, macOS : la même chose, dans l'image docker/dev.Dockerfile
cd web && npm run build && cd .. && node scripts/i18n-check.mjs   # si l'interface a changé : build et traductions
docker build -t moli-os .               # l'image (Svelte → Rust musl → scratch)
```

La CI de GitHub (`.github/workflows/check.yml`) refait tout cela sur chaque pull request.

Construire, lancer, sauvegarder, revenir en arrière : `docs/components/exploitation.md`.

## Piloter une instance

- MCP : `http://<hôte>:8790/mcp` (streamable HTTP). Liste des outils et règles : `docs/components/api.md`.
- REST + SSE : `http://<hôte>:8790/api/…` (même doc).
- Un agent est soumis à la **garde** : dans une pièce protégée ou aux heures calmes, sa commande devient une demande qu'un humain valide (202). Un automatisme écrit par un agent reste un brouillon jusqu'à la validation d'un humain.

## Règles

- Jamais de secret dans un fichier : ils vivent dans le coffre chiffré de Moli (`moli-os secrets set <instance> <nom>`, valeur par un tube, jamais au clavier ni affichée) ; la clé maître arrive par l'environnement. Ne jamais régénérer `MOLI_MASTER_KEY`.
- Jamais de donnée personnelle dans le dépôt : prénoms, domaine, adresses du réseau, adresses MAC, identifiants d'appareils, clés. Dans les tests et les captures d'appareils, des valeurs fictives (`aa:bb:cc:dd:ee:ff`, `192.168.1.x`, `user@example.com`). `scripts/leak-check.sh` le vérifie ; avec un dossier `../moli-maison/`, il applique aussi sa liste noire privée.
- Ce dépôt est public, messages de commit compris : rien qui désigne une maison, une personne ou un lieu.
- L'interface et les messages sont traduits dès l'écriture : chaque texte visible passe par `t('zone.cle')` (web) ou `tr!("zone.cle")` (Rust), dans `locales/fr` **et** `locales/en`. Voir `docs/components/traduction.md`.
- Jamais de LLM dans le chemin d'exécution des commandes : l'IA crée et explique, le moteur est déterministe.
- La performance est un budget (`docs/PERFORMANCE.md`) : une régression se corrige avant de continuer.
- Une commande sur un appareil physique = effet réel dans une maison habitée. **Jamais les pièces protégées** (on y dort, des enfants parfois). Tester sur une cible neutre et remettre l'état initial.
- Ne rien envoyer à des tiers (messages, mails) sans l'accord du propriétaire.

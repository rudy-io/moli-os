# Agent Moli pour Windows

Ce qu'il fait, sans droits administrateur, depuis la session Windows de l'utilisateur :

- **relevés toutes les 5 s** vers Moli (`POST /api/machines/<id>/report`, jeton porteur) :
  processeur, mémoire, carte graphique (un `nvidia-smi` continu), disques, réseau, les
  8 programmes les plus gourmands (un seul appel système), et la température du processeur
  quand LibreHardwareMonitor tourne ;
- **ordres reçus en réponse** (rien n'écoute sur le PC) : `shutdown` et `restart` (30 s d'avis),
  `cancel_shutdown`, `sleep`, `remote` ; un ordre n'est jamais exécuté deux fois ;
- **mode à distance** (`remote`, et à chaque ouverture de session si `remote_at_start`) :
  ouvre l'appli Claude (ses sessions se connectent à Remote Control) et l'appli Codex (son
  exec-server vers Codex Cloud) si elles ne tournent pas. Depuis le téléphone : l'appli Claude
  (onglet Code) et l'appli ChatGPT (Codex).

Coût mesuré sur un PC de bureau (7 oct. 2026) : 0,94 % d'un cœur, 251 Mo (PowerShell).

## Installer (ou mettre à jour)

Depuis la racine du dépôt cloné :

```powershell
# Le jeton sur l'entrée standard, jamais affiché ; Moli n'en garde que le SHA-256
# (agent_sha256 de la machine dans moli.toml).
<commande qui produit le jeton> | pwsh -NoProfile -File .\agent\windows\install.ps1
# Mise à jour du script seulement (jeton conservé) :
pwsh -NoProfile -File .\agent\windows\install.ps1
```

Fichiers dans `%LOCALAPPDATA%\MoliAgent\` (jamais dans un dossier synchronisé) :
`moli-agent.ps1`, `config.json`, `token.dpapi` (chiffré pour ce compte Windows), `state.json`,
`agent.log`. Tâche planifiée « Moli Agent » à l'ouverture de session, sans fenêtre
(`conhost --headless`).

## Une fois, en administrateur

```powershell
pwsh -ExecutionPolicy Bypass -File .\agent\windows\setup-admin.ps1
```

Autorise la carte filaire à réveiller le PC (veille, et arrêt si elle le permet) et installe les
capteurs de température (`install-sensors.ps1`, LibreHardwareMonitor).

## Ce qu'il faut savoir

- Après un **arrêt**, Windows attend sur l'écran de connexion : aucune session, donc ni agent ni
  applis. Pour retrouver Claude et Codex depuis le téléphone, **mettre le PC en veille** : Moli
  le réveille, la session et les applis sont toujours là.
- Désinstaller : `pwsh -File uninstall.ps1 -Confirm`.

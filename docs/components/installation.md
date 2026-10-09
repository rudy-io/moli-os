# Installer Moli chez soi

Moli tourne dans un conteneur Docker, sur n'importe quelle machine Linux allumée en permanence (mini-PC,
NAS, serveur). Home Assistant n'est pas nécessaire. Pour l'instant l'image se construit sur place (x86-64 ;
ARM à venir).

## 1. Démarrer

```sh
git clone https://github.com/rudy-io/moli-os && cd moli-os
mkdir -p data && sudo chown 1000:1000 data    # le conteneur tourne en 1000:1000
docker compose up -d --build
docker compose logs moli-os                   # chercher « code d'installation »
```

Au premier démarrage, Moli part de rien :

- il écrit une configuration de départ, `data/moli.toml`, sans aucun appareil ;
- il crée sa **clé maître**, `data/master.key`, qui chiffre le coffre des secrets (`data/secrets.enc`) ;
- il écrit dans ses journaux un **code d'installation** (`ABCD-EFGH`), valable une heure, nouveau à
  chaque démarrage tant que la maison n'a pas de code. Terminer l'installation sans attendre : un outil
  qui affiche les journaux (Portainer, Dozzle…) les montre à d'autres.

## 2. Choisir le code de la maison

Ouvrir `http://<adresse de la machine>:8790` depuis un téléphone ou un ordinateur du réseau. L'écran
« Bienvenue » demande le code d'installation, puis le code de la maison (6 à 8 chiffres, deux fois ; 4
suffisent quand un propriétaire est reconnu par Cloudflare Access). C'est
tout : la session est ouverte, le code d'installation ne sert plus à rien.

Pourquoi ce détour : seul quelqu'un qui accède à la machine lit ses journaux. Le code de la maison sert
ensuite à valider ce qui engage la maison (un automatisme écrit par un agent, une commande dans une pièce
protégée, aux heures calmes).

## 3. Mettre la clé maître à l'abri

`data/master.key` est indispensable pour relire le coffre. Les sauvegardes (`moli-os backup`) ne la
contiennent **jamais** : en garder une copie ailleurs (gestionnaire de mots de passe, clé USB rangée).
Pour aller plus loin, la fournir par l'environnement plutôt que sur le disque (`MOLI_MASTER_KEY_FILE`
vers un fichier en mémoire, depuis un gestionnaire de secrets) : voir [exploitation.md](exploitation.md).

## 4. Ajouter ses appareils

Une intégration à la fois, dans `data/moli.toml` : un bloc `[[driver]]` par pont ou appareil, puis
`docker compose restart moli-os`.

- Ce qui existe : `integrations/` (un dossier par intégration, avec son `onboarding.md`) et
  [pilotes.md](pilotes.md). L'exemple commenté complet : `moli.example.toml`.
- Les secrets (mot de passe, jeton) ne vont jamais dans `moli.toml` : ils passent par un tube, jamais au
  clavier ni à l'écran :
  `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set <instance> <nom>`.
- Avec un agent (Claude Code, Codex…) : ouvrir ce dépôt avec lui, il lit `AGENTS.md`.
- Depuis Home Assistant : les outils de `tools/home-assistant/` reprennent les pièces, les automatismes
  (en brouillons à valider), l'historique d'énergie et les clés Tuya.

## 5. Y accéder hors de chez soi (facultatif)

Ne jamais ouvrir le port 8790 sur Internet. Deux façons sûres :

- **Tailscale** (ou un autre VPN) en **routeur de sous-réseau** : le téléphone joint Moli à son adresse
  sur le réseau de la maison. Une adresse Tailscale directe (`100.x`) est traitée comme l'extérieur.
- **Cloudflare Tunnel + Access** : Moli vérifie lui-même l'identité signée par Access ; section
  `[server.access]` de `moli.toml` (`team`, `aud`, et `owners` pour les e-mails qui peuvent choisir le code
  sans connaître l'actuel). Voir [api.md](api.md).

Jamais de proxy inverse sur la même machine (Nginx, Caddy, `tailscale serve`…) devant Moli : tout le
monde y paraîtrait local, donc de la maison. Moli refuse d'ailleurs une requête locale qui dit avoir été
relayée.

## Changer ou retrouver le code

- **Changer** : page Système → « Code du tableau de bord » (le code actuel, puis le nouveau deux fois).
- **Oublié** : arrêter Moli, effacer le code et ses sessions, redémarrer ; un nouveau code d'installation
  apparaît dans les journaux.

  ```sh
  docker compose stop moli-os
  docker compose run --rm moli-os secrets forget core ui_pin
  docker compose run --rm moli-os secrets forget core ui_sessions
  docker compose start moli-os && docker compose logs moli-os
  ```

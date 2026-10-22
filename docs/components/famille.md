# Famille : les personnes, leurs droits, leurs connexions

Code : `crates/moli-api/src/people.rs` (le registre), `people_api.rs` (les routes),
`session.rs` (le code de la maison, les blocages), `caller.rs` (qui appelle). Interface :
page **Famille** (`#/famille`) du dashboard Maison.

> À ne pas confondre : un **profil**, dans Moli, est la description d'un appareil
> (`integrations/*/profile.toml`, voir [profils.md](profils.md)), pas une personne.

## Les quatre façons d'être reconnu

| Façon | Qui | Ce que ça ouvre |
|---|---|---|
| **Le réseau de la maison** | tout appareil du Wi-Fi, sans se connecter (`household`) | commander la maison, sauf les pièces protégées et les machines (décision D0) |
| **Une personne connectée** | mot de passe personnel, ou e-mail vérifié par Cloudflare Access | selon son rôle (ci-dessous) ; depuis n'importe où |
| **Le code de la maison (PIN)** | partagé, pour un écran commun (tablette murale) | valider, autoriser un agent, les pièces protégées, sur cet appareil-là (session liée à son adresse) |
| **Un téléphone appairé** | l'appli Moli, avec son jeton | envoyer ses rapports (présence, batterie), rien d'autre |

Un **adulte** ou un **administrateur** connecté n'a jamais besoin du code de la maison en plus
de son mot de passe : un seul secret par personne (`Sessions::human`). Le code de la maison
reste pour les écrans partagés, où personne n'est connecté.

Le réseau de la maison reste « la famille » : un invité à qui l'on donne le Wi-Fi peut
commander hors des pièces protégées s'il ouvre Moli sans se connecter. Pour qu'un invité n'ait
que ses pièces, il faut qu'il ne soit pas sur le réseau principal (réseau invité de la box) et
qu'il se connecte avec son compte.

## Les rôles

| Rôle (`role`) | Affiché | Peut |
|---|---|---|
| `owner` | Administrateur | tout : les personnes, le code de la maison, les intégrations |
| `member` | Adulte | commander toute la maison, valider les automatismes et les demandes des agents |
| `guest` | Invité | voir et commander **ses pièces** et **les appareils qu'on lui a donnés**, jusqu'à sa date de fin ; ni caméras, ni réglages, ni journal, ni où sont les autres |

- Un appareil est à un invité s'il est dans sa liste `devices`, ou si **toutes** ses pièces
  sont les siennes (un groupe compte les pièces de ses membres ; un appareil sans pièce n'est à
  personne). Jamais un appareil qui dit où est quelqu'un (téléphones, présence, personnes),
  même donné.
- Il reste toujours au moins un administrateur.
- La date de fin passée, l'invité ne peut plus se connecter et ses téléphones ne reçoivent
  plus rien.

## Mots de passe

- **Inviter** : l'administrateur crée la personne, puis « Inviter » donne un code de
  8 lettres valable 7 jours, à usage unique. La personne le tape sur l'écran de connexion et
  choisit son mot de passe. Pour un mot de passe oublié : inviter à nouveau.
- **Règle** (NIST SP 800-63B-4, mot de passe seul facteur) : 15 caractères au moins (200 au
  plus), pas de règle de composition. Refusés : les mots de passe les plus courants (liste du
  NCSC, `common-passwords.txt`), moins de 5 caractères différents, et ce qui n'est guère plus
  que le prénom, l'identifiant ou le nom de la maison. Conseil affiché : une phrase de trois
  ou quatre mots.
- **Gardés** dans le coffre chiffré (`core/person/<id>/password`), hachés en **Argon2id**
  (RFC 9106, réglage OWASP : 12 Mio, 3 passes, 1 voie). Un mot de passe gardé par une version
  plus ancienne (PBKDF2-SHA256) est accepté puis haché à nouveau en Argon2id à la connexion
  suivante.
- **Calcul** : hors des tâches du serveur (`spawn_blocking`), un à la fois, 8 en attente au
  plus (au-delà : « trop d'essais »). Un identifiant inconnu est vérifié contre un leurre,
  aussi lentement : la réponse ne dit jamais qui existe.
- **Blocages** : les mêmes que le code de la maison (`session.rs`) : 5 échecs par appareil,
  puis 15 minutes, doublées jusqu'à 24 h ; un budget à part pour Internet. Le blocage est
  vérifié avant le calcul, puis à nouveau en comptant l'essai.
- Changer son mot de passe ferme toutes ses connexions.

## Connexions (sessions)

- Cookie `moli_person` : `HttpOnly; SameSite=Strict; Path=/`, 30 jours, `Secure` quand la
  page arrive en HTTPS (le tunnel, ou un proxy de la maison qui envoie
  `X-Forwarded-Proto: https`). Sur le réseau local en HTTP simple, il ne peut pas l'être : le
  navigateur ne le renverrait pas.
- Moli ne garde que le SHA-256 du jeton, avec la personne, la date, l'appareil (« iPhone ·
  Safari », déduit du navigateur) et si c'était depuis Internet. 10 connexions par personne au
  plus (la plus ancienne part).
- **Mes connexions** (page Famille) : la personne voit les siennes et en ferme une
  (`DELETE /api/me/sessions/{id}`, `id` = début de l'empreinte). L'administrateur peut
  déconnecter quelqu'un partout (`DELETE /api/people/{id}/sessions`), sans changer son mot de
  passe.

## Journal

Chaque accès est écrit au journal (`Action::Access`, origine `ui`, l'appelant en acteur) :
connexion, mot de passe refusé (pour une personne connue seulement : jamais ce qu'un inconnu a
tapé), déconnexion, mot de passe changé, connexion fermée, personne ajoutée, changée, retirée,
invitée. Jamais un mot de passe ni un code.

## Routes

| Méthode | Route | Qui | Quoi |
|---|---|---|---|
| GET | `/api/people` | le foyer ; un invité ne voit que lui | personnes et zones ; l'administrateur voit aussi e-mails, pièces, appareils, fin, mot de passe posé |
| POST | `/api/people` | administrateur | ajoute (`name`, `role`, `emails?`, `color?`, `rooms?`, `devices?`, `expires?`, `macs?`) |
| PUT, DELETE | `/api/people/{id}` | administrateur | change, retire (ses connexions, son mot de passe et ses téléphones partent avec) |
| POST | `/api/people/{id}/invite` | administrateur | `{code, expires}`, montré une fois |
| PUT | `/api/people/{id}/password` | la personne elle-même | `{password}` |
| DELETE | `/api/people/{id}/sessions` | administrateur | déconnecte partout |
| GET | `/api/me/sessions` | une personne connectée (invité compris) | `{sessions:[{id, created, device, remote, current}]}` |
| DELETE | `/api/me/sessions/{id}` | une personne connectée | ferme une de ses connexions |
| POST | `/api/session` | tout le monde | `{login, password}` (prénom, identifiant ou e-mail) ou `{pin}` |
| POST | `/api/invitation` | tout le monde | `{code, password}` : choisit le mot de passe et connecte |

## Tests

`people.rs` : Argon2id et ancien PBKDF2 (accepté, haché à nouveau), réglages abusifs refusés
sans calcul, mots de passe trop faciles, appareil donné à un invité, connexions listées et
fermées, nom inconnu refusé comme un mauvais mot de passe, invitations. `session.rs` : un
adulte connecté décide sans code, un invité non, un appareil bloqué n'est pas admis à essayer.
`caller.rs` : ce qu'un invité atteint.

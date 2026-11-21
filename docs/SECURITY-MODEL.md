# Modèle de sécurité

Le texte de référence de la sécurité de Moli : ce qu'on protège, contre qui, et les règles qui ne
doivent jamais être violées, chacune avec sa preuve. `SECURITY.md` (signaler une faille, ce que Moli
croit) et la doc des composants renvoient ici plutôt que de répéter ces règles. Toute PR qui touche
au modèle d'accès met ce document à jour : un invariant ou une menace en plus, ou une preuve.

Décidé dans #136. La suite se fait par petites PR, chacune validée au vu du code (§ 6).

## 1. Ce qu'on protège, du plus grave au moins grave

1. **Sécurité physique** : serrures, portail, chauffage, machines, pièces où l'on dort.
2. **Vie privée** : caméras, présence (qui est où), historique, enregistrements.
3. **Intégrité du contrôle** : automatismes, approbations, agents. Personne ne fait agir la maison à
   la place d'une personne de la maison.
4. **Secrets** : coffre, clé maître, clés d'API des fournisseurs.
5. **Disponibilité** : la maison marche sans Internet et sans accès distant.

## 2. Qui menace

| Attaquant | Exemple |
|---|---|
| **Erreur d'installation** (la plus probable) | proxy ou VPN mal déclaré, sous-réseau trop large |
| Robot d'Internet | scans, essais de mots de passe en masse |
| Téléphone perdu ou volé | session encore ouverte |
| Ancien membre ou invité | séjour fini, accès gardé |
| Appareil compromis du réseau | prise, caméra, télé |
| Agent manipulé | injection de prompt, MCP appelé de l'extérieur |
| Page web ouverte par quelqu'un de la maison | DNS rebinding, formulaire ou script qui écrit vers Moli depuis un autre site |
| Fournisseur d'identité ou de tunnel compromis | impact total, probabilité faible |

La menace la plus probable est la mauvaise configuration : la conception la rend **difficile,
visible et refusée**, plutôt que de compter sur la doc.

## 3. Frontières de confiance

Internet → entrée déclarée (réseau de la maison, VPN, tunnel, proxy) → **Moli, qui revérifie
lui-même** → garde → pilotes → appareils.

Chaque frontière vérifie sans se fier à la précédente. Moli ne tient jamais l'identité d'un en-tête
qu'un intermédiaire lui passe en clair.

## 4. Invariants

« Prouvé » : un test le casse s'il est violé. « À venir » : l'étape du § 6 qui apportera la preuve.

| # | Invariant | Preuve |
|---|---|---|
| I1 | **Refus par défaut** : un chemin d'accès ou une route non déclarés sont refusés. | Prouvé pour les routes : `crates/moli-api/tests/routes.txt` et `routes.rs`. Entrées : étape 2. |
| I2 | **La position réseau ne prouve l'identité** que sur le réseau de la maison et les VPN déclarés. | Tests de `caller.rs` ; entrées déclarées : étape 2. |
| I3 | **L'identité est une preuve cryptographique** vérifiée par Moli (mot de passe, assertion signée, plus tard passkey), jamais un en-tête en clair. | Assertion Cloudflare Access : tests de `access.rs`. Générique : étape 4. |
| I4 | **Moindre privilège** : les droits à distance sont un sous-ensemble des droits à la maison ; MCP reste à la maison sauf choix contraire ; un agent qui n'a qu'à lire se branche sur `/mcp/readonly` (les 3 outils d'écriture n'y existent pas), et la maison coupe un outil pour tous (`[mcp] disabled_tools`). | Colonne `distant` de `routes.txt` (`/mcp` et `/mcp/readonly` `non`, vérifié) ; outils absents et refusés vérifiés par `tests/http.rs` (`mcp_readonly_route_refuses_every_write`, `mcp_tool_switched_off_by_the_house_is_gone`) ; appliquée aux entrées distantes : étape 2. |
| I5 | **Échec fermé** : config invalide, fournisseur d'identité injoignable, fichier illisible = refus, jamais ouverture ; un fichier illisible n'est jamais écrasé. | `a_file_that_is_not_valid_is_never_overwritten` (règles de l'assistant), tests de `people.rs`, `a_trim_keeps_the_lines_it_could_not_read_aside` (historique des automatismes) ; entrées : étape 2. |
| I6 | **Sûr par défaut** : l'accès distant est coupé tant qu'aucune entrée n'est déclarée ; les combinaisons dangereuses sont refusées au démarrage. | `check-config` : étape 2. |
| I7 | **Indépendance locale** : rien de la maison ne dépend de l'accès distant ni d'un compteur ; un bouton coupe tout le distant. | Étape 3. |
| I8 | **Traçabilité** : chaque événement distant est journalisé avec la vraie IP, sans secret ; les propriétaires sont avertis d'un nouvel appareil. | Avis d'appairage d'un téléphone (existant) ; journal d'authentification : étape 5. |
| I9 | **Surface minimale** : Moli n'écoute jamais Internet directement ; garde `Host` sur toutes les routes (un nom que la maison n'a pas déclaré est refusé : le DNS rebinding passe par un nom, jamais par une IP) ; une écriture qu'un navigateur envoie depuis un autre site (`Origin`, `Sec-Fetch-Site`) est refusée. Le réseau de la maison fait confiance au tableau de bord, pas à toute page que ses navigateurs ouvrent (`crates/moli-api/src/guard.rs`, [api.md § 1.2](components/api.md)). | Garde `Host` : `foreign_host_is_rejected_everywhere` et tests de `guard.rs` ; écoute publique refusée : étape 2. |
| I10 | **Révocable et limité dans le temps** : chaque session et chaque appareil, seul ou tous ensemble. | Sessions PIN de 30 jours (existant) ; par appareil : étape 3. |

### TaHoma cloud : observation privée et déconnexion

Les routes du complément cloud exigent un propriétaire authentifié ; elles restent classées
`humain non` dans l'inventaire, avec une vérification propriétaire supplémentaire dans leur
extracteur. La déconnexion est accessible même après retrait de la configuration cloud. Elle
efface uniquement les données cloud de l'instance choisie, sans retirer les secrets du pilote
local. Elle n'envoie aucune commande à un équipement ou à un scénario.

Une déconnexion invalide la génération persistante avant de purger le catalogue ; une ancienne
écriture encore en cours ne doit pas pouvoir le recréer. Les observations sont chiffrées dans
un fichier séparé du coffre, et une observation identique ne réécrit pas ce fichier. La dernière
lecture conservée après redémarrage ne prouve jamais la disponibilité actuelle du compte.

Preuves : `crates/moli-api/tests/overkiz.rs` (propriétaire, refus des invités et déconnexion),
les tests du stockage privé dans `crates/moli-runtime/src/` (génération, concurrence, écriture
durable et chiffrement), `crates/moli-overkiz/src/cloud/runtime_tests.rs` (session, reprise et
indépendance locale) et `crates/moli-os/src/backup.rs` (restauration sans cache plus récent).

### Airzone : autoriser et révoquer le pilotage

Les routes d'installation Airzone exigent une personne authentifiée avant de lire le corps
de la demande ; elles sont classées `humain non` dans l'inventaire. Une session encore valide
ne suffit pas pour activer le pilotage : le serveur vérifie aussi le rôle propriétaire et le
code fourni dans cette demande, avec le même compteur d'essais que la connexion. Ce code ne
parvient ni au gestionnaire des passerelles ni au journal. Le retour en lecture seule reste
accessible à une personne authentifiée sans nouveau code.

Un `enabled = false` ou `read_only = true` explicite dans le TOML reste prioritaire sur le
registre. Si ce registre ne peut pas être enregistré, retirer une passerelle ou révoquer son
pilotage prend tout de même effet en mémoire ; l'interface indique que ce résultat n'est pas
garanti après redémarrage. Une restauration ancienne sans registre ne conserve pas les
autorisations d'un registre plus récent.

Preuves : les tests `airzone_control_requires_a_fresh_code_in_the_request_not_only_a_session`,
`airzone_control_denies_non_owners_even_with_the_code_but_allows_revocation` et
`airzone_fresh_code_attempts_share_the_session_lockout` dans `crates/moli-api/tests/http.rs` ; les
tests de modes dans `crates/moli-os/src/managed_airzone/tests/modes.rs` ;
`legacy_restore_clears_newer_airzone_registry_but_preserves_unrelated_omissions` dans
`crates/moli-os/src/backup.rs`.

### Garde : le code à chaque fois, ce que les agents peuvent

Deux règles de `[guard]`, écrites par une personne dans le fichier, jamais par un agent :

- `[[guard.code_each_time]]` (point + valeurs, toutes si aucune) : cet ordre attend le code
  tapé pour lui, même d'une personne dont la session est ouverte (désarmer l'alarme). Il reste
  en attente ; valider sans code est refusé (`CodeNeeded`) et il attend toujours. Le code
  arrive dans la demande de validation (`pin`), vérifié comme à la connexion et avec le même
  compteur d'essais, et seulement pour une personne en session (sans session, aucun code n'est
  essayé). Seul un automatisme validé passe sans : **le valider ou l'allumer demande donc le code**
  dès qu'il porte un tel ordre (un `toggle` sur le point compte), une session seule ne suffit
  pas. Les ordres que la maison envoie en son nom (`system` : scènes, luminaires, charges
  pilotées) sont retenus comme les autres, et une scène ou une charge qui en viserait un est
  refusée au démarrage. La valeur se compare par son sens, une fois validée pour le point.
- `[[guard.agents_may]]` (point + valeurs) : un agent (l'assistant, une voix, MCP, un script)
  peut donner cet ordre lui-même sur une commande ou un appareil protégé, sans qu'une personne
  le valide (armer l'alarme à la voix). Les pièces protégées et les heures calmes s'appliquent
  toujours, et une valeur non listée attend une personne comme avant. Donc sans effet avec
  `dashboard = "pin"` (tout ordre y attend le code), ni avec `protect_unassigned` pour un
  appareil sans pièce.

Sans ces sections, rien ne change. Preuves : `some_orders_want_the_code_each_time_even_from_a_session`,
`the_house_s_own_orders_and_toggles_want_the_code_each_time_too` et
`agents_may_what_a_person_allowed_them_in_the_file` dans `crates/moli-runtime/tests/hub.rs` ;
`a_rule_reads_the_value_by_meaning_not_by_spelling` dans `crates/moli-runtime/src/guard.rs` ;
`the_code_on_a_decision_is_tried_only_in_a_session` (code faux, compteur partagé avec la
connexion, code juste sans session) et `an_automation_that_disarms_is_approved_with_the_code`
dans `crates/moli-api/tests/household.rs` ;
`a_scene_never_carries_an_order_that_wants_the_code_each_time` dans `crates/moli-os/src/config.rs`.

Limite connue : un automatisme validé **avant** qu'une règle `code_each_time` soit écrite
continue de tourner ; après avoir ajouté la règle, le revalider (avec le code). Moli les nomme
au démarrage (avertissement au journal) : la validation retient si le code a été tapé
(`approved_with_code`, hors de l'empreinte, jamais repris de ce qui est enregistré). Un
automatisme validé avant cette version est nommé aussi tant qu'il n'est pas revalidé. Preuve :
`an_automation_approved_before_the_rule_is_named_at_start` dans `crates/moli-automation/tests/engine.rs`.

## 5. Règles de conception

Ce que les PR suivantes respectent, décidé dans #136 :

1. **Rien ne casse sans changement de config.** Le réseau de la maison et `[server.access]`
   marchent comme avant. Une entrée nouvelle (`vpn`, `proxy`, `tunnel`) s'ajoute, elle ne devient
   pas obligatoire. Ce qui restreint l'accès existant par Cloudflare Access (colonne `distant`,
   authentification récente) se choisit.
2. **Proxy** : l'IP du client n'est crue que venant d'une adresse de proxy déclarée, et seulement
   le dernier saut (la valeur ajoutée par ce proxy, pas celles que le client a pu écrire avant).
   Tout autre en-tête de relais est celui d'un inconnu. Un proxy ou un tunnel n'est jamais « le
   foyer ».
3. **La force brute ne bloque jamais la maison** : comptée par (IP réelle, compte) ; le réseau de
   la maison ne dépend d'aucun compteur.
4. **Agents** : MCP coupé à distance par défaut ; s'il est ouvert, un jeton dédié, limité et qui
   expire, et la garde s'applique toujours.
5. **Ce qui ouvre ou engage la maison** demande à distance une authentification récente, pas une
   session ancienne.
6. **Moli ne fait pas le bouncer** : CrowdSec, fail2ban ou le pare-feu bloquent devant Moli, à
   partir de son journal d'authentification (étape 5).

## 6. Étapes

1. Ce document et la colonne `distant` de `routes.txt`.
2. Entrées déclarées (`lan`, `vpn`, `proxy`, `tunnel`) et règles de `check-config` (I1, I2, I4,
   I5, I6, I9).
3. Sessions par appareil, révocation, bouton « couper l'accès distant » (I7, I10).
4. Identité signée générique (JWKS : Cloudflare Access, Authentik, Authelia, Pomerium, Keycloak)
   et authentification récente (I3, I4).
5. Journal d'authentification, fichiers CrowdSec et fail2ban dans `tools/`, batterie d'attaque
   contre une instance jetable (I8).
6. Passkeys et TOTP.

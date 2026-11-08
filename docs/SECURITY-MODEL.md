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
| I4 | **Moindre privilège** : les droits à distance sont un sous-ensemble des droits à la maison ; MCP reste à la maison sauf choix contraire. | Colonne `distant` de `routes.txt` (MCP `non` vérifié) ; appliquée aux entrées distantes : étape 2. |
| I5 | **Échec fermé** : config invalide, fournisseur d'identité injoignable, fichier illisible = refus, jamais ouverture ; un fichier illisible n'est jamais écrasé. | `a_file_that_is_not_valid_is_never_overwritten` (règles de l'assistant), tests de `people.rs`, `a_trim_keeps_the_lines_it_could_not_read_aside` (historique des automatismes) ; entrées : étape 2. |
| I6 | **Sûr par défaut** : l'accès distant est coupé tant qu'aucune entrée n'est déclarée ; les combinaisons dangereuses sont refusées au démarrage. | `check-config` : étape 2. |
| I7 | **Indépendance locale** : rien de la maison ne dépend de l'accès distant ni d'un compteur ; un bouton coupe tout le distant. | Étape 3. |
| I8 | **Traçabilité** : chaque événement distant est journalisé avec la vraie IP, sans secret ; les propriétaires sont avertis d'un nouvel appareil. | Avis d'appairage d'un téléphone (existant) ; journal d'authentification : étape 5. |
| I9 | **Surface minimale** : Moli n'écoute jamais Internet directement ; garde `Host` sur toutes les routes. | Garde `Host` : `foreign_host_is_rejected_everywhere` ; écoute publique refusée : étape 2. |
| I10 | **Révocable et limité dans le temps** : chaque session et chaque appareil, seul ou tous ensemble. | Sessions PIN de 30 jours (existant) ; par appareil : étape 3. |

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

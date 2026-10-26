# Jura (Wi-Fi Connect, application J.O.E.)

- Aucun code repris. Le protocole local du Wi-Fi Connect (TCP 51515) est décrit par
  jura-connect (makefu, MIT, `docs/PROTOCOL.md`) et l'intégration Home Assistant de
  dominikgysin (MIT) ; Moli le parle lui-même (`crates/moli-jura`). Les trames de référence des
  tests ont été produites par jura-connect, elle-même vérifiée sur des machines réelles.
- Trame : `*`, un octet de clé, le texte brouillé (permutation par quartet sous deux tables
  fixes, sa propre inverse : un brouillage, pas un chiffrement), `\r\n` ; cinq octets
  échappés (`0x1B` puis l'octet XOR `0x80`). Le texte finit lui-même par `\r\n`.
- Une seule connexion à la fois : Moli ouvre une session courte toutes les 20 s (5 s quand la
  machine travaille), et l'application J.O.E. trouve la place entre deux. Une E8 accepte une
  nouvelle session 1 s après la précédente ; Moli attend 3 s.
- Les noms des boissons et les réglages dépendent du modèle (groupe J.O.E.). Seul l'E8 (EC),
  `EF1092`, est décrit ici, d'après sa machine réelle ; un autre modèle a les codes communs, ses
  autres boissons sous leur code (`drink_28`), et pas de réglages. Les fichiers de l'application
  J.O.E. (propriété de Jura) ne sont pas copiés.
- Vérifié sur une E8 (EC) réelle (firmware TT237W V08.27), par des sondes en lecture seule :
  appairage avec code de sécurité, état et alertes (`@TF:`, bits du plus fort au plus faible),
  compteurs et pourcentages d'entretien (`@TG:43`, `@TG:C0` ; 255 = sans objet, comme le
  détartrage quand un filtre est en place), compteurs par boisson (`@TR:32`, total = somme),
  recettes (`@TM:41`, somme de contrôle juste sur les 12), réglages (`@TM:<argument>`).
- Une boisson est préparée avec la recette que la machine garde (celle réglée à son écran ou
  dans J.O.E.) : la recette relue est la commande de préparation (`@TP:`), l'octet 8 mis à 1.
  Sur l'E8, l'espresso donne exactement la commande avec laquelle un E6 a été servi
  (jura-connect). Les doubles (2 Espressi, 2 Cafés) n'ont pas de recette propre : celle de la
  boisson simple, sous le code du double — **pas encore essayé sur la machine**.
- Pas encore essayés sur la machine réelle : préparer une boisson, les entretiens (`@TG:21` à
  `@TG:26`), annuler (`@TG:FF`), écrire un réglage (`@TS:01`, `@TM:<argument>,<valeur><somme>`,
  `@TS:00`, relu). L'intégration Home Assistant les a vus marcher sur une E8 (SDS) pour le café,
  les rinçages et l'arrêt automatique ; nettoyage, détartrage et filtre nulle part.
- Pas fait : préparation avec des réglages changés pour une fois, minuteur, verrouillage,
  mise à jour de la clé (peut la bloquer).

# Onboarding : VELUX ACTIVE with NETATMO (passerelle KIX 300)

1. Demander : les VELUX sont-ils dans l'application VELUX ACTIVE ? Avec quel e-mail se
   connecte-t-on ? Le mot de passe, jamais.
2. Les identifiants vont au coffre, tapés par la personne :
   `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set velux email`,
   puis la même chose avec `password`.
3. Déclarer `kind = "velux"` (`id = "velux"`) ; `moli-os check-config`, redémarrer.
4. Vérifier : le journal dit `velux modules loaded` ; fenêtres et stores apparaissent avec leur
   ouverture, le capteur avec ses mesures. Ils arrivent dans les pièces de l'application
   VELUX (qui comptent pour la garde) : les ranger dans les pièces de Moli.
5. Essayer un store d'une pièce où personne ne dort : Ouvrir, Stop, Fermer depuis sa ligne de
   pièce ; il bouge en quelques secondes, Moli suit. Remettre comme avant.
6. Les fenêtres de toit demandent la clé de la passerelle : la personne près de la passerelle,
   mise en appairage par le cloud (`retrieve_key`), appui sur son bouton quand le voyant
   clignote, échange local sur le port 25050 (voir `docs/components/pilotes.md`) ; la clé va au
   coffre par un tube (`hash_sign_key`, `sign_key_id`), jamais affichée. Redémarrer : le journal
   dit `windows=true`. Une fenêtre demande le code de la maison au dashboard, et l'accord d'un
   humain à un agent.

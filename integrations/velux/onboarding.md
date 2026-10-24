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
5. Pour l'instant, lecture seule : bouger un store depuis l'application, le voir changer dans
   Moli dans la minute.

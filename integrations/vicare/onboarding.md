# Onboarding : Viessmann (ViCare)

1. Demander : l'e-mail et le mot de passe du compte de l'application **ViCare** (celui où
   l'installation apparaît), et si le compte a plusieurs installations, laquelle suivre.
2. Créer une clé d'API sur le portail développeur Viessmann
   (`developer.viessmann-climatesolutions.com` → API Keys) : **client public** (sans secret),
   avec l'adresse de redirection `vicare://oauth-callback/everest` (à défaut, une autre adresse
   déclarée, donnée dans l'option `redirect_uri`). Le *Client ID* est la clé.
3. L'humain range les trois valeurs par un tube, jamais au clavier dans un fichier :
   `moli-os secrets set <instance> client_id`, puis `username`, puis `password`.
4. Déclarer `kind = "vicare"` (option `installation = <numéro>` seulement s'il y a plusieurs
   installations, `redirect_uri` seulement pour une clé qui en a une autre) ;
   `moli-os check-config`, redémarrer.
5. Vérifier : la température extérieure, celle du ballon ECS, le mode de chaque circuit doivent
   suivre ceux de l'application ViCare (la lecture a lieu toutes les 2 min 30). Un compte à
   double authentification ou connecté par un service tiers (Google, Apple) ne passe pas par
   cette connexion.
6. Tester une commande **sur une cible neutre** (la consigne ECS, pas la chambre d'un enfant) et
   remettre l'état comme avant : l'effet est réel sur le chauffage de la maison.

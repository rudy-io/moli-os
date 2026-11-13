# Viessmann (pompes à chaleur et chaudières, via le cloud ViCare)

Pilote natif `crates/moli-vicare`, **lecture et commandes**.

- **Pourquoi le cloud** : la passerelle Wi-Fi (Vitoconnect) n'ouvre aucun port sur le réseau
  de la maison ; elle ne parle qu'au cloud Viessmann.
- **Transport** : `api.viessmann-climatesolutions.com/iot/v2` et `iam.viessmann-climatesolutions.com/idp/v3`,
  **HTTPS seulement**. Connexion comme l'application ViCare, sans navigateur : e-mail et mot de
  passe (Basic) contre un code d'autorisation (PKCE S256), puis jeton d'accès d'une heure,
  renouvelé par le jeton de rafraîchissement. Une seule requête lit toute la PAC, toutes les
  2 min 30 : le cloud limite à environ 1 450 appels par jour et 120 par 10 minutes ; un refus
  pour quota attend l'heure de reprise que le cloud indique.
  La connexion sans navigateur n'est pas le parcours documenté par Viessmann (l'API, elle,
  l'est). Elle ne passe pas avec une double
  authentification ni un compte Google ou Apple. Des identifiants refusés arrêtent le pilote
  (à corriger puis redémarrer), pour ne pas faire bloquer le compte.
- **Appareils** : la PAC (`pac` : températures extérieure, départ, retour, source, ballon
  tampon et ECS ; compresseur : actif, phase, charge, démarrages, heures ; COP ; ECS en charge)
  et un appareil par circuit de chauffage (`circuit<n>`, nommé comme dans ViCare : mode,
  programme actif, consignes, températures de pièce et de départ, pompe, hors gel).
- **Commandes** : mode du circuit (parmi ceux que le cloud liste), consignes confort / normale /
  réduite, confort activé, consignes ECS, charge unique de l'ECS. Les bornes viennent des
  commandes du cloud, jamais supposées. Un point sans commande acceptée, ou dont la commande
  n'a pas de bornes, reste en lecture seule. Un mode, une phase ou un programme inconnu s'affiche
  avec son identifiant brut.
- **Essai réel** : le 2026-11-13, sur une PAC (régulation CU401B_S), après les corrections de la
  revue : une consigne d'eau chaude changée depuis la page Chauffage, vue dans l'appli ViCare,
  puis remise comme avant.
- **Origine** : voir `integration.toml`. Réponses réelles capturées puis anonymisées (`fixtures/`).
- **Pas encore** : plannings (chauffage, ECS), courbe de chauffe, vacances, ventilation,
  refroidissement, appareils autres que la PAC (thermostat d'ambiance, gestion de la demande),
  consommations électriques (l'installation de référence n'en expose pas).

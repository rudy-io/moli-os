# Onboarding : NAS Synology (DSM)

1. Demander : l'adresse du NAS (nom ou IP fixe), le port HTTPS de DSM (5001 si inchangé) et, pour
   pouvoir le réveiller, si Wake-on-LAN est activé (Panneau de configuration → Matériel et
   alimentation → Général).
2. **Créer un compte DSM pour Moli** (pas le compte personnel), **sans double authentification**.
   Lire seulement suffit à l'état du NAS ; les ordres `shutdown` et `reboot` demandent un compte du
   groupe **administrators**. Dire ce choix à l'humain : un compte administrateur dans le coffre de
   Moli peut éteindre le NAS, et un agent n'y touche qu'avec son accord.
3. L'humain range les deux valeurs par un tube, jamais au clavier dans un fichier :
   `printf %s "$V" | moli-os secrets set <instance> username`, puis `password`.
4. Déclarer `kind = "synology"` avec `host` (et `port`, `poll_secs`, `mac` seulement si besoin) ;
   `moli-os check-config`, redémarrer. Au premier contact, Moli épingle le certificat du NAS
   (auto-signé) avant d'envoyer le mot de passe.
   Si le NAS présente un certificat Let's Encrypt renouvelé automatiquement, l'empreinte épinglée
   changera à chaque renouvellement : le dire à l'humain (voir le README).
5. Vérifier : la température, l'occupation des volumes, l'état de chaque disque et le dernier
   résultat de chaque tâche Hyper Backup doivent être ceux de DSM. Un point resté « inconnu » :
   le compte n'a pas le droit, ou la version de DSM nomme le champ autrement (le dire, c'est
   la partie à corriger). Les lectures ont été vérifiées sur DSM 7.4 avec un compte administrateur.
6. Tester `wake` et `shutdown` **un humain présent** : l'effet est réel, le NAS sert la maison.
   Ne jamais éteindre un NAS pendant une sauvegarde ; pour le réveil, l'éteindre à la main d'abord.
7. Si DSM bloque l'adresse de Moli (« identifiants refusés »), la débloquer dans DSM (Panneau de
   configuration → Sécurité → Protection) avant de corriger les identifiants.

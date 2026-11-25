# Onboarding : Proxmox VE

1. Demander : l'adresse du serveur (nom ou IP fixe), le port HTTPS (8006 si inchangé), le nom du nœud
   s'il y en a plusieurs.
2. **Créer un utilisateur et un jeton pour Moli**, en lecture seule. Sur le serveur, en root :
   `pveum user add moli@pve`, `pveum acl modify / --users moli@pve --roles PVEAuditor`, puis
   `pveum user token add moli@pve moli --privsep 0`. Le secret du jeton n'est affiché qu'une fois.
   (Ou dans l'interface : Centre de données → Permissions → Utilisateurs, puis Jetons d'API.)
3. L'humain range le secret par un tube, jamais au clavier dans un fichier :
   `printf %s "$SECRET" | moli-os secrets set <instance> token_secret`.
4. Déclarer `kind = "proxmox"` avec `host` et `token_id = "moli@pve!moli"` (et `port`, `node`,
   `poll_secs`, `full_percent`, `vm_full_percent`, `backup_hours` seulement si besoin) ; `moli-os check-config`,
   redémarrer. Au premier contact, Moli épingle le certificat du serveur avant d'envoyer le jeton.
5. Vérifier : processeur, mémoire et version du nœud, l'état de chaque conteneur, l'occupation des
   stockages et le dernier résultat de sauvegarde doivent être ceux de l'interface de Proxmox.
